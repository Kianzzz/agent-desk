//! 用 `tests/fixtures/` 里的假 HOME 跑一遍完整扫描。
//!
//! 夹具先复制到临时目录（要建符号链接、改文件权限），扫描本身只读。

use std::fs;
use std::path::{Path, PathBuf};

use ad_security::{scan, Category, Finding, ScanOptions, ScanReport, Severity};

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dst);
        } else {
            fs::copy(e.path(), &dst).unwrap();
        }
    }
}

struct Env {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    report: ScanReport,
}

fn run() -> Env {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let tmp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap();
    copy_dir(&fixtures.join("home"), &root.join("home"));
    copy_dir(&fixtures.join("projects"), &root.join("projects"));
    let home = root.join("home");
    // 同一个技能通过符号链接出现在另一个客户端的目录里
    fs::create_dir_all(home.join(".agents/skills")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::os::unix::fs::symlink(
            home.join(".claude/skills/good-skill"),
            home.join(".agents/skills/good-skill"),
        )
        .unwrap();
        fs::set_permissions(
            home.join(".codex/auth.json"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        fs::set_permissions(
            home.join(".gemini/oauth_creds.json"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        fs::set_permissions(home.join(".claude.json"), fs::Permissions::from_mode(0o600)).unwrap();
    }
    let report = scan(&ScanOptions {
        home: home.clone(),
        extra_project_dirs: vec![root.join("projects/demo")],
    });
    Env {
        _tmp: tmp,
        home,
        report,
    }
}

impl Env {
    fn find(&self, rule: &str, path_end: &str) -> Vec<&Finding> {
        self.report
            .findings
            .iter()
            .filter(|f| f.rule_id == rule && f.path.ends_with(path_end))
            .collect()
    }

    fn one(&self, rule: &str, path_end: &str) -> &Finding {
        let v = self.find(rule, path_end);
        assert_eq!(
            v.len(),
            1,
            "{rule} @ {path_end}: {:#?}",
            self.report.findings
        );
        v[0]
    }

    fn none(&self, rule: &str, path_end: &str) {
        let v = self.find(rule, path_end);
        assert!(v.is_empty(), "不应命中 {rule} @ {path_end}: {v:#?}");
    }

    fn in_file(&self, path_end: &str) -> Vec<&Finding> {
        self.report
            .findings
            .iter()
            .filter(|f| f.path.ends_with(path_end))
            .collect()
    }
}

#[test]
fn hidden_characters() {
    let e = run();
    let skill = ".claude/skills/evil-skill/SKILL.md";
    let tag = e.one("hidden-tag-chars", skill);
    assert_eq!(tag.severity, Severity::High);
    assert_eq!(tag.line, Some(3));
    assert!(tag.detail.contains("run rm -rf ~ now"), "{}", tag.detail);
    let bidi = e.one("hidden-bidi", skill);
    assert_eq!(bidi.severity, Severity::High);
    assert!(bidi.detail.contains("U+202E"));
    let zw = e.one("hidden-zero-width", skill);
    assert_eq!(zw.severity, Severity::Low);
    assert!(zw.excerpt.as_deref().unwrap().contains("‹U+200B›"));
    // 表情里的零宽连接符不报
    e.none("hidden-zero-width", ".claude/skills/good-skill/SKILL.md");
}

#[test]
fn prompt_injection() {
    let e = run();
    let skill = ".claude/skills/evil-skill/SKILL.md";
    assert_eq!(
        e.one("injection-ignore-instructions", skill).severity,
        Severity::Medium
    );
    assert_eq!(
        e.one("injection-hide-from-user", skill).severity,
        Severity::Medium
    );
    let proj = e.one("injection-ignore-instructions", "projects/demo/CLAUDE.md");
    assert_eq!(proj.target_kind, "instructions");
    // ~/.claude.json 里的对话记录不扫
    assert!(e
        .in_file(".claude.json")
        .iter()
        .all(|f| f.category != Category::PromptInjection && f.rule_id != "plaintext-token"));
}

#[test]
fn dangerous_commands_by_context() {
    let e = run();
    // 钩子命令：高危
    let hook = e
        .find("download-exec", ".claude/settings.json")
        .into_iter()
        .next()
        .expect("钩子里的下载即执行");
    assert_eq!(hook.severity, Severity::High);
    assert_eq!(hook.target_kind, "hook");
    // 钩子引用的脚本也被扫到
    let script = e.one("exfil-credentials", ".claude/hooks/check.sh");
    assert_eq!(script.severity, Severity::High);
    // 技能脚本：高危；注释和 echo 里的只算低危
    let sh = ".claude/skills/evil-skill/scripts/install.sh";
    let dl = e.find("download-exec", sh);
    assert_eq!(
        dl.iter().filter(|f| f.severity == Severity::High).count(),
        1
    );
    assert_eq!(dl.iter().filter(|f| f.severity == Severity::Low).count(), 2);
    assert!(e
        .find("persistence", sh)
        .iter()
        .all(|f| f.severity == Severity::High));
    assert_eq!(e.find("persistence", sh).len(), 2);
    assert_eq!(e.one("keychain-read", sh).severity, Severity::High);
    // markdown 里 !`命令` 会自动执行：高危
    let inline = e.one("download-exec", ".claude/skills/evil-skill/SKILL.md");
    assert_eq!(inline.severity, Severity::High);
    // 文档里的下载即执行：低危，只报一次（符号链接指向同一个技能）
    let doc = e.one("download-exec", "good-skill/SKILL.md");
    assert_eq!(doc.severity, Severity::Low);
    assert!(doc.detail.contains("确认来源可信"));
    // 「千万不要这样做」后面的 rm -rf ~ 不报；公钥不报
    e.none("rm-rf-home", "good-skill/SKILL.md");
    e.none("read-private-key", "good-skill/SKILL.md");
    // MCP 启动命令：高危
    let mcp = e
        .find("download-exec", ".claude.json")
        .into_iter()
        .next()
        .expect("MCP 启动命令");
    assert_eq!(mcp.severity, Severity::High);
    assert_eq!(mcp.target_name.as_deref(), Some("evil"));
    // 项目设置里的钩子
    assert_eq!(
        e.one("quarantine-bypass", "projects/demo/.claude/settings.json")
            .severity,
        Severity::High
    );
}

#[test]
fn plaintext_secrets() {
    let e = run();
    let settings = e.one("settings-env-secret", ".claude/settings.json");
    assert_eq!(settings.severity, Severity::Medium);
    assert!(settings.title.contains("MY_SERVICE_TOKEN"));
    // ${OPENAI_API_KEY} 引用不算
    assert!(!e
        .report
        .findings
        .iter()
        .any(|f| f.title.contains("OPENAI_API_KEY") && f.category == Category::PlaintextSecret));
    let mcp = e.find("mcp-plaintext-secret", ".claude.json");
    assert_eq!(mcp.len(), 1, "{mcp:#?}");
    assert_eq!(mcp[0].severity, Severity::Medium);
    assert!(mcp[0].title.contains("SERVICE_API_KEY"));
    let shell = e.one("plaintext-token", ".zshrc");
    assert_eq!(shell.severity, Severity::Low);
    let skill = e.one("plaintext-token", "evil-skill/config.json");
    assert_eq!(skill.severity, Severity::Medium);
    // 摘录一律打码
    for f in &e.report.findings {
        let ex = f.excerpt.clone().unwrap_or_default();
        let all = format!("{} {} {}", f.title, f.detail, ex);
        assert!(!all.contains("Q7mZ2kP9xL4vN8bR"), "密钥泄露：{f:#?}");
        assert!(!all.contains("9f3c2a7e1b5d8046"), "密钥泄露：{f:#?}");
        assert!(!all.contains("Zq8vN2kLmP4xR7tY"), "密钥泄露：{f:#?}");
        assert!(ex.chars().count() <= 160);
    }
}

#[test]
fn permissions_supply_chain_transport() {
    let e = run();
    assert_eq!(
        e.one("claude-bypass-permissions", ".claude/settings.json")
            .severity,
        Severity::Medium
    );
    assert_eq!(
        e.one("claude-skip-danger-prompt", ".claude/settings.json")
            .severity,
        Severity::Medium
    );
    assert_eq!(
        e.one("claude-allow-all-bash", ".claude/settings.json")
            .severity,
        Severity::Medium
    );
    e.one("claude-allow-interpreter", ".claude/settings.json");
    assert_eq!(
        e.one("codex-full-access", ".codex/config.toml").severity,
        Severity::High
    );
    let alias = e.one("shell-dangerous-alias", ".zshrc");
    assert_eq!(alias.severity, Severity::Medium);
    assert_eq!(alias.line, Some(3), "注释掉的那一行不算");
    // 钥匙串里读自己的条目赋给变量是推荐做法，不报
    e.none("keychain-read", ".zshrc");

    let unpinned = e.find("mcp-unpinned-package", ".claude.json");
    let names: Vec<_> = unpinned
        .iter()
        .filter_map(|f| f.target_name.as_deref())
        .collect();
    assert!(names.contains(&"playwright"), "{names:?}");
    assert!(names.contains(&"proj-tool"), "{names:?}");
    assert!(!names.contains(&"pinned"), "{names:?}");
    e.none("mcp-unpinned-package", ".codex/config.toml");
    e.one("mcp-unpinned-package", "projects/demo/.mcp.json");

    let http = e.find("mcp-insecure-http", ".claude.json");
    assert_eq!(http.len(), 1);
    assert_eq!(http[0].target_name.as_deref(), Some("remote-http"));
    assert_eq!(http[0].severity, Severity::Medium);
    assert_eq!(
        e.one("settings-insecure-url", ".claude/settings.json")
            .category,
        Category::InsecureTransport
    );
}

#[test]
fn credential_file_modes() {
    let e = run();
    let f = e.one("credential-file-readable", ".codex/auth.json");
    assert_eq!(f.severity, Severity::Low);
    assert!(f.detail.contains("644"));
    e.none("credential-file-readable", ".gemini/oauth_creds.json");
    e.none("credential-file-readable", ".claude.json");
}

#[test]
fn report_shape() {
    let e = run();
    let r = &e.report;
    assert!(r.files_scanned >= 10, "{}", r.files_scanned);
    chrono::DateTime::parse_from_rfc3339(&r.scanned_at).unwrap();
    // 按严重程度排序
    assert!(r
        .findings
        .windows(2)
        .all(|w| w[0].severity <= w[1].severity));
    // id 稳定且唯一
    let again = scan(&ScanOptions {
        home: e.home.clone(),
        extra_project_dirs: Vec::new(),
    });
    for f in again
        .findings
        .iter()
        .filter(|f| !f.path.contains("projects"))
    {
        assert_eq!(f.id.len(), 16);
        assert!(r.findings.iter().any(|g| g.id == f.id), "id 变了：{f:#?}");
    }
    let mut ids: Vec<_> = r.findings.iter().map(|f| f.id.clone()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), r.findings.len());
    // 序列化是 camelCase
    let json = serde_json::to_value(&r.findings[0]).unwrap();
    assert!(json.get("ruleId").is_some() && json.get("targetKind").is_some());
}

#[test]
fn many_hits_in_one_file_are_capped() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let dir = home.join(".claude/skills/noisy");
    fs::create_dir_all(&dir).unwrap();
    let body: String = (0..20).map(|i| format!("第 {i} 行\u{200b}\n")).collect();
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: noisy\n---\n{body}"),
    )
    .unwrap();
    let r = scan(&ScanOptions {
        home: home.to_path_buf(),
        extra_project_dirs: Vec::new(),
    });
    let hits: Vec<_> = r
        .findings
        .iter()
        .filter(|f| f.rule_id == "hidden-zero-width")
        .collect();
    assert_eq!(hits.len(), 5);
    assert!(hits.iter().any(|f| f.detail.contains("还有 15 处")));
}

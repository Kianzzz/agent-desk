//! 扫描的主流程：先读结构化配置（MCP、权限、钩子、凭据文件），再把文本文件（技能、
//! 说明、钩子脚本、shell 配置）并行扫一遍，最后去重、限流、排序。

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Instant;

use rayon::prelude::*;
use regex::Regex;

use crate::config;
use crate::finding::{self, Draft, Target};
use crate::rules::commands::{self, Keychain};
use crate::rules::{hidden, injection, secrets};
use crate::sources;
use crate::text::{self, Lines, MdContext};
use crate::{Category, Finding, ScanOptions, ScanReport, Severity};

/// 一段文本所处的上下文，决定同一个模式有多严重。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ctx {
    /// 会被直接执行：脚本、钩子命令、MCP 启动命令、shell 配置
    Exec,
    /// 说明文字：markdown、纯文本
    Doc,
    /// 数据和配置文件：json、yaml、toml、.env
    Data,
}

/// 文本文件在 AI 工具里扮演的角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    /// 技能目录里的文件
    Skill,
    /// 斜杠命令、子代理、Codex 提示词
    Command,
    /// CLAUDE.md / AGENTS.md / GEMINI.md
    Instructions,
    /// 钩子命令里引用到的脚本
    HookScript,
    /// ~/.zshrc 之类
    Shell,
}

pub(crate) struct Job {
    pub path: PathBuf,
    pub role: Role,
    pub target: Target,
    /// 标题里的位置描述，比如「技能「x」」「项目「y」的 CLAUDE.md」
    pub label: String,
}

pub(crate) struct Scanner {
    pub home: PathBuf,
    seen: HashSet<PathBuf>,
    /// 还没认领（没解析真实路径）的文本文件
    pending: Vec<Job>,
    pub jobs: Vec<Job>,
    pub findings: Vec<Finding>,
    pub files: u32,
}

impl Scanner {
    fn new(home: &Path) -> Self {
        Scanner {
            home: home.to_path_buf(),
            seen: HashSet::new(),
            pending: Vec::new(),
            jobs: Vec::new(),
            findings: Vec::new(),
            files: 0,
        }
    }

    /// 认领一个文件：同一真实路径只扫一次。返回真实路径。
    pub(crate) fn claim(&mut self, p: &Path) -> Option<PathBuf> {
        let real = fs::canonicalize(p).ok()?;
        self.seen.insert(real.clone()).then_some(real)
    }

    /// 加一个要扫的文本文件。真实路径在 [`Scanner::flush`] 里批量并行解析。
    pub(crate) fn push_job(&mut self, p: &Path, role: Role, target: Target, label: String) {
        if sources::skip_file(p) {
            return;
        }
        self.pending.push(Job {
            path: p.to_path_buf(),
            role,
            target,
            label,
        });
    }

    /// 并行解析待扫文件的真实路径，按加入顺序去重后放进任务列表。
    fn flush(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        let reals: Vec<Option<PathBuf>> = pending
            .par_iter()
            .map(|j| fs::canonicalize(&j.path).ok())
            .collect();
        for (mut job, real) in pending.into_iter().zip(reals) {
            let Some(real) = real else { continue };
            if self.seen.insert(real.clone()) {
                job.path = real;
                self.jobs.push(job);
            }
        }
    }

    /// `~/.claude/x` 这种短写法，用在说明文字里。
    pub(crate) fn short(&self, p: &Path) -> String {
        match p.strip_prefix(&self.home) {
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => p.display().to_string(),
        }
    }
}

pub(crate) fn run(opts: &ScanOptions) -> ScanReport {
    let started = Instant::now();
    let home = &opts.home;
    let mut s = Scanner::new(home);

    // 一、结构化配置
    let claude_projects = config::scan_configs(&mut s);

    // 二、全局说明文件、命令、技能、shell 配置
    for (rel, name) in [
        (".claude/CLAUDE.md", "~/.claude/CLAUDE.md"),
        (".codex/AGENTS.md", "~/.codex/AGENTS.md"),
        (".gemini/GEMINI.md", "~/.gemini/GEMINI.md"),
    ] {
        s.push_job(
            &home.join(rel),
            Role::Instructions,
            Target::new("instructions", name),
            format!("全局说明文件 {name}"),
        );
    }
    for rel in [".claude/commands", ".claude/agents", ".codex/prompts"] {
        add_command_dir(&mut s, &home.join(rel), None);
    }
    for rel in [
        ".claude/skills",
        ".codex/skills",
        ".agents/skills",
        ".gemini/skills",
    ] {
        add_skill_root(&mut s, &home.join(rel), 3, None);
    }
    add_skill_root(&mut s, &home.join(".claude/plugins"), 7, None);
    for rel in [
        ".zshrc",
        ".zprofile",
        ".zshenv",
        ".bashrc",
        ".bash_profile",
        ".profile",
        ".config/fish/config.fish",
    ] {
        let p = home.join(rel);
        let name = rel.rsplit('/').next().unwrap_or(rel).to_string();
        s.push_job(
            &p,
            Role::Shell,
            Target::new("shell", name),
            format!("Shell 配置 ~/{rel}"),
        );
    }
    // 三、项目目录
    let mut dirs: Vec<PathBuf> = opts.extra_project_dirs.clone();
    dirs.extend(claude_projects.into_iter().map(PathBuf::from));
    dirs.extend(sources::codex_session_cwds(home));
    for dir in sources::dedupe_projects(home, dirs) {
        scan_project(&mut s, &dir);
    }
    // 四、文本文件并行扫
    s.flush();
    let results: Vec<Option<Vec<Finding>>> = s.jobs.par_iter().map(scan_job).collect();
    for r in results.into_iter().flatten() {
        s.files += 1;
        s.findings.extend(r);
    }

    let findings = finding::finalize(std::mem::take(&mut s.findings));
    ScanReport {
        scanned_at: chrono::Local::now().to_rfc3339(),
        files_scanned: s.files,
        duration_ms: started.elapsed().as_millis() as u64,
        findings,
    }
}

fn add_skill_root(s: &mut Scanner, root: &Path, depth: usize, project: Option<&str>) {
    let mut dirs_files: Vec<(String, String, PathBuf)> = Vec::new();
    for dir in sources::skill_dirs(root, depth) {
        let name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        let label = match project {
            Some(p) => format!("项目「{p}」的技能「{name}」"),
            None => format!("技能「{name}」"),
        };
        dirs_files.push((name, label, dir));
    }
    let walked: Vec<Vec<PathBuf>> = dirs_files
        .par_iter()
        .map(|(_, _, dir)| sources::walk_files(dir, 8, 2000))
        .collect();
    for ((name, label, _), files) in dirs_files.into_iter().zip(walked) {
        for f in files {
            s.push_job(
                &f,
                Role::Skill,
                Target::new("skill", name.clone()),
                label.clone(),
            );
        }
    }
}

fn add_command_dir(s: &mut Scanner, dir: &Path, project: Option<&str>) {
    if !dir.is_dir() {
        return;
    }
    for f in sources::walk_files(dir, 4, 500) {
        let name = f
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        let label = match project {
            Some(p) => format!("项目「{p}」的命令文件 {name}"),
            None => format!("命令文件 {}", s.short(&f)),
        };
        s.push_job(&f, Role::Command, Target::new("instructions", name), label);
    }
}

fn scan_project(s: &mut Scanner, dir: &Path) {
    let pname = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("?")
        .to_string();
    for f in [
        "CLAUDE.md",
        "CLAUDE.local.md",
        ".claude/CLAUDE.md",
        "AGENTS.md",
        "AGENTS.override.md",
        "GEMINI.md",
    ] {
        let p = dir.join(f);
        if p.is_file() {
            s.push_job(
                &p,
                Role::Instructions,
                Target::new("instructions", format!("{pname}/{f}")),
                format!("项目「{pname}」的 {f}"),
            );
        }
    }
    for f in [".claude/settings.json", ".claude/settings.local.json"] {
        let p = dir.join(f);
        if p.is_file() {
            config::claude_settings(
                s,
                &p,
                &format!("项目「{pname}」的 Claude Code 设置"),
                Some(dir),
            );
        }
    }
    let mcp = dir.join(".mcp.json");
    if mcp.is_file() {
        config::mcp_json_file(s, &mcp, &format!("项目「{pname}」"));
    }
    add_skill_root(s, &dir.join(".claude/skills"), 2, Some(&pname));
    add_command_dir(s, &dir.join(".claude/commands"), Some(&pname));
    add_command_dir(s, &dir.join(".claude/agents"), Some(&pname));
}

/// 处理建议，按角色说。
fn advice(role: Role) -> &'static str {
    match role {
        Role::Skill => "不认识或看不懂，就先停用这个技能。",
        Role::Command => "不认识或看不懂，就先删掉这个命令文件。",
        Role::Instructions => "如果不是你自己写的，检查一下这个文件是从哪来的。",
        Role::HookScript => "不认识或看不懂，就先删掉引用它的钩子。",
        Role::Shell => "不是你自己加的，就删掉这一行。",
    }
}

static SHELL_ALIAS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?-u:\b)(claude|codex|gemini|qwen|opencode)(?-u:\b)[^\n#]*?(--dangerously-skip-permissions|--dangerously-bypass-approvals-and-sandbox|--yolo(?-u:\b)|--permission-mode[ =]+bypassPermissions|--approval-mode[ =]+yolo)").unwrap()
});

static INLINE_EXEC_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"!`([^`\n]+)`").unwrap());

/// 脚本里这一行看起来是检测规则（正则、grep 模式），不是要执行的命令。
fn pattern_like(line: &str) -> bool {
    const MARKS: &[&str] = &[
        r"\|",
        r"\s",
        r"\b",
        ".*",
        "[^",
        "(?",
        "re.compile",
        "RegExp(",
        "regex",
        "pattern",
        "PATTERN",
    ];
    MARKS.iter().any(|m| line.contains(m))
}

/// 脚本里这一行是在打印说明文字，不是执行命令。
fn printing(prefix: &str) -> bool {
    const MARKS: &[&str] = &[
        "echo ",
        "echo\t",
        "printf ",
        "print(",
        "console.log",
        "console.error",
        "console.warn",
        "logger.",
        "log.info",
        "log.warn",
        "log.error",
        "puts ",
        "help=",
        "description=",
    ];
    MARKS.iter().any(|m| prefix.contains(m))
}

fn is_comment(line: &str) -> bool {
    let t = line.trim_start();
    (t.starts_with('#') && !t.starts_with("#!"))
        || t.starts_with("//")
        || t.starts_with("/*")
        || t.starts_with("* ")
        || t.starts_with("--")
        || t.starts_with("REM ")
}

/// 扫一个文本文件。读不了（太大、二进制、不存在）返回 None。
fn scan_job(job: &Job) -> Option<Vec<Finding>> {
    let text = text::read_text(&job.path, text::MAX_FILE_BYTES)?;
    let path = job.path.display().to_string();
    let ctx = match job.role {
        Role::Shell | Role::HookScript => Ctx::Exec,
        Role::Instructions => Ctx::Doc,
        _ => sources::file_ctx(&job.path, &text),
    };
    let lines = Lines::new(&text);
    let md = (ctx == Ctx::Doc && sources::is_markdown(&job.path)).then(|| MdContext::new(&lines));
    let mut out = Vec::new();
    let excerpt_at = |start: usize, end: usize| {
        let no = lines.line_no(start);
        let r = lines.range(no);
        let s = start.clamp(r.start, r.end) - r.start;
        let e = end.clamp(r.start, r.end) - r.start;
        (no, text::excerpt(lines.text(no), s..e.max(s)))
    };

    // 一、隐藏字符
    for h in hidden::scan(&text) {
        let (no, ex) = excerpt_at(h.start, h.end);
        out.push(hidden_finding(&h, &path, no, ex, &job.target, &job.label));
    }

    // 二、提示词注入（脚本、说明文字；数据文件里多是测试用例，不扫）
    if ctx != Ctx::Data && job.role != Role::Shell {
        for h in injection::scan(&text) {
            let no = lines.line_no(h.start);
            if ctx == Ctx::Doc {
                if injection::quoted(&text, h.start) {
                    continue;
                }
                let mut around = String::new();
                for i in no.saturating_sub(2).max(1)..=(no + 2).min(lines.count()) {
                    around.push_str(lines.text(i));
                    around.push('\n');
                }
                if injection::discussing(&around) {
                    continue;
                }
            }
            let (no, ex) = excerpt_at(h.start, h.end);
            let detail = if h.rule.severity == Severity::Low {
                h.rule.why.to_string()
            } else {
                format!(
                    "{}不一定是恶意的，但值得打开看看上下文。{}",
                    h.rule.why,
                    advice(job.role)
                )
            };
            out.push(
                Draft {
                    rule_id: h.rule.id,
                    severity: h.rule.severity,
                    category: Category::PromptInjection,
                    title: format!("{}里有{}", job.label, h.rule.what),
                    detail,
                    path: &path,
                    line: Some(no),
                    matched: &text[h.start..h.end],
                    excerpt: Some(ex),
                    target: &job.target,
                }
                .build(),
            );
        }
    }

    // 三、危险命令
    for h in commands::scan(&text) {
        let no = lines.line_no(h.start);
        let line = lines.text(no);
        let r = lines.range(no);
        let prefix = &text[r.start..h.start.max(r.start)];
        let mut eff = ctx;
        if ctx == Ctx::Exec
            && job.role != Role::Shell
            && (is_comment(line) || printing(prefix) || pattern_like(line))
        {
            eff = Ctx::Doc;
        }
        if job.role == Role::Shell && is_comment(line) {
            continue;
        }
        let matched = &text[h.start..h.end];
        let Some((sev, what, extra)) = grade_command(h.rule, matched, line, eff, job.role) else {
            continue;
        };
        if eff != Ctx::Exec && text::warned_context(&lines, md.as_ref(), no) {
            continue;
        }
        let (no, ex) = excerpt_at(h.start, h.end);
        out.push(command_finding(
            h.rule, sev, what, &extra, eff, job, &path, no, matched, ex,
        ));
    }
    // markdown 里 !`命令` 会在技能/命令加载时自动执行
    if ctx == Ctx::Doc && matches!(job.role, Role::Skill | Role::Command) {
        for c in INLINE_EXEC_RE.captures_iter(&text) {
            let inner = c.get(1).unwrap();
            for h in commands::scan(inner.as_str()) {
                let start = inner.start() + h.start;
                let end = inner.start() + h.end;
                let matched = &text[start..end];
                let no = lines.line_no(start);
                let Some((sev, what, extra)) =
                    grade_command(h.rule, matched, lines.text(no), Ctx::Exec, job.role)
                else {
                    continue;
                };
                let (no, ex) = excerpt_at(start, end);
                let mut f = command_finding(
                    h.rule,
                    sev,
                    what,
                    &extra,
                    Ctx::Exec,
                    job,
                    &path,
                    no,
                    matched,
                    ex,
                );
                f.title = text::spaced(&format!("{}加载时会自动执行{}", job.label, what));
                out.push(f);
            }
        }
    }

    // 四、明文密钥
    let shell = job.role == Role::Shell;
    for h in secrets::find_tokens(&text) {
        let no = lines.line_no(h.range.start);
        let line = lines.text(no);
        // 文档里的示例：同一行说明是示例/假的
        if ctx == Ctx::Doc && is_example_line(line) {
            continue;
        }
        let (no, ex) = excerpt_at(h.range.start, h.range.end);
        let (sev, title, detail) = if shell {
            (
                Severity::Low,
                format!("{}里直接写着{}", job.label, h.kind.label),
                shell_secret_detail(),
            )
        } else {
            (
                Severity::Medium,
                format!("{}里有明文的{}", job.label, h.kind.label),
                file_secret_detail(job.role),
            )
        };
        out.push(
            Draft {
                rule_id: "plaintext-token",
                severity: sev,
                category: Category::PlaintextSecret,
                title,
                detail,
                path: &path,
                line: Some(no),
                matched: &text[h.range.clone()],
                excerpt: Some(ex),
                target: &job.target,
            }
            .build(),
        );
    }
    let env_file = sources::file_ctx(&job.path, "") == Ctx::Data
        && job
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(".env") || n.ends_with(".env"));
    if shell || env_file || ctx == Ctx::Exec || ctx == Ctx::Data {
        let quoted_only = !(shell || env_file || is_shell_script(&job.path));
        for a in secrets::find_assignments(&text, quoted_only) {
            let no = lines.line_no(a.value_range.start);
            if shell && is_comment(lines.text(no)) {
                continue;
            }
            let (no, ex) = excerpt_at(a.value_range.start, a.value_range.end);
            let (title, detail) = if shell {
                (
                    format!("{}里直接写着 {} 的值", job.label, a.name),
                    shell_secret_detail(),
                )
            } else {
                (
                    format!("{}里把 {} 的值直接写在文件里", job.label, a.name),
                    file_secret_detail(job.role),
                )
            };
            out.push(
                Draft {
                    rule_id: "plaintext-assignment",
                    severity: Severity::Low,
                    category: Category::PlaintextSecret,
                    title,
                    detail,
                    path: &path,
                    line: Some(no),
                    matched: &text[a.value_range.clone()],
                    excerpt: Some(ex),
                    target: &job.target,
                }
                .build(),
            );
        }
    }
    // 同一行已经按前缀认出令牌的，不再按变量名重复报
    let token_lines: HashSet<Option<u32>> = out
        .iter()
        .filter(|f| f.rule_id == "plaintext-token")
        .map(|f| f.line)
        .collect();
    out.retain(|f| !(f.rule_id == "plaintext-assignment" && token_lines.contains(&f.line)));

    // 五、shell 里让 AI 跳过确认的 alias / 函数
    if shell {
        for m in SHELL_ALIAS_RE.captures_iter(&text) {
            let whole = m.get(0).unwrap();
            let no = lines.line_no(whole.start());
            let line = lines.text(no);
            if is_comment(line) {
                continue;
            }
            let tool = m.get(1).unwrap().as_str().to_lowercase();
            let flag = m.get(2).unwrap().as_str();
            let (no, ex) = excerpt_at(whole.start(), whole.end());
            let what = match tool.as_str() {
                "codex" => "不经确认、也不受沙箱限制地运行",
                _ => "跳过所有权限确认",
            };
            out.push(
                Draft {
                    rule_id: "shell-dangerous-alias",
                    severity: Severity::Medium,
                    category: Category::BroadPermission,
                    title: format!("Shell 配置里让 {tool} 默认{what}"),
                    detail: format!(
                        "这一行让每次运行 {tool} 都自动带上 {flag}：AI 执行任何命令、改任何文件都不再问你，一旦被网页、文档里的恶意内容诱导，也会直接照做。建议去掉这个 alias，只在可信的项目里临时手动加这个参数。"
                    ),
                    path: &path,
                    line: Some(no),
                    matched: line.trim(),
                    excerpt: Some(ex),
                    target: &job.target,
                }
                .build(),
            );
        }
    }
    Some(out)
}

fn is_shell_script(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()).unwrap_or_default(),
        "sh" | "bash" | "zsh" | "env"
    )
}

/// 文档里同一行写明了是示例、占位符。
fn is_example_line(line: &str) -> bool {
    let low = line.to_lowercase();
    [
        "example", "示例", "例如", "比如", "sample", "fake", "假的", "占位",
    ]
    .iter()
    .any(|w| low.contains(w))
}

pub(crate) fn shell_secret_detail() -> String {
    "把密钥 export 在 shell 配置里是常见做法，但本机任何程序都能读到这个文件，AI 工具执行的每条命令也都能看到这个环境变量。更稳妥的做法是存进 macOS 钥匙串，启动时用 security find-generic-password -w 读出来。".to_string()
}

pub(crate) fn file_secret_detail(role: Role) -> String {
    let extra = match role {
        Role::Skill => "技能目录经常被同步、备份到 Git 或分享给别人，密钥会跟着一起流出去。",
        _ => "",
    };
    format!(
        "密钥直接写在文件里，任何能读这个文件的程序（包括 AI 工具本身）都能拿到。{extra}建议改成从环境变量或钥匙串读取；如果这个文件分享或上传过，尽快到服务商那里作废这把密钥并重新生成。"
    )
}

/// 按上下文给一条危险命令定级。返回 None 表示这一处不报。
/// 返回 (级别, 标题里的「有……」, 说明里的补充)。
pub(crate) fn grade_command(
    rule: &'static commands::Rule,
    matched: &str,
    line: &str,
    ctx: Ctx,
    role: Role,
) -> Option<(Severity, &'static str, String)> {
    let exec = ctx == Ctx::Exec;
    if rule.id == commands::KEYCHAIN_ID {
        return match commands::keychain(matched) {
            Keychain::Dump => Some((
                if exec { Severity::High } else { Severity::Low },
                "导出整个钥匙串的命令",
                "导出钥匙串会拿到你保存的所有网站、Wi-Fi 和应用密码。".to_string(),
            )),
            Keychain::Sensitive(svc) => Some((
                if exec { Severity::High } else { Severity::Low },
                "从钥匙串读取敏感密码的命令",
                match svc {
                    Some(s) => format!("它读的是钥匙串里的「{}」，这通常是浏览器、系统或别的应用的密码，不是这个工具自己存的。", text::safe_text(&s)),
                    None => "它没有指明读哪个条目，可能拿到任意一个保存的密码。".to_string(),
                },
            )),
            Keychain::Own(svc) => {
                // 读自己存的那一项是推荐做法：shell 配置里赋给变量、文档里提到都不报
                if !exec || role == Role::Shell && (line.contains("$(") || line.contains('`')) {
                    return None;
                }
                Some((
                    Severity::Low,
                    "从钥匙串读取密码的命令",
                    format!("它读的是钥匙串里名为「{}」的条目。把密钥存在钥匙串里再读出来是推荐做法，确认这个条目确实是给它用的就行。", text::safe_text(&svc)),
                ))
            }
        };
    }
    let sev = if exec { Some(rule.exec) } else { rule.doc };
    sev.map(|s| (s, rule.what, String::new()))
}

#[allow(clippy::too_many_arguments)]
fn command_finding(
    rule: &'static commands::Rule,
    sev: Severity,
    what: &str,
    extra: &str,
    ctx: Ctx,
    job: &Job,
    path: &str,
    no: usize,
    matched: &str,
    excerpt: String,
) -> Finding {
    let (title, detail) = if ctx == Ctx::Exec {
        (
            format!("{}里有{}", job.label, what),
            format!(
                "{}{}这段命令会被直接执行，不会先问你。{}",
                rule.why,
                extra,
                advice(job.role)
            ),
        )
    } else {
        (
            format!("{}里提到了{}", job.label, what),
            format!(
                "文档里出现了这条命令，如果是让 AI 执行的步骤，确认来源可信。{}{}",
                rule.why, extra
            ),
        )
    };
    Draft {
        rule_id: rule.id,
        severity: sev,
        category: Category::DangerousCommand,
        title,
        detail,
        path,
        line: Some(no),
        matched,
        excerpt: Some(excerpt),
        target: &job.target,
    }
    .build()
}

pub(crate) fn hidden_finding(
    h: &hidden::Hit,
    path: &str,
    line: usize,
    excerpt: String,
    target: &Target,
    label: &str,
) -> Finding {
    let cps = hidden::codepoint_list(&h.codepoints);
    let count = if h.count > 1 {
        format!("（这一行共 {} 个）", h.count)
    } else {
        String::new()
    };
    let (rule_id, severity, title, detail) = match h.kind {
        hidden::Kind::Tag => (
            "hidden-tag-chars",
            Severity::High,
            format!("{label}里藏着看不见的 Unicode 标签字符"),
            format!(
                "第 {line} 行有 Unicode 标签字符{count}。它们在编辑器里完全看不见，但 AI 会原样读到，可以用来藏一整段指令。{}建议删掉这些字符；来源不明的话先停用。",
                if h.decoded.trim().is_empty() {
                    String::new()
                } else {
                    format!("解码出来的内容是：「{}」。", text::safe_text(&h.decoded))
                }
            ),
        ),
        hidden::Kind::Bidi => (
            "hidden-bidi",
            Severity::High,
            format!("{label}里有双向控制符，看到的顺序和实际内容不一致"),
            format!(
                "第 {line} 行有双向控制符 {cps}{count}。它会让屏幕上显示的文字顺序和实际字符顺序不同，可以把一条命令伪装成另一条。正常的中英文文档用不到它，建议删掉。"
            ),
        ),
        hidden::Kind::ZeroWidth => (
            "hidden-zero-width",
            Severity::Low,
            format!("{label}里有看不见的零宽字符"),
            format!(
                "第 {line} 行有零宽字符 {cps}{count}。它在编辑器里看不见，但 AI 会读到。多数是从网页复制时带进来的，偶尔也被用来藏内容。建议在编辑器里搜索并删掉。"
            ),
        ),
    };
    Draft {
        rule_id,
        severity,
        category: Category::HiddenChars,
        title,
        detail,
        path,
        line: Some(line),
        matched: &format!("{}:{}", line, cps),
        excerpt: Some(excerpt),
        target,
    }
    .build()
}

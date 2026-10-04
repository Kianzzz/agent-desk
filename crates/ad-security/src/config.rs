//! 结构化配置：Claude Code / Codex / Gemini / Cursor / Claude 桌面版的 MCP、权限、钩子，
//! 以及凭据文件的权限。
//!
//! `~/.claude.json` 很大、含对话记录，只按 JSON 解析 `mcpServers` 和 `projects[*].mcpServers`，
//! 不对整个文件跑任何文本规则。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::engine::{self, Ctx, Role, Scanner};
use crate::finding::{Draft, Target};
use crate::rules::commands::{self};
use crate::rules::{hidden, secrets};
use crate::text::{self, Lines};
use crate::{Category, Severity};

/// `~/.claude.json` 最多读多大。它只做 JSON 解析，所以比普通文件宽。
const CLAUDE_JSON_MAX: u64 = 256 * 1024 * 1024;

/// 读所有结构化配置，返回 `~/.claude.json` 里记录的项目目录。
pub(crate) fn scan_configs(s: &mut Scanner) -> Vec<String> {
    let home = s.home.clone();
    for (rel, label) in [
        (".claude/settings.json", "Claude Code 设置"),
        (".claude/settings.local.json", "Claude Code 本地设置"),
    ] {
        claude_settings(s, &home.join(rel), label, None);
    }
    codex_config(s, &home.join(".codex/config.toml"));
    hooks_file(s, &home.join(".codex/hooks.json"), "Codex 钩子", None);
    gemini_settings(s, &home.join(".gemini/settings.json"));
    let projects = claude_json(s, &home.join(".claude.json"));
    mcp_json_file(
        s,
        &home.join("Library/Application Support/Claude/claude_desktop_config.json"),
        "Claude 桌面版",
    );
    mcp_json_file(s, &home.join(".cursor/mcp.json"), "Cursor");
    for root in crate::sources::plugin_roots(&home.join(".claude/plugins"), 6) {
        let name = root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        hooks_file(
            s,
            &root.join("hooks/hooks.json"),
            &format!("插件「{name}」的钩子"),
            Some(&root),
        );
        mcp_json_file(s, &root.join(".mcp.json"), &format!("插件「{name}」"));
    }
    credential_files(s);
    claude_dir_configs(s);
    projects
}

/// 读一份配置文件并记账。读不了返回 None。
fn read_config(s: &mut Scanner, p: &Path, max: u64) -> Option<(PathBuf, String)> {
    let real = s.claim(p)?;
    let text = text::read_text(&real, max)?;
    s.files += 1;
    Some((real, text))
}

/// 整份配置文件里的隐藏字符。
fn hidden_in_file(s: &mut Scanner, path: &str, text: &str, target: &Target, label: &str) {
    let lines = Lines::new(text);
    for h in hidden::scan(text) {
        let no = lines.line_no(h.start);
        let r = lines.range(no);
        let ex = text::excerpt(lines.text(no), (h.start - r.start)..(h.end - r.start));
        s.findings
            .push(engine::hidden_finding(&h, path, no, ex, target, label));
    }
}

fn json_key(k: &str) -> String {
    serde_json::to_string(k).unwrap_or_default()
}

// ------------------------------------------------------------------ Claude Code 设置

/// `settings.json` / `settings.local.json`：权限、环境变量、钩子和其他会执行命令的设置。
pub(crate) fn claude_settings(s: &mut Scanner, p: &Path, label: &str, project: Option<&Path>) {
    let Some((real, text)) = read_config(s, p, text::MAX_FILE_BYTES) else {
        return;
    };
    let path = real.display().to_string();
    let target = Target::new("settings", s.short(&real));
    hidden_in_file(s, &path, &text, &target, label);
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return;
    };
    let at = |needle: &str| text::line_of(&text, needle);
    let push = |s: &mut Scanner,
                rule_id: &str,
                severity: Severity,
                category: Category,
                title: String,
                detail: String,
                line: Option<usize>,
                matched: &str| {
        let excerpt = line.map(|no| {
            let l = Lines::new(&text);
            let t = l.text(no);
            text::excerpt(t, 0..t.len().min(1))
        });
        s.findings.push(
            Draft {
                rule_id,
                severity,
                category,
                title,
                detail,
                path: &path,
                line,
                matched,
                excerpt,
                target: &target,
            }
            .build(),
        );
    };

    // 权限
    let mode = v
        .pointer("/permissions/defaultMode")
        .or_else(|| v.get("permissionMode"))
        .or_else(|| v.get("defaultMode"))
        .and_then(|m| m.as_str());
    if mode == Some("bypassPermissions") {
        push(
            s,
            "claude-bypass-permissions",
            Severity::Medium,
            Category::BroadPermission,
            format!("{label}默认跳过所有权限确认"),
            "权限模式设成了 bypassPermissions：AI 执行任何命令、改任何文件都不再问你。一旦它被网页或文档里的恶意内容诱导，也会直接照做。建议改回默认模式，只在可信项目里临时放开。".into(),
            at("bypassPermissions"),
            "bypassPermissions",
        );
    }
    if v.get("skipDangerousModePermissionPrompt")
        .and_then(|b| b.as_bool())
        == Some(true)
    {
        push(
            s,
            "claude-skip-danger-prompt",
            Severity::Medium,
            Category::BroadPermission,
            format!("{label}关掉了「跳过权限」模式的警告"),
            "skipDangerousModePermissionPrompt 为 true 时，进入跳过所有权限确认的模式不会再提醒你。配合 --dangerously-skip-permissions 使用时，AI 的每个操作都不会再经过你。建议关掉这个开关，让风险提示保留。".into(),
            at("skipDangerousModePermissionPrompt"),
            "skipDangerousModePermissionPrompt",
        );
    }
    if v.get("enableAllProjectMcpServers")
        .and_then(|b| b.as_bool())
        == Some(true)
    {
        push(
            s,
            "claude-enable-all-project-mcp",
            Severity::Low,
            Category::BroadPermission,
            format!("{label}自动启用项目里的所有 MCP 服务"),
            "打开任何项目时，它的 .mcp.json 里写的 MCP 服务都会自动运行，不再逐个问你。从网上克隆的项目可能借此在你电脑上运行程序。建议关掉，需要时逐个批准。".into(),
            at("enableAllProjectMcpServers"),
            "enableAllProjectMcpServers",
        );
    }
    if let Some(allow) = v.pointer("/permissions/allow").and_then(|a| a.as_array()) {
        for rule in allow.iter().filter_map(|r| r.as_str()) {
            let r = rule.trim();
            let compact: String = r.chars().filter(|c| !c.is_whitespace()).collect();
            if matches!(
                compact.as_str(),
                "Bash" | "Bash(*)" | "Bash(:*)" | "Bash(**)" | "Bash()" | "Bash(*:*)"
            ) {
                push(
                    s,
                    "claude-allow-all-bash",
                    Severity::Medium,
                    Category::BroadPermission,
                    format!("{label}允许 AI 不经确认运行任意命令"),
                    format!("权限白名单里有「{r}」，等于允许 AI 不经你确认就执行任何命令，包括删除文件、上传数据。建议换成具体的命令，比如 Bash(git status:*)。"),
                    at(&json_key(rule)),
                    rule,
                );
            } else if let Some(prog) = interpreter_wildcard(r) {
                push(
                    s,
                    "claude-allow-interpreter",
                    Severity::Low,
                    Category::BroadPermission,
                    format!("{label}允许 AI 不经确认运行任意 {prog} 代码"),
                    format!("权限白名单里有「{r}」：{prog} 能做任何事，允许它不经确认运行，效果上接近允许任意命令。如果只是偶尔需要，建议去掉这条，让 AI 每次先问你。"),
                    at(&json_key(rule)),
                    rule,
                );
            }
        }
    }

    // 环境变量里的密钥和明文地址
    if let Some(env) = v.get("env").and_then(|e| e.as_object()) {
        for (k, val) in env {
            let Some(val) = val.as_str() else { continue };
            let line = at(&json_key(val)).or_else(|| at(&json_key(k)));
            if let Some(kind) = secret_value(k, val) {
                push(
                    s,
                    "settings-env-secret",
                    Severity::Medium,
                    Category::PlaintextSecret,
                    format!("{label}的环境变量 {k} 是明文的{kind}"),
                    "密钥直接写在设置文件里，任何能读这个文件的程序都能拿到；这个文件也常被备份、同步。建议改成在启动前从钥匙串读取，或者用 apiKeyHelper 调用读钥匙串的命令。".into(),
                    line,
                    val,
                );
            }
            if k.to_ascii_uppercase().ends_with("_URL") && insecure_url(val) {
                push(
                    s,
                    "settings-insecure-url",
                    Severity::Medium,
                    Category::InsecureTransport,
                    format!("{label}让 AI 请求走不加密的 http 连接"),
                    format!("{k} 指向一个 http:// 的远程地址：你的对话内容和密钥会以明文在网络上传输，同一网络里的人可以看到或篡改。建议改成 https://。"),
                    line,
                    val,
                );
            }
        }
    }

    // 会执行命令的设置：钩子、状态栏、密钥助手……
    let mut cmds: Vec<(String, String)> = Vec::new();
    if let Some(h) = v.get("hooks") {
        hook_commands(h, "", &mut cmds);
    }
    for (ptr, name) in [
        ("/statusLine/command", "statusLine"),
        ("/apiKeyHelper", "apiKeyHelper"),
        ("/awsAuthRefresh", "awsAuthRefresh"),
        ("/awsCredentialExport", "awsCredentialExport"),
        ("/otelHeadersHelper", "otelHeadersHelper"),
    ] {
        if let Some(c) = v.pointer(ptr).and_then(|c| c.as_str()) {
            cmds.push((name.to_string(), c.to_string()));
        }
    }
    let bases = Bases {
        project: project.map(Path::to_path_buf),
        plugin: None,
    };
    for (event, cmd) in cmds {
        exec_command(s, &cmd, &event, &real, &text, label, &bases);
    }
}

/// `Bash(python3:*)` 这类：允许一个能跑任意代码的解释器。
fn interpreter_wildcard(rule: &str) -> Option<String> {
    let inner = rule.strip_prefix("Bash(")?.strip_suffix(')')?;
    let prog = inner
        .strip_suffix(":*")
        .or_else(|| inner.strip_suffix(" *"))
        .or_else(|| inner.strip_suffix('*'))?
        .trim();
    const INTERP: &[&str] = &[
        "python",
        "python3",
        "node",
        "bash",
        "sh",
        "zsh",
        "perl",
        "ruby",
        "osascript",
        "eval",
        "sudo",
        "env",
        "bun",
        "deno",
        "php",
        "xargs",
        "uv run",
        "npx",
    ];
    INTERP.contains(&prog).then(|| prog.to_string())
}

/// 钩子段里的所有 `command`，连同它所在的事件名。递归着找，不依赖某一版的嵌套形状。
fn hook_commands(v: &Value, event: &str, out: &mut Vec<(String, String)>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                if k == "command" {
                    if let Some(c) = x.as_str() {
                        out.push((event.to_string(), c.to_string()));
                        continue;
                    }
                }
                let ev = if event.is_empty() { k.as_str() } else { event };
                hook_commands(x, ev, out);
            }
        }
        Value::Array(xs) => xs.iter().for_each(|x| hook_commands(x, event, out)),
        _ => {}
    }
}

/// 钩子命令里路径变量的取值。
struct Bases {
    project: Option<PathBuf>,
    plugin: Option<PathBuf>,
}

/// 单独的钩子文件（Codex 的 hooks.json、插件的 hooks/hooks.json）。
fn hooks_file(s: &mut Scanner, p: &Path, label: &str, plugin: Option<&Path>) {
    let Some((real, text)) = read_config(s, p, text::MAX_FILE_BYTES) else {
        return;
    };
    let path = real.display().to_string();
    let target = Target::new("hook", s.short(&real));
    hidden_in_file(s, &path, &text, &target, label);
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return;
    };
    let mut cmds = Vec::new();
    hook_commands(v.get("hooks").unwrap_or(&v), "", &mut cmds);
    let bases = Bases {
        project: None,
        plugin: plugin.map(Path::to_path_buf),
    };
    for (event, cmd) in cmds {
        exec_command(s, &cmd, &event, &real, &text, label, &bases);
    }
}

/// 一条会被自动执行的命令（钩子、状态栏、Codex notify）：危险命令按高危报，明文令牌按
/// 中危报，命令里引用到的脚本加入扫描。
fn exec_command(
    s: &mut Scanner,
    cmd: &str,
    event: &str,
    file: &Path,
    text: &str,
    label: &str,
    bases: &Bases,
) {
    let path = file.display().to_string();
    let target = Target::new("hook", if event.is_empty() { "hook" } else { event });
    let place = if event.is_empty() {
        format!("{label}的钩子命令")
    } else {
        format!("{label}的 {event} 命令")
    };
    let line = text::line_of(text, &json_escape_inner(cmd))
        .or_else(|| text::line_of(text, cmd.lines().next().unwrap_or(cmd)))
        .or_else(|| {
            (!event.is_empty())
                .then(|| {
                    text::line_of(text, &format!("{event} ="))
                        .or_else(|| text::line_of(text, &json_key(event)))
                })
                .flatten()
        });
    for h in commands::scan(cmd) {
        let matched = &cmd[h.start..h.end];
        let Some((sev, what, extra)) =
            engine::grade_command(h.rule, matched, cmd, Ctx::Exec, Role::HookScript)
        else {
            continue;
        };
        s.findings.push(
            Draft {
                rule_id: h.rule.id,
                severity: sev,
                category: Category::DangerousCommand,
                title: format!("{place}里有{what}"),
                detail: format!(
                    "{}{}这条命令由 AI 工具在后台自动执行，不经过 AI 判断，也不会问你。不认识的话建议删掉这个钩子。",
                    h.rule.why, extra
                ),
                path: &path,
                line,
                matched,
                excerpt: Some(text::excerpt(cmd, h.start..h.end)),
                target: &target,
            }
            .build(),
        );
    }
    for h in secrets::find_tokens(cmd) {
        s.findings.push(
            Draft {
                rule_id: "plaintext-token",
                severity: Severity::Medium,
                category: Category::PlaintextSecret,
                title: format!("{place}里有明文的{}", h.kind.label),
                detail: "密钥直接写在命令里，任何能读这个配置文件的程序都能拿到。建议改成从钥匙串读取，比如 security find-generic-password -s 名字 -w。".into(),
                path: &path,
                line,
                matched: &cmd[h.range.clone()],
                excerpt: Some(text::excerpt(cmd, h.range.clone())),
                target: &target,
            }
            .build(),
        );
    }
    for script in referenced_scripts(cmd, &s.home, bases) {
        let name = script
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        let label = format!("{place}调用的脚本 {name}");
        s.push_job(&script, Role::HookScript, target.clone(), label);
    }
}

/// JSON 字符串里的样子（不带两边引号），用来在文件里找行号。
fn json_escape_inner(s: &str) -> String {
    let q = serde_json::to_string(s).unwrap_or_default();
    q.trim_matches('"').to_string()
}

/// 命令里引用到的、确实存在的脚本文件。
fn referenced_scripts(cmd: &str, home: &Path, bases: &Bases) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for raw in cmd.split(|c: char| {
        c.is_whitespace() || matches!(c, ';' | '|' | '&' | '(' | ')' | '<' | '>' | '`')
    }) {
        let tok = raw.trim_matches(|c| c == '"' || c == '\'');
        if !tok.contains('/') {
            continue;
        }
        let home_s = home.display().to_string();
        let mut t = tok.replace("${HOME}", &home_s).replace("$HOME", &home_s);
        if let Some(rest) = t.strip_prefix("~/") {
            t = format!("{home_s}/{rest}");
        }
        for (var, base) in [
            ("CLAUDE_PROJECT_DIR", &bases.project),
            ("CLAUDE_PLUGIN_ROOT", &bases.plugin),
        ] {
            if let Some(b) = base {
                let b = b.display().to_string();
                t = t
                    .replace(&format!("${{{var}}}"), &b)
                    .replace(&format!("${var}"), &b);
            }
        }
        if t.contains('$') || !t.starts_with('/') {
            continue;
        }
        let p = PathBuf::from(&t);
        if p.is_file() && !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

// ------------------------------------------------------------------ Codex

fn codex_config(s: &mut Scanner, p: &Path) {
    let Some((real, text)) = read_config(s, p, text::MAX_FILE_BYTES) else {
        return;
    };
    let path = real.display().to_string();
    let target = Target::new("settings", "~/.codex/config.toml");
    let label = "Codex 设置";
    hidden_in_file(s, &path, &text, &target, label);
    let Ok(doc) = text.parse::<toml_edit::DocumentMut>() else {
        return;
    };
    let v = toml_to_json(doc.as_item());

    // 审批和沙箱：顶层，以及每个 profile（profile 里没写的继承顶层）
    let top_ap = v.get("approval_policy").and_then(|x| x.as_str());
    let top_sb = v.get("sandbox_mode").and_then(|x| x.as_str());
    let mut combos: Vec<(String, Option<&str>, Option<&str>)> =
        vec![(String::new(), top_ap, top_sb)];
    if let Some(profiles) = v.get("profiles").and_then(|p| p.as_object()) {
        for (name, prof) in profiles {
            let ap = prof
                .get("approval_policy")
                .and_then(|x| x.as_str())
                .or(top_ap);
            let sb = prof.get("sandbox_mode").and_then(|x| x.as_str()).or(top_sb);
            combos.push((name.clone(), ap, sb));
        }
    }
    for (profile, ap, sb) in combos {
        let never = ap == Some("never");
        let full = sb == Some("danger-full-access");
        if !never && !full {
            continue;
        }
        let scope = if profile.is_empty() {
            String::new()
        } else {
            format!("（配置方案 {profile}）")
        };
        let line = if full {
            text::line_of(&text, "danger-full-access")
        } else {
            text::line_of(&text, "approval_policy")
        };
        let (rule_id, severity, title, detail) = if never && full {
            (
                "codex-full-access",
                Severity::High,
                format!("Codex{scope}设置成不问你、也不限制访问范围"),
                "approval_policy = \"never\" 加上 sandbox_mode = \"danger-full-access\"：AI 可以不经确认执行任何命令、读写电脑上任何文件、随意联网。一旦被恶意内容诱导，没有任何一道关卡。建议改回 on-request + workspace-write，需要时再临时放开。".to_string(),
            )
        } else if never {
            (
                "codex-never-approve",
                Severity::Low,
                format!("Codex{scope}设置成从不请求确认"),
                "approval_policy = \"never\"：AI 不会再请你确认任何操作，只靠沙箱限制它能做什么。确认 sandbox_mode 没有放开到 danger-full-access。".to_string(),
            )
        } else {
            (
                "codex-full-sandbox",
                Severity::Low,
                format!("Codex{scope}关掉了沙箱限制"),
                "sandbox_mode = \"danger-full-access\"：AI 运行的命令能读写电脑上任何文件、随意联网，只靠逐条确认把关。建议平时用 workspace-write。".to_string(),
            )
        };
        s.findings.push(
            Draft {
                rule_id,
                severity,
                category: Category::BroadPermission,
                title,
                detail,
                path: &path,
                line,
                matched: &profile,
                excerpt: line.map(|no| text::safe_text(Lines::new(&text).text(no))),
                target: &target,
            }
            .build(),
        );
    }

    // notify：每轮结束都会执行的命令
    let notify = match v.get("notify") {
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| {
                x.as_str()
                    .map(String::from)
                    .unwrap_or_else(|| x.to_string())
            })
            .collect::<Vec<_>>()
            .join(" "),
        Some(Value::String(c)) => c.clone(),
        _ => String::new(),
    };
    if !notify.trim().is_empty() {
        let bases = Bases {
            project: None,
            plugin: None,
        };
        exec_command(s, &notify, "notify", &real, &text, label, &bases);
    }

    // 环境变量策略里直接写的值
    if let Some(set) = v
        .pointer("/shell_environment_policy/set")
        .and_then(|x| x.as_object())
    {
        for (k, val) in set {
            let Some(val) = val.as_str() else { continue };
            if let Some(kind) = secret_value(k, val) {
                let line = text::line_of(&text, &json_key(val));
                s.findings.push(
                    Draft {
                        rule_id: "settings-env-secret",
                        severity: Severity::Medium,
                        category: Category::PlaintextSecret,
                        title: format!("Codex 设置的环境变量 {k} 是明文的{kind}"),
                        detail: "密钥直接写在 config.toml 里，任何能读这个文件的程序都能拿到。建议改成从钥匙串读取后再启动 Codex。".into(),
                        path: &path,
                        line,
                        matched: val,
                        excerpt: line.map(|no| text::safe_text(Lines::new(&text).text(no))),
                        target: &target,
                    }
                    .build(),
                );
            }
        }
    }

    if let Some(servers) = v.get("mcp_servers") {
        let file = McpFile {
            path: &path,
            text: &text,
            label: "Codex",
        };
        for srv in parse_servers(servers) {
            let anchors = vec![
                format!("mcp_servers.{}", srv.name),
                format!("mcp_servers.{}", json_key(&srv.name)),
            ];
            check_server(s, &file, &srv, anchors);
        }
    }
}

fn toml_to_json(item: &toml_edit::Item) -> Value {
    use toml_edit::Item;
    match item {
        Item::None => Value::Null,
        Item::Value(v) => toml_value(v),
        Item::Table(t) => Value::Object(
            t.iter()
                .map(|(k, v)| (k.to_string(), toml_to_json(v)))
                .collect(),
        ),
        Item::ArrayOfTables(a) => Value::Array(
            a.iter()
                .map(|t| {
                    Value::Object(
                        t.iter()
                            .map(|(k, v)| (k.to_string(), toml_to_json(v)))
                            .collect(),
                    )
                })
                .collect(),
        ),
    }
}

fn toml_value(v: &toml_edit::Value) -> Value {
    use toml_edit::Value as V;
    match v {
        V::String(s) => Value::String(s.value().clone()),
        V::Integer(i) => Value::from(*i.value()),
        V::Float(f) => serde_json::Number::from_f64(*f.value())
            .map(Value::Number)
            .unwrap_or(Value::Null),
        V::Boolean(b) => Value::Bool(*b.value()),
        V::Datetime(d) => Value::String(d.value().to_string()),
        V::Array(a) => Value::Array(a.iter().map(toml_value).collect()),
        V::InlineTable(t) => Value::Object(
            t.iter()
                .map(|(k, v)| (k.to_string(), toml_value(v)))
                .collect(),
        ),
    }
}

// ------------------------------------------------------------------ Gemini

fn gemini_settings(s: &mut Scanner, p: &Path) {
    let Some((real, text)) = read_config(s, p, text::MAX_FILE_BYTES) else {
        return;
    };
    let path = real.display().to_string();
    let target = Target::new("settings", "~/.gemini/settings.json");
    hidden_in_file(s, &path, &text, &target, "Gemini 设置");
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return;
    };
    if let Some(servers) = v.get("mcpServers") {
        let file = McpFile {
            path: &path,
            text: &text,
            label: "Gemini",
        };
        for srv in parse_servers(servers) {
            let anchors = vec![json_key("mcpServers"), json_key(&srv.name)];
            check_server(s, &file, &srv, anchors);
        }
    }
    if let Some(h) = v.get("hooks") {
        let mut cmds = Vec::new();
        hook_commands(h, "", &mut cmds);
        let bases = Bases {
            project: None,
            plugin: None,
        };
        for (event, cmd) in cmds {
            exec_command(s, &cmd, &event, &real, &text, "Gemini 设置", &bases);
        }
    }
}

// ------------------------------------------------------------------ ~/.claude.json

#[derive(Deserialize, Default)]
struct ClaudeJson {
    #[serde(rename = "mcpServers", default)]
    mcp_servers: Option<Value>,
    #[serde(default)]
    projects: Option<BTreeMap<String, ClaudeProject>>,
}

#[derive(Deserialize, Default)]
struct ClaudeProject {
    #[serde(rename = "mcpServers", default)]
    mcp_servers: Option<Value>,
}

/// 只解析 `mcpServers` 和 `projects[*].mcpServers`；返回项目目录列表。
fn claude_json(s: &mut Scanner, p: &Path) -> Vec<String> {
    let Some((real, text)) = read_config(s, p, CLAUDE_JSON_MAX) else {
        return Vec::new();
    };
    let Ok(cj) = serde_json::from_str::<ClaudeJson>(&text) else {
        return Vec::new();
    };
    let path = real.display().to_string();
    let file = McpFile {
        path: &path,
        text: &text,
        label: "Claude Code",
    };
    if let Some(servers) = &cj.mcp_servers {
        for srv in parse_servers(servers) {
            let anchors = vec![json_key("mcpServers"), json_key(&srv.name)];
            check_server(s, &file, &srv, anchors);
        }
    }
    let mut dirs = Vec::new();
    for (proj, entry) in cj.projects.iter().flatten() {
        dirs.push(proj.clone());
        let Some(servers) = &entry.mcp_servers else {
            continue;
        };
        let pname = Path::new(proj)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(proj)
            .to_string();
        let label = format!("Claude Code（项目「{pname}」）");
        let file = McpFile {
            path: &path,
            text: &text,
            label: &label,
        };
        for srv in parse_servers(servers) {
            let anchors = vec![
                json_key("projects"),
                json_key(proj),
                json_key("mcpServers"),
                json_key(&srv.name),
            ];
            check_server(s, &file, &srv, anchors);
        }
    }
    dirs
}

/// `{"mcpServers": {...}}` 形状的文件（Claude 桌面版、Cursor、项目和插件的 .mcp.json）。
/// 插件的 .mcp.json 也可能直接是 `{名字: {...}}`。
pub(crate) fn mcp_json_file(s: &mut Scanner, p: &Path, label: &str) {
    let Some((real, text)) = read_config(s, p, text::MAX_FILE_BYTES) else {
        return;
    };
    let path = real.display().to_string();
    let target = Target::new("mcp", s.short(&real));
    hidden_in_file(s, &path, &text, &target, &format!("{label}的 MCP 配置"));
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return;
    };
    let servers = match v.get("mcpServers") {
        Some(x) => x.clone(),
        None => {
            // 直接是 {名字: {command|url…}}
            let direct = v.as_object().is_some_and(|m| {
                !m.is_empty()
                    && m.values().all(|x| {
                        x.get("command").is_some()
                            || x.get("url").is_some()
                            || x.get("type").is_some()
                    })
            });
            if direct {
                v.clone()
            } else {
                return;
            }
        }
    };
    let file = McpFile {
        path: &path,
        text: &text,
        label,
    };
    for srv in parse_servers(&servers) {
        let anchors = vec![json_key(&srv.name)];
        check_server(s, &file, &srv, anchors);
    }
}

// ------------------------------------------------------------------ MCP 服务

#[derive(Debug, Clone, Default)]
pub(crate) struct McpServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub url: Option<String>,
    pub headers: Vec<(String, String)>,
    pub enabled: bool,
}

fn str_of(v: &Value) -> String {
    v.as_str()
        .map(String::from)
        .unwrap_or_else(|| v.to_string())
}

fn string_map(v: Option<&Value>) -> Vec<(String, String)> {
    v.and_then(|x| x.as_object())
        .map(|m| m.iter().map(|(k, v)| (k.clone(), str_of(v))).collect())
        .unwrap_or_default()
}

pub(crate) fn parse_servers(v: &Value) -> Vec<McpServer> {
    let Some(m) = v.as_object() else {
        return Vec::new();
    };
    m.iter()
        .filter(|(_, cfg)| cfg.is_object())
        .map(|(name, cfg)| {
            let (command, mut args) = match cfg.get("command") {
                Some(Value::Array(a)) => {
                    let mut it = a.iter().map(str_of);
                    (it.next().unwrap_or_default(), it.collect())
                }
                Some(c) => (str_of(c), Vec::new()),
                None => (String::new(), Vec::new()),
            };
            if let Some(a) = cfg.get("args").and_then(|a| a.as_array()) {
                args.extend(a.iter().map(str_of));
            }
            let mut headers = string_map(cfg.get("headers"));
            headers.extend(string_map(cfg.get("http_headers")));
            if let Some(t) = cfg.get("bearer_token").and_then(|t| t.as_str()) {
                headers.push(("Authorization".into(), format!("Bearer {t}")));
            }
            let mut env = string_map(cfg.get("env"));
            env.extend(string_map(cfg.get("environment")));
            McpServer {
                name: name.clone(),
                command,
                args,
                env,
                url: cfg
                    .get("url")
                    .or_else(|| cfg.get("serverUrl"))
                    .or_else(|| cfg.get("httpUrl"))
                    .and_then(|u| u.as_str())
                    .map(String::from),
                headers,
                enabled: !(cfg.get("enabled").and_then(|b| b.as_bool()) == Some(false)
                    || cfg.get("disabled").and_then(|b| b.as_bool()) == Some(true)),
            }
        })
        .collect()
}

struct McpFile<'a> {
    path: &'a str,
    text: &'a str,
    /// 「Claude Code」「Codex」「项目「x」」
    label: &'a str,
}

/// 一个 MCP 服务的全部检查。
fn check_server(s: &mut Scanner, file: &McpFile, srv: &McpServer, anchors: Vec<String>) {
    let target = Target::new("mcp", srv.name.clone());
    let place = format!("{}的 MCP 服务「{}」", file.label, srv.name);
    let line_for = |extra: &str| {
        let mut a = anchors.clone();
        if !extra.is_empty() {
            a.push(json_key(extra));
        }
        text::locate(file.text, &a)
    };
    let push = |s: &mut Scanner,
                rule_id: &str,
                severity: Severity,
                category: Category,
                title: String,
                detail: String,
                line: Option<usize>,
                matched: &str,
                excerpt: String| {
        s.findings.push(
            Draft {
                rule_id,
                severity,
                category,
                title,
                detail,
                path: file.path,
                line,
                matched: &format!("{}:{}", srv.name, matched),
                excerpt: Some(excerpt),
                target: &target,
            }
            .build(),
        );
    };

    // 隐藏字符：只看这个服务自己的字符串
    let mut strings: Vec<&str> = vec![srv.command.as_str()];
    strings.extend(srv.args.iter().map(String::as_str));
    strings.extend(srv.env.iter().map(|(_, v)| v.as_str()));
    strings.extend(srv.headers.iter().map(|(_, v)| v.as_str()));
    if let Some(u) = &srv.url {
        strings.push(u);
    }
    for st in &strings {
        for h in hidden::scan(st) {
            let ex = text::excerpt(st, h.start..h.end);
            let line = line_for(st).or_else(|| line_for("")).unwrap_or(1);
            s.findings.push(engine::hidden_finding(
                &h, file.path, line, ex, &target, &place,
            ));
        }
    }

    let exec_text = std::iter::once(srv.command.as_str())
        .chain(srv.args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    if srv.enabled && !srv.command.is_empty() {
        // 危险命令：启动命令会被直接执行
        for h in commands::scan(&exec_text) {
            let matched = &exec_text[h.start..h.end];
            let Some((sev, what, extra)) =
                engine::grade_command(h.rule, matched, &exec_text, Ctx::Exec, Role::HookScript)
            else {
                continue;
            };
            push(
                s,
                h.rule.id,
                sev,
                Category::DangerousCommand,
                format!("{place}的启动命令里有{what}"),
                format!(
                    "{}{}每次 AI 工具启动这个 MCP 服务都会执行它，不会问你。不认识的话建议停用这个服务。",
                    h.rule.why, extra
                ),
                line_for(""),
                matched,
                text::excerpt(&exec_text, h.start..h.end),
            );
        }
        // 供应链：没锁版本
        if let Some(pkg) = unpinned_package(&srv.command, &srv.args) {
            let latest = pkg.ends_with("@latest");
            push(
                s,
                "mcp-unpinned-package",
                Severity::Low,
                Category::SupplyChain,
                format!("{place}每次启动都会下载最新版本"),
                format!(
                    "它用 {} 运行「{}」，{}每次启动都可能拿到一个新版本。如果这个包被人抢注、或者作者账号被盗，恶意版本会在你下次启动时自动运行。建议写上固定版本号，升级时再手动改。",
                    basename(&srv.command),
                    text::safe_text(&pkg),
                    if latest { "指定的是 @latest，" } else { "没有写版本号，" }
                ),
                line_for(&pkg).or_else(|| line_for("")),
                &pkg,
                text::excerpt(&exec_text, 0..exec_text.len().min(1)),
            );
        }
    }
    // 明文 http
    if let Some(u) = &srv.url {
        if srv.enabled && insecure_url(u) {
            push(
                s,
                "mcp-insecure-http",
                Severity::Medium,
                Category::InsecureTransport,
                format!("{place}用不加密的 http 连接远程服务器"),
                "http:// 传输的内容（你的对话上下文、工具调用结果、可能还有登录令牌）在网络上是明文，同一网络里的人可以看到或篡改。建议改成 https://。".into(),
                line_for(u),
                u,
                text::safe_text(u),
            );
        }
        // 网址里带的密钥
        for (k, v) in url_secrets(u) {
            push(
                s,
                "mcp-url-secret",
                Severity::Medium,
                Category::PlaintextSecret,
                format!("{place}的地址里带着明文密钥"),
                format!("地址里的 {k} 参数是一把密钥，它会出现在配置文件、日志和历史记录里。建议改用请求头加环境变量引用的方式传。"),
                line_for(u),
                &v,
                text::safe_text(u),
            );
        }
    }
    // 环境变量和请求头里的密钥
    for (k, v) in srv.env.iter().chain(srv.headers.iter()) {
        if let Some(kind) = secret_value(k, v) {
            let is_header = srv.headers.iter().any(|(hk, hv)| hk == k && hv == v);
            push(
                s,
                "mcp-plaintext-secret",
                Severity::Medium,
                Category::PlaintextSecret,
                format!(
                    "{place}的{} {k} 是明文{kind}",
                    if is_header { "请求头" } else { "环境变量" }
                ),
                "密钥直接写在 MCP 配置里，任何能读这个文件的程序都能拿到，这个文件也常被备份、同步。建议写成 ${变量名} 引用环境变量，把真正的值放进钥匙串。".into(),
                line_for(v),
                v,
                text::safe_text(&format!("{k}={v}")),
            );
        }
    }
    // 参数里的密钥
    for (i, a) in srv.args.iter().enumerate() {
        let mut found: Option<(String, String)> = None;
        if let Some((flag, val)) = a.split_once('=') {
            if flag.starts_with('-')
                && secrets::secretish_name(flag.trim_start_matches('-'))
                && secrets::looks_secret(val)
            {
                found = Some((flag.to_string(), val.to_string()));
            }
        } else if a.starts_with('-') && secrets::secretish_name(a.trim_start_matches('-')) {
            if let Some(next) = srv.args.get(i + 1) {
                if secrets::looks_secret(next) {
                    found = Some((a.clone(), next.clone()));
                }
            }
        }
        if found.is_none() {
            if let Some(h) = secrets::find_tokens(a).into_iter().next() {
                found = Some((h.kind.label.to_string(), a[h.range].to_string()));
            }
        }
        if let Some((what, val)) = found {
            push(
                s,
                "mcp-plaintext-secret",
                Severity::Medium,
                Category::PlaintextSecret,
                format!("{place}的启动参数里有明文密钥"),
                format!("启动参数 {what} 后面直接写着密钥，它会出现在配置文件里，运行时也能在进程列表里看到。建议改成通过环境变量传入。"),
                line_for(&val),
                &val,
                text::safe_text(a),
            );
        }
    }
}

fn basename(cmd: &str) -> String {
    let b = cmd.rsplit(['/', '\\']).next().unwrap_or(cmd);
    b.trim_end_matches(".cmd")
        .trim_end_matches(".exe")
        .to_string()
}

/// npx / bunx / uvx 运行的包没有锁定版本时返回包名。
pub(crate) fn unpinned_package(command: &str, args: &[String]) -> Option<String> {
    let base = basename(command);
    let first = args.first().map(String::as_str).unwrap_or("");
    let (py, rest): (bool, &[String]) = match base.as_str() {
        "npx" | "bunx" | "pnpx" => (false, args),
        "pnpm" | "yarn" | "bun" if matches!(first, "dlx" | "x") => (false, &args[1..]),
        "npm" if first == "exec" => (false, &args[1..]),
        "uvx" => (true, args),
        "uv" if first == "tool" && args.get(1).map(String::as_str) == Some("run") => {
            (true, &args[2..])
        }
        "pipx" if first == "run" => (true, &args[1..]),
        _ => return None,
    };
    const PY_VALUE_FLAGS: &[&str] = &[
        "--python",
        "-p",
        "--with",
        "--index-url",
        "--extra-index-url",
        "--index",
        "--refresh-package",
        "--constraint",
        "-c",
        "--python-preference",
        "--directory",
    ];
    let mut pkg: Option<String> = None;
    let mut i = 0;
    while i < rest.len() {
        let a = rest[i].as_str();
        if a == "--" {
            i += 1;
            continue;
        }
        if let Some(v) = a
            .strip_prefix("--package=")
            .or_else(|| a.strip_prefix("--from="))
        {
            pkg = Some(v.to_string());
            break;
        }
        if (!py && (a == "-p" || a == "--package")) || (py && a == "--from") {
            pkg = rest.get(i + 1).cloned();
            break;
        }
        if py && PY_VALUE_FLAGS.contains(&a) {
            i += 2;
            continue;
        }
        if a.starts_with('-') {
            i += 1;
            continue;
        }
        pkg = Some(a.to_string());
        break;
    }
    let pkg = pkg?;
    if pkg.starts_with('.')
        || pkg.starts_with('/')
        || pkg.starts_with('~')
        || pkg.contains("://")
        || pkg.starts_with("git+")
        || pkg.starts_with("github:")
        || pkg.starts_with("file:")
    {
        return None;
    }
    let pinned = if py {
        let ver = pkg
            .split_once("==")
            .map(|(_, v)| v)
            .or_else(|| pkg.split_once('@').map(|(_, v)| v));
        ver.is_some_and(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit()))
    } else {
        let at = if let Some(rest) = pkg.strip_prefix('@') {
            rest.find('@').map(|i| i + 1)
        } else {
            pkg.find('@')
        };
        match at {
            Some(i) => {
                let v = &pkg[i + 1..];
                let v = v.strip_prefix('v').unwrap_or(v);
                v.chars().next().is_some_and(|c| c.is_ascii_digit())
                    && !v.contains(['^', '~', '>', '<', '*', ' ', '|'])
                    && !v.contains(".x")
            }
            None => false,
        }
    };
    (!pinned).then_some(pkg)
}

/// 地址指向本机。按主机名比，不按子串（移植自 ThinkWatch 的 is_local）。
pub(crate) fn is_local_url(url: &str) -> bool {
    let Some((_, rest)) = url.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = match host_port.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => host_port.split(':').next().unwrap_or_default(),
    };
    host.eq_ignore_ascii_case("localhost")
        || host.to_ascii_lowercase().ends_with(".localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback() || ip.is_unspecified())
}

pub(crate) fn insecure_url(url: &str) -> bool {
    url.get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("http://"))
        && !is_local_url(url)
}

/// 网址里的密钥：用户名口令、名字像密钥的查询参数。
fn url_secrets(url: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some((_, rest)) = url.split_once("://") {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        if let Some((user, _)) = authority.rsplit_once('@') {
            if let Some((_, pass)) = user.split_once(':') {
                if pass.len() >= 6 && !pass.starts_with('$') {
                    out.push(("口令".to_string(), pass.to_string()));
                }
            }
        }
    }
    if let Some((_, q)) = url.split_once('?') {
        for kv in q.split(['&', '#']) {
            if let Some((k, v)) = kv.split_once('=') {
                if secrets::secretish_name(k) && secrets::looks_secret(v) {
                    out.push((k.to_string(), v.to_string()));
                }
            }
        }
    }
    out
}

/// 一个环境变量/请求头的值是不是明文密钥；是的话返回它的中文名。
fn secret_value(name: &str, value: &str) -> Option<&'static str> {
    if let Some(h) = secrets::find_tokens(value).into_iter().next() {
        return Some(h.kind.label);
    }
    let v = value.trim();
    let v = v.strip_prefix("Bearer ").unwrap_or(v);
    if (secrets::secretish_name(name) || name.eq_ignore_ascii_case("authorization"))
        && secrets::looks_secret(v)
    {
        return Some("密钥");
    }
    None
}

// ------------------------------------------------------------------ 技能自己的配置文件

/// `~/.claude/` 顶层的 JSON 配置（多是技能自己的配置，比如 xxx-config.json 和它的备份），
/// 里面常常直接写着 API 密钥。每个文件汇总成一条。
fn claude_dir_configs(s: &mut Scanner) {
    let dir = s.home.join(".claude");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    files.sort();
    const SKIP: &[&str] = &[
        ".claude.json",
        "settings.json",
        "settings.local.json",
        "stats-cache.json",
        "mcp-needs-auth-cache.json",
        ".last-update-result.json",
    ];
    for p in files {
        let name = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        if !name.contains(".json") || name.ends_with(".jsonl") || SKIP.contains(&name.as_str()) {
            continue;
        }
        if !p.is_file() {
            continue;
        }
        let Some((real, text)) = read_config(s, &p, text::MAX_FILE_BYTES) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let mut found: Vec<(String, String)> = Vec::new();
        collect_secrets(&v, "", &mut found);
        if found.is_empty() {
            continue;
        }
        let mut keys: Vec<&str> = Vec::new();
        for (k, _) in &found {
            if !keys.contains(&k.as_str()) {
                keys.push(k);
            }
        }
        let shown = if keys.len() > 4 {
            format!("{} 等", keys[..4].join("、"))
        } else {
            keys.join("、")
        };
        let line = text::line_of(&text, &json_key(&found[0].1));
        let perm = {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::metadata(&real)
                    .map(|m| m.permissions().mode() & 0o777)
                    .ok()
                    .filter(|m| m & 0o044 != 0)
                    .map(|m| format!("文件权限是 {m:o}，这台电脑上的其他用户也能读。"))
                    .unwrap_or_default()
            }
            #[cfg(not(unix))]
            {
                String::new()
            }
        };
        let path = real.display().to_string();
        let target = Target::new("settings", name.clone());
        let matched = keys.join(",");
        s.findings.push(
            Draft {
                rule_id: "config-file-secret",
                severity: Severity::Low,
                category: Category::PlaintextSecret,
                title: format!("配置文件 ~/.claude/{name} 里有 {} 个明文密钥", found.len()),
                detail: format!(
                    "这个文件多半是某个技能自己的配置，里面直接写着 {shown} 的值。任何能读这个文件的程序（包括 AI 执行的命令）都能拿到。{perm}建议把密钥存进钥匙串、让技能运行时读取；至少把文件权限改成 600。{}",
                    if name.contains("backup") || name.contains(".bak") {
                        "这是一个旧的备份文件，用不上的话可以移到废纸篓。"
                    } else {
                        ""
                    }
                ),
                path: &path,
                line,
                matched: &matched,
                excerpt: line.map(|no| text::safe_text(Lines::new(&text).text(no))),
                target: &target,
            }
            .build(),
        );
    }
}

fn collect_secrets(v: &Value, key: &str, out: &mut Vec<(String, String)>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                collect_secrets(x, k, out);
            }
        }
        Value::Array(xs) => xs.iter().for_each(|x| collect_secrets(x, key, out)),
        Value::String(sv) => {
            if secret_value(key, sv).is_some() {
                out.push((key.to_string(), sv.clone()));
            }
        }
        _ => {}
    }
}

// ------------------------------------------------------------------ 凭据文件权限

fn credential_files(s: &mut Scanner) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let home = s.home.clone();
        for (rel, what) in [
            (".claude/.credentials.json", "Claude Code 的登录凭据"),
            (".codex/auth.json", "Codex 的登录凭据"),
            (".gemini/oauth_creds.json", "Gemini 的登录凭据"),
            (".gemini/mcp-oauth-tokens-v2.json", "MCP 服务的登录令牌"),
            (
                ".claude.json",
                "Claude Code 的账号信息和 MCP 配置（可能含密钥）",
            ),
        ] {
            let p = home.join(rel);
            let Ok(meta) = std::fs::metadata(&p) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let mode = meta.permissions().mode() & 0o777;
            if mode & 0o044 == 0 {
                continue;
            }
            let path = p.display().to_string();
            let name = rel.rsplit('/').next().unwrap_or(rel);
            let target = Target::new("credentials", name);
            s.findings.push(
                Draft {
                    rule_id: "credential-file-readable",
                    severity: Severity::Low,
                    category: Category::FilePermission,
                    title: format!("凭据文件 ~/{rel} 其他用户也能读"),
                    detail: format!(
                        "这个文件存着{what}，当前权限是 {mode:o}，这台电脑上的其他用户账户也能读取。建议在终端运行 chmod 600 ~/{rel} 收紧权限。"
                    ),
                    path: &path,
                    line: None,
                    matched: &format!("{mode:o}"),
                    excerpt: None,
                    target: &target,
                }
                .build(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn unpinned() {
        assert_eq!(
            unpinned_package("npx", &a(&["-y", "@playwright/mcp@latest"])).as_deref(),
            Some("@playwright/mcp@latest")
        );
        assert_eq!(
            unpinned_package("npx", &a(&["-y", "chrome-devtools-mcp"])).as_deref(),
            Some("chrome-devtools-mcp")
        );
        assert!(unpinned_package("npx", &a(&["-y", "@scope/pkg@1.2.3"])).is_none());
        assert!(unpinned_package("npx", &a(&["pkg@0.4.1"])).is_none());
        assert_eq!(
            unpinned_package("uvx", &a(&["--python", "3.12", "mcp-server-git"])).as_deref(),
            Some("mcp-server-git")
        );
        assert!(unpinned_package("uvx", &a(&["mcp-server-git==0.6.2"])).is_none());
        assert!(unpinned_package("/usr/local/bin/node", &a(&["server.js"])).is_none());
    }

    #[test]
    fn local_urls() {
        assert!(is_local_url("http://localhost:3000/mcp"));
        assert!(is_local_url("http://127.0.0.1:8080"));
        assert!(is_local_url("http://[::1]:3000/mcp"));
        assert!(!is_local_url("http://localhost.evil.example/mcp"));
        assert!(!is_local_url("http://evil.example/?r=http://127.0.0.1"));
        assert!(insecure_url("http://mcp.example.com/sse"));
        assert!(!insecure_url("https://mcp.example.com/sse"));
    }

    #[test]
    fn interpreters() {
        assert_eq!(
            interpreter_wildcard("Bash(python3:*)").as_deref(),
            Some("python3")
        );
        assert!(interpreter_wildcard("Bash(git status:*)").is_none());
    }
}

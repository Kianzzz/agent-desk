mod common;

use ad_inventory::{scan, Client, Inventory, McpServer, SkillEntry};

fn mcp<'a>(inv: &'a Inventory, client: Client, name: &str) -> &'a McpServer {
    inv.mcp
        .iter()
        .find(|m| m.client == client && m.name == name)
        .unwrap_or_else(|| panic!("没有 {client:?}/{name}"))
}

fn skill<'a>(inv: &'a Inventory, root: &str, dir: &str) -> &'a SkillEntry {
    inv.skills
        .iter()
        .find(|s| s.root_id == root && s.dir_name == dir)
        .unwrap_or_else(|| panic!("没有 {root}/{dir}"))
}

#[test]
fn mcp_claude_code_scopes() {
    let env = common::setup();
    let inv = scan(&env.home, &env.state);

    let pw = mcp(&inv, Client::ClaudeCode, "playwright");
    assert_eq!(pw.scope, "user");
    assert_eq!(pw.transport, "stdio");
    assert_eq!(pw.package.as_deref(), Some("@playwright/mcp@latest"));
    assert_eq!(pw.env_keys, vec!["PW_TOKEN"]);
    assert!(pw.manageable && pw.enabled);
    // 稳定 id：sha256("mcp|claudeCode|user||playwright") 前 16 位
    assert_eq!(pw.id.len(), 16);
    assert_eq!(
        pw.id,
        scan(&env.home, &env.state)
            .mcp
            .iter()
            .find(|m| m.name == "playwright")
            .unwrap()
            .id
    );

    let remote = mcp(&inv, Client::ClaudeCode, "remote");
    assert_eq!(remote.transport, "http");
    assert_eq!(remote.header_keys, vec!["Authorization"]);

    let local = mcp(&inv, Client::ClaudeCode, "localsrv");
    assert_eq!(local.scope, "local");
    assert_eq!(
        local.scope_path.as_deref(),
        Some(env.home.join("proj").to_str().unwrap())
    );
    assert_eq!(local.package.as_deref(), Some("mcp-server-git"));
    assert!(local.manageable);

    // 项目目录已不存在：仍然列出，但不能开关
    let orphan = mcp(&inv, Client::ClaudeCode, "orphan");
    assert!(!orphan.manageable);

    // 项目 .mcp.json：只读；disabledMcpjsonServers 里的是停用
    let projsrv = mcp(&inv, Client::ClaudeCode, "projsrv");
    assert_eq!(projsrv.scope, "project");
    assert!(!projsrv.manageable && projsrv.enabled);
    assert_eq!(projsrv.package.as_deref(), Some("some-mcp"));
    let projoff = mcp(&inv, Client::ClaudeCode, "projoff");
    assert!(!projoff.enabled);
    assert_eq!(projoff.transport, "sse");

    // 插件 MCP：只列启用插件的
    let plug = mcp(&inv, Client::ClaudeCode, "plugmcp");
    assert_eq!(plug.scope, "plugin");
    assert_eq!(plug.scope_path.as_deref(), Some("plug"));
    assert!(!plug.manageable);
    assert!(!inv.mcp.iter().any(|m| m.name == "offmcp"));
}

#[test]
fn mcp_other_clients() {
    let env = common::setup();
    let inv = scan(&env.home, &env.state);

    let weather = mcp(&inv, Client::ClaudeDesktop, "weather");
    assert_eq!(weather.env_keys, vec!["WEATHER_API_KEY"]);
    assert_eq!(weather.package.as_deref(), Some("weather-mcp"));
    assert_eq!(mcp(&inv, Client::ClaudeDesktop, "notes").transport, "http");
    // 桌面扩展：只读，按设置文件里的开关
    let ext = mcp(&inv, Client::ClaudeDesktop, "演示扩展");
    assert!(!ext.manageable && !ext.enabled);
    assert_eq!(ext.env_keys, vec!["DEMO_KEY"]);

    let alpha = mcp(&inv, Client::Codex, "alpha");
    assert_eq!(alpha.package.as_deref(), Some("@foo/alpha-mcp"));
    assert_eq!(alpha.env_keys, vec!["ALPHA_KEY"]);
    let remote = mcp(&inv, Client::Codex, "remote");
    assert_eq!(remote.transport, "sse");
    assert_eq!(remote.header_keys, vec!["X-Team", "Authorization"]);
    assert!(!mcp(&inv, Client::Codex, "off").enabled);
    assert!(mcp(&inv, Client::Codex, "last").enabled);

    assert_eq!(mcp(&inv, Client::Gemini, "ghttp").transport, "http");
    assert_eq!(mcp(&inv, Client::Gemini, "gsse").transport, "sse");
    let gstdio = mcp(&inv, Client::Gemini, "gstdio");
    assert_eq!(gstdio.package.as_deref(), Some("gem-mcp"));
    assert!(!gstdio.enabled, "mcp.excluded 里的服务是停用的");

    assert!(!mcp(&inv, Client::Cursor, "cur").enabled);
}

#[test]
fn secrets_never_in_output() {
    let env = common::setup();
    let inv = scan(&env.home, &env.state);
    let json = serde_json::to_string(&inv).unwrap();
    for secret in [
        "secret-token-123456",
        "sk-live-abcdef",
        "wk-123456789",
        "alpha-secret-999",
        "team-secret",
        "plug-secret-value",
        "demo-secret",
    ] {
        assert!(!json.contains(secret), "输出里出现了密钥 {secret}");
    }
    // camelCase 序列化
    assert!(json.contains("\"scopePath\""));
    assert!(json.contains("\"skillGroups\""));
}

#[test]
fn skills_frontmatter_and_layout() {
    let env = common::setup();
    let inv = scan(&env.home, &env.state);

    let alpha = skill(&inv, "claude", "alpha");
    assert_eq!(alpha.name, "alpha");
    assert_eq!(alpha.description, "Alpha skill.");
    assert!(!alpha.has_scripts);
    assert!(alpha.enabled && alpha.manageable && !alpha.is_symlink);

    let beta = skill(&inv, "claude", "beta");
    assert_eq!(beta.description, "多行描述\n第二行");
    assert!(beta.has_scripts);
    assert_eq!(beta.file_count, 2);

    let gamma = skill(&inv, "claude", "gamma");
    assert_eq!(gamma.name, "gamma");
    assert_eq!(gamma.description, "Folded line one and two.");

    let music = skill(&inv, "claude", "chatcut-music");
    assert_eq!(music.name, "music", "名字取 frontmatter");
    assert_eq!(music.description, "ChatCut 配乐: 生成音乐");
    assert_eq!(music.synced_by.as_deref(), Some("ChatCut"));

    let nodesc = skill(&inv, "claude", "nodesc");
    assert_eq!(nodesc.name, "nodesc", "没有 name 用目录名");
    assert_eq!(nodesc.description, "");

    // 杂项文件、点开头目录、没有 SKILL.md 的目录、失效链接都不算技能
    for dir in ["notaskill", "UPGRADE_NOTE.md", ".hidden", "broken"] {
        assert!(
            !inv.skills.iter().any(|s| s.dir_name == dir),
            "{dir} 不应算技能"
        );
    }
    assert!(inv
        .warnings
        .iter()
        .any(|w| w.contains("失效") && w.contains("broken")));

    // 符号链接：大小和文件数按真实目录算
    let link = skill(&inv, "claude", "shared");
    let real = skill(&inv, "agents", "shared");
    assert!(link.is_symlink && !real.is_symlink);
    assert_eq!(link.real_path, real.real_path);
    assert_eq!(link.size_bytes, real.size_bytes);
    assert_eq!(link.file_count, 3);
    assert!(link.size_bytes >= 4096);
    assert!(link.has_scripts, "tool.py");
    assert!(skill(&inv, "codex", "shared").is_symlink);

    // Codex 自己在 config.toml 里停用的技能
    assert!(!skill(&inv, "codex", "beta").enabled);

    // 插件技能：只读；同步插件取当前代（~g2）目录
    let ps = skill(&inv, "plugin:plug", "plugskill");
    assert!(!ps.manageable && ps.enabled);
    assert!(skill(&inv, "plugin:syncp", "s1")
        .real_path
        .contains("syncp~g2"));
    assert!(!inv.skills.iter().any(|s| s.dir_name == "old"));

    let roots: Vec<(&str, u32)> = inv
        .skill_roots
        .iter()
        .map(|r| (r.id.as_str(), r.count))
        .collect();
    assert_eq!(
        roots,
        vec![
            ("claude", 6),
            ("codex", 3),
            ("agents", 1),
            ("gemini", 0),
            ("plugin:plug", 1),
            ("plugin:syncp", 1)
        ]
    );
    assert!(
        !inv.skill_roots
            .iter()
            .find(|r| r.id == "gemini")
            .unwrap()
            .exists
    );

    // read_skill_md
    let text = ad_inventory::read_skill_md(std::path::Path::new(&gamma.real_path)).unwrap();
    assert!(text.contains("Folded line one"));
    assert!(ad_inventory::read_skill_md(&env.home.join(".claude/skills/notaskill")).is_err());
}

#[test]
fn skill_groups() {
    let env = common::setup();
    let inv = scan(&env.home, &env.state);
    let group = |name: &str| inv.skill_groups.iter().find(|g| g.name == name);

    // .DS_Store 不同也算一致
    let alpha = group("alpha").unwrap();
    assert_eq!(alpha.entry_ids.len(), 2);
    assert!(alpha.identical);
    let beta = group("beta").unwrap();
    assert!(!beta.identical);
    let shared = group("shared").unwrap();
    assert_eq!(shared.entry_ids.len(), 3);
    assert!(shared.identical);
    assert!(group("gamma").is_none());
    assert_eq!(
        skill(&inv, "claude", "alpha").content_hash,
        skill(&inv, "codex", "alpha").content_hash
    );
}

#[test]
fn hooks_and_plugins() {
    let env = common::setup();
    let inv = scan(&env.home, &env.state);

    let user: Vec<_> = inv
        .hooks
        .iter()
        .filter(|h| h.client == Client::ClaudeCode && h.scope == "user")
        .collect();
    assert_eq!(user.len(), 3);
    let pre1 = user.iter().find(|h| h.command == "echo pre1").unwrap();
    assert_eq!(pre1.event, "PreToolUse");
    assert_eq!(pre1.matcher.as_deref(), Some("Bash"));
    assert_eq!(pre1.timeout_sec, Some(10));
    assert!(pre1.manageable);
    let stop = user.iter().find(|h| h.command == "say done").unwrap();
    assert_eq!(stop.matcher, None);

    let proj = inv
        .hooks
        .iter()
        .find(|h| h.command == "npm run lint")
        .unwrap();
    assert_eq!(proj.scope, "project");
    assert!(!proj.manageable);
    let local = inv
        .hooks
        .iter()
        .find(|h| h.command == "./local-stop.sh")
        .unwrap();
    assert_eq!(local.scope, "local");

    let plug = inv.hooks.iter().find(|h| h.scope == "plugin").unwrap();
    assert_eq!(plug.event, "PostToolUse");
    assert!(!plug.manageable);

    let notify = inv.hooks.iter().find(|h| h.event == "notify").unwrap();
    assert_eq!(notify.client, Client::Codex);
    assert_eq!(notify.command, "/usr/local/bin/notifier turn-ended");
    assert!(notify.manageable);
    assert!(inv
        .hooks
        .iter()
        .any(|h| h.client == Client::Codex && h.event == "SessionStart"));
    assert!(inv.warnings.iter().any(|w| w.contains("features.hooks")));

    let gem = inv
        .hooks
        .iter()
        .find(|h| h.client == Client::Gemini)
        .unwrap();
    assert_eq!(gem.timeout_sec, Some(30), "Gemini 的毫秒换算成秒");

    let p = inv.plugins.iter().find(|p| p.name == "plug").unwrap();
    assert!(p.enabled);
    assert_eq!(p.marketplace.as_deref(), Some("mkt"));
    assert_eq!(p.version.as_deref(), Some("1.0.0"));
    assert_eq!(
        (p.skills, p.mcp_servers, p.hooks, p.commands, p.agents),
        (1, 1, 1, 2, 1)
    );
    let off = inv.plugins.iter().find(|p| p.name == "offplug").unwrap();
    assert!(!off.enabled);
    assert_eq!(off.mcp_servers, 1);
    let synced = inv.plugins.iter().find(|p| p.name == "syncp").unwrap();
    assert!(synced.enabled);
    assert_eq!(synced.version.as_deref(), Some("0.3.0"));
    assert_eq!(synced.marketplace.as_deref(), Some("kw"));
}

#[test]
fn empty_home_is_fine() {
    let tmp = tempfile::tempdir().unwrap();
    let inv = scan(tmp.path(), &tmp.path().join("state"));
    assert!(
        inv.mcp.is_empty()
            && inv.skills.is_empty()
            && inv.hooks.is_empty()
            && inv.plugins.is_empty()
    );
    assert_eq!(inv.skill_roots.len(), 4);
    assert!(inv.warnings.is_empty());
}

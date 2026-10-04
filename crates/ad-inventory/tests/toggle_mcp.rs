mod common;

use ad_inventory::{scan, set_mcp_enabled, set_mcp_enabled_with_cli, Client};
use common::{backups, read, FakeCli};
use std::path::Path;

fn id_of(home: &Path, state: &Path, client: Client, name: &str) -> String {
    scan(home, state)
        .mcp
        .into_iter()
        .find(|m| m.client == client && m.name == name)
        .unwrap()
        .id
}

fn enabled_of(home: &Path, state: &Path, id: &str) -> bool {
    scan(home, state)
        .mcp
        .into_iter()
        .find(|m| m.id == id)
        .unwrap()
        .enabled
}

fn disabled_json(state: &Path) -> serde_json::Value {
    serde_json::from_str(&read(&state.join("inventory/disabled.json"))).unwrap()
}

#[test]
fn json_round_trip_keeps_format_and_order() {
    let env = common::setup();
    let cases = [
        (
            Client::ClaudeDesktop,
            "Library/Application Support/Claude/claude_desktop_config.json",
            vec!["fs", "weather", "notes"],
        ),
        (
            Client::Gemini,
            ".gemini/settings.json",
            vec!["ghttp", "gsse"],
        ),
    ];
    for (client, rel, names) in cases {
        let path = env.home.join(rel);
        let original = read(&path);
        for name in names {
            let id = id_of(&env.home, &env.state, client, name);
            let before_backups = backups(&env.state).len();
            set_mcp_enabled(&env.home, &env.state, &id, false).unwrap();

            let after = read(&path);
            let v: serde_json::Value = serde_json::from_str(&after).unwrap();
            assert!(v["mcpServers"].get(name).is_none(), "{name} 应被删掉");
            // 其余键顺序不变
            let orig_v: serde_json::Value = serde_json::from_str(&original).unwrap();
            let keys =
                |v: &serde_json::Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
            assert_eq!(keys(&v), keys(&orig_v));
            let mut expect: Vec<String> = orig_v["mcpServers"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            expect.retain(|k| k != name);
            assert_eq!(
                v["mcpServers"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>(),
                expect
            );
            // 有备份，存档里有原始配置
            assert_eq!(backups(&env.state).len(), before_backups + 1);
            let store = disabled_json(&env.state);
            assert!(store["mcp"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["id"] == id.as_str()));
            // 停用的仍出现在扫描结果里，id 不变
            assert!(!enabled_of(&env.home, &env.state, &id));

            set_mcp_enabled(&env.home, &env.state, &id, true).unwrap();
            assert_eq!(read(&path), original, "{name} 启用后文件应与原来完全一致");
            assert!(enabled_of(&env.home, &env.state, &id));
            assert!(disabled_json(&env.state)["mcp"]
                .as_array()
                .unwrap()
                .is_empty());
        }
    }
}

#[test]
fn json_disable_twice_then_enable_out_of_order() {
    let env = common::setup();
    let path = env
        .home
        .join("Library/Application Support/Claude/claude_desktop_config.json");
    let original = read(&path);
    let fs_id = id_of(&env.home, &env.state, Client::ClaudeDesktop, "fs");
    let notes_id = id_of(&env.home, &env.state, Client::ClaudeDesktop, "notes");
    set_mcp_enabled(&env.home, &env.state, &fs_id, false).unwrap();
    set_mcp_enabled(&env.home, &env.state, &notes_id, false).unwrap();
    // 重复停用是空操作
    set_mcp_enabled(&env.home, &env.state, &notes_id, false).unwrap();
    set_mcp_enabled(&env.home, &env.state, &fs_id, true).unwrap();
    set_mcp_enabled(&env.home, &env.state, &notes_id, true).unwrap();
    assert_eq!(read(&path), original);
}

#[test]
fn json_native_disabled_and_stale_archive() {
    let env = common::setup();
    let path = env.home.join(".cursor/mcp.json");
    let doc = read(&path);
    // cur 本身带 "disabled": true（客户端自己的停用）；启用会去掉这个标记
    let id = id_of(&env.home, &env.state, Client::Cursor, "cur");
    assert!(!enabled_of(&env.home, &env.state, &id));
    set_mcp_enabled(&env.home, &env.state, &id, true).unwrap();
    assert!(!read(&path).contains("disabled"));
    assert!(enabled_of(&env.home, &env.state, &id));
    // 停用后用户又手动加回同名服务：扫描以现有配置为准，并提示存档没用上
    set_mcp_enabled(&env.home, &env.state, &id, false).unwrap();
    assert!(!read(&path).contains("\"cur\""));
    std::fs::write(&path, &doc).unwrap();
    let inv = scan(&env.home, &env.state);
    assert_eq!(inv.mcp.iter().filter(|m| m.id == id).count(), 1);
    assert!(
        inv.warnings.iter().any(|w| w.contains("「cur」")),
        "存档和现有配置重名要提示"
    );
}

#[test]
fn toml_round_trip_keeps_comments() {
    let env = common::setup();
    let path = env.home.join(".codex/config.toml");
    let original = read(&path);
    for name in ["alpha", "remote", "last", "off"] {
        let id = id_of(&env.home, &env.state, Client::Codex, name);
        if name == "off" {
            // 已被 Codex 自己停用：再停用是空操作，文件不变
            set_mcp_enabled(&env.home, &env.state, &id, false).unwrap();
            assert_eq!(read(&path), original);
            continue;
        }
        set_mcp_enabled(&env.home, &env.state, &id, false).unwrap();
        let after = read(&path);
        assert!(
            !after.contains(&format!("[mcp_servers.{name}]")),
            "{name} 整张表应被删除"
        );
        assert!(after.contains("# Codex 配置") && after.contains("[desktop]"));
        let doc: toml_edit::DocumentMut = after.parse().unwrap();
        assert!(doc["mcp_servers"].get(name).is_none());
        assert!(!enabled_of(&env.home, &env.state, &id));
        // 存档里是 TOML 片段，包含注释
        let store = disabled_json(&env.state);
        let snippet = store["mcp"][0]["config"].as_str().unwrap().to_string();
        assert!(snippet.contains(&format!("[mcp_servers.{name}]")));

        set_mcp_enabled(&env.home, &env.state, &id, true).unwrap();
        assert_eq!(read(&path), original, "{name} 启用后文件应与原来完全一致");
    }
    assert!(!backups(&env.state).is_empty());
}

#[test]
fn toml_native_disabled_can_be_enabled() {
    let env = common::setup();
    let path = env.home.join(".codex/config.toml");
    let id = id_of(&env.home, &env.state, Client::Codex, "off");
    set_mcp_enabled(&env.home, &env.state, &id, true).unwrap();
    let after = read(&path);
    assert!(after.contains("[mcp_servers.off]\ncommand = \"/bin/off\"\n\n[mcp_servers.last]"));
    assert!(enabled_of(&env.home, &env.state, &id));
}

#[test]
fn claude_code_uses_cli_not_direct_edit() {
    let env = common::setup();
    let cli = FakeCli::new(&env.home);
    let id = id_of(&env.home, &env.state, Client::ClaudeCode, "playwright");

    set_mcp_enabled_with_cli(&env.home, &env.state, &id, false, &cli).unwrap();
    {
        let calls = cli.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].0,
            vec!["mcp", "remove", "playwright", "-s", "user"]
        );
        assert_eq!(calls[0].1, env.home);
    }
    assert!(!enabled_of(&env.home, &env.state, &id));
    let store = disabled_json(&env.state);
    assert_eq!(
        store["mcp"][0]["config"]["args"][1],
        "@playwright/mcp@latest"
    );
    assert_eq!(backups(&env.state).len(), 1, "改之前先备份 ~/.claude.json");

    set_mcp_enabled_with_cli(&env.home, &env.state, &id, true, &cli).unwrap();
    {
        let calls = cli.calls.borrow();
        assert_eq!(calls.len(), 2);
        let args = &calls[1].0;
        assert_eq!(&args[..3], &["mcp", "add-json", "playwright"]);
        assert_eq!(&args[4..], &["-s", "user"]);
        let restored: serde_json::Value = serde_json::from_str(&args[3]).unwrap();
        assert_eq!(restored["env"]["PW_TOKEN"], "secret-token-123456");
        assert_eq!(restored["type"], "stdio");
    }
    assert!(enabled_of(&env.home, &env.state, &id));
    assert!(disabled_json(&env.state)["mcp"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn claude_code_local_scope_runs_in_project_dir() {
    let env = common::setup();
    let cli = FakeCli::new(&env.home);
    let id = id_of(&env.home, &env.state, Client::ClaudeCode, "localsrv");
    set_mcp_enabled_with_cli(&env.home, &env.state, &id, false, &cli).unwrap();
    set_mcp_enabled_with_cli(&env.home, &env.state, &id, true, &cli).unwrap();
    let calls = cli.calls.borrow();
    assert_eq!(calls[0].0, vec!["mcp", "remove", "localsrv", "-s", "local"]);
    assert_eq!(calls[0].1, env.home.join("proj"));
    assert_eq!(calls[1].1, env.home.join("proj"));
    assert_eq!(calls[1].0[5], "local");
}

#[test]
fn claude_cli_failure_rolls_back_and_hides_secret() {
    let env = common::setup();
    let mut cli = FakeCli::new(&env.home);
    cli.fail = true;
    let id = id_of(&env.home, &env.state, Client::ClaudeCode, "playwright");
    let err = set_mcp_enabled_with_cli(&env.home, &env.state, &id, false, &cli)
        .unwrap_err()
        .to_string();
    assert!(err.contains("停用失败"), "{err}");
    assert!(
        !err.contains("secret-token-123456"),
        "错误信息不能带出密钥：{err}"
    );
    assert!(enabled_of(&env.home, &env.state, &id));
    assert!(disabled_json(&env.state)["mcp"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn unmanageable_scopes_are_refused() {
    let env = common::setup();
    let cli = FakeCli::new(&env.home);
    for (name, needle) in [
        ("projsrv", "项目"),
        ("plugmcp", "插件"),
        ("orphan", "不存在"),
    ] {
        let id = id_of(&env.home, &env.state, Client::ClaudeCode, name);
        let err = set_mcp_enabled_with_cli(&env.home, &env.state, &id, false, &cli)
            .unwrap_err()
            .to_string();
        assert!(err.contains(needle), "{name}: {err}");
    }
    assert!(cli.calls.borrow().is_empty());
    let err = set_mcp_enabled(&env.home, &env.state, "nope", false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("找不到"));
}

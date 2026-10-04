mod common;

use ad_inventory::{scan, set_hook_enabled, set_skill_enabled, testing::trash_skill_with, Client};
use common::{backups, read};
use std::fs;
use std::path::{Path, PathBuf};

fn skill_id(home: &Path, state: &Path, root: &str, dir: &str) -> String {
    scan(home, state)
        .skills
        .into_iter()
        .find(|s| s.root_id == root && s.dir_name == dir)
        .unwrap()
        .id
}

#[test]
fn skill_disable_enable_moves_directory() {
    let env = common::setup();
    let id = skill_id(&env.home, &env.state, "claude", "beta");
    let orig = env.home.join(".claude/skills/beta");

    set_skill_enabled(&env.home, &env.state, &id, false).unwrap();
    assert!(!orig.exists());
    let archived = env.state.join("inventory/disabled-skills/claude/beta");
    assert!(archived.join("SKILL.md").is_file());
    let inv = scan(&env.home, &env.state);
    let s = inv
        .skills
        .iter()
        .find(|s| s.id == id)
        .expect("停用的技能仍要列出");
    assert!(!s.enabled);
    assert_eq!(s.description, "多行描述\n第二行");
    assert_eq!(s.path, orig.to_string_lossy());
    assert_eq!(s.archived_path.as_deref(), Some(archived.to_str().unwrap()));
    assert!(ad_inventory::read_skill_md(Path::new(&s.real_path)).is_ok());

    set_skill_enabled(&env.home, &env.state, &id, true).unwrap();
    assert!(orig.join("scripts/run.sh").is_file());
    assert!(!archived.exists());
    let s = scan(&env.home, &env.state)
        .skills
        .into_iter()
        .find(|s| s.id == id)
        .unwrap();
    assert!(s.enabled && s.archived_path.is_none());
}

#[test]
fn skill_enable_conflict() {
    let env = common::setup();
    let id = skill_id(&env.home, &env.state, "claude", "alpha");
    set_skill_enabled(&env.home, &env.state, &id, false).unwrap();
    // 原位置又出现了同名技能（例如被同步回来）
    fs::create_dir_all(env.home.join(".claude/skills/alpha")).unwrap();
    fs::write(
        env.home.join(".claude/skills/alpha/SKILL.md"),
        "---\nname: alpha\n---\n",
    )
    .unwrap();
    let inv = scan(&env.home, &env.state);
    assert!(inv.warnings.iter().any(|w| w.contains("又出现在原位置")));
    let disabled = inv
        .skills
        .iter()
        .find(|s| s.dir_name == "alpha" && s.root_id == "claude" && !s.enabled)
        .unwrap();
    assert_ne!(disabled.id, id, "两份同时存在时 id 不能重复");
    let err = set_skill_enabled(&env.home, &env.state, &disabled.id, true)
        .unwrap_err()
        .to_string();
    assert!(err.contains("原位置已经有同名技能"), "{err}");
}

#[test]
fn symlink_skill_moves_link_only() {
    let env = common::setup();
    let id = skill_id(&env.home, &env.state, "claude", "shared");
    let link = env.home.join(".claude/skills/shared");
    let target = env.home.join(".agents/skills/shared");

    set_skill_enabled(&env.home, &env.state, &id, false).unwrap();
    assert!(fs::symlink_metadata(&link).is_err());
    assert!(target.join("SKILL.md").is_file(), "链接目标不能动");
    let archived = env.state.join("inventory/disabled-skills/claude/shared");
    assert!(fs::symlink_metadata(&archived)
        .unwrap()
        .file_type()
        .is_symlink());
    // 相对链接移走后仍按原位置解析出真实目录
    let inv = scan(&env.home, &env.state);
    let s = inv.skills.iter().find(|s| s.id == id).unwrap();
    assert!(!s.enabled && s.is_symlink);
    assert_eq!(s.description, "共享技能");
    assert_eq!(
        PathBuf::from(&s.real_path),
        fs::canonicalize(&target).unwrap()
    );

    set_skill_enabled(&env.home, &env.state, &id, true).unwrap();
    assert_eq!(
        fs::read_link(&link).unwrap(),
        PathBuf::from("../../.agents/skills/shared")
    );
    assert!(link.join("SKILL.md").is_file());
}

#[test]
fn chatcut_synced_skill_warns_when_disabled() {
    let env = common::setup();
    let id = skill_id(&env.home, &env.state, "claude", "chatcut-music");
    set_skill_enabled(&env.home, &env.state, &id, false).unwrap();
    let inv = scan(&env.home, &env.state);
    assert!(inv
        .warnings
        .iter()
        .any(|w| w.contains("chatcut-music") && w.contains("ChatCut")));
    let s = inv.skills.iter().find(|s| s.id == id).unwrap();
    assert_eq!(s.synced_by.as_deref(), Some("ChatCut"));
}

#[test]
fn codex_native_disabled_skill_enable_edits_config() {
    let env = common::setup();
    let id = skill_id(&env.home, &env.state, "codex", "beta");
    let cfg = env.home.join(".codex/config.toml");
    set_skill_enabled(&env.home, &env.state, &id, true).unwrap();
    let after = read(&cfg);
    assert!(!after.contains("[[skills.config]]"));
    assert!(after.contains("[mcp_servers.last]") && after.contains("[desktop]"));
    assert!(!backups(&env.state).is_empty());
    assert!(
        scan(&env.home, &env.state)
            .skills
            .iter()
            .find(|s| s.id == id)
            .unwrap()
            .enabled
    );
}

#[test]
fn plugin_skills_are_read_only() {
    let env = common::setup();
    let id = skill_id(&env.home, &env.state, "plugin:plug", "plugskill");
    let err = set_skill_enabled(&env.home, &env.state, &id, false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("插件"), "{err}");
    let err = trash_skill_with(&env.home, &env.state, &id, &|_| panic!("不应调用"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("插件"), "{err}");
}

#[test]
fn trash_moves_entry_not_target() {
    let env = common::setup();
    let trash = env.state.join("fake-trash");
    fs::create_dir_all(&trash).unwrap();
    let mover = |p: &Path| -> anyhow::Result<()> {
        fs::rename(p, trash.join(p.file_name().unwrap()))?;
        Ok(())
    };

    // 符号链接：只移走链接
    let id = skill_id(&env.home, &env.state, "codex", "shared");
    trash_skill_with(&env.home, &env.state, &id, &mover).unwrap();
    assert!(fs::symlink_metadata(env.home.join(".codex/skills/shared")).is_err());
    assert!(fs::symlink_metadata(trash.join("shared"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(env.home.join(".agents/skills/shared/SKILL.md").is_file());

    // 停用后的技能：从存档位置移走，并清掉存档记录
    let id = skill_id(&env.home, &env.state, "claude", "gamma");
    set_skill_enabled(&env.home, &env.state, &id, false).unwrap();
    trash_skill_with(&env.home, &env.state, &id, &mover).unwrap();
    assert!(trash.join("gamma/skill.md").is_file());
    let inv = scan(&env.home, &env.state);
    assert!(!inv.skills.iter().any(|s| s.dir_name == "gamma"));
    let log = read(&env.state.join("inventory/trashed.json"));
    assert!(log.contains("gamma") && log.contains("shared"));
}

fn hook_id(home: &Path, state: &Path, command: &str) -> String {
    scan(home, state)
        .hooks
        .into_iter()
        .find(|h| h.command == command)
        .unwrap()
        .id
}

#[test]
fn hook_round_trips() {
    let env = common::setup();
    let path = env.home.join(".claude/settings.json");
    let original = read(&path);

    // 组里还有别的钩子
    let id = hook_id(&env.home, &env.state, "echo pre1");
    set_hook_enabled(&env.home, &env.state, &id, false).unwrap();
    let v: serde_json::Value = serde_json::from_str(&read(&path)).unwrap();
    assert_eq!(
        v["hooks"]["PreToolUse"][0]["hooks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let h = scan(&env.home, &env.state)
        .hooks
        .into_iter()
        .find(|h| h.id == id)
        .unwrap();
    assert!(!h.enabled && h.manageable);
    assert_eq!(h.matcher.as_deref(), Some("Bash"));
    set_hook_enabled(&env.home, &env.state, &id, true).unwrap();
    assert_eq!(read(&path), original);

    // 删掉后事件为空：整个事件清理掉
    let id = hook_id(&env.home, &env.state, "say done");
    set_hook_enabled(&env.home, &env.state, &id, false).unwrap();
    let v: serde_json::Value = serde_json::from_str(&read(&path)).unwrap();
    assert!(v["hooks"].get("Stop").is_none());
    set_hook_enabled(&env.home, &env.state, &id, true).unwrap();
    assert_eq!(read(&path), original);

    // 全部停用：hooks 键被删掉，按相反顺序启用后完全恢复
    let ids: Vec<String> = ["echo pre1", "echo pre2", "say done"]
        .iter()
        .map(|c| hook_id(&env.home, &env.state, c))
        .collect();
    for id in &ids {
        set_hook_enabled(&env.home, &env.state, id, false).unwrap();
    }
    let v: serde_json::Value = serde_json::from_str(&read(&path)).unwrap();
    assert!(v.get("hooks").is_none());
    assert_eq!(
        v.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["model", "enabledPlugins", "theme"]
    );
    for id in ids.iter().rev() {
        set_hook_enabled(&env.home, &env.state, id, true).unwrap();
    }
    assert_eq!(read(&path), original);
    assert!(backups(&env.state).len() >= 6);
}

#[test]
fn codex_notify_and_gemini_hook_round_trip() {
    let env = common::setup();
    let cfg = env.home.join(".codex/config.toml");
    let original = read(&cfg);
    let id = scan(&env.home, &env.state)
        .hooks
        .into_iter()
        .find(|h| h.event == "notify")
        .unwrap()
        .id;
    set_hook_enabled(&env.home, &env.state, &id, false).unwrap();
    assert!(!read(&cfg).contains("notify"));
    let h = scan(&env.home, &env.state)
        .hooks
        .into_iter()
        .find(|h| h.id == id)
        .unwrap();
    assert!(!h.enabled);
    assert_eq!(h.command, "/usr/local/bin/notifier turn-ended");
    set_hook_enabled(&env.home, &env.state, &id, true).unwrap();
    assert_eq!(read(&cfg), original);

    let gem = env.home.join(".gemini/settings.json");
    let original = read(&gem);
    let id = hook_id(&env.home, &env.state, "check.sh");
    set_hook_enabled(&env.home, &env.state, &id, false).unwrap();
    assert!(!read(&gem).contains("check.sh"));
    set_hook_enabled(&env.home, &env.state, &id, true).unwrap();
    assert_eq!(read(&gem), original);
}

#[test]
fn project_and_plugin_hooks_are_read_only() {
    let env = common::setup();
    for cmd in ["npm run lint", "${CLAUDE_PLUGIN_ROOT}/fmt.sh"] {
        let id = hook_id(&env.home, &env.state, cmd);
        let err = set_hook_enabled(&env.home, &env.state, &id, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("只能在对应的项目或插件里修改"), "{err}");
    }
    let inv = scan(&env.home, &env.state);
    assert!(
        inv.hooks
            .iter()
            .filter(|h| h.client == Client::ClaudeCode)
            .count()
            >= 6
    );
}

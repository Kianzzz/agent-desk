//! 接入 Claude Code 状态栏：把 `~/.claude/settings.json` 的 `statusLine` 换成一个小脚本，
//! 它把 Claude Code 交给状态栏的输入（含 `rate_limits`）记到 `<state_dir>/claude/last.json`，
//! 再原样交给用户原来的状态栏命令，所以状态栏显示不变。
//!
//! 改设置前先备份；断开时把原来的 `statusLine` 放回去。

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BridgeStatus {
    /// 设置里的状态栏指向本工具的脚本，脚本也在
    Installed,
    NotInstalled,
    /// 设置指向本工具的脚本，但脚本或存档不见了
    Broken,
}

fn dir(state_dir: &Path) -> PathBuf {
    state_dir.join("claude")
}

fn script_path(state_dir: &Path) -> PathBuf {
    dir(state_dir).join("statusline.sh")
}

fn original_cmd_path(state_dir: &Path) -> PathBuf {
    dir(state_dir).join("original.sh")
}

fn original_json_path(state_dir: &Path) -> PathBuf {
    dir(state_dir).join("original-statusline.json")
}

pub fn last_path(state_dir: &Path) -> PathBuf {
    dir(state_dir).join("last.json")
}

fn settings_path(home: &Path) -> PathBuf {
    home.join(".claude").join("settings.json")
}

fn our_command(state_dir: &Path) -> String {
    format!("/bin/sh '{}'", script_path(state_dir).display())
}

fn read_settings(home: &Path) -> Result<Map<String, Value>> {
    let p = settings_path(home);
    match std::fs::read_to_string(&p) {
        Ok(text) if text.trim().is_empty() => Ok(Map::new()),
        Ok(text) => match serde_json::from_str::<Value>(&text).with_context(|| format!("{} 不是有效的 JSON", p.display()))? {
            Value::Object(m) => Ok(m),
            _ => bail!("{} 的内容不是一个对象", p.display()),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(e) => Err(e).with_context(|| format!("读不了 {}", p.display())),
    }
}

fn is_ours(settings: &Map<String, Value>, state_dir: &Path) -> bool {
    settings
        .get("statusLine")
        .and_then(|s| s.get("command"))
        .and_then(Value::as_str)
        .is_some_and(|c| c.contains(&script_path(state_dir).display().to_string()))
}

pub fn status(home: &Path, state_dir: &Path) -> BridgeStatus {
    let Ok(settings) = read_settings(home) else { return BridgeStatus::NotInstalled };
    if !is_ours(&settings, state_dir) {
        BridgeStatus::NotInstalled
    } else if script_path(state_dir).exists() && original_json_path(state_dir).exists() {
        BridgeStatus::Installed
    } else {
        BridgeStatus::Broken
    }
}

fn backup(home: &Path, state_dir: &Path) -> Result<()> {
    let src = settings_path(home);
    if !src.exists() {
        return Ok(());
    }
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let to = state_dir.join("backups").join(stamp.to_string());
    std::fs::create_dir_all(&to)?;
    std::fs::copy(&src, to.join("claude-settings.json")).context("备份 settings.json 失败")?;
    Ok(())
}

fn write_settings(home: &Path, settings: &Map<String, Value>) -> Result<()> {
    let p = settings_path(home);
    std::fs::create_dir_all(p.parent().unwrap())?;
    let tmp = p.with_extension("json.agent-desk-tmp");
    let mut text = serde_json::to_string_pretty(&Value::Object(settings.clone()))?;
    text.push('\n');
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

fn write_script(state_dir: &Path) -> Result<()> {
    let d = dir(state_dir);
    std::fs::create_dir_all(&d)?;
    let script = format!(
        r#"#!/bin/sh
# Agent Desk：记下 Claude Code 交给状态栏的额度，再交给原来的状态栏命令显示。
# 断开：Agent Desk › 设置 › Claude 额度，会把原来的状态栏设置放回去。
dir='{dir}'
input=$(cat)
case "$input" in
  *'"rate_limits"'*)
    tmp="$dir/last.json.$$"
    printf '%s' "$input" > "$tmp" && mv -f "$tmp" "$dir/last.json"
    ;;
esac
if [ -s "$dir/original.sh" ]; then
  printf '%s' "$input" | /bin/sh "$dir/original.sh"
fi
"#,
        dir = d.display()
    );
    let p = script_path(state_dir);
    std::fs::write(&p, script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// 接入。已经接入时只重写脚本（修复）。
pub fn install(home: &Path, state_dir: &Path) -> Result<()> {
    let mut settings = read_settings(home)?;
    if is_ours(&settings, state_dir) && original_json_path(state_dir).exists() {
        return write_script(state_dir);
    }
    let original = settings.get("statusLine").cloned().unwrap_or(Value::Null);
    let original_cmd = original
        .get("command")
        .and_then(Value::as_str)
        .filter(|_| original.get("type").and_then(Value::as_str).unwrap_or("command") == "command")
        .unwrap_or("")
        .to_string();

    std::fs::create_dir_all(dir(state_dir))?;
    std::fs::write(original_json_path(state_dir), serde_json::to_string_pretty(&original)?)?;
    std::fs::write(original_cmd_path(state_dir), original_cmd)?;
    write_script(state_dir)?;

    let mut line = Map::new();
    line.insert("type".into(), Value::String("command".into()));
    line.insert("command".into(), Value::String(our_command(state_dir)));
    if let Some(pad) = original.get("padding") {
        line.insert("padding".into(), pad.clone());
    }
    backup(home, state_dir)?;
    settings.insert("statusLine".into(), Value::Object(line));
    write_settings(home, &settings)
}

/// 断开：把原来的状态栏设置放回去。
pub fn uninstall(home: &Path, state_dir: &Path) -> Result<()> {
    let mut settings = read_settings(home)?;
    if !is_ours(&settings, state_dir) {
        bail!("Claude Code 的状态栏设置已经不是 Agent Desk 的了，没有改动");
    }
    let original: Value = std::fs::read_to_string(original_json_path(state_dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    backup(home, state_dir)?;
    if original.is_null() {
        settings.remove("statusLine");
    } else {
        settings.insert("statusLine".into(), original);
    }
    write_settings(home, &settings)?;
    for p in [script_path(state_dir), original_cmd_path(state_dir), original_json_path(state_dir)] {
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::{Command, Stdio};

    const ORIGINAL: &str = r#"{
  "model": "opus",
  "statusLine": {
    "type": "command",
    "command": "input=$(cat); printf 'ctx %s' \"$(echo \"$input\" | tr -cd 0-9 | head -c 2)\""
  },
  "env": { "A": "1" }
}
"#;

    fn setup() -> (tempfile::TempDir, tempfile::TempDir) {
        let home = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".claude")).unwrap();
        std::fs::write(home.path().join(".claude/settings.json"), ORIGINAL).unwrap();
        (home, state)
    }

    fn run_script(state: &Path, input: &str) -> String {
        let mut child = Command::new("/bin/sh")
            .arg(script_path(state))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap()
    }

    #[test]
    fn install_wraps_and_uninstall_restores() {
        let (home, state) = setup();
        assert_eq!(status(home.path(), state.path()), BridgeStatus::NotInstalled);
        install(home.path(), state.path()).unwrap();
        assert_eq!(status(home.path(), state.path()), BridgeStatus::Installed);

        let s = read_settings(home.path()).unwrap();
        assert_eq!(s.get("model").unwrap(), "opus");
        assert_eq!(s.get("env").unwrap().get("A").unwrap(), "1");
        assert!(s["statusLine"]["command"].as_str().unwrap().contains("statusline.sh"));
        // 备份了原文件
        let backups: Vec<_> = std::fs::read_dir(state.path().join("backups")).unwrap().collect();
        assert_eq!(backups.len(), 1);

        // 原来的状态栏照常输出；有 rate_limits 时记下来
        let out = run_script(state.path(), r#"{"x":42,"rate_limits":{"five_hour":{"used_percentage":10,"resets_at":1}}}"#);
        assert_eq!(out, "ctx 42");
        let last = std::fs::read_to_string(last_path(state.path())).unwrap();
        assert!(last.contains("rate_limits"));
        // 没有 rate_limits 的输入不覆盖上一次的记录
        run_script(state.path(), r#"{"x":7}"#);
        assert!(std::fs::read_to_string(last_path(state.path())).unwrap().contains("rate_limits"));

        // 再装一次不会把自己当成「原来的命令」
        install(home.path(), state.path()).unwrap();
        uninstall(home.path(), state.path()).unwrap();
        let s = read_settings(home.path()).unwrap();
        let restored: Value = serde_json::from_str(ORIGINAL).unwrap();
        assert_eq!(s["statusLine"], restored["statusLine"]);
        assert_eq!(status(home.path(), state.path()), BridgeStatus::NotInstalled);
    }

    #[test]
    fn install_without_previous_statusline() {
        let home = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        install(home.path(), state.path()).unwrap();
        assert_eq!(run_script(state.path(), r#"{"rate_limits":{}}"#), "");
        uninstall(home.path(), state.path()).unwrap();
        let s = read_settings(home.path()).unwrap();
        assert!(s.get("statusLine").is_none());
    }

    #[test]
    fn uninstall_refuses_when_user_changed_it() {
        let (home, state) = setup();
        install(home.path(), state.path()).unwrap();
        let mut s = read_settings(home.path()).unwrap();
        s.insert("statusLine".into(), serde_json::json!({"type":"command","command":"echo mine"}));
        write_settings(home.path(), &s).unwrap();
        assert!(uninstall(home.path(), state.path()).is_err());
        assert_eq!(read_settings(home.path()).unwrap()["statusLine"]["command"], "echo mine");
    }
}

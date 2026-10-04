use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;
use std::path::Path;

use crate::{bridge, is_past, Account, Tool, Window};

pub fn account(home: &Path, state_dir: &Path) -> Account {
    let profile = read_profile(home);
    let logged_in = profile.is_some();
    let plan = profile.as_ref().and_then(plan_label);
    let (windows, observed) = read_windows(state_dir);
    let hint = if !logged_in || !windows.is_empty() {
        None
    } else {
        Some(match bridge::status(home, state_dir) {
            bridge::BridgeStatus::Installed => {
                if observed.is_some() {
                    "最近一次记录里没有额度数据，在终端里用一次 Claude Code 后会更新".into()
                } else {
                    "已接入，在终端里用一次 Claude Code 后会显示额度".into()
                }
            }
            _ => "还没接入 Claude Code 状态栏，在设置里打开「Claude 额度」".into(),
        })
    };
    Account {
        tool: Tool::Claude,
        logged_in,
        login_kind: logged_in.then(|| "Claude 订阅".into()),
        plan,
        quota_source: (!windows.is_empty()).then(|| "Claude Code 状态栏".into()),
        windows,
        hint,
    }
}

/// `~/.claude.json` 的 `oauthAccount`：只有账号资料，没有凭据。
fn read_profile(home: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(home.join(".claude.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let acct = v.get("oauthAccount")?;
    acct.is_object().then(|| acct.clone())
}

fn plan_label(acct: &Value) -> Option<String> {
    let org = acct.get("organizationType").and_then(Value::as_str).unwrap_or("");
    let tier = acct
        .get("userRateLimitTier")
        .and_then(Value::as_str)
        .or_else(|| acct.get("organizationRateLimitTier").and_then(Value::as_str))
        .unwrap_or("");
    let base = match org {
        "claude_max" => "Max",
        "claude_pro" => "Pro",
        "claude_team" => "Team",
        "claude_enterprise" => "Enterprise",
        "" => return None,
        other => other.trim_start_matches("claude_"),
    };
    let mult = ["5x", "20x"].into_iter().find(|m| tier.ends_with(&format!("_{m}")));
    Some(match mult {
        Some(m) => format!("{base} {m}"),
        None => capitalize(base),
    })
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// 读状态栏脚本记下的最后一份输入。返回额度窗口和记录时间。
fn read_windows(state_dir: &Path) -> (Vec<Window>, Option<String>) {
    let path = bridge::last_path(state_dir);
    let Ok(text) = std::fs::read_to_string(&path) else { return (vec![], None) };
    let observed: DateTime<Utc> = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .map(DateTime::<Utc>::from)
        .unwrap_or_else(|_| Utc::now());
    let observed_at = observed.to_rfc3339();
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return (vec![], Some(observed_at)) };
    let Some(rl) = v.get("rate_limits") else { return (vec![], Some(observed_at)) };
    let mut out = Vec::new();
    for (key, kind, label) in [("five_hour", "fiveHour", "5 小时"), ("seven_day", "weekly", "本周")] {
        let Some(w) = rl.get(key) else { continue };
        let Some(used) = w.get("used_percentage").and_then(Value::as_f64) else { continue };
        let resets_at = w
            .get("resets_at")
            .and_then(Value::as_i64)
            .and_then(|s| Utc.timestamp_opt(s, 0).single())
            .map(|t| t.to_rfc3339());
        out.push(Window {
            kind: kind.into(),
            label: label.into(),
            used_percent: used,
            expired: is_past(resets_at.as_deref()),
            resets_at,
            observed_at: observed_at.clone(),
        });
    }
    (out, Some(observed_at))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plan_from_profile() {
        let p = json!({"organizationType":"claude_max","organizationRateLimitTier":"default_claude_max_5x","userRateLimitTier":null});
        assert_eq!(plan_label(&p).as_deref(), Some("Max 5x"));
        let p = json!({"organizationType":"claude_max","organizationRateLimitTier":"default_claude_max_20x"});
        assert_eq!(plan_label(&p).as_deref(), Some("Max 20x"));
        let p = json!({"organizationType":"claude_pro","organizationRateLimitTier":"default_claude_ai"});
        assert_eq!(plan_label(&p).as_deref(), Some("Pro"));
        assert_eq!(plan_label(&json!({})), None);
    }

    #[test]
    fn logged_out_without_profile() {
        let home = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(".claude.json"), r#"{"numStartups": 3}"#).unwrap();
        let a = account(home.path(), state.path());
        assert!(!a.logged_in);
        assert!(a.hint.is_none());
    }

    #[test]
    fn windows_from_statusline_record() {
        let home = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join(".claude.json"),
            r#"{"oauthAccount":{"organizationType":"claude_max","organizationRateLimitTier":"default_claude_max_5x"}}"#,
        )
        .unwrap();
        let future = Utc::now().timestamp() + 3600;
        let past = Utc::now().timestamp() - 60;
        std::fs::create_dir_all(state.path().join("claude")).unwrap();
        std::fs::write(
            bridge::last_path(state.path()),
            json!({"model":{"display_name":"Opus"},"rate_limits":{
                "five_hour":{"used_percentage":42.5,"resets_at":future},
                "seven_day":{"used_percentage":18,"resets_at":past}}})
            .to_string(),
        )
        .unwrap();
        let a = account(home.path(), state.path());
        assert!(a.logged_in);
        assert_eq!(a.plan.as_deref(), Some("Max 5x"));
        assert_eq!(a.windows.len(), 2);
        assert_eq!(a.windows[0].kind, "fiveHour");
        assert_eq!(a.windows[0].used_percent, 42.5);
        assert!(!a.windows[0].expired);
        assert_eq!(a.windows[1].kind, "weekly");
        assert!(a.windows[1].expired);
        assert_eq!(a.quota_source.as_deref(), Some("Claude Code 状态栏"));
    }

    #[test]
    fn hint_when_not_connected() {
        let home = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(".claude.json"), r#"{"oauthAccount":{"organizationType":"claude_pro"}}"#).unwrap();
        let a = account(home.path(), state.path());
        assert!(a.windows.is_empty());
        assert!(a.hint.unwrap().contains("还没接入"));
    }
}

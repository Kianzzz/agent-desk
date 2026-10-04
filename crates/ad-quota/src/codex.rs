use base64::Engine;
use serde_json::Value;
use std::path::Path;

use crate::{is_past, window_label, Account, Tool, Window};

pub fn account(home: &Path, quotas: &[ad_usage::QuotaWindow]) -> Account {
    let login = read_login(home);
    let logged_in = login.is_some();
    let (login_kind, plan) = login.unwrap_or((None, None));
    let windows: Vec<Window> = quotas
        .iter()
        .filter(|q| q.tool == Tool::Codex)
        .map(|q| {
            let (kind, label) = window_label(q.window_minutes);
            Window {
                kind,
                label,
                used_percent: q.used_percent,
                expired: is_past(q.resets_at.as_deref()),
                resets_at: q.resets_at.clone(),
                observed_at: q.observed_at.clone(),
            }
        })
        .collect();
    let hint = (logged_in && windows.is_empty()).then(|| "用一次 Codex 后会显示额度".to_string());
    Account {
        tool: Tool::Codex,
        logged_in,
        login_kind,
        plan,
        quota_source: (!windows.is_empty()).then(|| "Codex 会话日志".into()),
        windows,
        hint,
    }
}

/// 读 `~/.codex/auth.json`，返回（登录方式，套餐）。文件不存在视为未登录。
/// 只解出 ID token 声明里的套餐字段；token 本身不保存、不外发。
fn read_login(home: &Path) -> Option<(Option<String>, Option<String>)> {
    let text = std::fs::read_to_string(home.join(".codex").join("auth.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let mode = v.get("auth_mode").and_then(Value::as_str).unwrap_or("").to_ascii_lowercase();
    let id_token = v.get("tokens").and_then(|t| t.get("id_token")).and_then(Value::as_str);
    let has_key = v.get("OPENAI_API_KEY").and_then(Value::as_str).is_some_and(|s| !s.is_empty());

    if mode == "apikey" || (id_token.is_none() && has_key) {
        return Some((Some("API Key".into()), None));
    }
    let plan = id_token.and_then(plan_from_id_token).map(|p| plan_label(&p));
    if id_token.is_none() && !has_key {
        return None;
    }
    Some((Some("ChatGPT 账号".into()), plan))
}

fn plan_from_id_token(jwt: &str) -> Option<String> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    claims
        .get("https://api.openai.com/auth")
        .and_then(|a| a.get("chatgpt_plan_type"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn plan_label(raw: &str) -> String {
    match raw.to_ascii_lowercase().as_str() {
        "free" => "免费版".into(),
        "plus" => "Plus".into(),
        "pro" => "Pro".into(),
        "prolite" | "pro_lite" | "pro-lite" => "Pro Lite".into(),
        "team" => "Team".into(),
        "business" => "Business".into(),
        "enterprise" => "Enterprise".into(),
        "edu" => "Edu".into(),
        other => {
            // 没见过的套餐名：首字母大写、下划线换成空格
            let mut out = String::new();
            for (i, w) in other.split(['_', '-']).filter(|w| !w.is_empty()).enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                let mut c = w.chars();
                if let Some(f) = c.next() {
                    out.extend(f.to_uppercase());
                    out.push_str(c.as_str());
                }
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_jwt(claims: &str) -> String {
        let e = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!("{}.{}.sig", e.encode(r#"{"alg":"none"}"#), e.encode(claims))
    }

    fn write_auth(home: &Path, body: &str) {
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(home.join(".codex/auth.json"), body).unwrap();
    }

    fn quota(minutes: u64, pct: f64) -> ad_usage::QuotaWindow {
        ad_usage::QuotaWindow {
            tool: Tool::Codex,
            label: String::new(),
            used_percent: pct,
            window_minutes: minutes,
            resets_at: Some((chrono::Utc::now() + chrono::Duration::days(3)).to_rfc3339()),
            observed_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    #[test]
    fn plan_from_chatgpt_login() {
        let home = tempfile::tempdir().unwrap();
        let jwt = fake_jwt(r#"{"email":"a@b.c","https://api.openai.com/auth":{"chatgpt_plan_type":"pro"}}"#);
        write_auth(home.path(), &format!(r#"{{"OPENAI_API_KEY":null,"auth_mode":"chatgpt","tokens":{{"id_token":"{jwt}","access_token":"x"}}}}"#));
        let a = account(home.path(), &[quota(10080, 1.0)]);
        assert!(a.logged_in);
        assert_eq!(a.plan.as_deref(), Some("Pro"));
        assert_eq!(a.login_kind.as_deref(), Some("ChatGPT 账号"));
        assert_eq!(a.windows.len(), 1);
        assert_eq!(a.windows[0].label, "本周");
        assert!(!a.windows[0].expired);
    }

    #[test]
    fn api_key_login_has_no_plan() {
        let home = tempfile::tempdir().unwrap();
        write_auth(home.path(), r#"{"OPENAI_API_KEY":"sk-test","auth_mode":"apikey"}"#);
        let a = account(home.path(), &[]);
        assert!(a.logged_in);
        assert_eq!(a.login_kind.as_deref(), Some("API Key"));
        assert!(a.plan.is_none());
    }

    #[test]
    fn missing_auth_means_logged_out() {
        let home = tempfile::tempdir().unwrap();
        let a = account(home.path(), &[quota(300, 5.0)]);
        assert!(!a.logged_in);
    }

    #[test]
    fn plan_names() {
        assert_eq!(plan_label("prolite"), "Pro Lite");
        assert_eq!(plan_label("plus"), "Plus");
        assert_eq!(plan_label("new_tier"), "New Tier");
    }

    #[test]
    fn five_hour_and_weekly_labels() {
        assert_eq!(window_label(300).1, "5 小时");
        assert_eq!(window_label(10080).1, "本周");
    }
}

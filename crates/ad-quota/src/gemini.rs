use serde_json::Value;
use std::path::Path;

use crate::{Account, Tool};

/// Gemini CLI 只在 settings.json 里记登录方式；账号等级和额度本机没有记录。
pub fn account(home: &Path) -> Account {
    let dir = home.join(".gemini");
    let settings: Option<Value> = std::fs::read_to_string(dir.join("settings.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());
    let selected = settings.as_ref().and_then(|v| {
        v.pointer("/security/auth/selectedType")
            .or_else(|| v.get("selectedAuthType"))
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let login_kind = match selected.as_deref() {
        Some("oauth-personal") | Some("login-with-google") => Some("Google 账号".to_string()),
        Some("gemini-api-key") | Some("use-gemini") => Some("API Key".to_string()),
        Some("vertex-ai") | Some("use-vertex-ai") => Some("Vertex AI".to_string()),
        Some("cloud-shell") => Some("Cloud Shell".to_string()),
        Some(other) if !other.is_empty() => Some(other.to_string()),
        // 没选过登录方式，但留有 Google 登录文件（只看是否存在，不读内容）
        _ if dir.join("oauth_creds.json").exists() => Some("Google 账号".to_string()),
        _ => None,
    };
    let logged_in = login_kind.is_some();
    Account {
        tool: Tool::Gemini,
        logged_in,
        login_kind,
        plan: None,
        windows: vec![],
        quota_source: None,
        hint: logged_in.then(|| "Gemini CLI 不在本机记录账号等级和额度".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_login() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".gemini")).unwrap();
        std::fs::write(home.path().join(".gemini/settings.json"), r#"{"security":{"auth":{"selectedType":"oauth-personal"}}}"#).unwrap();
        let a = account(home.path());
        assert!(a.logged_in);
        assert_eq!(a.login_kind.as_deref(), Some("Google 账号"));
    }

    #[test]
    fn not_installed() {
        let home = tempfile::tempdir().unwrap();
        assert!(!account(home.path()).logged_in);
    }
}

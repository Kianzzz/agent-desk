use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Subscription {
    /// "claude" | "codex" | "gemini"
    pub tool: String,
    pub name: String,
    pub monthly_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// "dark" | "light" | "system"
    pub theme: String,
    pub edge_enabled: bool,
    /// "right" | "left"
    pub edge_side: String,
    pub tray_enabled: bool,
    /// 菜单栏图标旁边显示今天的 token 数（字段名沿用旧版）
    pub tray_show_cost: bool,
    /// "CNY" | "USD"
    pub currency: String,
    pub cny_rate: f64,
    pub subscriptions: Vec<Subscription>,
    pub ignored_findings: Vec<String>,
    pub launch_at_login: bool,
    pub refresh_seconds: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "dark".into(),
            edge_enabled: true,
            edge_side: "right".into(),
            tray_enabled: true,
            tray_show_cost: true,
            currency: "USD".into(),
            cny_rate: 7.1,
            subscriptions: Vec::new(),
            ignored_findings: Vec::new(),
            launch_at_login: false,
            refresh_seconds: 60,
        }
    }
}

impl Settings {
    pub fn load(state_dir: &Path) -> Self {
        std::fs::read_to_string(state_dir.join("settings.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, state_dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(state_dir)?;
        let tmp = state_dir.join("settings.json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(tmp, state_dir.join("settings.json"))?;
        Ok(())
    }

    /// 交给窗口的外观：None 表示跟随系统。
    pub fn native_theme(&self) -> Option<tauri::Theme> {
        match self.theme.as_str() {
            "light" => Some(tauri::Theme::Light),
            "system" => None,
            _ => Some(tauri::Theme::Dark),
        }
    }

    pub fn format_money(&self, usd: f64) -> String {
        if usd >= 1000.0 {
            format!("${:.1}k", usd / 1000.0)
        } else if usd >= 100.0 {
            format!("${usd:.0}")
        } else {
            format!("${usd:.2}")
        }
    }
}

/// 259.4M、672K、1.2B
pub fn format_tokens(n: u64) -> String {
    let v = n as f64;
    if v >= 1e9 {
        format!("{:.1}B", v / 1e9)
    } else if v >= 1e6 {
        format!("{:.1}M", v / 1e6)
    } else if v >= 1e3 {
        format!("{:.0}K", v / 1e3)
    } else {
        n.to_string()
    }
}

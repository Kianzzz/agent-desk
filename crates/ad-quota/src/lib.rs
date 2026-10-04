//! 各 AI 的登录状态、账号等级和订阅额度。
//!
//! 全部来自本机文件，不联网：
//! - Claude：等级读 `~/.claude.json` 的 `oauthAccount`；额度读 Claude Code 交给状态栏的
//!   `rate_limits`，由 [`bridge`] 装的状态栏脚本记到 `<state_dir>/claude/last.json`。
//! - Codex：等级从 `~/.codex/auth.json` 里 ID token 的声明解出（只取套餐字段，不保存、不外发
//!   任何凭据）；额度由 ad-usage 从会话日志里读出后传进来。
//! - Gemini：`~/.gemini/settings.json` 只记录登录方式，等级和额度本机没有。

pub mod bridge;
mod claude;
mod codex;
mod gemini;

use serde::{Deserialize, Serialize};
use std::path::Path;

pub use ad_usage::Tool;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    /// "fiveHour" | "weekly" | "other"
    pub kind: String,
    /// 「5 小时」「本周」
    pub label: String,
    pub used_percent: f64,
    /// RFC 3339
    pub resets_at: Option<String>,
    /// 这个数字是哪一刻记录下来的
    pub observed_at: String,
    /// 刷新时间已过：额度已经重置，数字是旧的
    pub expired: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub tool: Tool,
    pub logged_in: bool,
    /// 「Claude 订阅」「ChatGPT 账号」「API Key」「Google 账号」
    pub login_kind: Option<String>,
    /// 「Max 5x」「Pro」「Plus」
    pub plan: Option<String>,
    pub windows: Vec<Window>,
    /// 额度数据来自哪里，如「Claude Code 状态栏」「Codex 会话日志」
    pub quota_source: Option<String>,
    /// 没有额度时告诉用户为什么、怎么才会有
    pub hint: Option<String>,
}

/// `codex_quotas` 传 ad-usage 快照里的 `quotas`（只会用到 Codex 的）。
pub fn accounts(home: &Path, state_dir: &Path, codex_quotas: &[ad_usage::QuotaWindow]) -> Vec<Account> {
    vec![
        claude::account(home, state_dir),
        codex::account(home, codex_quotas),
        gemini::account(home),
    ]
}

pub(crate) fn window_label(minutes: u64) -> (String, String) {
    match minutes {
        300 => ("fiveHour".into(), "5 小时".into()),
        10080 => ("weekly".into(), "本周".into()),
        m if m % 1440 == 0 => ("other".into(), format!("{} 天", m / 1440)),
        m if m % 60 == 0 => ("other".into(), format!("{} 小时", m / 60)),
        m => ("other".into(), format!("{m} 分钟")),
    }
}

pub(crate) fn is_past(rfc3339: Option<&str>) -> bool {
    rfc3339
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .is_some_and(|t| t < chrono::Utc::now())
}

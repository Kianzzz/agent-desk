//! 读取本机各个 AI 客户端留下的日志，按天、按模型统计 token 和折算费用。
//!
//! 数据源：
//! - Claude Code：`~/.claude/projects/**/*.jsonl`（含 `subagents/`），以及 `~/.config/claude/projects`
//! - Codex：`~/.codex/sessions/**/rollout-*.jsonl`（以及 `~/.codex/archived_sessions`）
//! - Gemini CLI：`~/.gemini/tmp/*/chats/session-*.json`
//! - 其余工具（Grok、OpenCode、Kilo、Qwen、Copilot、Cline、Roo、Kimi、Droid、Amp、Pi、OpenClaw、
//!   CodeBuddy、Crush、Goose）的数据位置见各自的模块和 `tools.rs`
//!
//! 公共类型是和界面约定好的接口（见 docs/CONTRACT.md），改动前先同步界面。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

mod aggregate;
mod amp;
mod cache;
mod claude;
mod cline;
mod codebuddy;
mod codex;
mod copilot;
mod crush;
mod discover;
mod droid;
mod engine;
mod gemini;
mod goose;
mod grok;
mod kimi;
mod opencode;
mod pi;
mod pricing;
mod qwen;
mod record;
mod scan;
mod sqlite;
mod tools;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Tool {
    Claude,
    Codex,
    Gemini,
    /// Grok Build（xAI 官方）和社区版 grok-cli
    Grok,
    /// OpenCode（新旧存储）
    Opencode,
    /// Kilo CLI 和 Kilo Code 扩展
    Kilo,
    Qwen,
    /// GitHub Copilot CLI
    Copilot,
    /// Cline 扩展和 Cline CLI
    Cline,
    /// Roo Code 扩展
    Roo,
    /// Kimi CLI 和 Kimi Code
    Kimi,
    /// Factory Droid
    Droid,
    Amp,
    Pi,
    Openclaw,
    Codebuddy,
    Crush,
    Goose,
}

/// 各家口径统一后的 token 数。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenCounts {
    /// 没有命中缓存的输入
    pub input: u64,
    /// 输出，含推理/思考
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// output 里属于推理/思考的部分，只用于展示
    pub reasoning: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyModelRow {
    /// 本地时区的日期 `YYYY-MM-DD`
    pub date: String,
    pub tool: Tool,
    pub model: String,
    pub tokens: TokenCounts,
    pub cost_usd: f64,
    pub requests: u64,
    /// 找不到价格的请求数（这些请求不计入 cost_usd，而不是按 0 算）
    pub unpriced_requests: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRow {
    /// 工作目录（cwd）
    pub project: String,
    pub tool: Tool,
    pub tokens: TokenCounts,
    pub cost_usd: f64,
    pub requests: u64,
    pub sessions: u32,
    /// RFC 3339
    pub first_active: String,
    pub last_active: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRow {
    pub session_id: String,
    pub tool: Tool,
    pub project: Option<String>,
    /// 第一条用户消息的前 80 个字符（去掉系统提示、命令标签）
    pub title: Option<String>,
    pub models: Vec<String>,
    pub started_at: String,
    pub last_active: String,
    pub tokens: TokenCounts,
    pub cost_usd: f64,
    pub requests: u64,
}

/// 订阅额度窗口。Codex 日志里有官方给的 used_percent；Claude 没有，不输出。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaWindow {
    pub tool: Tool,
    /// 如「5 小时额度」「每周额度」
    pub label: String,
    pub used_percent: f64,
    pub window_minutes: u64,
    pub resets_at: Option<String>,
    /// 这个数字是哪一刻从日志里读到的
    pub observed_at: String,
}

/// Claude 的 5 小时计费窗口（与 ccusage 的 blocks 同口径）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveBlock {
    pub tool: Tool,
    pub start: String,
    pub end: String,
    pub cost_usd: f64,
    pub tokens: TokenCounts,
    pub requests: u64,
    pub burn_rate_usd_per_hour: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceStatus {
    pub tool: Tool,
    pub root: String,
    pub files: u32,
    pub records: u64,
    /// 原日志文件已被删除、但缓存里保留下来的记录数
    pub archived_records: u64,
    pub last_record_at: Option<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSnapshot {
    pub generated_at: String,
    /// 如 "Asia/Shanghai" 或 "+08:00"
    pub timezone: String,
    /// 按日期升序
    pub days: Vec<DailyModelRow>,
    /// 按费用降序
    pub projects: Vec<ProjectRow>,
    /// 最近活跃的 300 个会话，按 last_active 降序
    pub sessions: Vec<SessionRow>,
    pub quotas: Vec<QuotaWindow>,
    pub active_blocks: Vec<ActiveBlock>,
    pub sources: Vec<SourceStatus>,
    pub pricing_updated_at: Option<String>,
    pub unpriced_models: Vec<String>,
    /// 最近 8 天每次 Claude 请求的（Unix 毫秒，折算美元），按时间升序、已去重。
    /// 只在进程内用（估算 Claude 额度），不序列化给界面。
    #[serde(skip)]
    pub claude_costs: Vec<(i64, f64)>,
}

pub struct UsageEngine {
    home: PathBuf,
    state_dir: PathBuf,
    /// 内存里的增量缓存和价格表（第一次 refresh 时从 state_dir 载入）
    state: engine::State,
}

impl UsageEngine {
    /// `state_dir` 下放增量缓存和价格表；不存在就创建。
    pub fn new(home: PathBuf, state_dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(state_dir.join("usage"));
        Self {
            home,
            state_dir,
            state: engine::State::default(),
        }
    }

    /// 增量扫描所有数据源并返回快照。没变的文件（大小和修改时间都没变）不重读。
    pub fn refresh(&mut self) -> anyhow::Result<UsageSnapshot> {
        engine::refresh(&self.home, &self.state_dir, &mut self.state)
    }

    /// 从 LiteLLM 拉最新价格表，过滤后存进 state_dir，返回模型数。
    pub fn update_pricing_from_network(&mut self) -> anyhow::Result<usize> {
        engine::update_pricing(&self.state_dir, &mut self.state)
    }
}

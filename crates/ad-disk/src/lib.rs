//! 扫描 AI 工具在本机产生的文件：各工具的数据目录、每个项目的对话记录和构建产物，
//! 给出「能不能删」的判断，支持移到废纸篓和调用 Claude 订阅做二次评估。
//!
//! 删除只移到废纸篓，从不永久删除。
//! 公共类型是和界面约定好的接口（见 docs/CONTRACT.md），改动前先同步界面。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

mod assess;
mod locations;
mod projects;
mod scan;
mod transcripts;
mod trash_ops;
mod util;
mod walk;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Safety {
    /// 可以放心删：缓存、可重建的产物
    Safe,
    /// 需要看一眼再决定
    Review,
    /// 建议保留
    Keep,
    /// 不允许在本工具里删：配置、凭据、正在使用的数据库
    Protected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskItem {
    /// 路径的哈希
    pub id: String,
    pub path: String,
    /// 中文名，如「Codex 对话记录」
    pub label: String,
    /// 如 "claude_transcripts" | "codex_sessions" | "codex_images" | "claude_scratch" |
    /// "claude_vm" | "cache" | "skill_backups" | "worktree" | "build_artifact" | "database" | "other"
    pub category: String,
    /// "claude" | "codex" | "gemini" | "chatcut" | "other"
    pub tool: String,
    /// 归属的项目目录（cwd）
    pub project: Option<String>,
    pub size_bytes: u64,
    pub file_count: u64,
    /// 目录内最新的修改时间，RFC 3339
    pub modified: String,
    pub safety: Safety,
    /// 中文：为什么这么判断、删了会怎样
    pub reason: String,
    /// 最近 30 分钟内还在写入
    pub in_use: bool,
    /// 对话记录的标题（第一条用户消息，最多 80 字符）
    pub title: Option<String>,
    pub children: Vec<DiskItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectUsage {
    pub project: String,
    pub display_name: String,
    /// 项目文件夹是否还存在
    pub exists: bool,
    pub last_active: String,
    pub sessions: u32,
    /// "claude" | "codex" | "gemini"
    pub tools: Vec<String>,
    pub transcripts_bytes: u64,
    /// 项目文件夹本身的大小（不存在或未扫描时为 None）
    pub folder_bytes: Option<u64>,
    /// 构建产物、worktree 等可清理项
    pub artifacts: Vec<DiskItem>,
    /// 每个对话记录文件一项
    pub transcripts: Vec<DiskItem>,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskReport {
    pub scanned_at: String,
    pub duration_ms: u64,
    pub total_ai_bytes: u64,
    pub disk_total_bytes: u64,
    pub disk_free_bytes: u64,
    /// 按位置：各工具的数据目录，children 是拆开的子目录
    pub locations: Vec<DiskItem>,
    /// 按项目，按 total_bytes 降序
    pub projects: Vec<ProjectUsage>,
}

#[derive(Debug, Clone)]
pub struct DiskScanOptions {
    pub home: PathBuf,
    /// 是否统计项目文件夹本身和里面的构建产物（较慢）
    pub include_project_folders: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashResult {
    pub path: String,
    pub ok: bool,
    pub freed_bytes: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssessInput {
    pub id: String,
    pub path: String,
    pub label: String,
    pub category: String,
    pub project: Option<String>,
    pub project_exists: Option<bool>,
    pub size_bytes: u64,
    pub modified: String,
    pub title: Option<String>,
    pub safety: Safety,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Assessment {
    pub id: String,
    /// "delete" | "keep" | "review"
    pub verdict: String,
    /// 中文理由，一两句话
    pub reason: String,
}

pub fn scan(opts: &DiskScanOptions) -> anyhow::Result<DiskReport> {
    scan::run(opts)
}

/// 移到废纸篓。会重新检查每个路径：受保护的、30 分钟内在写入的、不在已知 AI 目录或
/// 已知项目里的路径一律拒绝。
pub fn move_to_trash(home: &Path, paths: &[String]) -> Vec<TrashResult> {
    trash_ops::move_to_trash(home, paths)
}

/// 用本机 `claude` 命令（走用户的订阅）评估哪些该删。只发送路径、大小、时间、标题这些元数据，
/// 不发送文件内容。
pub fn assess_with_ai(items: &[AssessInput]) -> anyhow::Result<Vec<Assessment>> {
    assess::assess_with_ai(items)
}

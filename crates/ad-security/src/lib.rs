//! 扫描本机 AI 客户端的配置、技能、钩子、项目说明文件和 shell 配置，找出安全隐患。
//!
//! 只读：不修改任何文件。输出里不出现密钥原文。
//! 公共类型是和界面约定好的接口（见 docs/CONTRACT.md），改动前先同步界面。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

mod config;
mod engine;
mod finding;
mod rules;
mod sources;
mod text;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    High,
    Medium,
    Low,
    Info,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Category {
    /// 隐藏字符：Unicode 标签字符、双向控制符、零宽字符
    HiddenChars,
    /// 提示词注入话术
    PromptInjection,
    /// 危险命令：下载即执行、外发环境变量/凭据、读私钥、装开机启动项等
    DangerousCommand,
    /// 配置里的明文密钥
    PlaintextSecret,
    /// 权限放得过宽：跳过所有确认、允许任意命令
    BroadPermission,
    /// 供应链：未锁版本的 npx/uvx 包等
    SupplyChain,
    /// 明文 http 连接远程 MCP
    InsecureTransport,
    /// 凭据文件权限过宽
    FilePermission,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    /// 稳定 id（规则 + 路径 + 行 + 匹配内容的哈希），用于「忽略」
    pub id: String,
    pub rule_id: String,
    pub severity: Severity,
    pub category: Category,
    /// 中文标题，一句话
    pub title: String,
    /// 中文说明：为什么有风险、建议怎么处理
    pub detail: String,
    pub path: String,
    pub line: Option<u32>,
    /// 命中的那一段，密钥打码，最多 160 字符
    pub excerpt: Option<String>,
    /// "skill" | "hook" | "mcp" | "settings" | "instructions" | "credentials" | "shell"
    pub target_kind: String,
    pub target_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub scanned_at: String,
    pub files_scanned: u32,
    pub duration_ms: u64,
    /// 按严重程度排序
    pub findings: Vec<Finding>,
}

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub home: PathBuf,
    /// 额外要扫描说明文件（CLAUDE.md / AGENTS.md / GEMINI.md）的项目目录；
    /// 不传时由 `scan` 自己从客户端记录里找最近用过的项目。
    pub extra_project_dirs: Vec<PathBuf>,
}

/// 只读扫描一遍，返回按严重程度排好序的发现。
pub fn scan(opts: &ScanOptions) -> ScanReport {
    engine::run(opts)
}

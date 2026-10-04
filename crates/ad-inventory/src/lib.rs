//! 汇总本机各个 AI 客户端的 MCP 服务、技能、钩子和插件，并提供可撤销的开关。
//!
//! 公共类型是和界面约定好的接口（见 docs/CONTRACT.md），改动前先同步界面。

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Client {
    ClaudeCode,
    ClaudeDesktop,
    Codex,
    Gemini,
    Cursor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServer {
    /// 稳定 id，跨扫描不变，用于开关
    pub id: String,
    pub client: Client,
    pub name: String,
    /// "user" | "local" | "project" | "plugin"
    pub scope: String,
    /// 项目路径或插件名
    pub scope_path: Option<String>,
    /// "stdio" | "http" | "sse"
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    /// 只给变量名，不给值
    pub env_keys: Vec<String>,
    pub header_keys: Vec<String>,
    pub enabled: bool,
    /// 能否在本工具里开关（项目级、插件级为 false）
    pub manageable: bool,
    pub config_path: String,
    /// npx / uvx / bunx 启动的包，如 "@playwright/mcp@latest"
    pub package: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillRoot {
    /// "claude" | "codex" | "agents" | "gemini" | "plugin:<name>"
    pub id: String,
    pub label: String,
    pub path: String,
    pub exists: bool,
    pub count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub root_id: String,
    pub root_label: String,
    /// 技能目录（未解析符号链接）
    pub path: String,
    /// 解析符号链接后的真实目录
    pub real_path: String,
    pub is_symlink: bool,
    pub size_bytes: u64,
    pub file_count: u32,
    pub modified: String,
    /// 目录内容的哈希，用来判断同名技能是否一致
    pub content_hash: String,
    pub enabled: bool,
    pub manageable: bool,
    /// 目录里有可执行脚本（.sh/.py/.js/.ts 等）
    pub has_scripts: bool,
    /// （新增）磁盘上的目录名；frontmatter 里的 name 可能和它不同（如 chatcut-music 的 name 是 music）
    pub dir_name: String,
    /// （新增）由哪个工具同步进来的，目前只识别 "ChatCut"（根目录里的 .chatcut-desktop-skills.json）。
    /// 这类技能停用后，该工具下次同步时可能会把它放回原位置。
    pub synced_by: Option<String>,
    /// （新增）被本工具停用时，条目现在存放的位置（`<state_dir>/inventory/disabled-skills/...`）；
    /// 启用中的技能为 None。预览 SKILL.md 时请用 `real_path`，它对停用的技能也有效。
    pub archived_path: Option<String>,
}

/// 同名技能出现在多个位置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillGroup {
    pub name: String,
    pub entry_ids: Vec<String>,
    /// 所有位置内容完全一致（或都是指向同一目录的链接）
    pub identical: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookEntry {
    pub id: String,
    pub client: Client,
    /// "user" | "local" | "project" | "plugin"
    pub scope: String,
    pub scope_path: Option<String>,
    /// 如 "PreToolUse"、"Stop"；Codex 的 notify 记为 "notify"
    pub event: String,
    pub matcher: Option<String>,
    pub command: String,
    pub timeout_sec: Option<u32>,
    pub enabled: bool,
    pub manageable: bool,
    pub config_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginEntry {
    pub id: String,
    pub name: String,
    pub marketplace: Option<String>,
    pub version: Option<String>,
    pub enabled: bool,
    pub path: String,
    pub skills: u32,
    pub mcp_servers: u32,
    pub hooks: u32,
    pub commands: u32,
    pub agents: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Inventory {
    pub scanned_at: String,
    pub mcp: Vec<McpServer>,
    pub skill_roots: Vec<SkillRoot>,
    pub skills: Vec<SkillEntry>,
    pub skill_groups: Vec<SkillGroup>,
    pub hooks: Vec<HookEntry>,
    pub plugins: Vec<PluginEntry>,
    pub warnings: Vec<String>,
}

mod cli;
mod frontmatter;
mod hooks;
mod mcp;
mod plugins;
mod skills;
mod store;
mod util;

pub use cli::{ClaudeCli, CliOutput, SystemClaudeCli};

/// 扫描。被本工具停用的条目也要列出来（enabled = false）。
/// 不改任何用户配置；只会在 `<state_dir>/inventory/hash-cache.json` 写技能内容哈希缓存。
pub fn scan(home: &Path, state_dir: &Path) -> Inventory {
    let mut warnings = Vec::new();
    let st = store::DisabledStore::load(state_dir).unwrap_or_else(|e| {
        warnings.push(format!("{e:#}"));
        store::DisabledStore::default()
    });
    let plugin_infos = plugins::scan_plugins(home, &mut warnings);
    let ((skill_roots, skills, skill_groups, skill_warnings), (mcp, hooks, other_warnings)) =
        rayon::join(
            || {
                let mut w = Vec::new();
                let (r, s, g) = skills::scan(home, state_dir, &st, &plugin_infos, &mut w);
                (r, s, g, w)
            },
            || {
                let mut w = Vec::new();
                let mcp: Vec<McpServer> = mcp::scan_records(home, &st, &plugin_infos, &mut w)
                    .into_iter()
                    .map(|r| r.server)
                    .collect();
                let hooks: Vec<HookEntry> = hooks::scan_records(home, &st, &plugin_infos, &mut w)
                    .into_iter()
                    .map(|r| r.entry)
                    .collect();
                (mcp, hooks, w)
            },
        );
    warnings.extend(other_warnings);
    warnings.extend(skill_warnings);
    let mut seen = std::collections::HashSet::new();
    warnings.retain(|w| seen.insert(w.clone()));
    Inventory {
        scanned_at: util::now_rfc3339(),
        mcp,
        skill_roots,
        skills,
        skill_groups,
        hooks,
        plugins: plugin_infos.into_iter().map(|p| p.entry).collect(),
        warnings,
    }
}

/// 停用：把技能条目本身（目录或符号链接）移到 `<state_dir>/inventory/disabled-skills/<root_id>/`；
/// 启用：移回原位，原位已有同名条目时报错。插件里的技能不能开关。
pub fn set_skill_enabled(
    home: &Path,
    state_dir: &Path,
    id: &str,
    enabled: bool,
) -> anyhow::Result<()> {
    skills::set_enabled(home, state_dir, id, enabled)
}

/// 用本机的 `claude` 命令开关 MCP 服务（Claude Code 的 user / local 级通过官方命令修改）。
pub fn set_mcp_enabled(
    home: &Path,
    state_dir: &Path,
    id: &str,
    enabled: bool,
) -> anyhow::Result<()> {
    let cli = LazyCli { home };
    mcp::set_enabled(home, state_dir, id, enabled, &cli)
}

/// 同 [`set_mcp_enabled`]，但由调用方提供 `claude` 命令行的实现（测试或自定义路径时使用）。
pub fn set_mcp_enabled_with_cli(
    home: &Path,
    state_dir: &Path,
    id: &str,
    enabled: bool,
    cli: &dyn ClaudeCli,
) -> anyhow::Result<()> {
    mcp::set_enabled(home, state_dir, id, enabled, cli)
}

/// 只有真正要调用 `claude` 时才去找可执行文件，开关其他客户端时不受影响。
struct LazyCli<'a> {
    home: &'a Path,
}

impl ClaudeCli for LazyCli<'_> {
    fn run(&self, args: &[String], cwd: &Path) -> anyhow::Result<CliOutput> {
        SystemClaudeCli::locate(self.home)?.run(args, cwd)
    }
}

/// 用户级钩子：备份后从配置里删掉那一条并存档，启用时插回原事件和 matcher 下。
pub fn set_hook_enabled(
    home: &Path,
    state_dir: &Path,
    id: &str,
    enabled: bool,
) -> anyhow::Result<()> {
    hooks::set_enabled(home, state_dir, id, enabled)
}

/// 把技能目录移到废纸篓（不是永久删除）。符号链接只移走链接本身。
pub fn trash_skill(home: &Path, state_dir: &Path, id: &str) -> anyhow::Result<()> {
    skills::trash_with(home, state_dir, id, &skills::system_trash)
}

/// 读 SKILL.md 全文，用于预览。
pub fn read_skill_md(skill_dir: &Path) -> anyhow::Result<String> {
    skills::read_skill_md(skill_dir)
}

/// 仅供集成测试使用的内部入口。
#[doc(hidden)]
pub mod testing {
    use std::path::Path;

    /// 用自定义的“废纸篓”函数删除技能，测试时不碰真实废纸篓。
    pub fn trash_skill_with(
        home: &Path,
        state_dir: &Path,
        id: &str,
        trasher: &dyn Fn(&Path) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        crate::skills::trash_with(home, state_dir, id, trasher)
    }
}

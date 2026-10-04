//! 停用条目的存档：`<state_dir>/inventory/disabled.json`。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

use crate::util::atomic_write;
use crate::Client;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DisabledStore {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub mcp: Vec<DisabledMcp>,
    #[serde(default)]
    pub skills: Vec<DisabledSkill>,
    #[serde(default)]
    pub hooks: Vec<DisabledHook>,
}

/// 存档格式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ArchiveFormat {
    /// JSON 配置文件里的一个条目（原始对象）
    Json,
    /// TOML 片段文本
    Toml,
    /// 通过 `claude mcp` 命令移除，原始 JSON 存在这里
    ClaudeCli,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DisabledMcp {
    pub id: String,
    pub client: Client,
    pub scope: String,
    pub scope_path: Option<String>,
    pub name: String,
    pub config_path: String,
    pub format: ArchiveFormat,
    /// JSON：原始配置对象；TOML：片段文本（字符串）
    pub config: Value,
    pub prev_key: Option<String>,
    pub next_key: Option<String>,
    #[serde(default)]
    pub index: usize,
    pub disabled_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DisabledSkill {
    pub id: String,
    pub root_id: String,
    pub dir_name: String,
    pub original_path: String,
    pub archived_path: String,
    pub is_symlink: bool,
    /// 符号链接的原始目标（可能是相对路径，相对于原来的上级目录）
    pub link_target: Option<String>,
    pub synced_by: Option<String>,
    pub disabled_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DisabledHook {
    pub id: String,
    pub client: Client,
    pub scope: String,
    pub config_path: String,
    pub format: ArchiveFormat,
    pub event: String,
    pub matcher: Option<String>,
    /// JSON：钩子对象；TOML（Codex notify）：片段文本
    pub hook: Value,
    /// 钩子所在 matcher 组除 `hooks` 外的字段（组被清空删除后用来重建）
    #[serde(default)]
    pub group: Value,
    #[serde(default)]
    pub event_index: usize,
    #[serde(default)]
    pub group_index: usize,
    #[serde(default)]
    pub hook_index: usize,
    /// 停用时整个 `hooks` 键被删掉了，记下它在根对象里的位置
    #[serde(default)]
    pub hooks_key_index: Option<usize>,
    pub prev_key: Option<String>,
    pub next_key: Option<String>,
    pub disabled_at: String,
}

pub(crate) fn inventory_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("inventory")
}

fn store_path(state_dir: &Path) -> PathBuf {
    inventory_dir(state_dir).join("disabled.json")
}

impl DisabledStore {
    pub fn load(state_dir: &Path) -> Result<Self> {
        let p = store_path(state_dir);
        match fs::read_to_string(&p) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("停用存档已损坏：{}", p.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("无法读取 {}", p.display())),
        }
    }

    pub fn save(&self, state_dir: &Path) -> Result<()> {
        let mut s = self.clone();
        s.version = 1;
        let data = serde_json::to_vec_pretty(&s)?;
        atomic_write(&store_path(state_dir), &data)
    }
}

/// 记录移到废纸篓的技能，方便用户找回。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TrashRecord {
    pub id: String,
    pub name: String,
    pub root_id: String,
    pub original_path: String,
    pub is_symlink: bool,
    pub trashed_at: String,
}

pub(crate) fn append_trash_log(state_dir: &Path, rec: TrashRecord) -> Result<()> {
    let p = inventory_dir(state_dir).join("trashed.json");
    let mut list: Vec<TrashRecord> = fs::read_to_string(&p)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    list.push(rec);
    atomic_write(&p, &serde_json::to_vec_pretty(&list)?)
}

//! 公共小工具：稳定 id、备份、保留格式的 JSON / TOML 读写、原子写入。

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::Client;

/// sha256(parts 用 `|` 连接) 取前 16 位 hex。
pub(crate) fn stable_id(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    h.update(parts.join("|").as_bytes());
    let full = hex::encode(h.finalize());
    full[..16].to_string()
}

pub(crate) fn client_key(c: Client) -> &'static str {
    match c {
        Client::ClaudeCode => "claudeCode",
        Client::ClaudeDesktop => "claudeDesktop",
        Client::Codex => "codex",
        Client::Gemini => "gemini",
        Client::Cursor => "cursor",
    }
}

pub(crate) fn client_label(c: Client) -> &'static str {
    match c {
        Client::ClaudeCode => "Claude Code",
        Client::ClaudeDesktop => "Claude 桌面版",
        Client::Codex => "Codex",
        Client::Gemini => "Gemini",
        Client::Cursor => "Cursor",
    }
}

pub(crate) fn now_rfc3339() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

pub(crate) fn time_to_rfc3339(t: SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Local> = t.into();
    dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

pub(crate) fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// 修改用户配置前备份到 `<state_dir>/backups/<时间戳>/`，返回备份文件路径。
/// 文件不存在时不备份，返回 None。
pub(crate) fn backup_file(home: &Path, state_dir: &Path, file: &Path) -> Result<Option<PathBuf>> {
    if !file.exists() {
        return Ok(None);
    }
    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S%.3f").to_string();
    let rel: PathBuf = match file.strip_prefix(home) {
        Ok(r) => PathBuf::from("home").join(r),
        Err(_) => file
            .components()
            .filter(|c| matches!(c, std::path::Component::Normal(_)))
            .collect(),
    };
    let dest = state_dir.join("backups").join(ts).join(rel);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("无法创建备份目录 {}", parent.display()))?;
    }
    fs::copy(file, &dest).with_context(|| format!("备份 {} 失败", file.display()))?;
    Ok(Some(dest))
}

/// 原子写入：写临时文件再改名；保留原文件权限；原路径是符号链接时写到链接目标。
pub(crate) fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let target = match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        }
        _ => path.to_path_buf(),
    };
    let dir = target
        .parent()
        .ok_or_else(|| anyhow!("路径没有上级目录：{}", target.display()))?;
    fs::create_dir_all(dir).with_context(|| format!("无法创建目录 {}", dir.display()))?;
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{}.agent-desk-{}.tmp", name, std::process::id()));
    {
        let mut f =
            fs::File::create(&tmp).with_context(|| format!("无法写入 {}", tmp.display()))?;
        f.write_all(data)?;
        f.sync_all().ok();
    }
    if let Ok(meta) = fs::metadata(&target) {
        fs::set_permissions(&tmp, meta.permissions()).ok();
    }
    fs::rename(&tmp, &target).with_context(|| format!("无法写入 {}", target.display()))?;
    Ok(())
}

/// 保留缩进风格的 JSON 文档。
pub(crate) struct JsonDoc {
    pub value: Value,
    indent: Option<Vec<u8>>,
    trailing_newline: bool,
}

impl JsonDoc {
    pub fn parse(text: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(text)?;
        let body = text.trim_end();
        let indent = if body.contains('\n') {
            Some(detect_indent(body).unwrap_or_else(|| b"  ".to_vec()))
        } else {
            None
        };
        Ok(JsonDoc {
            value,
            indent,
            trailing_newline: text.ends_with('\n'),
        })
    }

    pub fn read(path: &Path) -> Result<Self> {
        let text =
            fs::read_to_string(path).with_context(|| format!("无法读取 {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("无法解析 {}", path.display()))
    }

    pub fn render(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        match &self.indent {
            Some(ind) => {
                let fmt = serde_json::ser::PrettyFormatter::with_indent(ind);
                let mut ser = serde_json::Serializer::with_formatter(&mut out, fmt);
                serde::Serialize::serialize(&self.value, &mut ser)?;
            }
            None => serde_json::to_writer(&mut out, &self.value)?,
        }
        if self.trailing_newline {
            out.push(b'\n');
        }
        Ok(out)
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        atomic_write(path, &self.render()?)
    }
}

fn detect_indent(text: &str) -> Option<Vec<u8>> {
    for line in text.lines().skip(1) {
        let ws: String = line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        if !ws.is_empty() && line.len() > ws.len() {
            return Some(ws.into_bytes());
        }
    }
    None
}

pub(crate) fn read_toml_doc(path: &Path) -> Result<toml_edit::DocumentMut> {
    let text = fs::read_to_string(path).with_context(|| format!("无法读取 {}", path.display()))?;
    text.parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("无法解析 {}", path.display()))
}

/// 把 TOML 值转成 JSON，方便统一处理。
pub(crate) fn toml_item_to_json(item: &toml_edit::Item) -> Value {
    match item {
        toml_edit::Item::None => Value::Null,
        toml_edit::Item::Value(v) => toml_value_to_json(v),
        toml_edit::Item::Table(t) => {
            let mut m = serde_json::Map::new();
            for (k, v) in t.iter() {
                m.insert(k.to_string(), toml_item_to_json(v));
            }
            Value::Object(m)
        }
        toml_edit::Item::ArrayOfTables(a) => Value::Array(
            a.iter()
                .map(|t| {
                    let mut m = serde_json::Map::new();
                    for (k, v) in t.iter() {
                        m.insert(k.to_string(), toml_item_to_json(v));
                    }
                    Value::Object(m)
                })
                .collect(),
        ),
    }
}

fn toml_value_to_json(v: &toml_edit::Value) -> Value {
    use toml_edit::Value as V;
    match v {
        V::String(s) => Value::String(s.value().clone()),
        V::Integer(i) => Value::from(*i.value()),
        V::Float(f) => serde_json::Number::from_f64(*f.value())
            .map(Value::Number)
            .unwrap_or(Value::Null),
        V::Boolean(b) => Value::Bool(*b.value()),
        V::Datetime(d) => Value::String(d.value().to_string()),
        V::Array(a) => Value::Array(a.iter().map(toml_value_to_json).collect()),
        V::InlineTable(t) => {
            let mut m = serde_json::Map::new();
            for (k, v) in t.iter() {
                m.insert(k.to_string(), toml_value_to_json(v));
            }
            Value::Object(m)
        }
    }
}

/// 把 `item` 插回 `table`，位置放在 `prev` 之后；没有 `prev` 时放在 `next` 之前；都没有就放最后。
/// 普通表的文档位置清空，让它跟着前一张表输出（toml_edit 按位置排序、同位置按遍历顺序）。
pub(crate) fn toml_insert_at(
    table: &mut toml_edit::Table,
    key: toml_edit::Key,
    mut item: toml_edit::Item,
    prev: Option<&str>,
    next: Option<&str>,
) {
    clear_positions(&mut item);
    let keys: Vec<String> = table.iter().map(|(k, _)| k.to_string()).collect();
    let insert_idx = prev
        .and_then(|p| keys.iter().position(|k| k == p).map(|i| i + 1))
        .or_else(|| next.and_then(|n| keys.iter().position(|k| k == n)))
        .unwrap_or(keys.len());
    let mut entries: Vec<(toml_edit::Key, toml_edit::Item)> = Vec::with_capacity(keys.len() + 1);
    for k in &keys {
        if let Some(e) = table.remove_entry(k) {
            entries.push(e);
        }
    }
    entries.insert(insert_idx.min(entries.len()), (key, item));
    for (k, v) in entries {
        table.insert_formatted(&k, v);
    }
}

fn clear_positions(item: &mut toml_edit::Item) {
    match item {
        toml_edit::Item::Table(t) => {
            t.set_position(None);
            for (_, v) in t.iter_mut() {
                clear_positions(v);
            }
        }
        toml_edit::Item::ArrayOfTables(a) => {
            for t in a.iter_mut() {
                t.set_position(None);
                for (_, v) in t.iter_mut() {
                    clear_positions(v);
                }
            }
        }
        _ => {}
    }
}

/// 把一个 TOML 条目序列化成可以存档的文本片段（包含前面的注释）。
/// `parents` 是条目所在的表路径，例如 `["mcp_servers"]`；根级条目传空。
pub(crate) fn toml_snippet(
    parents: &[&str],
    key: &toml_edit::Key,
    item: &toml_edit::Item,
) -> String {
    let mut doc = toml_edit::DocumentMut::new();
    let mut cur = doc.as_table_mut();
    for p in parents {
        let mut t = toml_edit::Table::new();
        t.set_implicit(true);
        cur.insert(p, toml_edit::Item::Table(t));
        cur = cur
            .get_mut(p)
            .and_then(|i| i.as_table_mut())
            .expect("刚插入的表");
    }
    cur.insert_formatted(key, item.clone());
    doc.to_string()
}

/// 从存档片段里取回条目（含键的格式）。
pub(crate) fn toml_from_snippet(
    snippet: &str,
    parents: &[&str],
    name: &str,
) -> Result<(toml_edit::Key, toml_edit::Item)> {
    let mut doc: toml_edit::DocumentMut = snippet.parse().context("存档内容已损坏")?;
    let mut cur = doc.as_table_mut();
    for p in parents {
        cur = cur
            .get_mut(p)
            .and_then(|i| i.as_table_mut())
            .ok_or_else(|| anyhow!("存档内容已损坏"))?;
    }
    cur.remove_entry(name)
        .ok_or_else(|| anyhow!("存档内容已损坏"))
}

/// 规范化路径用于比较（存在时解析符号链接）。
pub(crate) fn canon(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

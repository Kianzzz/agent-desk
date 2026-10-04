//! Gemini CLI：`~/.gemini/tmp/<hash 或项目名>/chats/session-*.json`（整份 JSON），
//! 新版本也有 `session-*.jsonl`（第一行是会话信息，之后每行一条消息，同一条消息会被重写多次）。

use std::collections::HashMap;
use std::io;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::record::{clean_title, fnv64, parse_ts, FileEntry, Rec};

#[derive(Deserialize)]
struct SessionFile {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "projectHash")]
    project_hash: Option<String>,
    #[serde(default)]
    messages: Vec<Message>,
}

#[derive(Deserialize, Default)]
struct Message {
    // jsonl 第一行的会话信息
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "projectHash")]
    project_hash: Option<String>,

    id: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    model: Option<String>,
    tokens: Option<Tokens>,
    content: Option<Value>,
    #[serde(rename = "displayContent")]
    display_content: Option<Value>,
}

#[derive(Deserialize)]
struct Tokens {
    #[serde(default)]
    input: u64,
    #[serde(default)]
    output: u64,
    #[serde(default)]
    cached: u64,
    #[serde(default)]
    thoughts: u64,
    #[serde(default)]
    tool: u64,
}

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => usable_text(s),
        Value::Array(items) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(Value::as_str))
            .find_map(usable_text),
        _ => None,
    }
}

fn usable_text(s: &str) -> Option<String> {
    let t = s.trim_start();
    if t.is_empty() || t.starts_with('<') {
        return None;
    }
    clean_title(t)
}

/// 解析整个文件。`dir_project` 是从目录（`.project_root` 或哈希）推出来的项目路径。
pub(crate) fn parse(
    path: &Path,
    entry: &mut FileEntry,
    dir_project: Option<String>,
    by_hash: &HashMap<String, String>,
) -> io::Result<()> {
    let bytes = std::fs::read(path)?;
    let (session_id, project_hash, messages) =
        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            let mut sid = None;
            let mut phash = None;
            let mut order: Vec<Message> = Vec::new();
            let mut by_id: HashMap<String, usize> = HashMap::new();
            for line in bytes.split(|b| *b == b'\n') {
                if line.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                let Ok(m) = serde_json::from_slice::<Message>(line) else {
                    continue;
                };
                if m.session_id.is_some() && m.kind.is_none() {
                    sid = sid.or(m.session_id);
                    phash = phash.or(m.project_hash);
                    continue;
                }
                if m.kind.is_none() {
                    continue; // `$set` 之类的更新行
                }
                // 同一条消息后写的版本覆盖先写的
                match m.id.clone() {
                    Some(id) => match by_id.get(&id) {
                        Some(&i) => order[i] = m,
                        None => {
                            by_id.insert(id, order.len());
                            order.push(m);
                        }
                    },
                    None => order.push(m),
                }
            }
            (sid, phash, order)
        } else {
            let f: SessionFile = serde_json::from_slice(&bytes)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            (f.session_id, f.project_hash, f.messages)
        };

    let session_id = session_id.unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let project = dir_project.or_else(|| project_hash.and_then(|h| by_hash.get(&h).cloned()));
    let sidx = entry.session_idx(&session_id);
    entry.sessions[sidx as usize].project = project;

    let mut keys = entry.key_index();
    for m in messages {
        let ts = m.timestamp.as_deref().and_then(parse_ts);
        match m.kind.as_deref() {
            Some("user") => {
                let title = m
                    .display_content
                    .as_ref()
                    .and_then(text_of)
                    .or_else(|| m.content.as_ref().and_then(text_of));
                if let Some(t) = title {
                    entry.sessions[sidx as usize].set_title(t, false, ts.unwrap_or(0));
                }
            }
            Some("gemini") => {
                let (Some(tok), Some(ts)) = (m.tokens, ts) else {
                    continue;
                };
                let model = m.model.unwrap_or_else(|| "unknown".to_string());
                let key = match &m.id {
                    Some(id) => fnv64(&[b"gemini", session_id.as_bytes(), id.as_bytes()]),
                    None => fnv64(&[b"gemini", session_id.as_bytes(), &ts.to_le_bytes()]),
                };
                let rec = Rec {
                    ts,
                    key,
                    model: entry.model_idx(&model),
                    session: sidx,
                    // input 含缓存命中；工具调用的提示 token 另算在输入里
                    input: tok.input.saturating_sub(tok.cached) + tok.tool,
                    cache_read: tok.cached,
                    cache_write_5m: 0,
                    cache_write_1h: 0,
                    // thoughts 不在 output 里
                    output: tok.output + tok.thoughts,
                    reasoning: tok.thoughts,
                    flags: 0,
                };
                if !rec.is_empty() {
                    entry.push_dedup(&mut keys, rec);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

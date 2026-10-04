//! Claude Code：`~/.claude/projects/**/*.jsonl`

use std::collections::HashMap;
use std::io;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::record::{clean_title, fnv64, parse_ts, starts_with_tag, FileEntry, Rec, FLAG_FAST};
use crate::scan::{contains, find_str_value, scan_lines, LineSink};

#[derive(Deserialize)]
struct AssistantLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    cwd: Option<String>,
    message: Option<AssistantMessage>,
}

#[derive(Deserialize)]
struct AssistantMessage {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}

#[derive(Deserialize, Default)]
struct Usage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation: Option<CacheCreation>,
    output_tokens_details: Option<OutputDetails>,
    speed: Option<String>,
}

#[derive(Deserialize)]
struct CacheCreation {
    ephemeral_1h_input_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct OutputDetails {
    thinking_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct UserLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    cwd: Option<String>,
    #[serde(rename = "isSidechain")]
    is_sidechain: Option<bool>,
    #[serde(rename = "isMeta")]
    is_meta: Option<bool>,
    message: Option<UserMessage>,
}

#[derive(Deserialize)]
struct UserMessage {
    content: Option<Value>,
}

/// 除了 `<command-…>`、`<local-command…>`、`<system-reminder>`、`<task-notification>` 这类
/// 标签开头的注入内容，还要跳过的前缀。
const SKIP_PREFIXES: [&str; 2] = ["Caveat:", "[Request interrupted"];

fn usable_text(s: &str) -> Option<String> {
    let t = s.trim_start();
    if t.is_empty() || starts_with_tag(t) || SKIP_PREFIXES.iter().any(|p| t.starts_with(p)) {
        return None;
    }
    clean_title(t)
}

/// 用户消息内容 → 标题。工具结果不算用户消息。
pub(crate) fn title_from_content(content: &Value) -> Option<String> {
    match content {
        Value::String(s) => usable_text(s),
        Value::Array(items) => {
            if items
                .iter()
                .any(|i| i.get("type").and_then(Value::as_str) == Some("tool_result"))
            {
                return None;
            }
            items
                .iter()
                .filter(|i| i.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|i| i.get("text").and_then(Value::as_str))
                .find_map(usable_text)
        }
        _ => None,
    }
}

struct Sink<'a> {
    entry: &'a mut FileEntry,
    keys: HashMap<u64, usize>,
    fallback_session: String,
}

impl Sink<'_> {
    fn session(&mut self, id: Option<&str>, cwd: Option<&str>) -> u32 {
        let id = id.unwrap_or(&self.fallback_session).to_string();
        let idx = self.entry.session_idx(&id);
        let s = &mut self.entry.sessions[idx as usize];
        if s.project.is_none() {
            s.project = cwd.filter(|c| !c.is_empty()).map(str::to_string);
        }
        idx
    }

    fn assistant(&mut self, line: &[u8]) -> bool {
        let Ok(l) = serde_json::from_slice::<AssistantLine>(line) else {
            return false;
        };
        if l.kind.as_deref() != Some("assistant") {
            return false;
        }
        let Some(msg) = l.message else { return false };
        let Some(u) = msg.usage else { return false };
        let model = msg.model.unwrap_or_else(|| "unknown".to_string());
        if model == "<synthetic>" {
            return true;
        }
        let Some(ts) = l.timestamp.as_deref().and_then(parse_ts) else {
            return true;
        };
        let session = self.session(l.session_id.as_deref(), l.cwd.as_deref());
        let key = match (&msg.id, &l.request_id) {
            (Some(id), Some(req)) => fnv64(&[b"claude", id.as_bytes(), req.as_bytes()]),
            _ => 0,
        };
        let cache_total = u.cache_creation_input_tokens.unwrap_or(0);
        let (w5, w1h) = match &u.cache_creation {
            // 细分和总数对不上时以总数为准，1 小时以外的部分都按 5 分钟算
            Some(cc) => {
                let h = cc.ephemeral_1h_input_tokens.unwrap_or(0).min(cache_total);
                (cache_total - h, h)
            }
            None => (cache_total, 0),
        };
        let rec = Rec {
            ts,
            key,
            model: self.entry.model_idx(&model),
            session,
            input: u.input_tokens.unwrap_or(0),
            output: u.output_tokens.unwrap_or(0),
            cache_read: u.cache_read_input_tokens.unwrap_or(0),
            cache_write_5m: w5,
            cache_write_1h: w1h,
            reasoning: u
                .output_tokens_details
                .and_then(|d| d.thinking_tokens)
                .unwrap_or(0),
            flags: if u.speed.as_deref() == Some("fast") {
                FLAG_FAST
            } else {
                0
            },
            known_cost: 0,
        };
        self.entry.push_dedup(&mut self.keys, rec);
        true
    }

    fn user(&mut self, line: &[u8]) -> bool {
        // 便宜的预判：这个会话已经有主对话标题了就不解析整行
        let side = contains(line, b"\"isSidechain\":true");
        if let Some(sid) = find_str_value(line, b"\"sessionId\":\"") {
            if let Some(s) = self.entry.find_session(sid) {
                if !s.wants_title(side) {
                    return false;
                }
            }
        }
        let Ok(l) = serde_json::from_slice::<UserLine>(line) else {
            return false;
        };
        if l.kind.as_deref() != Some("user") || l.is_meta == Some(true) {
            return false;
        }
        let Some(title) = l
            .message
            .and_then(|m| m.content)
            .as_ref()
            .and_then(title_from_content)
        else {
            return false;
        };
        let ts = l.timestamp.as_deref().and_then(parse_ts).unwrap_or(0);
        let side = l.is_sidechain.unwrap_or(false);
        let idx = self.session(l.session_id.as_deref(), l.cwd.as_deref());
        self.entry.sessions[idx as usize].set_title(title, side, ts);
        true
    }
}

impl LineSink for Sink<'_> {
    fn want(&mut self, _head: &[u8]) -> bool {
        // Claude 的字段顺序不固定，整行读出来再用子串预筛
        true
    }

    fn line(&mut self, line: &[u8]) -> bool {
        if contains(line, b"\"usage\"") && contains(line, b"\"assistant\"") && self.assistant(line)
        {
            return true;
        }
        if contains(line, b"\"type\":\"user\"") && !contains(line, b"\"type\":\"tool_result\"") {
            return self.user(line);
        }
        false
    }
}

/// 从 `offset` 开始解析，结果追加到 `entry`。返回读到的位置。
pub(crate) fn parse(path: &Path, entry: &mut FileEntry, offset: u64) -> io::Result<u64> {
    let fallback_session = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let keys = entry.key_index();
    let mut sink = Sink {
        entry,
        keys,
        fallback_session,
    };
    scan_lines(path, offset, &mut sink)
}

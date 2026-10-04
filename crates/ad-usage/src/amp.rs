//! Amp：`$XDG_DATA_HOME`（默认 `~/.local/share`）`/amp/threads/T-*.json`，每个线程一个 JSON。
//!
//! 助手消息的 `usage.{model,inputTokens,outputTokens,cacheCreationInputTokens,cacheReadInputTokens,
//! credits,timestamp}` 和 `usageLedger.events[]` 是同一份用量的两种记法（事件用 `toMessageId` 指向消息）。
//! 以消息为准（它有缓存拆分）；指向的消息没有用量的事件（子代理、工具里的调用）另外计入。
//! `inputTokens` 不含缓存；`credits` 按 Amp 的说明等于美元（个人和非企业工作区按 API 原价扣）。

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::claude::title_from_content;
use crate::discover::{self, Ctx, Found, Kind, Source};
use crate::record::{
    clean_title, fnv64, parse_time_str, percent_decode, usd_to_cost, FileEntry, Rec,
};
use crate::Tool;

pub(crate) fn root(ctx: &Ctx) -> PathBuf {
    ctx.xdg_data().join("amp").join("threads")
}

pub(crate) fn discover(ctx: &Ctx, out: &mut Vec<Found>) -> Source {
    let root = root(ctx);
    let mut src = Source::new(Tool::Amp, root.clone());
    let mut paths = Vec::new();
    discover::walk(
        &root,
        11,
        &|n| n.starts_with("T-") && n.ends_with(".json"),
        &mut paths,
        &mut src.errors,
    );
    discover::push_found(Tool::Amp, Kind::Amp, paths, out, &mut src.errors);
    src
}

#[derive(Deserialize)]
struct Thread {
    id: Option<String>,
    created: Option<Value>,
    title: Option<String>,
    #[serde(default)]
    messages: Vec<Message>,
    #[serde(rename = "usageLedger")]
    usage_ledger: Option<Ledger>,
    env: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Message {
    role: Option<String>,
    message_id: Option<Value>,
    usage: Option<Usage>,
    content: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Usage {
    model: Option<String>,
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    credits: Option<f64>,
    timestamp: Option<String>,
}

#[derive(Deserialize)]
struct Ledger {
    #[serde(default)]
    events: Vec<LedgerEvent>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LedgerEvent {
    id: Option<String>,
    timestamp: Option<String>,
    model: Option<String>,
    credits: Option<f64>,
    tokens: Option<LedgerTokens>,
    to_message_id: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LedgerTokens {
    #[serde(default)]
    input: u64,
    #[serde(default)]
    output: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

fn id_str(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// `env.initial.trees[].uri`（`file:///…`）里的工作目录，有就用
fn thread_cwd(env: Option<&Value>) -> Option<String> {
    let trees = env?.get("initial")?.get("trees")?.as_array()?;
    trees.iter().find_map(|t| {
        let uri = t.get("uri")?.as_str()?;
        let p = percent_decode(uri.strip_prefix("file://")?);
        p.starts_with('/').then_some(p)
    })
}

pub(crate) fn parse(path: &Path, entry: &mut FileEntry) -> io::Result<()> {
    let t: Thread = serde_json::from_slice(&std::fs::read(path)?)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let tid = t.id.clone().unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let created = t.created.as_ref().and_then(crate::record::parse_time_value);
    let session = entry.session_idx(&tid);
    {
        let s = &mut entry.sessions[session as usize];
        s.project = thread_cwd(t.env.as_ref());
        if let Some(title) = t.title.as_deref().and_then(clean_title) {
            s.set_title(title, false, 0);
        }
    }
    let mut keys = entry.key_index();
    let mut covered: HashSet<String> = HashSet::new();
    for m in &t.messages {
        if m.role.as_deref() == Some("user") {
            if entry.sessions[session as usize].title.is_none() {
                if let Some(title) = m.content.as_ref().and_then(title_from_content) {
                    entry.sessions[session as usize].set_title(title, false, 0);
                }
            }
            continue;
        }
        let Some(u) = &m.usage else { continue };
        let mid = m.message_id.as_ref().and_then(id_str).unwrap_or_default();
        let Some(ts) = u.timestamp.as_deref().and_then(parse_time_str).or(created) else {
            continue;
        };
        covered.insert(mid.clone());
        let model = u
            .model
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        let rec = Rec {
            ts,
            key: fnv64(&[b"amp", tid.as_bytes(), b"msg", mid.as_bytes()]),
            model: entry.model_idx(&model),
            session,
            input: u.input_tokens,
            output: u.output_tokens,
            cache_read: u.cache_read_input_tokens,
            cache_write_5m: u.cache_creation_input_tokens,
            known_cost: u.credits.map(usd_to_cost).unwrap_or(0),
            ..Default::default()
        };
        if !rec.is_empty() {
            entry.push_dedup(&mut keys, rec);
        }
    }
    for ev in t.usage_ledger.map(|l| l.events).unwrap_or_default() {
        let to = ev.to_message_id.as_ref().and_then(id_str);
        if to.as_ref().is_some_and(|m| covered.contains(m)) {
            continue;
        }
        let Some(ts) = ev.timestamp.as_deref().and_then(parse_time_str).or(created) else {
            continue;
        };
        let tok = ev.tokens.unwrap_or(LedgerTokens {
            input: 0,
            output: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        });
        let model = ev
            .model
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        let ident = ev.id.or(to).unwrap_or_else(|| ts.to_string());
        let rec = Rec {
            ts,
            key: fnv64(&[b"amp", tid.as_bytes(), b"ledger", ident.as_bytes()]),
            model: entry.model_idx(&model),
            session,
            input: tok.input,
            output: tok.output,
            cache_read: tok.cache_read_input_tokens,
            cache_write_5m: tok.cache_creation_input_tokens,
            known_cost: ev.credits.map(usd_to_cost).unwrap_or(0),
            ..Default::default()
        };
        if !rec.is_empty() || rec.known_cost > 0 {
            entry.push_dedup(&mut keys, rec);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::COST_UNITS_PER_USD;

    #[test]
    fn messages_and_unmatched_ledger_events() {
        let dir = tempfile::tempdir().unwrap();
        let threads = dir.path().join(".local/share/amp/threads");
        std::fs::create_dir_all(&threads).unwrap();
        std::fs::write(
            threads.join("T-7f3c.json"),
            r#"{"id":"T-7f3c","created":1789000000000,"title":"整理依赖",
              "env":{"initial":{"trees":[{"displayName":"repo","uri":"file:///Users/alice/my%20repo"}]}},
              "messages":[{"role":"user","messageId":0,"content":[{"type":"text","text":"整理一下"}]},
               {"role":"assistant","messageId":1,"usage":{"model":"claude-sonnet-4-5-20250929","inputTokens":10,"outputTokens":178,"cacheCreationInputTokens":986,"cacheReadInputTokens":11372,"totalInputTokens":12368,"timestamp":"2026-09-19T11:42:10.652Z","credits":0.0213}},
               {"role":"assistant","messageId":2}],
              "usageLedger":{"events":[
               {"id":"evt_1","timestamp":"2026-09-19T11:42:10.700Z","model":"claude-sonnet-4-5-20250929","credits":0.0213,"tokens":{"input":10,"output":178},"operationType":"inference","fromMessageId":0,"toMessageId":1},
               {"id":"evt_2","timestamp":"2026-09-19T11:43:00.000Z","model":"claude-haiku-4-5","credits":0.001,"tokens":{"input":300,"output":20},"operationType":"subagent","fromMessageId":1,"toMessageId":2}]}}"#,
        )
        .unwrap();
        std::fs::write(threads.join("notes.json"), "{}").unwrap();
        let mut found = Vec::new();
        let src = discover(&Ctx::with_vars(dir.path(), &[]), &mut found);
        assert!(src.errors.is_empty());
        assert_eq!(found.len(), 1);
        let mut e = FileEntry::new(Tool::Amp, String::new(), 0, 0);
        parse(&found[0].path, &mut e).unwrap();
        assert_eq!(e.recs.len(), 2);
        let r = &e.recs[0];
        assert_eq!(
            (r.input, r.output, r.cache_read, r.cache_write_5m),
            (10, 178, 11372, 986)
        );
        assert_eq!(r.known_cost as f64 / COST_UNITS_PER_USD, 0.0213);
        assert_eq!(e.models[e.recs[1].model as usize], "claude-haiku-4-5");
        let s = &e.sessions[0];
        assert_eq!(s.id, "T-7f3c");
        assert_eq!(s.title.as_deref(), Some("整理依赖"));
        assert_eq!(s.project.as_deref(), Some("/Users/alice/my repo"));
    }

    #[test]
    fn missing_dir_is_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let mut found = Vec::new();
        let src = discover(&Ctx::with_vars(dir.path(), &[]), &mut found);
        assert!(found.is_empty() && src.errors.is_empty());
    }
}

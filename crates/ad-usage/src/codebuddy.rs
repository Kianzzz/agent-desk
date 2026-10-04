//! CodeBuddy Code（腾讯云 CLI）：`~/.codebuddy/projects/<cwd 斜杠换成 ->/<会话 id>.jsonl`
//!
//! 用量在最终的 `type:"message"`（`role:"assistant"`、`status:"completed"`）或 `type:"function_call"`
//! 行上（同一次调用只出现在其中一行）：`message.usage.{input_tokens,output_tokens,total_tokens,
//! cache_read_input_tokens}`，`input_tokens` 含缓存读（`total = input + output`）；模型在
//! `providerData.model`，推理 `providerData.usage.outputTokensDetails[].reasoning_tokens` 是输出的一部分；
//! `timestamp` 是毫秒。按会话 + `providerData.messageId`（没有就用行 `id`）去重。

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::discover::{self, Ctx, Found, Kind, Source};
use crate::record::{clean_title, fnv64, parse_time_value, FileEntry, Rec};
use crate::scan::{contains, scan_lines, LineSink};
use crate::Tool;

pub(crate) fn root(ctx: &Ctx) -> PathBuf {
    ctx.home.join(".codebuddy").join("projects")
}

pub(crate) fn discover(ctx: &Ctx, out: &mut Vec<Found>) -> Source {
    let root = root(ctx);
    let mut src = Source::new(Tool::Codebuddy, root.clone());
    let mut paths = Vec::new();
    for d in discover::subdirs(&root, &mut src.errors) {
        discover::walk(
            &d,
            11,
            &|n| n.ends_with(".jsonl"),
            &mut paths,
            &mut src.errors,
        );
    }
    discover::push_found(
        Tool::Codebuddy,
        Kind::Codebuddy,
        paths,
        out,
        &mut src.errors,
    );
    src
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Line {
    id: Option<String>,
    timestamp: Option<Value>,
    #[serde(rename = "type")]
    kind: Option<String>,
    role: Option<String>,
    status: Option<String>,
    session_id: Option<String>,
    cwd: Option<String>,
    ai_title: Option<String>,
    content: Option<Value>,
    provider_data: Option<ProviderData>,
    message: Option<Msg>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderData {
    model: Option<String>,
    request_model_id: Option<String>,
    message_id: Option<String>,
    usage: Option<PdUsage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PdUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    #[serde(default)]
    input_tokens_details: Vec<Value>,
    #[serde(default)]
    output_tokens_details: Vec<Value>,
}

#[derive(Deserialize)]
struct Msg {
    usage: Option<MsgUsage>,
    content: Option<Value>,
}

#[derive(Deserialize)]
struct MsgUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

fn detail_sum(v: &[Value], key: &str) -> u64 {
    v.iter()
        .filter_map(|d| d.get(key).and_then(Value::as_u64))
        .sum()
}

struct Sink<'a> {
    entry: &'a mut FileEntry,
    keys: HashMap<u64, usize>,
    fallback_session: String,
}

impl Sink<'_> {
    fn session(&mut self, id: Option<&str>, cwd: Option<&str>) -> u32 {
        let id = id
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.fallback_session)
            .to_string();
        let idx = self.entry.session_idx(&id);
        let s = &mut self.entry.sessions[idx as usize];
        if s.project.is_none() {
            s.project = cwd.filter(|c| !c.is_empty()).map(str::to_string);
        }
        idx
    }
}

impl LineSink for Sink<'_> {
    fn want(&mut self, _head: &[u8]) -> bool {
        true
    }

    fn line(&mut self, line: &[u8]) -> bool {
        let usage = contains(line, b"\"usage\"");
        let title = contains(line, b"\"ai-title\"") || contains(line, b"\"role\":\"user\"");
        if !usage && !title {
            return false;
        }
        let Ok(l) = serde_json::from_slice::<Line>(line) else {
            return false;
        };
        let sidx = self.session(l.session_id.as_deref(), l.cwd.as_deref());
        if l.kind.as_deref() == Some("ai-title") {
            if let Some(t) = l.ai_title.as_deref().and_then(clean_title) {
                // 模型生成的标题比第一条用户消息更合适
                let s = &mut self.entry.sessions[sidx as usize];
                s.title = Some(t);
            }
            return true;
        }
        if l.role.as_deref() == Some("user") {
            if self.entry.sessions[sidx as usize].title.is_none() {
                let text = l
                    .message
                    .as_ref()
                    .and_then(|m| m.content.as_ref())
                    .or(l.content.as_ref())
                    .and_then(crate::record::first_text_title);
                if let Some(t) = text {
                    self.entry.sessions[sidx as usize].set_title(t, false, 0);
                }
            }
            return true;
        }
        let kind = l.kind.as_deref().unwrap_or("");
        if kind != "message" && kind != "function_call" {
            return true;
        }
        if kind == "message" && l.role.as_deref() != Some("assistant") {
            return true;
        }
        // 未完成的调用不计
        if l.status.as_deref().is_some_and(|s| s != "completed") {
            return true;
        }
        let Some(ts) = l.timestamp.as_ref().and_then(parse_time_value) else {
            return true;
        };
        let pd = l.provider_data;
        let (input, output, cr, cw) = match (
            l.message.and_then(|m| m.usage),
            pd.as_ref().and_then(|p| p.usage.as_ref()),
        ) {
            (Some(u), _) => (
                u.input_tokens,
                u.output_tokens,
                u.cache_read_input_tokens,
                u.cache_creation_input_tokens,
            ),
            (None, Some(u)) => (
                u.input_tokens.unwrap_or(0),
                u.output_tokens.unwrap_or(0),
                detail_sum(&u.input_tokens_details, "cached_tokens"),
                0,
            ),
            (None, None) => return true,
        };
        let reasoning = pd
            .as_ref()
            .and_then(|p| p.usage.as_ref())
            .map(|u| detail_sum(&u.output_tokens_details, "reasoning_tokens"))
            .unwrap_or(0);
        let model = pd
            .as_ref()
            .and_then(|p| p.model.clone().or_else(|| p.request_model_id.clone()))
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        let ident = pd
            .as_ref()
            .and_then(|p| p.message_id.clone())
            .or(l.id)
            .unwrap_or_else(|| ts.to_string());
        let sid = self.entry.sessions[sidx as usize].id.clone();
        // input 含缓存读；缓存写字段很少见，含在 input 里时一并减掉
        let uncached = if input >= cr + cw {
            input - cr - cw
        } else {
            input.saturating_sub(cr)
        };
        let rec = Rec {
            ts,
            key: fnv64(&[b"codebuddy", sid.as_bytes(), ident.as_bytes()]),
            model: self.entry.model_idx(&model),
            session: sidx,
            input: uncached,
            output,
            cache_read: cr,
            cache_write_5m: cw,
            reasoning: reasoning.min(output),
            ..Default::default()
        };
        if !rec.is_empty() {
            self.entry.push_dedup(&mut self.keys, rec);
        }
        true
    }
}

pub(crate) fn parse(path: &Path, entry: &mut FileEntry) -> io::Result<()> {
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
    scan_lines(path, 0, &mut sink)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_lines_status_and_dedup() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join(".codebuddy/projects/Users-alice-repo");
        std::fs::create_dir_all(&proj).unwrap();
        let done = r#"{"id":"r1","parentId":"u1","timestamp":1790572628778,"type":"message","role":"assistant","status":"completed","sessionId":"9bc94bda","cwd":"/Users/alice/repo","content":[],"providerData":{"messageId":"m-1","traceId":"t-1","model":"hy4-preview","usage":{"requests":1,"inputTokens":195935,"outputTokens":383,"totalTokens":196318,"inputTokensDetails":[{"cached_tokens":195712}],"outputTokensDetails":[{"reasoning_tokens":34}]},"rawUsage":{"prompt_tokens":195935,"credit":1.26}},"message":{"usage":{"input_tokens":195935,"output_tokens":383,"total_tokens":196318,"cache_read_input_tokens":195712}}}"#;
        let lines = [
            r#"{"id":"u1","timestamp":1790572600000,"type":"message","role":"user","sessionId":"9bc94bda","cwd":"/Users/alice/repo","content":[{"type":"input_text","text":"看看这个"}]}"#,
            done,
            done,
            // function_call 行上的用量（只有 providerData）
            r#"{"id":"r2","timestamp":1790572700000,"type":"function_call","sessionId":"9bc94bda","cwd":"/Users/alice/repo","providerData":{"messageId":"m-2","model":"hy4-preview","usage":{"inputTokens":1000,"outputTokens":50,"inputTokensDetails":[{"cached_tokens":800}]}}}"#,
            // 未完成
            r#"{"id":"r3","timestamp":1790572800000,"type":"message","role":"assistant","status":"incomplete","sessionId":"9bc94bda","providerData":{"messageId":"m-3","model":"hy4-preview"},"message":{"usage":{"input_tokens":5,"output_tokens":5}}}"#,
            r#"{"id":"t1","timestamp":1790572900000,"type":"ai-title","sessionId":"9bc94bda","aiTitle":"排查构建失败"}"#,
        ];
        std::fs::write(proj.join("9bc94bda.jsonl"), lines.join("\n") + "\n").unwrap();
        let mut found = Vec::new();
        let src = discover(&Ctx::with_vars(dir.path(), &[]), &mut found);
        assert!(src.errors.is_empty());
        let mut e = FileEntry::new(Tool::Codebuddy, String::new(), 0, 0);
        parse(&found[0].path, &mut e).unwrap();
        assert_eq!(e.recs.len(), 2);
        let r = &e.recs[0];
        assert_eq!(
            (r.input, r.cache_read, r.output, r.reasoning),
            (223, 195712, 383, 34)
        );
        assert_eq!((e.recs[1].input, e.recs[1].cache_read), (200, 800));
        assert_eq!(e.sessions[0].project.as_deref(), Some("/Users/alice/repo"));
        assert_eq!(e.sessions[0].title.as_deref(), Some("排查构建失败"));
        assert_eq!(e.models, vec!["hy4-preview"]);
    }

    #[test]
    fn missing_dir_is_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let mut found = Vec::new();
        let src = discover(&Ctx::with_vars(dir.path(), &[]), &mut found);
        assert!(found.is_empty() && src.errors.is_empty());
    }
}

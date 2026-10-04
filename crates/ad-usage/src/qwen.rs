//! Qwen Code：`$QWEN_RUNTIME_DIR` > `$QWEN_HOME` > `~/.qwen`，
//! 下面 `projects/<cwd 里非字母数字换成 ->/chats/*.jsonl`（含 `chats/archive/`），
//! 以及后台子代理的 `projects/<…>/subagents/<会话>/agent-*.jsonl`。
//!
//! 每次模型调用写一条 `type:"assistant"`，用量在 `usageMetadata`：`promptTokenCount` 含缓存命中
//! （`cachedContentTokenCount`）；思考 `thoughtsTokenCount` 在 OpenAI 兼容和 qwen-oauth 通道下
//! 已含在 `candidatesTokenCount` 里，只有原生 Gemini 通道是另算的——用 `totalTokenCount`
//! 等于 prompt + candidates + thoughts 来识别后者。
//! 旧版和 Gemini CLI 一样的 `tmp/<hash>/chats` 现在只放工具输出，不再有对话记录。
//! `/branch` 会把记录原样复制到新会话（`uuid` 不变），按 `uuid` 全局去重。

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::discover::{self, Ctx, Found, Kind, Source};
use crate::record::{clean_title, fnv64, parse_time_str, FileEntry, Rec};
use crate::scan::{contains, scan_lines, LineSink};
use crate::Tool;

pub(crate) fn root(ctx: &Ctx) -> PathBuf {
    ctx.path_var("QWEN_RUNTIME_DIR")
        .or_else(|| ctx.path_var("QWEN_HOME"))
        .unwrap_or_else(|| ctx.home.join(".qwen"))
}

pub(crate) fn discover(ctx: &Ctx, out: &mut Vec<Found>) -> Source {
    let root = root(ctx);
    let mut src = Source::new(Tool::Qwen, root.clone());
    let mut paths = Vec::new();
    for proj in discover::subdirs(&root.join("projects"), &mut src.errors) {
        for sub in ["chats", "subagents"] {
            discover::walk(
                &proj.join(sub),
                9,
                &|n| n.ends_with(".jsonl"),
                &mut paths,
                &mut src.errors,
            );
        }
    }
    discover::push_found(Tool::Qwen, Kind::Qwen, paths, out, &mut src.errors);
    src
}

#[derive(Deserialize)]
struct Record {
    uuid: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    cwd: Option<String>,
    model: Option<String>,
    #[serde(rename = "usageMetadata")]
    usage: Option<UsageMetadata>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    parts: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageMetadata {
    #[serde(default)]
    prompt_token_count: u64,
    #[serde(default)]
    candidates_token_count: u64,
    #[serde(default)]
    thoughts_token_count: u64,
    #[serde(default)]
    cached_content_token_count: u64,
    total_token_count: Option<u64>,
}

struct Sink<'a> {
    entry: &'a mut FileEntry,
    keys: HashMap<u64, usize>,
    fallback_session: String,
    /// 非空行的序号（没有 uuid 时用来去重）
    line_no: u64,
    /// 子代理记录没有 model 字段，沿用文件里最近出现的模型
    last_model: Option<String>,
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

    fn record(&mut self, line: &[u8]) -> bool {
        let Ok(r) = serde_json::from_slice::<Record>(line) else {
            return false;
        };
        let ts = r.timestamp.as_deref().and_then(parse_time_str);
        match r.kind.as_deref() {
            Some("user") => {
                let sidx = self.session(r.session_id.as_deref(), r.cwd.as_deref());
                if self.entry.sessions[sidx as usize].title.is_none() {
                    let text = r.message.as_ref().and_then(|m| {
                        m.parts
                            .iter()
                            .filter_map(|p| p.get("text").and_then(Value::as_str))
                            .map(str::trim_start)
                            .find(|t| !t.is_empty() && !t.starts_with('<'))
                            .and_then(clean_title)
                    });
                    if let Some(t) = text {
                        self.entry.sessions[sidx as usize].set_title(t, false, ts.unwrap_or(0));
                    }
                }
                true
            }
            Some("assistant") => {
                if let Some(m) = r.model.as_deref().filter(|m| !m.is_empty()) {
                    self.last_model = Some(m.to_string());
                }
                let (Some(u), Some(ts)) = (r.usage, ts) else {
                    return true;
                };
                let sidx = self.session(r.session_id.as_deref(), r.cwd.as_deref());
                let model = self
                    .last_model
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string());
                // 原生 Gemini 通道：思考另算，要加进输出
                let separate = u.thoughts_token_count > 0
                    && u.total_token_count
                        == Some(
                            u.prompt_token_count
                                + u.candidates_token_count
                                + u.thoughts_token_count,
                        );
                let output = if separate {
                    u.candidates_token_count + u.thoughts_token_count
                } else {
                    u.candidates_token_count
                };
                let key = match r.uuid.as_deref().filter(|s| !s.is_empty()) {
                    Some(id) => fnv64(&[b"qwen", id.as_bytes()]),
                    None => {
                        let sid = self.entry.sessions[sidx as usize].id.clone();
                        fnv64(&[b"qwen", sid.as_bytes(), &self.line_no.to_le_bytes()])
                    }
                };
                let rec = Rec {
                    ts,
                    key,
                    model: self.entry.model_idx(&model),
                    session: sidx,
                    input: u
                        .prompt_token_count
                        .saturating_sub(u.cached_content_token_count),
                    output,
                    cache_read: u.cached_content_token_count.min(u.prompt_token_count),
                    reasoning: u.thoughts_token_count.min(output),
                    ..Default::default()
                };
                if !rec.is_empty() {
                    self.entry.push_dedup(&mut self.keys, rec);
                }
                true
            }
            _ => false,
        }
    }
}

impl LineSink for Sink<'_> {
    fn want(&mut self, _head: &[u8]) -> bool {
        // 每个非空行都会经过这里一次：用来数行号
        self.line_no += 1;
        true
    }

    fn line(&mut self, line: &[u8]) -> bool {
        let assistant = contains(line, b"\"type\":\"assistant\"");
        let user = !assistant && contains(line, b"\"type\":\"user\"");
        if !(assistant || user) {
            return false;
        }
        if user {
            // 已经有标题的会话不再解析用户消息
            let has_title = self
                .entry
                .sessions
                .last()
                .is_some_and(|s| s.title.is_some());
            if has_title {
                return false;
            }
        }
        self.record(line)
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
        line_no: 0,
        last_model: None,
    };
    scan_lines(path, 0, &mut sink)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tools/qwen-home")
    }

    #[test]
    fn chats_branches_and_token_semantics() {
        let mut found = Vec::new();
        let src = discover(&Ctx::with_vars(&home(), &[]), &mut found);
        assert!(src.errors.is_empty());
        found.sort_by(|a, b| a.path.cmp(&b.path));
        let names: Vec<String> = found
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec![
                "q-arch.jsonl",
                "q-branch.jsonl",
                "q1.jsonl",
                "agent-a1.jsonl"
            ]
        );

        let parse_one = |name: &str| {
            let f = found.iter().find(|f| f.path.ends_with(name)).unwrap();
            let mut e = FileEntry::new(Tool::Qwen, String::new(), 0, 0);
            parse(&f.path, &mut e).unwrap();
            e
        };
        let e = parse_one("q1.jsonl");
        assert_eq!(e.sessions[0].id, "q1");
        assert_eq!(
            e.sessions[0].project.as_deref(),
            Some("/Users/test/qwen_proj")
        );
        assert_eq!(e.sessions[0].title.as_deref(), Some("加个测试"));
        assert_eq!(e.recs.len(), 3);
        // OpenAI 通道：promptTokenCount 含缓存，思考已在 candidates 里
        let r = &e.recs[0];
        assert_eq!(
            (r.input, r.cache_read, r.output, r.reasoning),
            (1406, 11008, 115, 39)
        );
        // 原生 Gemini 通道：total = prompt + candidates + thoughts，思考加进输出
        let g = &e.recs[1];
        assert_eq!((g.input, g.output, g.reasoning), (1000, 150, 50));
        // 缺字段：只有 promptTokenCount
        assert_eq!((e.recs[2].input, e.recs[2].output), (20, 0));

        // 分支会话复制了第一条（uuid 相同）
        let b = parse_one("q-branch.jsonl");
        assert_eq!(b.recs.len(), 2);
        assert_eq!(b.recs[0].key, e.recs[0].key);
        assert_ne!(b.recs[1].key, e.recs[0].key);

        // 子代理记录没有 model：沿用文件里出现过的，再没有就是 unknown
        let a = parse_one("agent-a1.jsonl");
        assert_eq!(a.models, vec!["unknown"]);
        assert_eq!(a.sessions[0].id, "q1");
    }

    #[test]
    fn env_precedence_and_missing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Ctx::with_vars(
            dir.path(),
            &[("QWEN_HOME", "/a"), ("QWEN_RUNTIME_DIR", "/b")],
        );
        assert_eq!(root(&ctx), PathBuf::from("/b"));
        let ctx = Ctx::with_vars(dir.path(), &[("QWEN_HOME", "/a")]);
        assert_eq!(root(&ctx), PathBuf::from("/a"));
        let ctx = Ctx::with_vars(dir.path(), &[]);
        let mut found = Vec::new();
        let src = discover(&ctx, &mut found);
        assert!(found.is_empty() && src.errors.is_empty());
        assert_eq!(src.root, dir.path().join(".qwen"));
    }
}

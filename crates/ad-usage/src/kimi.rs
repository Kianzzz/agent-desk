//! Kimi：
//! - Kimi CLI（kimi-cli）：`$KIMI_SHARE_DIR`（默认 `~/.kimi`）`/sessions/<md5(工作目录)>/<会话 id>/wire.jsonl`，
//!   子代理在 `<会话>/subagents/<id>/wire.jsonl`。每次模型调用一行 `StatusUpdate`，
//!   `token_usage.{input_other,output,input_cache_read,input_cache_creation}`，`input_other` 不含缓存；
//!   时间是浮点秒；行里没有模型名，取 `config.toml` 的默认模型。父会话里还会用 `SubagentEvent`
//!   包一份子代理的 `StatusUpdate`，和子代理自己的文件重复，按 `message_id` 全局去重（fork 复制的也一样）。
//! - Kimi Code：`$KIMI_CODE_HOME`（默认 `~/.kimi-code`）`/sessions/<目录键>/<会话 id>/agents/<代理>/wire.jsonl`，
//!   读 `type:"usage.record"` 且 `usageScope:"turn"` 的行，`usage.{inputOther,output,inputCacheRead,inputCacheCreation}`，
//!   `time` 是毫秒。工作目录记在 `session_index.jsonl`。Kimi 桌面版的 Work 运行时用同样的格式。

use std::collections::HashMap;
use std::io;
use std::path::{Component, Path, PathBuf};

use md5::{Digest, Md5};
use serde::Deserialize;
use serde_json::Value;

use crate::discover::{self, Ctx, Found, Kind, Source};
use crate::record::{clean_title, fnv64, parse_time_value, FileEntry, Rec};
use crate::scan::{contains, scan_lines, LineSink};
use crate::Tool;

pub(crate) fn share_dir(ctx: &Ctx) -> PathBuf {
    ctx.path_var("KIMI_SHARE_DIR")
        .unwrap_or_else(|| ctx.home.join(".kimi"))
}

fn code_roots(ctx: &Ctx) -> Vec<PathBuf> {
    vec![
        ctx.path_var("KIMI_CODE_HOME")
            .unwrap_or_else(|| ctx.home.join(".kimi-code")),
        ctx.app_support()
            .join("kimi-desktop/daimon-share/daimon/runtime/kimi-code/home"),
    ]
}

/// `sessions/<分组>/<会话>/…` 里的会话 id 和分组名
fn session_parts(sessions_root: &Path, file: &Path) -> Option<(String, String)> {
    let rel = file.strip_prefix(sessions_root).ok()?;
    let mut it = rel.components().filter_map(|c| match c {
        Component::Normal(s) => s.to_str(),
        _ => None,
    });
    let group = it.next()?.to_string();
    let sid = it.next()?.to_string();
    Some((group, sid))
}

/// `~/.kimi/kimi.json` 登记的工作目录：md5(路径) → 路径
fn kimi_work_dirs(share: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(text) = std::fs::read_to_string(share.join("kimi.json")) else {
        return map;
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return map;
    };
    for wd in v
        .get("work_dirs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(p) = wd.get("path").and_then(Value::as_str) {
            map.insert(hex::encode(Md5::digest(p.as_bytes())), p.to_string());
        }
    }
    map
}

/// Kimi Code 的 `session_index.jsonl`：会话 id → 工作目录
fn code_work_dirs(root: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(text) = std::fs::read_to_string(root.join("session_index.jsonl")) else {
        return map;
    };
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let (Some(s), Some(w)) = (
            v.get("sessionId").and_then(Value::as_str),
            v.get("workDir").and_then(Value::as_str),
        ) {
            map.insert(s.to_string(), w.to_string());
        }
    }
    map
}

pub(crate) fn discover(ctx: &Ctx, out: &mut Vec<Found>) -> Source {
    let share = share_dir(ctx);
    let mut src = Source::new(Tool::Kimi, share.clone());

    let sessions = share.join("sessions");
    let dirs = kimi_work_dirs(&share);
    let mut paths = Vec::new();
    discover::walk(
        &sessions,
        6,
        &|n| n == "wire.jsonl",
        &mut paths,
        &mut src.errors,
    );
    for p in paths {
        // 非本地后端的分组名是 `<kaos>_<md5>`
        let project = session_parts(&sessions, &p).and_then(|(g, _)| {
            let h = g.rsplit('_').next().unwrap_or(&g);
            dirs.get(h).cloned()
        });
        discover::push_with(
            Tool::Kimi,
            Kind::KimiWire,
            p,
            project,
            discover::stat,
            out,
            &mut src.errors,
        );
    }

    for root in code_roots(ctx) {
        let sessions = root.join("sessions");
        let dirs = code_work_dirs(&root);
        let mut paths = Vec::new();
        discover::walk(
            &sessions,
            6,
            &|n| n == "wire.jsonl",
            &mut paths,
            &mut src.errors,
        );
        for p in paths {
            let project = session_parts(&sessions, &p).and_then(|(_, s)| dirs.get(&s).cloned());
            discover::push_with(
                Tool::Kimi,
                Kind::KimiCode,
                p,
                project,
                discover::stat,
                out,
                &mut src.errors,
            );
        }
    }
    src
}

/// `config.toml`：`default_model = "<别名>"`，`[models.<别名>] model = "<模型>"`。
fn default_model(share: &Path) -> Option<String> {
    let text = std::fs::read_to_string(share.join("config.toml")).ok()?;
    let doc: toml_edit::DocumentMut = text.parse().ok()?;
    let alias = doc.get("default_model")?.as_str()?.to_string();
    let concrete = doc
        .get("models")
        .and_then(|m| m.get(&alias))
        .and_then(|m| m.get("model"))
        .and_then(|m| m.as_str())
        .map(str::to_string);
    concrete.or(Some(alias)).filter(|m| !m.is_empty())
}

fn sessions_root_of(path: &Path) -> Option<(PathBuf, PathBuf)> {
    // 往上找名为 sessions 的目录：(它的上一级即数据根目录, sessions 目录)
    let mut cur = path.parent();
    while let Some(p) = cur {
        if p.file_name().is_some_and(|n| n == "sessions") {
            return Some((p.parent()?.to_path_buf(), p.to_path_buf()));
        }
        cur = p.parent();
    }
    None
}

// ---------- kimi-cli ----------

#[derive(Deserialize)]
struct WireLine {
    timestamp: Option<Value>,
    message: Option<WireMsg>,
}

#[derive(Deserialize)]
struct WireMsg {
    #[serde(rename = "type")]
    kind: Option<String>,
    payload: Option<Value>,
}

#[derive(Deserialize)]
struct StatusPayload {
    token_usage: Option<TokenUsage>,
    message_id: Option<String>,
}

#[derive(Deserialize)]
struct TokenUsage {
    #[serde(default)]
    input_other: u64,
    #[serde(default)]
    output: u64,
    #[serde(default)]
    input_cache_read: u64,
    #[serde(default)]
    input_cache_creation: u64,
}

fn user_text(v: &Value) -> Option<String> {
    let s = match v {
        Value::String(s) => s.trim_start(),
        Value::Array(items) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(Value::as_str))
            .map(str::trim_start)
            .find(|t| !t.is_empty())?,
        _ => return None,
    };
    if s.is_empty() || s.starts_with('<') {
        return None;
    }
    clean_title(s)
}

struct WireSink<'a> {
    entry: &'a mut FileEntry,
    keys: HashMap<u64, usize>,
    session: u32,
    session_id: String,
    model: u32,
}

impl WireSink<'_> {
    fn status(&mut self, ts: i64, p: StatusPayload, wrapped: bool) {
        let Some(u) = p.token_usage else { return };
        let mid = p.message_id.filter(|m| !m.is_empty());
        // 父会话里包着的子代理用量：子代理自己的文件里也有，没有 message_id 就无法对上，不计
        if wrapped && mid.is_none() {
            return;
        }
        let key = match &mid {
            Some(m) => fnv64(&[b"kimi", m.as_bytes()]),
            None => fnv64(&[
                b"kimi",
                self.session_id.as_bytes(),
                &ts.to_le_bytes(),
                &u.input_other.to_le_bytes(),
                &u.output.to_le_bytes(),
            ]),
        };
        let rec = Rec {
            ts,
            key,
            model: self.model,
            session: self.session,
            input: u.input_other,
            output: u.output,
            cache_read: u.input_cache_read,
            cache_write_5m: u.input_cache_creation,
            ..Default::default()
        };
        if !rec.is_empty() {
            self.entry.push_dedup(&mut self.keys, rec);
        }
    }

    fn message(&mut self, ts: Option<i64>, kind: &str, payload: Value, depth: u32) {
        match kind {
            "StatusUpdate" => {
                if let (Some(ts), Ok(p)) = (ts, serde_json::from_value::<StatusPayload>(payload)) {
                    self.status(ts, p, depth > 0);
                }
            }
            "TurnBegin" if depth == 0 => {
                if self.entry.sessions[self.session as usize].title.is_none() {
                    if let Some(t) = payload.get("user_input").and_then(user_text) {
                        self.entry.sessions[self.session as usize].set_title(
                            t,
                            false,
                            ts.unwrap_or(0),
                        );
                    }
                }
            }
            "SubagentEvent" if depth < 4 => {
                let Some(ev) = payload.get("event") else {
                    return;
                };
                let (Some(k), Some(p)) = (
                    ev.get("type").and_then(Value::as_str).map(str::to_string),
                    ev.get("payload").cloned(),
                ) else {
                    return;
                };
                self.message(ts, &k, p, depth + 1);
            }
            _ => {}
        }
    }
}

impl LineSink for WireSink<'_> {
    fn want(&mut self, _head: &[u8]) -> bool {
        true
    }

    fn line(&mut self, line: &[u8]) -> bool {
        if !(contains(line, b"StatusUpdate") || contains(line, b"TurnBegin")) {
            return false;
        }
        let Ok(l) = serde_json::from_slice::<WireLine>(line) else {
            return false;
        };
        let ts = l.timestamp.as_ref().and_then(parse_time_value);
        let Some(m) = l.message else { return true };
        if let (Some(k), Some(p)) = (m.kind, m.payload) {
            self.message(ts, &k, p, 0);
        }
        true
    }
}

pub(crate) fn parse_wire(
    path: &Path,
    project: Option<String>,
    entry: &mut FileEntry,
) -> io::Result<()> {
    let (share, sessions) = sessions_root_of(path).unwrap_or_default();
    let session_id = session_parts(&sessions, path)
        .map(|(_, s)| s)
        .unwrap_or_else(|| {
            path.parent()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
    let model = default_model(&share).unwrap_or_else(|| "kimi".to_string());
    let session = entry.session_idx(&session_id);
    entry.sessions[session as usize].project = project;
    let model = entry.model_idx(&model);
    let keys = entry.key_index();
    let mut sink = WireSink {
        entry,
        keys,
        session,
        session_id,
        model,
    };
    scan_lines(path, 0, &mut sink)?;
    Ok(())
}

// ---------- Kimi Code ----------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodeLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    model: Option<String>,
    usage: Option<CodeUsage>,
    usage_scope: Option<String>,
    time: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodeUsage {
    #[serde(default)]
    input_other: u64,
    #[serde(default)]
    output: u64,
    #[serde(default)]
    input_cache_read: u64,
    #[serde(default)]
    input_cache_creation: u64,
}

fn clean_code_model(m: &str) -> Option<String> {
    let m = m.trim();
    let m = m.strip_prefix("kimi-code/").unwrap_or(m);
    // `__kimi_env_model__` 这类占位名不是真实模型
    (!m.is_empty() && !m.starts_with("__")).then(|| m.to_string())
}

struct CodeSink<'a> {
    entry: &'a mut FileEntry,
    keys: HashMap<u64, usize>,
    session: u32,
    file_tag: String,
    last_request_model: Option<String>,
}

impl LineSink for CodeSink<'_> {
    fn want(&mut self, head: &[u8]) -> bool {
        contains(head, b"usage.record") || contains(head, b"llm.request")
    }

    fn line(&mut self, line: &[u8]) -> bool {
        let Ok(l) = serde_json::from_slice::<CodeLine>(line) else {
            return false;
        };
        match l.kind.as_deref() {
            Some("llm.request") => {
                if let Some(m) = l.model.as_deref().and_then(clean_code_model) {
                    self.last_request_model = Some(m);
                }
            }
            Some("usage.record") => {
                if l.usage_scope.as_deref().is_some_and(|s| s != "turn") {
                    return true;
                }
                let (Some(u), Some(ts)) = (l.usage, l.time.as_ref().and_then(parse_time_value))
                else {
                    return true;
                };
                let model = l
                    .model
                    .as_deref()
                    .and_then(clean_code_model)
                    .or_else(|| self.last_request_model.clone())
                    .unwrap_or_else(|| "kimi".to_string());
                let key = fnv64(&[
                    b"kimi-code",
                    self.file_tag.as_bytes(),
                    &ts.to_le_bytes(),
                    &u.input_other.to_le_bytes(),
                    &u.output.to_le_bytes(),
                    &u.input_cache_read.to_le_bytes(),
                ]);
                let rec = Rec {
                    ts,
                    key,
                    model: self.entry.model_idx(&model),
                    session: self.session,
                    input: u.input_other,
                    output: u.output,
                    cache_read: u.input_cache_read,
                    cache_write_5m: u.input_cache_creation,
                    ..Default::default()
                };
                if !rec.is_empty() {
                    self.entry.push_dedup(&mut self.keys, rec);
                }
            }
            _ => return false,
        }
        true
    }
}

pub(crate) fn parse_code(
    path: &Path,
    project: Option<String>,
    entry: &mut FileEntry,
) -> io::Result<()> {
    let (_, sessions) = sessions_root_of(path).unwrap_or_default();
    let session_id = session_parts(&sessions, path)
        .map(|(_, s)| s)
        .unwrap_or_default();
    // 同一会话的各个代理各有一个文件
    let agent = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let session = entry.session_idx(&session_id);
    entry.sessions[session as usize].project = project;
    let keys = entry.key_index();
    let mut sink = CodeSink {
        entry,
        keys,
        session,
        file_tag: format!("{session_id}/{agent}"),
        last_request_model: None,
    };
    scan_lines(path, 0, &mut sink)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tools/kimi-home")
    }

    fn parse_found(f: &Found) -> FileEntry {
        let mut e = FileEntry::new(Tool::Kimi, String::new(), 0, 0);
        match f.kind {
            Kind::KimiWire => parse_wire(&f.path, f.project.clone(), &mut e).unwrap(),
            _ => parse_code(&f.path, f.project.clone(), &mut e).unwrap(),
        }
        e
    }

    #[test]
    fn kimi_cli_wire_files() {
        let mut found = Vec::new();
        let src = discover(&Ctx::with_vars(&home(), &[]), &mut found);
        assert!(src.errors.is_empty());
        let wires: Vec<&Found> = found.iter().filter(|f| f.kind == Kind::KimiWire).collect();
        assert_eq!(wires.len(), 2);
        let is_sub = |f: &&&Found| f.path.to_string_lossy().contains("subagents");
        let main = parse_found(wires.iter().find(|f| !is_sub(f)).unwrap());
        assert_eq!(main.sessions[0].id, "s-1");
        assert_eq!(
            main.sessions[0].project.as_deref(),
            Some("/Users/test/kimiproj")
        );
        assert_eq!(main.sessions[0].title.as_deref(), Some("重构一下"));
        assert_eq!(main.models, vec!["kimi-for-coding"]);
        // 两次 StatusUpdate + 一个带 message_id 的子代理包装；同一 message_id 写了两次只算一次；
        // 没有 message_id 的包装不计
        assert_eq!(main.recs.len(), 3);
        let r = &main.recs[0];
        assert_eq!(
            (r.input, r.output, r.cache_read, r.cache_write_5m),
            (1562, 2463, 16870, 0)
        );
        assert_eq!(r.ts, 1_770_983_426_420);
        // 子代理自己的文件：和包装的那条键相同
        let sub = parse_found(wires.iter().find(is_sub).unwrap());
        assert_eq!(sub.sessions[0].id, "s-1");
        assert_eq!(sub.recs.len(), 1);
        assert!(main.recs.iter().any(|r| r.key == sub.recs[0].key));
    }

    #[test]
    fn kimi_code_usage_records() {
        let mut found = Vec::new();
        discover(&Ctx::with_vars(&home(), &[]), &mut found);
        let code: Vec<&Found> = found.iter().filter(|f| f.kind == Kind::KimiCode).collect();
        assert_eq!(code.len(), 1);
        let e = parse_found(code[0]);
        assert_eq!(e.sessions[0].id, "kc-1");
        assert_eq!(e.sessions[0].project.as_deref(), Some("/Users/test/kc"));
        // turn 两条（其中一条模型是占位名，用前面 llm.request 的模型）；session 范围的不计
        assert_eq!(e.recs.len(), 2);
        assert_eq!(e.models, vec!["kimi-for-coding", "kimi-k2.6"]);
        assert_eq!(e.models[e.recs[1].model as usize], "kimi-k2.6");
        assert_eq!(e.recs[0].ts, 1_782_113_184_943);
        assert_eq!((e.recs[0].input, e.recs[0].cache_read), (3064, 14848));
    }

    #[test]
    fn missing_dirs_and_env() {
        let dir = tempfile::tempdir().unwrap();
        let mut found = Vec::new();
        let src = discover(&Ctx::with_vars(dir.path(), &[]), &mut found);
        assert!(found.is_empty() && src.errors.is_empty());
        assert_eq!(src.root, dir.path().join(".kimi"));
        let ctx = Ctx::with_vars(
            dir.path(),
            &[("KIMI_SHARE_DIR", "/s"), ("KIMI_CODE_HOME", "/c")],
        );
        assert_eq!(share_dir(&ctx), PathBuf::from("/s"));
        assert_eq!(code_roots(&ctx)[0], PathBuf::from("/c"));
        assert_eq!(
            clean_code_model("kimi-code/kimi-for-coding").as_deref(),
            Some("kimi-for-coding")
        );
        assert_eq!(clean_code_model("__kimi_env_model__"), None);
    }
}

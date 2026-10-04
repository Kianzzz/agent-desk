//! Pi（pi-mono coding agent）和 OpenClaw，两者的会话事件是同一种格式：
//! - Pi：`$PI_CODING_AGENT_DIR`（默认 `~/.pi/agent`）`/sessions/--<cwd>--/*.jsonl`，
//!   以及 `$PI_CODING_AGENT_SESSION_DIR`
//! - OpenClaw：`$OPENCLAW_STATE_DIR`（默认 `~/.openclaw`，旧名 `~/.clawdbot`、`~/.moltbot`）下
//!   `agents/<id>/agent/openclaw-agent.sqlite` 的 `transcript_events` 表，
//!   旧版 `agents/<id>/sessions/*.jsonl`（含 `.jsonl.deleted.*`、`.jsonl.reset.*` 归档）
//!
//! 第一行（或第一条事件）是会话头 `{"type":"session","id","cwd"}`；用量在
//! `type:"message"` 的 `message.usage.{input,output,cacheRead,cacheWrite,cacheWrite1h,cost.total}`，
//! `input` 不含缓存，推理已含在 `output` 里，`cacheWrite1h` 是 `cacheWrite` 的一部分。
//! Pi 自己统计会话总量时还算上 `usage` 事件、压缩/分支摘要和工具结果里的用量，这里同样计入。
//! `/fork` 会把事件原样复制到新文件（id、时间、用量都相同），去重键因此不含会话。

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::claude::title_from_content;
use crate::discover::{self, Ctx, Found, Kind, Source};
use crate::record::{fnv64, parse_time_value, usd_to_cost, FileEntry, Rec};
use crate::scan::{contains, scan_lines, LineSink};
use crate::sqlite;
use crate::Tool;

// ---------- 路径 ----------

pub(crate) fn pi_root(ctx: &Ctx) -> PathBuf {
    ctx.path_var("PI_CODING_AGENT_DIR")
        .unwrap_or_else(|| ctx.home.join(".pi").join("agent"))
        .join("sessions")
}

pub(crate) fn discover_pi(ctx: &Ctx, out: &mut Vec<Found>) -> Source {
    let root = pi_root(ctx);
    let mut src = Source::new(Tool::Pi, root.clone());
    let mut dirs = vec![root];
    if let Some(d) = ctx.path_var("PI_CODING_AGENT_SESSION_DIR") {
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    let mut paths = Vec::new();
    for d in &dirs {
        discover::walk(
            d,
            0,
            &|n| n.ends_with(".jsonl"),
            &mut paths,
            &mut src.errors,
        );
    }
    paths.sort();
    paths.dedup();
    discover::push_found(Tool::Pi, Kind::Pi, paths, out, &mut src.errors);
    src
}

fn openclaw_states(ctx: &Ctx) -> Vec<PathBuf> {
    let main = ctx.path_var("OPENCLAW_STATE_DIR").unwrap_or_else(|| {
        ctx.path_var("OPENCLAW_HOME")
            .unwrap_or_else(|| ctx.home.clone())
            .join(".openclaw")
    });
    let mut v = vec![main];
    for old in [".clawdbot", ".moltbot"] {
        let p = ctx.home.join(old);
        if !v.contains(&p) {
            v.push(p);
        }
    }
    v
}

/// 旧版 jsonl 及其归档；备份（`.bak`）、隔离（`.broken-*`）、压缩归档和检查点不算。
fn openclaw_jsonl_name(n: &str) -> bool {
    if n.ends_with(".bak")
        || n.ends_with(".zst")
        || n.contains(".broken")
        || n.contains(".checkpoint.")
    {
        return false;
    }
    n.ends_with(".jsonl") || n.contains(".jsonl.deleted.") || n.contains(".jsonl.reset.")
}

pub(crate) fn discover_openclaw(ctx: &Ctx, out: &mut Vec<Found>) -> Source {
    let states = openclaw_states(ctx);
    let mut src = Source::new(Tool::Openclaw, states[0].clone());
    for state in &states {
        for agent in discover::subdirs(&state.join("agents"), &mut src.errors) {
            discover::push_with(
                Tool::Openclaw,
                Kind::OpenclawDb,
                agent.join("agent").join("openclaw-agent.sqlite"),
                None,
                discover::stat_db,
                out,
                &mut src.errors,
            );
            let mut paths = Vec::new();
            discover::walk(
                &agent.join("sessions"),
                11,
                &|n| openclaw_jsonl_name(n),
                &mut paths,
                &mut src.errors,
            );
            discover::push_found(Tool::Openclaw, Kind::Pi, paths, out, &mut src.errors);
        }
    }
    src
}

// ---------- 事件 ----------

#[derive(Deserialize)]
struct Event {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<String>,
    timestamp: Option<Value>,
    // 会话头
    cwd: Option<String>,
    // model_change
    #[serde(rename = "modelId")]
    model_id: Option<String>,
    // usage 事件的模型
    model: Option<String>,
    // usage / compaction / branch_summary
    usage: Option<Usage>,
    // session_info
    name: Option<String>,
    // OpenClaw 的模型快照：custom + customType:"model-snapshot"
    #[serde(rename = "customType")]
    custom_type: Option<String>,
    data: Option<ModelSnapshot>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct ModelSnapshot {
    #[serde(rename = "modelId")]
    model_id: Option<String>,
}

#[derive(Deserialize)]
struct Message {
    role: Option<String>,
    api: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    #[serde(rename = "responseId")]
    response_id: Option<String>,
    usage: Option<Usage>,
    timestamp: Option<Value>,
    content: Option<Value>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Usage {
    #[serde(default)]
    input: u64,
    #[serde(default)]
    output: u64,
    #[serde(default)]
    cache_read: u64,
    #[serde(default)]
    cache_write: u64,
    #[serde(default)]
    cache_write1h: u64,
    #[serde(default)]
    reasoning: u64,
    #[serde(default)]
    reasoning_tokens: u64,
    cost: Option<Cost>,
}

#[derive(Deserialize)]
struct Cost {
    total: Option<f64>,
}

struct EventParser<'a> {
    entry: &'a mut FileEntry,
    keys: HashMap<u64, usize>,
    prefix: &'static [u8],
    openclaw: bool,
    session: u32,
    /// 最近一次看到的模型（压缩摘要、工具结果这类没有模型字段的用量用它）
    model: Option<String>,
    /// OpenClaw：会话窗口记录的模型，最后的兜底
    default_model: Option<String>,
}

impl EventParser<'_> {
    fn set_session(&mut self, id: &str) {
        self.session = self.entry.session_idx(id);
    }

    /// 不是真实请求的转录副本（OpenClaw 自己插进去的消息）
    fn is_artifact(m: &Message) -> bool {
        m.api.as_deref() == Some("openclaw-transcript")
            || (m.provider.as_deref() == Some("openclaw")
                && matches!(
                    m.model.as_deref(),
                    Some("delivery-mirror" | "gateway-injected")
                ))
    }

    fn event(&mut self, ev: Event, fallback_ts: Option<i64>) {
        let ts = ev.timestamp.as_ref().and_then(parse_time_value);
        match ev.kind.as_deref() {
            Some("session") => {
                if let Some(id) = ev.id.as_deref().filter(|s| !s.is_empty()) {
                    self.set_session(id);
                }
                if let Some(cwd) = ev.cwd.filter(|c| !c.is_empty()) {
                    let s = &mut self.entry.sessions[self.session as usize];
                    if s.project.is_none() {
                        s.project = Some(cwd);
                    }
                }
            }
            Some("model_change") => {
                if let Some(m) = ev.model_id.filter(|m| !m.is_empty()) {
                    self.model = Some(m);
                }
            }
            Some("custom") if ev.custom_type.as_deref() == Some("model-snapshot") => {
                if let Some(m) = ev.data.and_then(|d| d.model_id).filter(|m| !m.is_empty()) {
                    self.model = Some(m);
                }
            }
            Some("session_info") => {
                if let Some(t) = ev.name.as_deref().and_then(crate::record::clean_title) {
                    self.entry.sessions[self.session as usize].set_title(t, false, 0);
                }
            }
            Some("usage") | Some("compaction") | Some("branch_summary") => {
                if let Some(u) = ev.usage {
                    let model = ev.model.filter(|m| !m.is_empty());
                    let kind = ev.kind.unwrap_or_default();
                    self.push(
                        u,
                        model,
                        ts.or(fallback_ts),
                        ev.id.as_deref(),
                        None,
                        kind.as_bytes(),
                    );
                }
            }
            Some("message") => {
                let Some(m) = ev.message else { return };
                let role = m.role.as_deref().unwrap_or("");
                if role == "user" {
                    let s = &self.entry.sessions[self.session as usize];
                    if s.title.is_none() {
                        if let Some(t) = m.content.as_ref().and_then(title_from_content) {
                            let tts = ts.unwrap_or(0);
                            self.entry.sessions[self.session as usize].set_title(t, false, tts);
                        }
                    }
                    return;
                }
                if role == "assistant" {
                    if self.openclaw && Self::is_artifact(&m) {
                        return;
                    }
                    if let Some(model) = m.model.clone().filter(|s| !s.is_empty()) {
                        self.model = Some(model);
                    }
                }
                if role != "assistant" && role != "toolResult" {
                    return;
                }
                let Some(u) = m.usage else { return };
                // 条目自己的时间优先；没有再看消息里的毫秒时间
                let ts = ts
                    .or_else(|| m.timestamp.as_ref().and_then(parse_time_value))
                    .or(fallback_ts);
                let model = if role == "assistant" {
                    m.model.filter(|s| !s.is_empty())
                } else {
                    None
                };
                let resp = m.response_id.filter(|r| !r.is_empty());
                self.push(
                    u,
                    model,
                    ts,
                    ev.id.as_deref(),
                    resp.as_deref(),
                    role.as_bytes(),
                );
            }
            _ => {}
        }
    }

    fn push(
        &mut self,
        u: Usage,
        model: Option<String>,
        ts: Option<i64>,
        id: Option<&str>,
        response_id: Option<&str>,
        kind: &[u8],
    ) {
        let Some(ts) = ts else { return };
        let model = model
            .or_else(|| self.model.clone())
            .or_else(|| self.default_model.clone())
            .unwrap_or_else(|| "unknown".to_string());
        let w1h = u.cache_write1h.min(u.cache_write);
        let reasoning = u.reasoning.max(u.reasoning_tokens).min(u.output);
        let key = match response_id {
            Some(r) => fnv64(&[self.prefix, b"resp", r.as_bytes()]),
            None => fnv64(&[
                self.prefix,
                kind,
                id.unwrap_or("").as_bytes(),
                &ts.to_le_bytes(),
                &u.input.to_le_bytes(),
                &u.output.to_le_bytes(),
            ]),
        };
        let rec = Rec {
            ts,
            key,
            model: self.entry.model_idx(&model),
            session: self.session,
            input: u.input,
            output: u.output,
            cache_read: u.cache_read,
            cache_write_5m: u.cache_write - w1h,
            cache_write_1h: w1h,
            reasoning,
            flags: 0,
            known_cost: u.cost.and_then(|c| c.total).map(usd_to_cost).unwrap_or(0),
        };
        if !rec.is_empty() {
            self.entry.push_dedup(&mut self.keys, rec);
        }
    }
}

impl LineSink for EventParser<'_> {
    fn want(&mut self, _head: &[u8]) -> bool {
        true
    }

    fn line(&mut self, line: &[u8]) -> bool {
        // 只解析可能有用的行：会话头、模型变化、带用量的事件、还没有标题时的用户消息
        let useful = contains(line, b"\"usage\"")
            || contains(line, b"\"type\":\"session")
            || contains(line, b"\"model_change\"")
            || contains(line, b"\"model-snapshot\"")
            || (self.entry.sessions[self.session as usize].title.is_none()
                && contains(line, b"\"role\":\"user\""));
        if !useful {
            return false;
        }
        match serde_json::from_slice::<Event>(line) {
            Ok(ev) => {
                self.event(ev, None);
                true
            }
            Err(_) => false,
        }
    }
}

/// 一个 jsonl 会话文件（Pi 或 OpenClaw 旧版）。
pub(crate) fn parse_jsonl(path: &Path, tool: Tool, entry: &mut FileEntry) -> io::Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = name.split(".jsonl").next().unwrap_or(&name);
    // Pi 的文件名是 `<时间>_<会话 id>.jsonl`；会话头里的 id 会覆盖它
    let fallback = match tool {
        Tool::Pi => stem.split_once('_').map_or(stem, |(_, id)| id),
        _ => stem,
    };
    let session = entry.session_idx(fallback);
    let keys = entry.key_index();
    let openclaw = tool == Tool::Openclaw;
    let mut p = EventParser {
        entry,
        keys,
        prefix: if openclaw { b"openclaw" } else { b"pi" },
        openclaw,
        session,
        model: None,
        default_model: None,
    };
    scan_lines(path, 0, &mut p)?;
    Ok(())
}

/// OpenClaw 的 SQLite：`transcript_events` 每行一条事件。
/// 超过 1 MiB 的事件以 zstd 压缩存放（`event_json` 为空），这里不解压，跳过这些行。
pub(crate) fn parse_openclaw_db(path: &Path, entry: &mut FileEntry) -> io::Result<()> {
    let conn = sqlite::open(path)?;
    if !sqlite::has_table(&conn, "transcript_events")? {
        return Ok(());
    }
    let mut window_model: HashMap<String, String> = HashMap::new();
    let wcols = sqlite::columns(&conn, "session_windows")?;
    if wcols.contains("session_id") && wcols.contains("model") {
        let mut stmt = conn
            .prepare("SELECT session_id, model FROM session_windows")
            .map_err(sqlite::to_io)?;
        let mut rows = stmt.query([]).map_err(sqlite::to_io)?;
        while let Some(r) = rows.next().map_err(sqlite::to_io)? {
            if let (Some(s), Some(m)) = (
                r.get_ref(0).ok().and_then(sqlite::text),
                r.get_ref(1)
                    .ok()
                    .and_then(sqlite::text)
                    .filter(|m| !m.is_empty()),
            ) {
                window_model.insert(s, m);
            }
        }
    }
    let mut stmt = conn
        .prepare(
            "SELECT session_id, event_json, created_at FROM transcript_events \
             WHERE event_json IS NOT NULL ORDER BY session_id, seq",
        )
        .map_err(sqlite::to_io)?;
    let mut rows = stmt.query([]).map_err(sqlite::to_io)?;
    let keys = entry.key_index();
    let mut p = EventParser {
        entry,
        keys,
        prefix: b"openclaw",
        openclaw: true,
        session: 0,
        model: None,
        default_model: None,
    };
    let mut current: Option<String> = None;
    while let Some(r) = rows.next().map_err(sqlite::to_io)? {
        let Some(sid) = r.get_ref(0).ok().and_then(sqlite::text) else {
            continue;
        };
        if current.as_deref() != Some(sid.as_str()) {
            p.set_session(&sid);
            p.model = None;
            p.default_model = window_model.get(&sid).cloned();
            current = Some(sid);
        }
        let Some(json) = r.get_ref(1).ok().and_then(sqlite::text) else {
            continue;
        };
        let created = r
            .get_ref(2)
            .ok()
            .and_then(sqlite::int)
            .and_then(|n| crate::record::num_to_ms(n as f64));
        if let Ok(ev) = serde_json::from_str::<Event>(&json) {
            p.event(ev, created);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::COST_UNITS_PER_USD;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tools")
    }

    fn entry(tool: Tool) -> FileEntry {
        FileEntry::new(tool, String::new(), 0, 0)
    }

    #[test]
    fn pi_sessions_usage_kinds_and_fork_dedup() {
        let ctx = Ctx::with_vars(&fixtures().join("pi-home"), &[]);
        let mut found = Vec::new();
        let src = discover_pi(&ctx, &mut found);
        assert!(src.errors.is_empty());
        found.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(found.len(), 2);

        let mut parent = entry(Tool::Pi);
        parse_jsonl(&found[0].path, Tool::Pi, &mut parent).unwrap();
        let s = &parent.sessions[parent.sessions.len() - 1];
        assert_eq!(s.id, "s1");
        assert_eq!(s.project.as_deref(), Some("/tmp/demo"));
        assert_eq!(s.title.as_deref(), Some("你好"));
        // 助手消息 2 条 + usage 事件 1 条 + 压缩摘要 1 条
        assert_eq!(parent.recs.len(), 4);
        let r = &parent.recs[0];
        assert_eq!((r.input, r.output, r.cache_read), (100, 50, 300));
        assert_eq!(
            (r.cache_write_5m, r.cache_write_1h, r.reasoning),
            (15, 5, 10)
        );
        assert_eq!(r.known_cost as f64 / COST_UNITS_PER_USD, 0.001215);
        assert_eq!(
            parent.models[parent.recs[0].model as usize],
            "claude-sonnet-4-5"
        );
        // 第二条助手消息没有 cost：按价格表
        assert_eq!(parent.recs[1].known_cost, 0);
        // 压缩摘要没有模型字段：沿用最近的模型
        assert_eq!(parent.models[parent.recs[3].model as usize], "gpt-5.1");

        // fork 复制了父会话的前两条：去重键相同
        let mut fork = entry(Tool::Pi);
        parse_jsonl(&found[1].path, Tool::Pi, &mut fork).unwrap();
        let keys: Vec<u64> = parent.recs.iter().map(|r| r.key).collect();
        assert_eq!(fork.recs.len(), 3);
        assert_eq!(
            fork.recs.iter().filter(|r| keys.contains(&r.key)).count(),
            2
        );
    }

    #[test]
    fn openclaw_sqlite_and_jsonl_share_keys() {
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join(".openclaw/agents/main");
        std::fs::create_dir_all(agent.join("agent")).unwrap();
        std::fs::create_dir_all(agent.join("sessions")).unwrap();
        let header = r#"{"type":"session","version":4,"id":"sess-a","timestamp":"2025-08-30T10:00:00.000Z","cwd":"/w"}"#;
        let model = r#"{"type":"model_change","id":"m0","parentId":null,"timestamp":"2025-08-30T10:00:00.100Z","provider":"anthropic","modelId":"claude-sonnet-4-5"}"#;
        let msg = r#"{"type":"message","id":"e1a2b3c4","parentId":"m0","timestamp":"2025-08-30T10:00:01.000Z","message":{"role":"assistant","content":[],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{"input":120,"output":40,"cacheRead":800,"cacheWrite":0,"totalTokens":960,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0.0012}},"stopReason":"stop","timestamp":1756548001000}}"#;
        let mirror = r#"{"type":"message","id":"x9","parentId":"e1a2b3c4","timestamp":"2025-08-30T10:00:02.000Z","message":{"role":"assistant","content":[],"api":"openclaw-transcript","provider":"openclaw","model":"delivery-mirror","usage":{"input":5,"output":5}}}"#;
        // 没有模型字段的助手消息：用会话窗口里的模型
        let nomodel = r#"{"type":"message","id":"n1","timestamp":"2025-08-30T11:00:00.000Z","message":{"role":"assistant","content":[],"usage":{"input":1,"output":2,"reasoningTokens":1}}}"#;

        let db = agent.join("agent/openclaw-agent.sqlite");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE session_windows (session_id TEXT NOT NULL PRIMARY KEY, session_key TEXT NOT NULL,
                previous_session_id TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
                model_provider TEXT, model TEXT, agent_harness_id TEXT);
             CREATE TABLE transcript_events (session_id TEXT NOT NULL, seq INTEGER NOT NULL, event_json TEXT,
                created_at INTEGER NOT NULL, event_zstd BLOB, event_utf8_bytes INTEGER, navigation_json TEXT,
                PRIMARY KEY (session_id, seq));
             INSERT INTO session_windows VALUES ('sess-b','k',NULL,1,1,'openai','gpt-5.1',NULL);",
        )
        .unwrap();
        let ins = |sid: &str, seq: i64, json: Option<&str>| {
            conn.execute(
                "INSERT INTO transcript_events (session_id, seq, event_json, created_at, event_zstd) VALUES (?1, ?2, ?3, 1756548000000, ?4)",
                rusqlite::params![sid, seq, json, json.is_none().then_some(vec![1u8, 2, 3])],
            )
            .unwrap();
        };
        ins("sess-a", 0, Some(header));
        ins("sess-a", 1, Some(model));
        ins("sess-a", 2, Some(msg));
        ins("sess-a", 3, Some(mirror));
        ins("sess-a", 4, None);
        ins("sess-b", 0, Some(nomodel));
        drop(conn);
        std::fs::write(
            agent.join("sessions/sess-a.jsonl"),
            [header, model, msg].join("\n") + "\n",
        )
        .unwrap();
        std::fs::write(agent.join("sessions/sess-a.jsonl.bak"), "x").unwrap();

        let ctx = Ctx::with_vars(dir.path(), &[]);
        let mut found = Vec::new();
        let src = discover_openclaw(&ctx, &mut found);
        assert!(src.errors.is_empty(), "{:?}", src.errors);
        assert_eq!(found.len(), 2);

        let mut a = entry(Tool::Openclaw);
        parse_openclaw_db(&db, &mut a).unwrap();
        assert_eq!(a.recs.len(), 2, "转录副本和压缩行跳过");
        assert_eq!(
            a.sessions[a.recs[0].session as usize].project.as_deref(),
            Some("/w")
        );
        assert_eq!(a.recs[0].known_cost as f64 / COST_UNITS_PER_USD, 0.0012);
        assert_eq!(a.models[a.recs[1].model as usize], "gpt-5.1");
        assert_eq!(a.recs[1].reasoning, 1);

        let mut b = entry(Tool::Openclaw);
        parse_jsonl(&agent.join("sessions/sess-a.jsonl"), Tool::Openclaw, &mut b).unwrap();
        assert_eq!(b.recs.len(), 1);
        assert_eq!(b.recs[0].key, a.recs[0].key);
    }

    #[test]
    fn missing_dirs_are_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Ctx::with_vars(dir.path(), &[]);
        let mut found = Vec::new();
        assert!(discover_pi(&ctx, &mut found).errors.is_empty());
        let src = discover_openclaw(&ctx, &mut found);
        assert!(src.errors.is_empty() && found.is_empty());
        assert_eq!(src.root, dir.path().join(".openclaw"));
        let ctx = Ctx::with_vars(dir.path(), &[("PI_CODING_AGENT_DIR", "/x/agent")]);
        assert_eq!(pi_root(&ctx), PathBuf::from("/x/agent/sessions"));
        assert!(openclaw_jsonl_name("a.jsonl.reset.2026"));
        assert!(!openclaw_jsonl_name("a.jsonl.pre-doctor-1.bak"));
        assert!(!openclaw_jsonl_name("a.checkpoint.123.jsonl"));
    }
}

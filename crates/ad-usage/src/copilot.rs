//! GitHub Copilot CLI：`$COPILOT_HOME`（默认 `~/.copilot`）
//! - 新版：`session-store.db` 的 `assistant_usage_events` 表，每次模型调用一行（优先用它）
//! - 旧版：`session-state/<id>/events.jsonl`（更早是 `session-state/<id>.jsonl`）。逐次调用的
//!   `assistant.usage` 事件不落盘，落盘的只有 `session.shutdown` 里按模型汇总的 `modelMetrics`。
//!   恢复会话后再退出会再写一份：如果各项都不小于上一份，就是累计值，取差；有任何一项变小，
//!   说明计数从零重新开始，整份计入。
//!
//! 两处的 `input` 都含缓存读和缓存写，推理是输出的一部分。费用 `totalNanoAiu` 是十亿分之一个
//! AI 点数，1 点 = 0.01 美元。模型名里 Claude 的版本号用点（`claude-sonnet-4.5`），换成价格表的连字符写法。

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::discover::{self, Ctx, Found, Kind, Source};
use crate::record::{clean_title, parse_time_str, FileEntry, Rec};
use crate::scan::{contains, scan_lines, LineSink};
use crate::sqlite;
use crate::Tool;

pub(crate) fn root(ctx: &Ctx) -> PathBuf {
    ctx.path_var("COPILOT_HOME")
        .unwrap_or_else(|| ctx.home.join(".copilot"))
}

fn store_path(root: &Path) -> PathBuf {
    root.join("session-store.db")
}

fn wal_of(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push("-wal");
    PathBuf::from(s)
}

pub(crate) fn discover(ctx: &Ctx, out: &mut Vec<Found>) -> Source {
    let root = root(ctx);
    let mut src = Source::new(Tool::Copilot, root.clone());
    let store = store_path(&root);
    discover::push_with(
        Tool::Copilot,
        Kind::CopilotDb,
        store.clone(),
        None,
        discover::stat_db,
        out,
        &mut src.errors,
    );
    // 事件文件要看库里有没有这个会话的逐次记录，所以库变了也要重读
    let extra = vec![store.clone(), wal_of(&store)];
    let state = root.join("session-state");
    let mut paths = Vec::new();
    for dir in discover::subdirs(&state, &mut src.errors) {
        let p = dir.join("events.jsonl");
        if p.is_file() {
            paths.push(p);
        }
    }
    if let Ok(rd) = std::fs::read_dir(&state) {
        for ent in rd.flatten() {
            let p = ent.path();
            if p.extension().is_some_and(|e| e == "jsonl") && p.is_file() {
                paths.push(p);
            }
        }
    }
    paths.sort();
    for p in paths {
        discover::push_with(
            Tool::Copilot,
            Kind::CopilotEvents,
            p,
            None,
            |p| discover::stat_multi(p, &extra),
            out,
            &mut src.errors,
        );
    }
    src
}

/// `claude-sonnet-4.5` → `claude-sonnet-4-5`；其他模型不动
pub(crate) fn normalize_model(m: &str) -> String {
    let m = m.trim();
    if m.starts_with("claude-") {
        m.replace('.', "-")
    } else {
        m.to_string()
    }
}

/// nano AIU → `Rec::known_cost`（1e-10 美元）：1e9 nano = 1 点 = 0.01 美元
fn nano_aiu_cost(n: i64) -> u64 {
    if n > 0 {
        (n as u64) / 10
    } else {
        0
    }
}

// ---------- session-store.db ----------

pub(crate) fn parse_store(path: &Path, entry: &mut FileEntry) -> io::Result<()> {
    let conn = sqlite::open(path)?;
    let cols = sqlite::columns(&conn, "assistant_usage_events")?;
    if cols.is_empty() {
        return Ok(());
    }
    // 会话 id → (工作目录, 摘要, 创建时间)
    type Meta = (Option<String>, Option<String>, Option<String>);
    let mut meta: HashMap<String, Meta> = HashMap::new();
    let scols = sqlite::columns(&conn, "sessions")?;
    if scols.contains("id") {
        let pick = |c: &str| {
            if scols.contains(c) {
                c.to_string()
            } else {
                "NULL".to_string()
            }
        };
        let sql = format!(
            "SELECT id, {}, {}, {} FROM sessions",
            pick("cwd"),
            pick("summary"),
            pick("created_at")
        );
        let mut stmt = conn.prepare(&sql).map_err(sqlite::to_io)?;
        let mut rows = stmt.query([]).map_err(sqlite::to_io)?;
        while let Some(r) = rows.next().map_err(sqlite::to_io)? {
            let Some(id) = r.get_ref(0).ok().and_then(sqlite::text) else {
                continue;
            };
            let get = |i: usize| r.get_ref(i).ok().and_then(sqlite::text);
            meta.insert(id, (get(1), get(2), get(3)));
        }
    }
    let pick = |c: &str| {
        if cols.contains(c) {
            c.to_string()
        } else {
            "NULL".to_string()
        }
    };
    let sql = format!(
        "SELECT session_id, {}, {}, {}, {}, {}, {}, {}, {}, {} FROM assistant_usage_events ORDER BY {}",
        pick("copilot_usage_model"),
        pick("model"),
        pick("input_tokens"),
        pick("output_tokens"),
        pick("cache_read_tokens"),
        pick("cache_write_tokens"),
        pick("reasoning_tokens"),
        pick("total_nano_aiu"),
        pick("created_at"),
        if cols.contains("id") { "id" } else { "rowid" },
    );
    let mut stmt = conn.prepare(&sql).map_err(sqlite::to_io)?;
    let mut rows = stmt.query([]).map_err(sqlite::to_io)?;
    while let Some(r) = rows.next().map_err(sqlite::to_io)? {
        let text = |i: usize| r.get_ref(i).ok().and_then(sqlite::text);
        let num = |i: usize| r.get_ref(i).ok().and_then(sqlite::int).unwrap_or(0).max(0) as u64;
        let sid = text(0).unwrap_or_default();
        let m = meta.get(&sid);
        let Some(ts) = text(9)
            .and_then(|s| parse_time_str(&s))
            .or_else(|| m.and_then(|m| m.2.as_deref()).and_then(parse_time_str))
        else {
            continue;
        };
        let model = text(1)
            .filter(|s| !s.is_empty())
            .or_else(|| text(2).filter(|s| !s.is_empty()))
            .map(|s| normalize_model(&s))
            .unwrap_or_else(|| "auto".to_string());
        let (input, output, cr, cw, reasoning) = (num(3), num(4), num(5), num(6), num(7));
        let session = entry.session_idx(&sid);
        if let Some((cwd, summary, _)) = m {
            let s = &mut entry.sessions[session as usize];
            if s.project.is_none() {
                s.project = cwd.clone().filter(|c| !c.is_empty());
            }
            if let Some(t) = summary.as_deref().and_then(clean_title) {
                s.set_title(t, false, 0);
            }
        }
        let rec = Rec {
            ts,
            key: 0,
            model: entry.model_idx(&model),
            session,
            input: input.saturating_sub(cr).saturating_sub(cw),
            output,
            cache_read: cr,
            cache_write_5m: cw,
            reasoning: reasoning.min(output),
            known_cost: r
                .get_ref(8)
                .ok()
                .and_then(sqlite::int)
                .map(nano_aiu_cost)
                .unwrap_or(0),
            ..Default::default()
        };
        if !rec.is_empty() {
            entry.recs.push(rec);
        }
    }
    Ok(())
}

/// 库里已经有逐次记录的会话
fn sessions_in_store(store: &Path) -> io::Result<std::collections::HashSet<String>> {
    let mut out = std::collections::HashSet::new();
    if !store.exists() {
        return Ok(out);
    }
    let conn = sqlite::open(store)?;
    if !sqlite::has_table(&conn, "assistant_usage_events")? {
        return Ok(out);
    }
    let mut stmt = conn
        .prepare("SELECT DISTINCT session_id FROM assistant_usage_events")
        .map_err(sqlite::to_io)?;
    let mut rows = stmt.query([]).map_err(sqlite::to_io)?;
    while let Some(r) = rows.next().map_err(sqlite::to_io)? {
        if let Some(s) = r.get_ref(0).ok().and_then(sqlite::text) {
            out.insert(s);
        }
    }
    Ok(out)
}

// ---------- events.jsonl ----------

#[derive(Deserialize)]
struct Event {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<String>,
    timestamp: Option<String>,
    data: Option<EventData>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventData {
    session_id: Option<String>,
    context: Option<Context>,
    content: Option<String>,
    #[serde(default)]
    model_metrics: HashMap<String, ModelMetric>,
}

#[derive(Deserialize)]
struct Context {
    cwd: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelMetric {
    #[serde(default)]
    usage: MetricUsage,
    total_nano_aiu: Option<i64>,
}

#[derive(Deserialize, Default, Clone, Copy, PartialEq)]
#[serde(rename_all = "camelCase")]
struct MetricUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_tokens: u64,
    #[serde(default)]
    cache_write_tokens: u64,
    #[serde(default)]
    reasoning_tokens: u64,
}

impl MetricUsage {
    fn arr(&self) -> [u64; 5] {
        [
            self.input_tokens,
            self.output_tokens,
            self.cache_read_tokens,
            self.cache_write_tokens,
            self.reasoning_tokens,
        ]
    }
}

struct EventsSink<'a> {
    entry: &'a mut FileEntry,
    session: u32,
    skip_usage: bool,
    covered: std::collections::HashSet<String>,
    seen_ids: std::collections::HashSet<String>,
    /// 每个模型上一份快照：token 和费用
    prev: HashMap<String, ([u64; 5], i64)>,
}

impl EventsSink<'_> {
    fn set_session(&mut self, id: &str) {
        self.session = self.entry.session_idx(id);
        self.skip_usage = self.covered.contains(id);
    }

    fn shutdown(&mut self, ts: i64, metrics: HashMap<String, ModelMetric>) {
        let mut models: Vec<_> = metrics.into_iter().collect();
        models.sort_by(|a, b| a.0.cmp(&b.0));
        for (raw, m) in models {
            let cur = m.usage.arr();
            let cost = m.total_nano_aiu.unwrap_or(0).max(0);
            let (delta, dcost) = match self.prev.get(&raw) {
                Some((p, pc)) if cur.iter().zip(p).all(|(a, b)| a >= b) => {
                    let mut d = [0u64; 5];
                    for i in 0..5 {
                        d[i] = cur[i] - p[i];
                    }
                    (d, (cost - pc).max(0))
                }
                _ => (cur, cost),
            };
            self.prev.insert(raw.clone(), (cur, cost));
            if self.skip_usage {
                continue;
            }
            let model = normalize_model(&raw);
            let [input, output, cr, cw, reasoning] = delta;
            let rec = Rec {
                ts,
                key: 0,
                model: self.entry.model_idx(&model),
                session: self.session,
                input: input.saturating_sub(cr).saturating_sub(cw),
                output,
                cache_read: cr,
                cache_write_5m: cw,
                reasoning: reasoning.min(output),
                known_cost: nano_aiu_cost(dcost),
                ..Default::default()
            };
            if !rec.is_empty() {
                self.entry.recs.push(rec);
            }
        }
    }
}

impl LineSink for EventsSink<'_> {
    fn want(&mut self, head: &[u8]) -> bool {
        contains(head, b"\"session.") || contains(head, b"\"user.message\"")
    }

    fn line(&mut self, line: &[u8]) -> bool {
        let Ok(ev) = serde_json::from_slice::<Event>(line) else {
            return false;
        };
        let ts = ev.timestamp.as_deref().and_then(parse_time_str);
        let Some(data) = ev.data else { return true };
        match ev.kind.as_deref() {
            Some("session.start") | Some("session.resume") => {
                if let Some(id) = data.session_id.as_deref().filter(|s| !s.is_empty()) {
                    self.set_session(id);
                }
                if let Some(cwd) = data.context.and_then(|c| c.cwd).filter(|c| !c.is_empty()) {
                    self.entry.sessions[self.session as usize].project = Some(cwd);
                }
            }
            Some("user.message") => {
                if let Some(t) = data.content.as_deref().and_then(clean_title) {
                    self.entry.sessions[self.session as usize].set_title(t, false, ts.unwrap_or(0));
                }
            }
            Some("session.shutdown") => {
                // 同一事件被重复写入时只算一次
                if let Some(id) = ev.id {
                    if !self.seen_ids.insert(id) {
                        return true;
                    }
                }
                if let Some(ts) = ts {
                    self.shutdown(ts, data.model_metrics);
                }
            }
            _ => {}
        }
        true
    }
}

pub(crate) fn parse_events(path: &Path, entry: &mut FileEntry) -> io::Result<()> {
    // `session-state/<id>/events.jsonl` 或旧版 `session-state/<id>.jsonl`
    let fallback = if path.file_name().is_some_and(|n| n == "events.jsonl") {
        path.parent().and_then(|p| p.file_name())
    } else {
        path.file_stem()
    }
    .map(|s| s.to_string_lossy().into_owned())
    .unwrap_or_default();
    let state_dir = if path.file_name().is_some_and(|n| n == "events.jsonl") {
        path.parent().and_then(Path::parent)
    } else {
        path.parent()
    };
    let covered = match state_dir.and_then(Path::parent) {
        Some(root) => sessions_in_store(&store_path(root))?,
        None => Default::default(),
    };
    let mut sink = EventsSink {
        session: 0,
        skip_usage: false,
        covered,
        seen_ids: Default::default(),
        prev: HashMap::new(),
        entry,
    };
    sink.set_session(&fallback);
    scan_lines(path, 0, &mut sink)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::COST_UNITS_PER_USD;

    fn start(sid: &str) -> String {
        format!(
            r#"{{"type":"session.start","id":"e0","parentId":null,"timestamp":"2026-04-15T09:40:00.000Z","data":{{"sessionId":"{sid}","version":1,"producer":"copilot-agent","copilotVersion":"1.0.21","startTime":"2026-04-15T09:40:00.000Z","selectedModel":"claude-sonnet-4.5","context":{{"cwd":"/Users/me/proj","branch":"main"}}}}}}"#
        )
    }

    fn shutdown(
        id: &str,
        ts: &str,
        input: u64,
        output: u64,
        cr: u64,
        cw: u64,
        nano: i64,
    ) -> String {
        format!(
            r#"{{"type":"session.shutdown","id":"{id}","parentId":"x","timestamp":"{ts}","data":{{"shutdownType":"routine","totalPremiumRequests":3,"modelMetrics":{{"claude-sonnet-4.5":{{"requests":{{"count":7,"cost":3}},"usage":{{"inputTokens":{input},"outputTokens":{output},"cacheReadTokens":{cr},"cacheWriteTokens":{cw}}},"totalNanoAiu":{nano}}}}},"currentModel":"claude-sonnet-4.5"}}}}"#
        )
    }

    #[test]
    fn shutdown_snapshots_are_diffed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".copilot");
        let sess = root.join("session-state/0fe0");
        std::fs::create_dir_all(&sess).unwrap();
        // 没有 events.jsonl 的会话目录（本机就是这样）：安静跳过
        std::fs::create_dir_all(root.join("session-state/empty")).unwrap();
        let lines = [
            start("0fe0"),
            r#"{"type":"user.message","id":"u1","timestamp":"2026-04-15T09:40:01.000Z","data":{"content":"解释一下这个仓库"}}"#.to_string(),
            shutdown("s1", "2026-04-15T09:52:27.352Z", 184320, 2210, 150112, 20480, 556570000),
            // 重复写入的同一事件
            shutdown("s1", "2026-04-15T09:52:27.352Z", 184320, 2210, 150112, 20480, 556570000),
            // 恢复后再退出：累计值增长，取差
            shutdown("s2", "2026-04-16T10:00:00.000Z", 200000, 3000, 160000, 20480, 600000000),
            // 计数重新开始：整份计入
            shutdown("s3", "2026-04-17T10:00:00.000Z", 1000, 10, 0, 0, 0),
        ];
        std::fs::write(sess.join("events.jsonl"), lines.join("\n") + "\n").unwrap();

        let mut found = Vec::new();
        let src = discover(&Ctx::with_vars(dir.path(), &[]), &mut found);
        assert!(src.errors.is_empty());
        assert_eq!(found.len(), 1);
        let mut e = FileEntry::new(Tool::Copilot, String::new(), 0, 0);
        parse_events(&found[0].path, &mut e).unwrap();
        assert_eq!(e.sessions[0].id, "0fe0");
        assert_eq!(e.sessions[0].project.as_deref(), Some("/Users/me/proj"));
        assert_eq!(e.sessions[0].title.as_deref(), Some("解释一下这个仓库"));
        assert_eq!(e.models, vec!["claude-sonnet-4-5"]);
        assert_eq!(e.recs.len(), 3);
        let r = &e.recs[0];
        assert_eq!(
            (r.input, r.output, r.cache_read, r.cache_write_5m),
            (13728, 2210, 150112, 20480)
        );
        assert_eq!(r.known_cost as f64 / COST_UNITS_PER_USD, 0.0055657);
        let r = &e.recs[1];
        assert_eq!(
            (r.input, r.output, r.cache_read, r.cache_write_5m),
            (5792, 790, 9888, 0)
        );
        assert_eq!(e.recs[2].input, 1000);
    }

    #[test]
    fn store_rows_take_precedence() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".copilot");
        std::fs::create_dir_all(root.join("session-state/0fe0")).unwrap();
        std::fs::write(
            root.join("session-state/0fe0/events.jsonl"),
            [
                start("0fe0"),
                shutdown("s1", "2026-04-15T09:52:27.352Z", 100, 10, 0, 0, 0),
            ]
            .join("\n")
                + "\n",
        )
        .unwrap();
        let db = store_path(&root);
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, cwd TEXT, repository TEXT, branch TEXT, summary TEXT,
                created_at TEXT DEFAULT (datetime('now')), updated_at TEXT);
             CREATE TABLE assistant_usage_events (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL,
                turn_index INTEGER, model TEXT, copilot_usage_model TEXT, input_tokens INTEGER, output_tokens INTEGER,
                cache_read_tokens INTEGER, cache_write_tokens INTEGER, reasoning_tokens INTEGER,
                total_nano_aiu INTEGER, duration_ms INTEGER, created_at TEXT);
             INSERT INTO sessions VALUES ('0fe0','/Users/me/proj',NULL,'main','解释一下这个仓库...','2026-07-01T12:00:00.000Z',NULL);
             INSERT INTO assistant_usage_events (session_id, turn_index, model, copilot_usage_model, input_tokens,
                output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens, total_nano_aiu, duration_ms, created_at)
                VALUES ('0fe0',0,'auto','claude-sonnet-4.5',21343,100,0,20974,0,556570000,1234,'2026-07-01 12:34:56'),
                       ('0fe0',1,'gpt-5.1',NULL,500,50,400,0,20,NULL,10,NULL);",
        )
        .unwrap();
        drop(conn);

        let mut found = Vec::new();
        discover(&Ctx::with_vars(dir.path(), &[]), &mut found);
        assert_eq!(found.len(), 2);
        let mut s = FileEntry::new(Tool::Copilot, String::new(), 0, 0);
        parse_store(&db, &mut s).unwrap();
        assert_eq!(s.recs.len(), 2);
        let r = &s.recs[0];
        assert_eq!((r.input, r.cache_write_5m, r.output), (369, 20974, 100));
        assert_eq!(s.models, vec!["claude-sonnet-4-5", "gpt-5.1"]);
        assert_eq!(r.ts, parse_time_str("2026-07-01T12:34:56Z").unwrap());
        // 没有 created_at：用会话的创建时间
        assert_eq!(
            s.recs[1].ts,
            parse_time_str("2026-07-01T12:00:00.000Z").unwrap()
        );
        assert_eq!((s.recs[1].input, s.recs[1].reasoning), (100, 20));
        assert_eq!(s.sessions[0].project.as_deref(), Some("/Users/me/proj"));

        // 库里有这个会话：事件文件不再计用量
        let ev = found
            .iter()
            .find(|f| f.kind == Kind::CopilotEvents)
            .unwrap();
        let mut e = FileEntry::new(Tool::Copilot, String::new(), 0, 0);
        parse_events(&ev.path, &mut e).unwrap();
        assert!(e.recs.is_empty());
    }

    #[test]
    fn missing_root_is_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let mut found = Vec::new();
        let src = discover(&Ctx::with_vars(dir.path(), &[]), &mut found);
        assert!(found.is_empty() && src.errors.is_empty());
        assert_eq!(src.root, dir.path().join(".copilot"));
    }
}

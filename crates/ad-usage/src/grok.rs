//! Grok：
//! - Grok Build（xAI 官方，命令 `grok`）：`$GROK_HOME`（默认 `~/.grok`）`/sessions/<URL 编码的 cwd>/<会话 id>/updates.jsonl`
//! - 社区版 grok-cli（superagent-ai）：`~/.grok/grok.db` 的 `usage_events` 表
//!
//! Grok Build 每个回合结束写一行 `turn_completed`，`usage` 是这一回合的合计（含子代理），
//! `modelUsage` 按模型拆开。口径（以 xai-org/grok-build 源码为准）：
//! `inputTokens` 是完整提示，含缓存读和缓存写；`outputTokens` 含推理；
//! `costUsdTicks` 是 1e10 分之一美元，用量不完整或费用不全时会被清掉。

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::discover::{self, Ctx, Found, Kind, Source};
use crate::record::{clean_title, fnv64, parse_time_str, percent_decode, FileEntry, Rec};
use crate::scan::{contains, scan_lines, LineSink};
use crate::sqlite;
use crate::Tool;

pub(crate) fn root(ctx: &Ctx) -> PathBuf {
    ctx.path_var("GROK_HOME")
        .unwrap_or_else(|| ctx.home.join(".grok"))
}

/// grok-cli 固定写在 `~/.grok`，不看 `GROK_HOME`。
fn dev_db(ctx: &Ctx) -> PathBuf {
    ctx.home.join(".grok").join("grok.db")
}

pub(crate) fn discover(ctx: &Ctx, out: &mut Vec<Found>) -> Source {
    let root = root(ctx);
    let mut src = Source::new(Tool::Grok, root.clone());
    for group in discover::subdirs(&root.join("sessions"), &mut src.errors) {
        for sess in discover::subdirs(&group, &mut src.errors) {
            let p = sess.join("updates.jsonl");
            if p.is_file() {
                discover::push_with(
                    Tool::Grok,
                    Kind::GrokUpdates,
                    p,
                    None,
                    discover::stat,
                    out,
                    &mut src.errors,
                );
            }
        }
    }
    discover::push_with(
        Tool::Grok,
        Kind::GrokDevDb,
        dev_db(ctx),
        None,
        discover::stat_db,
        out,
        &mut src.errors,
    );
    src
}

// ---------- Grok Build ----------

#[derive(Deserialize, Default)]
struct Summary {
    info: Option<SummaryInfo>,
    current_model_id: Option<String>,
    session_kind: Option<String>,
    generated_title: Option<String>,
    session_summary: Option<String>,
    git_root_dir: Option<String>,
    updated_at: Option<String>,
    created_at: Option<String>,
}

#[derive(Deserialize)]
struct SummaryInfo {
    id: Option<String>,
    cwd: Option<String>,
}

#[derive(Deserialize)]
struct UpdateLine {
    timestamp: Option<Value>,
    params: Option<Params>,
}

#[derive(Deserialize)]
struct Params {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    update: Option<Update>,
    #[serde(rename = "_meta")]
    meta: Option<Meta>,
}

#[derive(Deserialize)]
struct Update {
    #[serde(rename = "sessionUpdate")]
    session_update: Option<String>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Meta {
    #[serde(rename = "eventId")]
    event_id: Option<String>,
    #[serde(rename = "agentTimestampMs")]
    agent_timestamp_ms: Option<Value>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
struct ModelUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cached_read_tokens: u64,
    #[serde(default)]
    cache_creation_tokens: u64,
    #[serde(default)]
    reasoning_tokens: u64,
    cost_usd_ticks: Option<i64>,
    #[serde(default)]
    cost_is_partial: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Usage {
    #[serde(flatten)]
    totals: ModelUsage,
    #[serde(default)]
    model_usage: HashMap<String, ModelUsage>,
    #[serde(default)]
    usage_is_incomplete: bool,
}

/// 子代理的会话（含它复制来的父会话历史）用量已经并进父会话那一回合的 `usage`，不再单独计。
fn is_subagent(kind: Option<&str>) -> bool {
    kind.is_some_and(|k| k.starts_with("subagent"))
}

/// 会话目录的上一级是按 cwd 分组的目录：名字是 URL 编码的路径；太长时改用短名，原路径写在 `.cwd` 里。
fn project_from_dir(session_dir: &Path) -> Option<String> {
    let group = session_dir.parent()?;
    if let Ok(s) = std::fs::read_to_string(group.join(".cwd")) {
        let s = s.trim();
        if !s.is_empty() {
            return Some(s.to_string());
        }
    }
    let name = group.file_name()?.to_str()?;
    let decoded = percent_decode(name);
    decoded.starts_with('/').then_some(decoded)
}

struct UpdatesSink<'a> {
    entry: &'a mut FileEntry,
    keys: HashMap<u64, usize>,
    session: u32,
    session_id: String,
    default_model: String,
    fallback_ts: Option<i64>,
}

impl UpdatesSink<'_> {
    fn turn(&mut self, line: &[u8]) -> bool {
        let Ok(l) = serde_json::from_slice::<UpdateLine>(line) else {
            return false;
        };
        let Some(p) = l.params else { return false };
        let Some(u) = p.update else { return false };
        if u.session_update.as_deref() != Some("turn_completed") {
            return false;
        }
        let Some(usage) = u.usage else { return true };
        let meta = p.meta;
        let ts = meta
            .as_ref()
            .and_then(|m| m.agent_timestamp_ms.as_ref())
            .and_then(crate::record::parse_time_value)
            .or_else(|| {
                l.timestamp
                    .as_ref()
                    .and_then(crate::record::parse_time_value)
            })
            .or(self.fallback_ts);
        let Some(ts) = ts else { return true };
        let event_id = meta.and_then(|m| m.event_id).filter(|e| !e.is_empty());
        let session_id = p.session_id.unwrap_or_else(|| self.session_id.clone());

        // 费用不可信（不完整或只算了一部分）时不用它，改按价格表
        let trust_cost = !usage.usage_is_incomplete && !usage.totals.cost_is_partial;
        let mut rows: Vec<(String, ModelUsage)> = if usage.model_usage.is_empty() {
            vec![(self.default_model.clone(), usage.totals.clone())]
        } else {
            let mut v: Vec<_> = usage.model_usage.into_iter().collect();
            v.sort_by(|a, b| a.0.cmp(&b.0));
            v
        };
        for (model, mu) in rows.drain(..) {
            let input = mu
                .input_tokens
                .saturating_sub(mu.cached_read_tokens)
                .saturating_sub(mu.cache_creation_tokens);
            let known_cost = match mu.cost_usd_ticks {
                // 1e10 ticks = 1 美元，和 Rec::known_cost 的单位相同
                Some(t) if t > 0 && trust_cost && !mu.cost_is_partial => t as u64,
                _ => 0,
            };
            let key = match &event_id {
                // fork 会把事件原样复制到别的会话目录：同一事件的 eventId 和时间都相同。
                // 老版本重启进程后计数器从头开始，同一文件里 eventId 可能重复，所以带上时间。
                Some(e) => fnv64(&[b"grok", e.as_bytes(), &ts.to_le_bytes(), model.as_bytes()]),
                None => fnv64(&[
                    b"grok",
                    session_id.as_bytes(),
                    &ts.to_le_bytes(),
                    model.as_bytes(),
                    &input.to_le_bytes(),
                    &mu.output_tokens.to_le_bytes(),
                ]),
            };
            let rec = Rec {
                ts,
                key,
                model: self.entry.model_idx(&model),
                session: self.session,
                input,
                output: mu.output_tokens,
                cache_read: mu.cached_read_tokens,
                cache_write_5m: mu.cache_creation_tokens,
                cache_write_1h: 0,
                reasoning: mu.reasoning_tokens,
                flags: 0,
                known_cost,
            };
            if !rec.is_empty() {
                self.entry.push_dedup(&mut self.keys, rec);
            }
        }
        true
    }
}

impl LineSink for UpdatesSink<'_> {
    fn want(&mut self, _head: &[u8]) -> bool {
        // 字段顺序不固定（`agentResult` 可能排在 `sessionUpdate` 前面），整行读出来再用子串预筛
        true
    }

    fn line(&mut self, line: &[u8]) -> bool {
        contains(line, b"\"turn_completed\"") && self.turn(line)
    }
}

pub(crate) fn parse_updates(path: &Path, entry: &mut FileEntry) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let summary: Summary = std::fs::read(dir.join("summary.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let session_id = summary
        .info
        .as_ref()
        .and_then(|i| i.id.clone())
        .filter(|s| !s.is_empty())
        .or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let project = summary
        .info
        .as_ref()
        .and_then(|i| i.cwd.clone())
        .filter(|s| !s.is_empty())
        .or_else(|| summary.git_root_dir.clone().filter(|s| !s.is_empty()))
        .or_else(|| project_from_dir(dir));
    let sidx = entry.session_idx(&session_id);
    {
        let s = &mut entry.sessions[sidx as usize];
        s.project = project;
        if let Some(t) = summary
            .generated_title
            .as_deref()
            .or(summary.session_summary.as_deref())
            .and_then(clean_title)
        {
            s.set_title(t, false, 0);
        }
    }
    if is_subagent(summary.session_kind.as_deref()) {
        return Ok(());
    }
    let fallback_ts = summary
        .updated_at
        .as_deref()
        .or(summary.created_at.as_deref())
        .and_then(parse_time_str);
    let keys = entry.key_index();
    let mut sink = UpdatesSink {
        entry,
        keys,
        session: sidx,
        session_id,
        default_model: summary
            .current_model_id
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| "unknown".to_string()),
        fallback_ts,
    };
    scan_lines(path, 0, &mut sink)?;
    Ok(())
}

// ---------- grok-cli（grok.db）----------

pub(crate) fn parse_dev_db(path: &Path, entry: &mut FileEntry) -> io::Result<()> {
    let conn = sqlite::open(path)?;
    if !sqlite::has_table(&conn, "usage_events")? {
        return Ok(());
    }
    // 会话的标题和工作目录
    let mut meta: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
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
            "SELECT id, {}, {} FROM sessions",
            pick("title"),
            pick("cwd_at_start")
        );
        let mut stmt = conn.prepare(&sql).map_err(sqlite::to_io)?;
        let mut rows = stmt.query([]).map_err(sqlite::to_io)?;
        while let Some(r) = rows.next().map_err(sqlite::to_io)? {
            let Some(id) = r.get_ref(0).ok().and_then(sqlite::text) else {
                continue;
            };
            let title = r.get_ref(1).ok().and_then(sqlite::text);
            let cwd = r.get_ref(2).ok().and_then(sqlite::text);
            meta.insert(id, (title, cwd));
        }
    }

    let mut stmt = conn
        .prepare(
            "SELECT session_id, model, input_tokens, output_tokens, cost_micros, created_at \
             FROM usage_events ORDER BY id",
        )
        .map_err(sqlite::to_io)?;
    let mut rows = stmt.query([]).map_err(sqlite::to_io)?;
    while let Some(r) = rows.next().map_err(sqlite::to_io)? {
        let get = |i: usize| r.get_ref(i).ok();
        let Some(ts) = get(5)
            .and_then(sqlite::text)
            .and_then(|s| parse_time_str(&s))
        else {
            continue;
        };
        let sid = get(0).and_then(sqlite::text).unwrap_or_default();
        let model = get(1)
            .and_then(sqlite::text)
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        let input = get(2).and_then(sqlite::int).unwrap_or(0).max(0) as u64;
        let output = get(3).and_then(sqlite::int).unwrap_or(0).max(0) as u64;
        // cost_micros 是百万分之一美元，换成 1e-10 美元
        let known_cost = get(4).and_then(sqlite::int).unwrap_or(0).max(0) as u64 * 10_000;
        let session = entry.session_idx(&sid);
        if let Some((title, cwd)) = meta.get(&sid) {
            let s = &mut entry.sessions[session as usize];
            if s.project.is_none() {
                s.project = cwd.clone().filter(|c| !c.is_empty());
            }
            if let Some(t) = title.as_deref().and_then(clean_title) {
                s.set_title(t, false, 0);
            }
        }
        let rec = Rec {
            ts,
            key: 0,
            model: entry.model_idx(&model),
            session,
            input,
            output,
            known_cost,
            ..Default::default()
        };
        if !rec.is_empty() {
            entry.recs.push(rec);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::COST_UNITS_PER_USD;

    fn fixture(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/tools")
            .join(rel)
    }

    fn entry_for(path: &Path) -> FileEntry {
        FileEntry::new(Tool::Grok, path.to_string_lossy().into_owned(), 0, 0)
    }

    #[test]
    fn updates_turns_models_costs_and_dedup() {
        let ctx = Ctx::with_vars(&fixture("grok-home"), &[]);
        let mut found = Vec::new();
        let src = discover(&ctx, &mut found);
        assert!(src.errors.is_empty());
        let mut updates: Vec<_> = found
            .iter()
            .filter(|f| f.kind == Kind::GrokUpdates)
            .map(|f| f.path.clone())
            .collect();
        updates.sort();
        assert_eq!(updates.len(), 3);

        // 主会话
        let main = updates
            .iter()
            .find(|p| p.to_string_lossy().contains("sess-main"))
            .unwrap();
        let mut e = entry_for(main);
        parse_updates(main, &mut e).unwrap();
        assert_eq!(e.sessions[0].id, "sess-main");
        assert_eq!(
            e.sessions[0].project.as_deref(),
            Some("/Users/test/grokproj")
        );
        assert_eq!(e.sessions[0].title.as_deref(), Some("修复登录 bug"));
        // 回合 1 按两个模型拆开；回合 2 没有 modelUsage，用 summary 里的模型；
        // 同一行重复写了一次只算一次；进行中的流式事件不算
        assert_eq!(e.recs.len(), 3);
        let by_model = |m: &str| {
            let i = e.models.iter().position(|x| x == m).unwrap() as u32;
            e.recs.iter().filter(move |r| r.model == i)
        };
        let r = by_model("grok-4.5-build").next().unwrap();
        // inputTokens 1000 含缓存读 600、缓存写 100
        assert_eq!((r.input, r.cache_read, r.cache_write_5m), (300, 600, 100));
        assert_eq!((r.output, r.reasoning), (200, 50));
        assert_eq!(r.known_cost as f64 / COST_UNITS_PER_USD, 0.0123);
        assert_eq!(r.ts, 1_790_000_000_000);
        let small = by_model("grok-code-fast-1").next().unwrap();
        assert_eq!(small.input, 100);
        assert_eq!(small.known_cost, 0, "这一行没有 costUsdTicks");
        let r2: Vec<_> = by_model("grok-4.6").collect();
        assert_eq!(r2.len(), 1);
        // usageIsIncomplete：费用被视为不可信
        assert_eq!(r2[0].known_cost, 0);
        assert_eq!(r2[0].input, 500);

        // fork 出来的会话复制了回合 1：去重键相同
        let fork = updates
            .iter()
            .find(|p| p.to_string_lossy().contains("sess-fork"))
            .unwrap();
        let mut f = entry_for(fork);
        parse_updates(fork, &mut f).unwrap();
        assert_eq!(
            f.sessions[0].project.as_deref(),
            Some("/Users/test/grok proj")
        );
        let main_keys: Vec<u64> = e.recs.iter().map(|r| r.key).collect();
        assert_eq!(f.recs.len(), 3);
        assert_eq!(
            f.recs.iter().filter(|r| main_keys.contains(&r.key)).count(),
            2,
            "复制来的回合 1 两个模型都能对上"
        );

        // 子代理会话不计
        let sub = updates
            .iter()
            .find(|p| p.to_string_lossy().contains("sess-sub"))
            .unwrap();
        let mut s = entry_for(sub);
        parse_updates(sub, &mut s).unwrap();
        assert!(s.recs.is_empty());
    }

    #[test]
    fn missing_dirs_are_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Ctx::with_vars(dir.path(), &[]);
        let mut found = Vec::new();
        let src = discover(&ctx, &mut found);
        assert!(found.is_empty() && src.errors.is_empty());
        assert_eq!(src.root, dir.path().join(".grok"));
        // GROK_HOME 覆盖会话目录，grok.db 仍在 ~/.grok
        let other = tempfile::tempdir().unwrap();
        let ctx = Ctx::with_vars(dir.path(), &[("GROK_HOME", other.path().to_str().unwrap())]);
        assert_eq!(root(&ctx), other.path());
        assert_eq!(dev_db(&ctx), dir.path().join(".grok/grok.db"));
    }

    #[test]
    fn grok_dev_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("grok.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE sessions (id TEXT PRIMARY KEY, workspace_id TEXT, title TEXT, model TEXT,
                cwd_at_start TEXT, cwd_last TEXT, created_at TEXT, updated_at TEXT);
             CREATE TABLE usage_events (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL,
                message_seq INTEGER, source TEXT NOT NULL, model TEXT NOT NULL,
                input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL DEFAULT 0, cost_micros INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL);
             INSERT INTO sessions VALUES ('s1','w','写个脚本','grok-4.5','/Users/test/dev','/Users/test/dev',
                '2026-10-01T00:00:00.000Z','2026-10-01T00:00:00.000Z');
             INSERT INTO usage_events (session_id, source, model, input_tokens, output_tokens, total_tokens, cost_micros, created_at)
                VALUES ('s1','message','grok-4.5',1000,100,1100,2500,'2026-10-01T01:00:00.000Z'),
                       ('s1','title','grok-code-fast-1',50,5,55,0,'2026-10-01T01:00:01.000Z'),
                       ('s1','message','grok-4.5',0,0,0,0,'2026-10-01T01:00:02.000Z');",
        )
        .unwrap();
        // 不关连接：数据还在 WAL 里也要读得到
        let (size_before, _) = discover::stat_db(&db).unwrap();
        assert!(size_before > std::fs::metadata(&db).unwrap().len());
        let mut e = entry_for(&db);
        parse_dev_db(&db, &mut e).unwrap();
        assert_eq!(e.recs.len(), 2);
        assert_eq!(e.recs[0].input, 1000);
        assert_eq!(e.recs[0].known_cost as f64 / COST_UNITS_PER_USD, 0.0025);
        assert_eq!(e.recs[1].known_cost, 0);
        assert_eq!(e.sessions[0].project.as_deref(), Some("/Users/test/dev"));
        assert_eq!(e.sessions[0].title.as_deref(), Some("写个脚本"));
        drop(conn);

        // 没有 usage_events 表的库（比如别的程序的 grok.db）：安静地没有记录
        let empty = dir.path().join("empty.db");
        rusqlite::Connection::open(&empty)
            .unwrap()
            .execute_batch("CREATE TABLE x (a INTEGER);")
            .unwrap();
        let mut e = entry_for(&empty);
        parse_dev_db(&empty, &mut e).unwrap();
        assert!(e.recs.is_empty());
    }
}

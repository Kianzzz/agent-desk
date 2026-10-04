//! Goose（block/goose）：`$GOOSE_PATH_ROOT/data/sessions/sessions.db`，未设置时按 XDG
//! 放在 `$XDG_DATA_HOME/goose/sessions/sessions.db`（macOS 也是 `~/.local/share/goose/…`）。
//!
//! 新版有 `usage_ledger` 表，每次模型调用一行（秒级时间、实际模型、token、费用），优先用它。
//! `sessions` 表的 `accumulated_*` 是会话累计值：没有逐次记录的会话（升级前的），或者累计值比
//! 逐次记录之和多出来的部分，整笔记在会话创建那天，模型取 `model_config_json.model_name`。
//! 口径：`input` 含缓存读和缓存写，输出含推理。1.10 之前的 `sessions/*.jsonl` 首次建库时已导入库里。

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::discover::{self, Ctx, Found, Kind, Source};
use crate::record::{clean_title, num_to_ms, parse_time_str, usd_to_cost, FileEntry, Rec};
use crate::sqlite;
use crate::Tool;

pub(crate) fn db_path(ctx: &Ctx) -> PathBuf {
    match ctx.path_var("GOOSE_PATH_ROOT").filter(|p| p.is_absolute()) {
        Some(r) => r.join("data").join("sessions").join("sessions.db"),
        None => ctx
            .xdg_data()
            .join("goose")
            .join("sessions")
            .join("sessions.db"),
    }
}

pub(crate) fn discover(ctx: &Ctx, out: &mut Vec<Found>) -> Source {
    let db = db_path(ctx);
    let mut src = Source::new(
        Tool::Goose,
        db.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| db.clone()),
    );
    discover::push_with(
        Tool::Goose,
        Kind::Goose,
        db,
        None,
        discover::stat_db,
        out,
        &mut src.errors,
    );
    src
}

/// input（含缓存）、output、缓存读、缓存写
type Tok = [u64; 4];

struct Sess {
    model: Option<String>,
    created: Option<i64>,
    acc: Tok,
    acc_cost: f64,
    ledger: Tok,
    ledger_cost: f64,
    has_ledger: bool,
}

fn rec_from(ts: i64, model: u32, session: u32, t: Tok, cost: f64) -> Rec {
    let [input, output, cr, cw] = t;
    Rec {
        ts,
        model,
        session,
        input: input.saturating_sub(cr).saturating_sub(cw),
        output,
        cache_read: cr,
        cache_write_5m: cw,
        known_cost: usd_to_cost(cost),
        ..Default::default()
    }
}

pub(crate) fn parse(path: &Path, entry: &mut FileEntry) -> io::Result<()> {
    let conn = sqlite::open(path)?;
    let cols = sqlite::columns(&conn, "sessions")?;
    if !cols.contains("id") {
        return Ok(());
    }
    let pick = |c: &str| {
        if cols.contains(c) {
            c.to_string()
        } else {
            "NULL".to_string()
        }
    };
    let sql = format!(
        "SELECT id, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {} FROM sessions",
        pick("name"),
        pick("description"),
        pick("working_dir"),
        pick("created_at"),
        pick("model_config_json"),
        pick("accumulated_input_tokens"),
        pick("accumulated_output_tokens"),
        pick("accumulated_cache_read_tokens"),
        pick("accumulated_cache_write_tokens"),
        pick("accumulated_cost"),
        pick("updated_at"),
    );
    let mut sessions: HashMap<String, Sess> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    {
        let mut stmt = conn.prepare(&sql).map_err(sqlite::to_io)?;
        let mut rows = stmt.query([]).map_err(sqlite::to_io)?;
        while let Some(r) = rows.next().map_err(sqlite::to_io)? {
            let Some(id) = r.get_ref(0).ok().and_then(sqlite::text) else {
                continue;
            };
            let text = |i: usize| r.get_ref(i).ok().and_then(sqlite::text);
            let num = |i: usize| r.get_ref(i).ok().and_then(sqlite::int).unwrap_or(0).max(0) as u64;
            let model = text(5).and_then(|j| {
                serde_json::from_str::<serde_json::Value>(&j)
                    .ok()?
                    .get("model_name")?
                    .as_str()
                    .filter(|m| !m.is_empty())
                    .map(str::to_string)
            });
            let created = text(4)
                .as_deref()
                .and_then(parse_time_str)
                .or_else(|| text(11).as_deref().and_then(parse_time_str));
            let sidx = entry.session_idx(&id);
            {
                let s = &mut entry.sessions[sidx as usize];
                s.project = text(3).filter(|d| !d.is_empty());
                if let Some(t) = text(1)
                    .filter(|t| !t.is_empty())
                    .or_else(|| text(2))
                    .as_deref()
                    .and_then(clean_title)
                {
                    s.set_title(t, false, 0);
                }
            }
            sessions.insert(
                id.clone(),
                Sess {
                    model,
                    created,
                    acc: [num(6), num(7), num(8), num(9)],
                    acc_cost: r.get_ref(10).ok().and_then(sqlite::real).unwrap_or(0.0),
                    ledger: [0; 4],
                    ledger_cost: 0.0,
                    has_ledger: false,
                },
            );
            order.push(id);
        }
    }

    // 逐次记录
    let lcols = sqlite::columns(&conn, "usage_ledger")?;
    if lcols.contains("session_id") {
        let pick = |c: &str| {
            if lcols.contains(c) {
                c.to_string()
            } else {
                "NULL".to_string()
            }
        };
        let sql = format!(
            "SELECT session_id, {}, {}, {}, {}, {}, {}, {} FROM usage_ledger ORDER BY {}",
            pick("created_timestamp"),
            pick("model"),
            pick("input_tokens"),
            pick("output_tokens"),
            pick("cache_read_tokens"),
            pick("cache_write_tokens"),
            pick("cost"),
            if lcols.contains("id") { "id" } else { "rowid" },
        );
        let mut stmt = conn.prepare(&sql).map_err(sqlite::to_io)?;
        let mut rows = stmt.query([]).map_err(sqlite::to_io)?;
        while let Some(r) = rows.next().map_err(sqlite::to_io)? {
            let Some(sid) = r.get_ref(0).ok().and_then(sqlite::text) else {
                continue;
            };
            let num = |i: usize| r.get_ref(i).ok().and_then(sqlite::int).unwrap_or(0).max(0) as u64;
            let t: Tok = [num(3), num(4), num(5), num(6)];
            let cost = r.get_ref(7).ok().and_then(sqlite::real).unwrap_or(0.0);
            let sess = sessions.get_mut(&sid);
            let ts = r
                .get_ref(1)
                .ok()
                .and_then(sqlite::int)
                .and_then(|n| num_to_ms(n as f64))
                .or_else(|| sess.as_ref().and_then(|s| s.created));
            let Some(ts) = ts else { continue };
            // 升级时补记的那一行没有模型
            let model = r
                .get_ref(2)
                .ok()
                .and_then(sqlite::text)
                .filter(|m| !m.is_empty())
                .or_else(|| sess.as_ref().and_then(|s| s.model.clone()))
                .unwrap_or_else(|| "unknown".to_string());
            if let Some(s) = sess {
                for (acc, v) in s.ledger.iter_mut().zip(t) {
                    *acc += v;
                }
                s.ledger_cost += cost;
                s.has_ledger = true;
            }
            let session = entry.session_idx(&sid);
            let midx = entry.model_idx(&model);
            let rec = rec_from(ts, midx, session, t, cost);
            if !rec.is_empty() {
                entry.recs.push(rec);
            }
        }
    }

    // 累计值里逐次记录没覆盖到的部分
    for id in order {
        let s = &sessions[&id];
        let Some(ts) = s.created else { continue };
        let rest: Tok = std::array::from_fn(|i| s.acc[i].saturating_sub(s.ledger[i]));
        let cost = if s.has_ledger {
            (s.acc_cost - s.ledger_cost).max(0.0)
        } else {
            s.acc_cost
        };
        if rest == [0; 4] {
            continue;
        }
        let model = s.model.clone().unwrap_or_else(|| "unknown".to_string());
        let session = entry.session_idx(&id);
        let midx = entry.model_idx(&model);
        let rec = rec_from(ts, midx, session, rest, cost);
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

    #[test]
    fn ledger_rows_and_accumulated_rest() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join(".local/share/goose/sessions/sessions.db");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, name TEXT, description TEXT, session_type TEXT,
                working_dir TEXT, created_at TIMESTAMP, updated_at TIMESTAMP,
                total_tokens INTEGER, input_tokens INTEGER, output_tokens INTEGER,
                accumulated_total_tokens INTEGER, accumulated_input_tokens INTEGER, accumulated_output_tokens INTEGER,
                accumulated_cache_read_tokens INTEGER, accumulated_cache_write_tokens INTEGER, accumulated_cost REAL,
                provider_name TEXT, model_config_json TEXT, parent_session_id TEXT);
             CREATE TABLE usage_ledger (id INTEGER PRIMARY KEY, session_id TEXT, created_timestamp INTEGER, model TEXT,
                input_tokens INTEGER, output_tokens INTEGER, total_tokens INTEGER, cache_read_tokens INTEGER,
                cache_write_tokens INTEGER, cost REAL, cost_source TEXT, is_compaction INTEGER);
             -- 有逐次记录的新会话
             INSERT INTO sessions VALUES ('20261004_3','重构解析器','','user','/Users/alice/repo','2026-10-04 08:15:02','2026-10-04 08:41:10',
                0,0,0, 41250,40800,450,38000,2100,0.0291, 'anthropic','{\"model_name\":\"claude-sonnet-4-5\",\"temperature\":null}',NULL);
             INSERT INTO usage_ledger VALUES (1,'20261004_3',1759565470,'claude-sonnet-4-5-20250929',40800,450,41250,38000,2100,0.0291,'estimated',0);
             -- 升级前的老会话：只有累计值
             INSERT INTO sessions VALUES ('20260901_1','','旧会话说明','user','/Users/alice/old','2026-09-01T10:00:00.123+00:00','2026-09-01T11:00:00Z',
                0,0,0, 1100,1000,100,600,0,NULL, 'openai','{\"model_name\":\"gpt-5.1\"}',NULL);",
        )
        .unwrap();
        drop(conn);
        let mut found = Vec::new();
        let src = discover(&Ctx::with_vars(dir.path(), &[]), &mut found);
        assert!(src.errors.is_empty());
        assert_eq!(found.len(), 1);
        let mut e = FileEntry::new(Tool::Goose, String::new(), 0, 0);
        parse(&found[0].path, &mut e).unwrap();
        assert_eq!(e.recs.len(), 2);
        let r = &e.recs[0];
        assert_eq!(
            (r.input, r.output, r.cache_read, r.cache_write_5m),
            (700, 450, 38000, 2100)
        );
        assert_eq!(r.ts, 1_759_565_470_000);
        assert_eq!(r.known_cost as f64 / COST_UNITS_PER_USD, 0.0291);
        assert_eq!(e.models[r.model as usize], "claude-sonnet-4-5-20250929");
        let old = &e.recs[1];
        assert_eq!((old.input, old.output, old.cache_read), (400, 100, 600));
        assert_eq!(e.models[old.model as usize], "gpt-5.1");
        assert_eq!(old.ts, parse_time_str("2026-09-01T10:00:00.123Z").unwrap());
        assert_eq!(old.known_cost, 0);
        let s = e.find_session("20261004_3").unwrap();
        assert_eq!(s.project.as_deref(), Some("/Users/alice/repo"));
        assert_eq!(s.title.as_deref(), Some("重构解析器"));
        assert_eq!(
            e.find_session("20260901_1").unwrap().title.as_deref(),
            Some("旧会话说明")
        );
    }

    #[test]
    fn paths_and_missing_db() {
        let dir = tempfile::tempdir().unwrap();
        let mut found = Vec::new();
        let src = discover(&Ctx::with_vars(dir.path(), &[]), &mut found);
        assert!(found.is_empty() && src.errors.is_empty());
        let ctx = Ctx::with_vars(dir.path(), &[("GOOSE_PATH_ROOT", "/g")]);
        assert_eq!(db_path(&ctx), PathBuf::from("/g/data/sessions/sessions.db"));
        // 相对路径不认
        let ctx = Ctx::with_vars(dir.path(), &[("GOOSE_PATH_ROOT", "g")]);
        assert_eq!(
            db_path(&ctx),
            dir.path().join(".local/share/goose/sessions/sessions.db")
        );
    }
}

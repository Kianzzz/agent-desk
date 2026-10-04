//! Crush（charmbracelet）：项目登记在 `$CRUSH_GLOBAL_DATA`（默认 `$XDG_DATA_HOME/crush`，
//! 即 `~/.local/share/crush`）`/projects.json`，每个项目的库在 `<data_dir>/crush.db`
//! （`data_dir` 默认 `<项目>/.crush`）。
//!
//! 能用的只有 `sessions.cost`：会话累计的美元费用，子会话（任务）和起标题的费用都加到根会话上。
//! `prompt_tokens` / `completion_tokens` 每一步都被覆盖成最近一次调用的上下文和输出，不是累计值，
//! 不采用。所以只记费用：把根会话的费用平均分给整棵会话树里的助手消息（带上各自的模型和时间），
//! token 记为 0。没有助手消息时整笔记在会话最后更新的时间。

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::discover::{self, Ctx, Found, Kind, Source};
use crate::record::{clean_title, num_to_ms, usd_to_cost, FileEntry, Rec};
use crate::sqlite;
use crate::Tool;

pub(crate) fn global_dir(ctx: &Ctx) -> PathBuf {
    ctx.path_var("CRUSH_GLOBAL_DATA")
        .unwrap_or_else(|| ctx.xdg_data().join("crush"))
}

#[derive(Deserialize)]
struct Projects {
    #[serde(default)]
    projects: Vec<Project>,
}

#[derive(Deserialize)]
struct Project {
    path: Option<String>,
    data_dir: Option<String>,
}

pub(crate) fn discover(ctx: &Ctx, out: &mut Vec<Found>) -> Source {
    let root = global_dir(ctx);
    let mut src = Source::new(Tool::Crush, root.clone());
    let projects: Vec<Project> = std::fs::read(root.join("projects.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Projects>(&b).ok())
        .map(|p| p.projects)
        .unwrap_or_default();
    let mut seen = Vec::new();
    for p in projects {
        let Some(path) = p.path.filter(|s| !s.is_empty()) else {
            continue;
        };
        let base = PathBuf::from(&path);
        let data = match p.data_dir.filter(|s| !s.is_empty()) {
            Some(d) if Path::new(&d).is_absolute() => PathBuf::from(d),
            Some(d) => base.join(d),
            None => base.join(".crush"),
        };
        let db = data.join("crush.db");
        if seen.contains(&db) {
            continue;
        }
        seen.push(db.clone());
        discover::push_with(
            Tool::Crush,
            Kind::Crush,
            db,
            Some(path),
            discover::stat_db,
            out,
            &mut src.errors,
        );
    }
    src
}

/// 按条数平均分，结果之和等于总数
fn split_even(total: u64, n: usize) -> Vec<u64> {
    if n == 0 {
        return Vec::new();
    }
    let n64 = n as u64;
    (0..n64)
        .map(|i| total * (i + 1) / n64 - total * i / n64)
        .collect()
}

pub(crate) fn parse(path: &Path, project: Option<String>, entry: &mut FileEntry) -> io::Result<()> {
    let conn = sqlite::open(path)?;
    let cols = sqlite::columns(&conn, "sessions")?;
    if !cols.contains("id") || !cols.contains("cost") {
        return Ok(());
    }
    let pick = |c: &str| {
        if cols.contains(c) {
            c.to_string()
        } else {
            "NULL".to_string()
        }
    };
    // 会话树：子会话 → 根会话
    let mut parent: HashMap<String, String> = HashMap::new();
    struct Root {
        title: Option<String>,
        cost: f64,
        ts: Option<i64>,
    }
    let mut roots: Vec<(String, Root)> = Vec::new();
    let sql = format!(
        "SELECT id, {}, {}, cost, {}, {} FROM sessions",
        pick("parent_session_id"),
        pick("title"),
        pick("updated_at"),
        pick("created_at")
    );
    let mut stmt = conn.prepare(&sql).map_err(sqlite::to_io)?;
    let mut rows = stmt.query([]).map_err(sqlite::to_io)?;
    while let Some(r) = rows.next().map_err(sqlite::to_io)? {
        let Some(id) = r.get_ref(0).ok().and_then(sqlite::text) else {
            continue;
        };
        match r
            .get_ref(1)
            .ok()
            .and_then(sqlite::text)
            .filter(|p| !p.is_empty())
        {
            Some(p) => {
                parent.insert(id, p);
            }
            None => {
                let ts = [4, 5]
                    .iter()
                    .find_map(|&i| r.get_ref(i).ok().and_then(sqlite::int))
                    .and_then(|n| num_to_ms(n as f64));
                roots.push((
                    id,
                    Root {
                        title: r.get_ref(2).ok().and_then(sqlite::text),
                        cost: r.get_ref(3).ok().and_then(sqlite::real).unwrap_or(0.0),
                        ts,
                    },
                ));
            }
        }
    }
    drop(rows);
    drop(stmt);
    let root_of = |mut id: String| -> String {
        for _ in 0..64 {
            match parent.get(&id) {
                Some(p) => id = p.clone(),
                None => break,
            }
        }
        id
    };

    // 助手消息：(时间, 模型)，按根会话归组
    let mut msgs: HashMap<String, Vec<(i64, String)>> = HashMap::new();
    let mcols = sqlite::columns(&conn, "messages")?;
    if mcols.contains("session_id") && mcols.contains("role") {
        let sql =
            format!(
            "SELECT session_id, {}, {} FROM messages WHERE role = 'assistant' ORDER BY created_at",
            if mcols.contains("model") { "model" } else { "NULL" },
            if mcols.contains("created_at") { "created_at" } else { "NULL" },
        );
        let mut stmt = conn.prepare(&sql).map_err(sqlite::to_io)?;
        let mut rows = stmt.query([]).map_err(sqlite::to_io)?;
        while let Some(r) = rows.next().map_err(sqlite::to_io)? {
            let Some(sid) = r.get_ref(0).ok().and_then(sqlite::text) else {
                continue;
            };
            let Some(ts) = r
                .get_ref(2)
                .ok()
                .and_then(sqlite::int)
                .and_then(|n| num_to_ms(n as f64))
            else {
                continue;
            };
            let model = r
                .get_ref(1)
                .ok()
                .and_then(sqlite::text)
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| "unknown".to_string());
            msgs.entry(root_of(sid)).or_default().push((ts, model));
        }
    }

    for (id, root) in roots {
        let total = usd_to_cost(root.cost);
        if total == 0 {
            continue;
        }
        let session = entry.session_idx(&id);
        {
            let s = &mut entry.sessions[session as usize];
            s.project = project.clone();
            if let Some(t) = root.title.as_deref().and_then(clean_title) {
                s.set_title(t, false, 0);
            }
        }
        let list = match msgs.remove(&id) {
            Some(v) if !v.is_empty() => v,
            _ => match root.ts {
                Some(ts) => vec![(ts, "unknown".to_string())],
                None => continue,
            },
        };
        let parts = split_even(total, list.len());
        for ((ts, model), cost) in list.into_iter().zip(parts) {
            if cost == 0 {
                continue;
            }
            let model = entry.model_idx(&model);
            entry.recs.push(Rec {
                ts,
                model,
                session,
                known_cost: cost,
                ..Default::default()
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::COST_UNITS_PER_USD;

    #[test]
    fn root_cost_split_over_tree_messages() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("repo");
        std::fs::create_dir_all(proj.join(".crush")).unwrap();
        let global = dir.path().join(".local/share/crush");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::write(
            global.join("projects.json"),
            format!(
                r#"{{"projects":[{{"path":"{}","data_dir":".crush","last_accessed":"2026-10-01T09:12:44Z"}},{{"path":"/nope","data_dir":"/nope/.crush"}}]}}"#,
                proj.display()
            ),
        )
        .unwrap();
        let conn = rusqlite::Connection::open(proj.join(".crush/crush.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, parent_session_id TEXT, title TEXT, message_count INTEGER,
                prompt_tokens INTEGER, completion_tokens INTEGER, cost REAL, updated_at INTEGER, created_at INTEGER,
                summary_message_id TEXT, todos TEXT, channel TEXT);
             CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT, role TEXT, parts TEXT, model TEXT,
                created_at INTEGER, updated_at INTEGER, finished_at INTEGER, provider TEXT, is_summary_message INTEGER);
             INSERT INTO sessions VALUES ('root','',  '修复不稳定的测试',14,48211,812,0.3,1759309964,1759309001,NULL,NULL,NULL);
             INSERT INTO sessions VALUES ('child','root','任务',3,100,10,0.05,1759309500,1759309400,NULL,NULL,NULL);
             INSERT INTO sessions VALUES ('lone',NULL,'没有消息',0,0,0,0.01,1759400000,1759390000,NULL,NULL,NULL);
             INSERT INTO sessions VALUES ('free',NULL,'本地模型',2,10,1,0,1759400000,1759390000,NULL,NULL,NULL);
             INSERT INTO messages VALUES ('m1','root','assistant','[]','claude-sonnet-4-5-20250929',1759309040,0,0,'anthropic',0);
             INSERT INTO messages VALUES ('m2','root','user','[]',NULL,1759309041,0,0,NULL,0);
             INSERT INTO messages VALUES ('m3','child','assistant','[]','gpt-5.1',1759309450,0,0,'openai',0);
             INSERT INTO messages VALUES ('m4','root','assistant','[]','claude-sonnet-4-5-20250929',1759395000,0,0,'anthropic',0);",
        )
        .unwrap();
        drop(conn);
        let mut found = Vec::new();
        let src = discover(&Ctx::with_vars(dir.path(), &[]), &mut found);
        assert!(src.errors.is_empty());
        assert_eq!(found.len(), 1);
        let mut e = FileEntry::new(Tool::Crush, String::new(), 0, 0);
        parse(&found[0].path, found[0].project.clone(), &mut e).unwrap();
        // 根会话 0.3 美元分给树里 3 条助手消息；子会话自己的 0.05 已含在根里，不再加；
        // 没有消息的会话整笔记一条；费用为 0 的不记
        assert_eq!(e.recs.len(), 4);
        let total: u64 = e.recs.iter().map(|r| r.known_cost).sum();
        assert!((total as f64 / COST_UNITS_PER_USD - 0.31).abs() < 1e-9);
        assert!(e.recs.iter().all(|r| r.input == 0 && r.output == 0));
        assert!(e.models.contains(&"gpt-5.1".to_string()));
        let root = e.find_session("root").unwrap();
        assert_eq!(root.project.as_deref(), Some(proj.to_str().unwrap()));
        assert_eq!(root.title.as_deref(), Some("修复不稳定的测试"));
        assert_eq!(e.recs[0].ts, 1_759_309_040_000);
        assert_eq!(split_even(10, 3), vec![3, 3, 4]);
    }

    #[test]
    fn missing_registry_is_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let mut found = Vec::new();
        let src = discover(&Ctx::with_vars(dir.path(), &[]), &mut found);
        assert!(found.is_empty() && src.errors.is_empty());
        assert_eq!(src.root, dir.path().join(".local/share/crush"));
    }
}

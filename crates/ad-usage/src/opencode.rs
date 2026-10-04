//! OpenCode 和 Kilo CLI（OpenCode 的分支，存储格式相同）：
//! - OpenCode：`$XDG_DATA_HOME`（默认 `~/.local/share`）`/opencode/opencode.db`（以及按发布通道命名的
//!   `opencode-<通道>.db`，`$OPENCODE_DB` 可指定），旧版 `storage/message/<会话>/<消息>.json`
//! - Kilo CLI：同样的结构在 `…/kilo/` 下，库名 `kilo.db` / `kilo-*.db`（`$KILO_DB`）
//!
//! 库里 v1 的 `message` 表和 v2 的 `session_message` 表可能同时有同一条消息（迁移保留 id），
//! 旧 JSON 在迁移到 SQLite 后也没删；v1 的 fork 会用新 id 原样复制消息。所以去重键用消息内容的
//! 指纹（创建时间、模型、各项 token），不用 id。
//! 口径：`tokens.input` 不含缓存；从 v1.3.16（2026-04-06）起 `output` 不含推理、推理另记在
//! `reasoning`，之前的 `output` 已含推理。`cost` 是美元，为 0 时按价格表算。

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::discover::{self, Ctx, Found, Kind, Source};
use crate::record::{clean_title, fnv64, usd_to_cost, FileEntry, Rec};
use crate::sqlite;
use crate::Tool;

/// 2026-04-06T00:00:00Z：此前写入的 `output` 已含推理
const REASONING_SPLIT_MS: i64 = 1_775_433_600_000;

struct Flavor {
    dir: &'static str,
    db_env: &'static str,
    /// 库文件名前缀：`<前缀>.db`、`<前缀>-*.db`
    prefixes: &'static [&'static str],
    key: &'static [u8],
}

const OPENCODE: Flavor = Flavor {
    dir: "opencode",
    db_env: "OPENCODE_DB",
    prefixes: &["opencode"],
    key: b"opencode",
};

/// Kilo 在通道库不存在时会沿用同目录下的 `opencode-<通道>.db`
const KILO: Flavor = Flavor {
    dir: "kilo",
    db_env: "KILO_DB",
    prefixes: &["kilo", "opencode"],
    key: b"kilo",
};

fn flavor(tool: Tool) -> &'static Flavor {
    if tool == Tool::Kilo {
        &KILO
    } else {
        &OPENCODE
    }
}

pub(crate) fn data_dir(ctx: &Ctx, tool: Tool) -> PathBuf {
    ctx.xdg_data().join(flavor(tool).dir)
}

fn is_db_name(name: &str, prefixes: &[&str]) -> bool {
    let Some(stem) = name.strip_suffix(".db") else {
        return false;
    };
    prefixes
        .iter()
        .any(|p| stem == *p || stem.strip_prefix(p).is_some_and(|r| r.starts_with('-')))
}

/// OpenCode / Kilo CLI 的库和旧 JSON；Kilo 的扩展部分由 `cline` 模块另外登记。
pub(crate) fn discover_into(ctx: &Ctx, tool: Tool, out: &mut Vec<Found>, src: &mut Source) {
    let fl = flavor(tool);
    let dir = data_dir(ctx, tool);
    let mut dbs = Vec::new();
    if let Some(v) = ctx.var(fl.db_env).filter(|v| *v != ":memory:") {
        let p = PathBuf::from(v);
        dbs.push(if p.is_absolute() { p } else { dir.join(p) });
    }
    if let Ok(rd) = std::fs::read_dir(&dir) {
        let mut names: Vec<PathBuf> = rd
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| is_db_name(n, fl.prefixes))
            })
            .map(|e| e.path())
            .collect();
        names.sort();
        for p in names {
            if !dbs.contains(&p) {
                dbs.push(p);
            }
        }
    }
    for p in dbs {
        discover::push_with(
            tool,
            Kind::OpencodeDb,
            p,
            None,
            discover::stat_db,
            out,
            &mut src.errors,
        );
    }
    // 旧版：每个会话一个目录，目录里每条消息一个 JSON；整个目录当作一个数据源
    let msg_root = dir.join("storage").join("message");
    for sess in discover::subdirs(&msg_root, &mut src.errors) {
        let files: Vec<PathBuf> = std::fs::read_dir(&sess)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "json"))
                    .collect()
            })
            .unwrap_or_default();
        if files.is_empty() {
            continue;
        }
        discover::push_with(
            tool,
            Kind::OpencodeJson,
            sess,
            None,
            |p| discover::stat_multi(p, &files),
            out,
            &mut src.errors,
        );
    }
}

pub(crate) fn discover(ctx: &Ctx, out: &mut Vec<Found>) -> Source {
    let mut src = Source::new(Tool::Opencode, data_dir(ctx, Tool::Opencode));
    discover_into(ctx, Tool::Opencode, out, &mut src);
    src
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct MsgData {
    #[serde(rename = "sessionID")]
    session_id: Option<String>,
    role: Option<String>,
    #[serde(rename = "modelID")]
    model_id: Option<String>,
    #[serde(rename = "providerID")]
    provider_id: Option<String>,
    /// v2：`model.{id,providerID}`
    model: Option<Value>,
    time: Option<MsgTime>,
    cost: Option<f64>,
    tokens: Option<Tokens>,
    path: Option<MsgPath>,
}

#[derive(Deserialize, Default)]
struct MsgTime {
    created: Option<f64>,
}

#[derive(Deserialize, Default)]
struct MsgPath {
    cwd: Option<String>,
    root: Option<String>,
}

#[derive(Deserialize, Default)]
struct Tokens {
    #[serde(default)]
    input: u64,
    #[serde(default)]
    output: u64,
    #[serde(default)]
    reasoning: u64,
    #[serde(default)]
    cache: CacheTokens,
}

#[derive(Deserialize, Default)]
struct CacheTokens {
    #[serde(default)]
    read: u64,
    #[serde(default)]
    write: u64,
}

struct Ingest<'a> {
    entry: &'a mut FileEntry,
    keys: HashMap<u64, usize>,
    key: &'static [u8],
    /// 会话 id → (工作目录, 标题)
    sessions: HashMap<String, (Option<String>, Option<String>)>,
}

impl Ingest<'_> {
    /// `v2`：来自 `session_message`，payload 里没有 role、输出总是不含推理
    fn message(&mut self, d: MsgData, sid: &str, fallback_ts: Option<i64>, v2: bool) {
        if !v2 && d.role.as_deref() != Some("assistant") {
            return;
        }
        let Some(t) = d.tokens else { return };
        let Some(ts) = d
            .time
            .as_ref()
            .and_then(|t| t.created)
            .and_then(crate::record::num_to_ms)
            .or(fallback_ts)
        else {
            return;
        };
        let model = d
            .model_id
            .clone()
            .or_else(|| {
                d.model
                    .as_ref()
                    .and_then(|m| m.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        let provider = d
            .provider_id
            .clone()
            .or_else(|| {
                d.model
                    .as_ref()
                    .and_then(|m| m.get("providerID"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        let output = if !v2 && ts < REASONING_SPLIT_MS {
            t.output
        } else {
            t.output + t.reasoning
        };
        let key = fnv64(&[
            self.key,
            &ts.to_le_bytes(),
            model.as_bytes(),
            provider.as_bytes(),
            &t.input.to_le_bytes(),
            &t.output.to_le_bytes(),
            &t.reasoning.to_le_bytes(),
            &t.cache.read.to_le_bytes(),
            &t.cache.write.to_le_bytes(),
        ]);
        let session = self.entry.session_idx(sid);
        {
            let s = &mut self.entry.sessions[session as usize];
            if s.project.is_none() || s.title.is_none() {
                let (dir, title) = self.sessions.get(sid).cloned().unwrap_or_default();
                if s.project.is_none() {
                    s.project = dir.filter(|d| !d.is_empty()).or_else(|| {
                        d.path
                            .as_ref()
                            .and_then(|p| p.cwd.clone().or_else(|| p.root.clone()))
                            .filter(|d| !d.is_empty())
                    });
                }
                if let Some(t) = title.as_deref().and_then(clean_title) {
                    s.set_title(t, false, 0);
                }
            }
        }
        let rec = Rec {
            ts,
            key,
            model: self.entry.model_idx(&model),
            session,
            input: t.input,
            output,
            cache_read: t.cache.read,
            cache_write_5m: t.cache.write,
            reasoning: t.reasoning.min(output),
            known_cost: d.cost.map(usd_to_cost).unwrap_or(0),
            ..Default::default()
        };
        if !rec.is_empty() {
            self.entry.push_dedup(&mut self.keys, rec);
        }
    }
}

fn load_sessions(
    conn: &rusqlite::Connection,
    table: &str,
    out: &mut HashMap<String, (Option<String>, Option<String>)>,
) -> io::Result<()> {
    let cols = sqlite::columns(conn, table)?;
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
        "SELECT id, {}, {} FROM \"{table}\"",
        pick("directory"),
        pick("title")
    );
    let mut stmt = conn.prepare(&sql).map_err(sqlite::to_io)?;
    let mut rows = stmt.query([]).map_err(sqlite::to_io)?;
    while let Some(r) = rows.next().map_err(sqlite::to_io)? {
        let Some(id) = r.get_ref(0).ok().and_then(sqlite::text) else {
            continue;
        };
        let get = |i: usize| r.get_ref(i).ok().and_then(sqlite::text);
        out.entry(id).or_insert((get(1), get(2)));
    }
    Ok(())
}

pub(crate) fn parse_db(path: &Path, tool: Tool, entry: &mut FileEntry) -> io::Result<()> {
    let conn = sqlite::open(path)?;
    let mut sessions = HashMap::new();
    load_sessions(&conn, "session", &mut sessions)?;
    load_sessions(&conn, "session_v2", &mut sessions)?;
    let keys = entry.key_index();
    let mut ing = Ingest {
        entry,
        keys,
        key: flavor(tool).key,
        sessions,
    };
    // v1：assistant 消息
    let cols = sqlite::columns(&conn, "message")?;
    if cols.contains("data") && cols.contains("session_id") {
        let created = if cols.contains("time_created") {
            "time_created"
        } else {
            "NULL"
        };
        let sql = format!(
            "SELECT session_id, data, {created} FROM message WHERE data LIKE '%\"assistant\"%'"
        );
        let mut stmt = conn.prepare(&sql).map_err(sqlite::to_io)?;
        let mut rows = stmt.query([]).map_err(sqlite::to_io)?;
        while let Some(r) = rows.next().map_err(sqlite::to_io)? {
            let sid = r.get_ref(0).ok().and_then(sqlite::text).unwrap_or_default();
            let Some(data) = r.get_ref(1).ok().and_then(sqlite::text) else {
                continue;
            };
            let fallback = r
                .get_ref(2)
                .ok()
                .and_then(sqlite::int)
                .and_then(|n| crate::record::num_to_ms(n as f64));
            if let Ok(d) = serde_json::from_str::<MsgData>(&data) {
                ing.message(d, &sid, fallback, false);
            }
        }
    }
    // v2：session_message 里 type = 'assistant' 的行
    let cols = sqlite::columns(&conn, "session_message")?;
    if cols.contains("data") && cols.contains("type") && cols.contains("session_id") {
        let created = if cols.contains("time_created") {
            "time_created"
        } else {
            "NULL"
        };
        let sql = format!(
            "SELECT session_id, data, {created} FROM session_message WHERE type = 'assistant'"
        );
        let mut stmt = conn.prepare(&sql).map_err(sqlite::to_io)?;
        let mut rows = stmt.query([]).map_err(sqlite::to_io)?;
        while let Some(r) = rows.next().map_err(sqlite::to_io)? {
            let sid = r.get_ref(0).ok().and_then(sqlite::text).unwrap_or_default();
            let Some(data) = r.get_ref(1).ok().and_then(sqlite::text) else {
                continue;
            };
            let fallback = r
                .get_ref(2)
                .ok()
                .and_then(sqlite::int)
                .and_then(|n| crate::record::num_to_ms(n as f64));
            if let Ok(d) = serde_json::from_str::<MsgData>(&data) {
                ing.message(d, &sid, fallback, true);
            }
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct LegacySession {
    directory: Option<String>,
    title: Option<String>,
}

/// 旧版 JSON：`path` 是 `storage/message/<会话 id>/` 目录。
pub(crate) fn parse_json_dir(path: &Path, tool: Tool, entry: &mut FileEntry) -> io::Result<()> {
    let sid = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    // `storage/session/<项目 id>/<会话 id>.json` 里有工作目录和标题
    let mut sessions = HashMap::new();
    if let Some(storage) = path.parent().and_then(Path::parent) {
        for proj in discover::subdirs(&storage.join("session"), &mut Vec::new()) {
            let f = proj.join(format!("{sid}.json"));
            if let Some(s) = std::fs::read(&f)
                .ok()
                .and_then(|b| serde_json::from_slice::<LegacySession>(&b).ok())
            {
                sessions.insert(sid.clone(), (s.directory, s.title));
                break;
            }
        }
    }
    let keys = entry.key_index();
    let mut ing = Ingest {
        entry,
        keys,
        key: flavor(tool).key,
        sessions,
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(path)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    for f in files {
        let Ok(bytes) = std::fs::read(&f) else {
            continue;
        };
        if let Ok(d) = serde_json::from_slice::<MsgData>(&bytes) {
            let msg_sid = d.session_id.clone().unwrap_or_else(|| sid.clone());
            ing.message(d, &msg_sid, None, false);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::COST_UNITS_PER_USD;

    const V1: &str = r#"{"role":"assistant","time":{"created":1767312000000,"completed":1767312004210},"parentID":"msg_p","modelID":"claude-sonnet-4-5","providerID":"anthropic","mode":"build","agent":"build","path":{"cwd":"/Users/alice/repo/sub","root":"/Users/alice/repo"},"cost":0.01842,"tokens":{"total":15960,"input":12,"output":410,"reasoning":96,"cache":{"read":14200,"write":1242}},"finish":"tool-calls"}"#;
    const V1_NEW: &str = r#"{"role":"assistant","time":{"created":1783882279705},"modelID":"gpt-5.1","providerID":"openai","cost":0,"tokens":{"input":100,"output":20,"reasoning":30,"cache":{"read":0,"write":0}}}"#;
    const V2: &str = r#"{"agent":"build","model":{"id":"gpt-5.1","providerID":"openai","variant":"default"},"content":[],"finish":"stop","cost":0,"tokens":{"input":100,"output":20,"reasoning":30,"cache":{"read":0,"write":0}},"time":{"created":1783882279705,"completed":1783882279943}}"#;
    const ZERO: &str = r#"{"role":"assistant","time":{"created":1783882300000},"modelID":"gpt-5.1","providerID":"openai","cost":0,"tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}}}"#;

    fn make_db(path: &Path) {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT, parent_id TEXT, slug TEXT,
                directory TEXT, title TEXT, version TEXT, time_created INTEGER, time_updated INTEGER);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);
             CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT, type TEXT, seq INTEGER,
                time_created INTEGER, time_updated INTEGER, data TEXT);
             INSERT INTO session VALUES ('ses_1','p','',NULL,'/Users/alice/repo/sub','修解析器','1.18.34',0,0);",
        )
        .unwrap();
        let ins = |id: &str, sid: &str, data: &str| {
            conn.execute(
                "INSERT INTO message VALUES (?1, ?2, 0, 0, ?3)",
                rusqlite::params![id, sid, data],
            )
            .unwrap();
        };
        ins("msg_1", "ses_1", V1);
        ins("msg_2", "ses_1", V1_NEW);
        ins("msg_3", "ses_1", ZERO);
        ins(
            "msg_u",
            "ses_1",
            r#"{"role":"user","time":{"created":1767311999000},"model":{"providerID":"anthropic","modelID":"x"}}"#,
        );
        // fork：新 id、内容相同
        ins("msg_9", "ses_fork", V1);
        conn.execute(
            "INSERT INTO session_message VALUES ('msg_2','ses_1','assistant',7,1783882279705,0,?1)",
            [V2],
        )
        .unwrap();
    }

    #[test]
    fn sqlite_tables_dedup_and_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join(".local/share/opencode");
        std::fs::create_dir_all(&data).unwrap();
        make_db(&data.join("opencode.db"));
        std::fs::write(data.join("opencode.db-shm"), "").unwrap();
        let mut found = Vec::new();
        let src = discover(&Ctx::with_vars(dir.path(), &[]), &mut found);
        assert!(src.errors.is_empty());
        assert_eq!(found.len(), 1);

        let mut e = FileEntry::new(Tool::Opencode, String::new(), 0, 0);
        parse_db(&found[0].path, Tool::Opencode, &mut e).unwrap();
        // v1 一条 + 新版一条（v2 表里的同一条去重）；fork 副本去重；全零跳过
        assert_eq!(e.recs.len(), 2);
        let old = &e.recs[0];
        // 2026-04-06 之前：output 已含推理
        assert_eq!((old.input, old.output, old.reasoning), (12, 410, 96));
        assert_eq!((old.cache_read, old.cache_write_5m), (14200, 1242));
        assert_eq!(old.known_cost as f64 / COST_UNITS_PER_USD, 0.01842);
        let new = &e.recs[1];
        assert_eq!((new.output, new.reasoning, new.known_cost), (50, 30, 0));
        let s = e.find_session("ses_1").unwrap();
        assert_eq!(s.project.as_deref(), Some("/Users/alice/repo/sub"));
        assert_eq!(s.title.as_deref(), Some("修解析器"));
    }

    #[test]
    fn legacy_json_and_kilo() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("x/kilo/storage");
        std::fs::create_dir_all(storage.join("message/ses_1")).unwrap();
        std::fs::create_dir_all(storage.join("session/proj1")).unwrap();
        let mut v: Value = serde_json::from_str(V1).unwrap();
        v["id"] = "msg_1".into();
        v["sessionID"] = "ses_1".into();
        std::fs::write(storage.join("message/ses_1/msg_1.json"), v.to_string()).unwrap();
        std::fs::write(
            storage.join("session/proj1/ses_1.json"),
            r#"{"id":"ses_1","projectID":"proj1","directory":"/Users/alice/old","title":"旧会话","time":{"created":1,"updated":2}}"#,
        )
        .unwrap();
        make_db(&dir.path().join("x/kilo/kilo.db"));
        let ctx = Ctx::with_vars(
            dir.path(),
            &[("XDG_DATA_HOME", dir.path().join("x").to_str().unwrap())],
        );
        let mut found = Vec::new();
        let mut src = Source::new(Tool::Kilo, data_dir(&ctx, Tool::Kilo));
        discover_into(&ctx, Tool::Kilo, &mut found, &mut src);
        assert_eq!(found.len(), 2);
        let json = found.iter().find(|f| f.kind == Kind::OpencodeJson).unwrap();
        let mut j = FileEntry::new(Tool::Kilo, String::new(), 0, 0);
        parse_json_dir(&json.path, Tool::Kilo, &mut j).unwrap();
        assert_eq!(j.recs.len(), 1);
        assert_eq!(j.sessions[0].project.as_deref(), Some("/Users/alice/old"));
        assert_eq!(j.sessions[0].title.as_deref(), Some("旧会话"));
        let db = found.iter().find(|f| f.kind == Kind::OpencodeDb).unwrap();
        let mut d = FileEntry::new(Tool::Kilo, String::new(), 0, 0);
        parse_db(&db.path, Tool::Kilo, &mut d).unwrap();
        // 旧 JSON 和库里的同一条消息键相同
        assert_eq!(d.recs[0].key, j.recs[0].key);
        assert!(is_db_name("opencode-beta.db", &["opencode"]));
        assert!(!is_db_name("opencode.db-wal", &["opencode"]));
        assert!(!is_db_name("opencodex.db", &["opencode"]));
    }

    #[test]
    fn missing_dir_is_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let mut found = Vec::new();
        let src = discover(&Ctx::with_vars(dir.path(), &[]), &mut found);
        assert!(found.is_empty() && src.errors.is_empty());
        assert_eq!(src.root, dir.path().join(".local/share/opencode"));
    }
}

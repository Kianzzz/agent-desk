//! 只读打开各工具的 SQLite 库。打不开、被锁住时返回中文错误，这次跳过，下次刷新再试。

use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, ErrorCode, OpenFlags};

/// 对方正在写入时最多等这么久
const BUSY_TIMEOUT: Duration = Duration::from_millis(1500);

pub(crate) fn open(path: &Path) -> io::Result<Connection> {
    if !path.exists() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "数据库文件不存在"));
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(to_io)?;
    conn.busy_timeout(BUSY_TIMEOUT).map_err(to_io)?;
    Ok(conn)
}

pub(crate) fn to_io(e: rusqlite::Error) -> io::Error {
    let msg = match &e {
        rusqlite::Error::SqliteFailure(f, _) => match f.code {
            ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => {
                "数据库正被占用，这次先跳过".to_string()
            }
            ErrorCode::CannotOpen | ErrorCode::PermissionDenied => {
                format!("数据库打不开（{e}）")
            }
            ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt => {
                format!("数据库文件损坏或不是 SQLite（{e}）")
            }
            _ => format!("数据库读取出错（{e}）"),
        },
        _ => format!("数据库读取出错（{e}）"),
    };
    io::Error::other(msg)
}

/// 表的列名；表不存在时返回空集合。
pub(crate) fn columns(conn: &Connection, table: &str) -> io::Result<HashSet<String>> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info(\"{table}\")"))
        .map_err(to_io)?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(to_io)?;
    let mut out = HashSet::new();
    for r in rows {
        out.insert(r.map_err(to_io)?);
    }
    Ok(out)
}

pub(crate) fn has_table(conn: &Connection, table: &str) -> io::Result<bool> {
    Ok(!columns(conn, table)?.is_empty())
}

/// 把 SQLite 里可能是整数、浮点或字符串的值读成 i64。
pub(crate) fn int(v: rusqlite::types::ValueRef<'_>) -> Option<i64> {
    use rusqlite::types::ValueRef;
    match v {
        ValueRef::Integer(i) => Some(i),
        ValueRef::Real(f) if f.is_finite() => Some(f as i64),
        ValueRef::Text(t) => std::str::from_utf8(t)
            .ok()?
            .trim()
            .parse::<f64>()
            .ok()
            .map(|f| f as i64),
        _ => None,
    }
}

pub(crate) fn real(v: rusqlite::types::ValueRef<'_>) -> Option<f64> {
    use rusqlite::types::ValueRef;
    match v {
        ValueRef::Integer(i) => Some(i as f64),
        ValueRef::Real(f) if f.is_finite() => Some(f),
        ValueRef::Text(t) => std::str::from_utf8(t).ok()?.trim().parse::<f64>().ok(),
        _ => None,
    }
}

pub(crate) fn text(v: rusqlite::types::ValueRef<'_>) -> Option<String> {
    use rusqlite::types::ValueRef;
    match v {
        ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Integer(i) => Some(i.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_reported_in_chinese() {
        let dir = tempfile::tempdir().unwrap();
        // 不存在：NotFound（扫描期间被删的文件不算错误）
        let e = open(&dir.path().join("none.db")).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);

        // 不是 SQLite
        let junk = dir.path().join("junk.db");
        std::fs::write(&junk, vec![b'x'; 4096]).unwrap();
        let e = open(&junk)
            .and_then(|c| columns(&c, "t").map(|_| ()))
            .unwrap_err();
        assert!(e.to_string().contains("不是 SQLite"), "{e}");

        // 别的进程正在写（独占锁）：等一会儿后放弃这次
        let db = dir.path().join("busy.db");
        let w = Connection::open(&db).unwrap();
        w.execute_batch("CREATE TABLE t (a INTEGER); INSERT INTO t VALUES (1);")
            .unwrap();
        w.execute_batch("BEGIN EXCLUSIVE; INSERT INTO t VALUES (2);")
            .unwrap();
        let r = open(&db).unwrap();
        let e = r
            .query_row("SELECT count(*) FROM t", [], |row| row.get::<_, i64>(0))
            .map_err(to_io)
            .unwrap_err();
        assert!(e.to_string().contains("占用"), "{e}");
        w.execute_batch("COMMIT;").unwrap();
        let n: i64 = r
            .query_row("SELECT count(*) FROM t", [], |row| row.get(0))
            .unwrap();
        assert_eq!(n, 2);
        // 只读打开：写不进去
        assert!(r.execute("INSERT INTO t VALUES (3)", []).is_err());
    }
}

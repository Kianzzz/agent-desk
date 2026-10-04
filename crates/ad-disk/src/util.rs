//! 小工具：路径哈希、时间格式、人类可读大小、受限读取文件头尾。

use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const MINUTE: i64 = 60;
pub(crate) const DAY: i64 = 86_400;
/// 「在用」的判定：最近 30 分钟内写过
pub(crate) const IN_USE_SECS: i64 = 30 * MINUTE;

/// id = 路径 sha256 的前 16 位十六进制
pub(crate) fn path_id(path: &str) -> String {
    let digest = Sha256::digest(path.as_bytes());
    hex::encode(digest)[..16].to_string()
}

pub(crate) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub(crate) fn system_time_secs(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(_) => 0,
    }
}

/// Unix 秒 → RFC 3339（本地时区）。0 表示未知，给出 1970 年的时间。
pub(crate) fn rfc3339(secs: i64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(t) => t.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        _ => chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0)
            .unwrap_or_default()
            .to_rfc3339(),
    }
}

pub(crate) fn parse_rfc3339(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.timestamp())
}

pub(crate) fn human_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else if v >= 100.0 {
        format!("{v:.0} {}", UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// 把 home 前缀换成 `~`
pub(crate) fn tilde(home: &Path, p: &str) -> String {
    let h = home.to_string_lossy();
    let h = h.trim_end_matches('/');
    if !h.is_empty() {
        if p == h {
            return "~".to_string();
        }
        if let Some(rest) = p.strip_prefix(h) {
            if rest.starts_with('/') {
                return format!("~{rest}");
            }
        }
    }
    p.to_string()
}

/// 路径里有没有 `..` 之类的成分
pub(crate) fn has_parent_component(p: &Path) -> bool {
    p.components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
}

pub(crate) fn path_string(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// 去掉结尾的 `/`（根目录除外）
pub(crate) fn normalize_dir_string(s: &str) -> String {
    let t = s.trim();
    if t.len() > 1 {
        t.trim_end_matches('/').to_string()
    } else {
        t.to_string()
    }
}

/// 截取前 n 个字符，空白折叠成一个空格
pub(crate) fn clip_title(s: &str, n: usize) -> String {
    let mut out = String::new();
    let mut last_space = false;
    let mut count = 0;
    for ch in s.trim().chars() {
        let ch = if ch.is_whitespace() { ' ' } else { ch };
        if ch == ' ' {
            if last_space {
                continue;
            }
            last_space = true;
        } else {
            last_space = false;
        }
        out.push(ch);
        count += 1;
        if count >= n {
            break;
        }
    }
    out.trim().to_string()
}

/// 读取文件开头时交给回调的一行
pub(crate) enum HeadLine<'a> {
    /// 完整的一行
    Full(&'a [u8]),
    /// 超长行（多半带 base64 图片）的开头部分，其余直接跳过
    Prefix(&'a [u8]),
}

/// 逐行读取文件开头。完整交出的字节最多 `budget`；超过 `max_line` 的行只交出开头
/// `max_line` 字节（剩下的照样要读过去，但总读取量不超过 budget 的 8 倍）。回调返回 false 时提前结束。
pub(crate) fn for_each_head_line(
    path: &Path,
    budget: usize,
    max_line: usize,
    mut f: impl FnMut(HeadLine) -> bool,
) {
    let Ok(file) = File::open(path) else { return };
    let hard_cap = budget.saturating_mul(8);
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut line: Vec<u8> = Vec::new();
    let mut skipping = false;
    let mut delivered = 0usize;
    let mut total = 0usize;
    loop {
        let buf = match reader.fill_buf() {
            Ok(b) => b,
            Err(_) => return,
        };
        if buf.is_empty() {
            if !skipping && !line.is_empty() {
                f(HeadLine::Full(&line));
            }
            return;
        }
        let len = buf.len();
        let mut consumed = 0;
        let mut stop = false;
        while consumed < len {
            let rest = &buf[consumed..];
            match memchr::memchr(b'\n', rest) {
                Some(pos) => {
                    if !skipping {
                        line.extend_from_slice(&rest[..pos]);
                        let cont = if line.len() <= max_line {
                            delivered += line.len();
                            f(HeadLine::Full(&line))
                        } else {
                            delivered += max_line;
                            f(HeadLine::Prefix(&line[..max_line]))
                        };
                        stop = !cont;
                    }
                    line.clear();
                    skipping = false;
                    consumed += pos + 1;
                    if stop {
                        break;
                    }
                }
                None => {
                    if !skipping {
                        line.extend_from_slice(rest);
                        if line.len() > max_line {
                            delivered += max_line;
                            stop = !f(HeadLine::Prefix(&line[..max_line]));
                            skipping = true;
                            line.clear();
                        }
                    }
                    consumed = len;
                }
            }
        }
        reader.consume(consumed);
        total += consumed;
        if stop || delivered >= budget || total >= hard_cap {
            return;
        }
    }
}

/// 在 JSON 片段里找 `"key":"…"` 形式的字符串值（片段可能不完整，不做整体解析）
pub(crate) fn json_string_values(hay: &[u8], key: &str) -> Vec<String> {
    let pat = format!("\"{key}\":\"");
    let mut out = Vec::new();
    for pos in memchr::memmem::find_iter(hay, pat.as_bytes()) {
        let q = pos + pat.len() - 1; // 开头的引号
        let mut i = q + 1;
        let mut end = None;
        while i < hay.len() {
            match hay[i] {
                b'\\' => i += 2,
                b'"' => {
                    end = Some(i);
                    break;
                }
                _ => i += 1,
            }
        }
        let Some(e) = end else { break };
        if let Ok(s) = serde_json::from_slice::<String>(&hay[q..=e]) {
            out.push(s);
        }
    }
    out
}

/// 读取文件第一行（最多 max 字节，超出返回 None）
pub(crate) fn read_first_line(path: &Path, max: usize) -> Option<Vec<u8>> {
    let mut out = None;
    for_each_head_line(path, max, max, |l| {
        if let HeadLine::Full(l) = l {
            out = Some(l.to_vec());
        }
        false
    });
    out
}

/// 读取文件末尾 n 字节里的完整行
pub(crate) fn tail_lines(path: &Path, n: u64) -> Vec<Vec<u8>> {
    let Ok(mut file) = File::open(path) else {
        return Vec::new();
    };
    let Ok(meta) = file.metadata() else {
        return Vec::new();
    };
    let len = meta.len();
    let start = len.saturating_sub(n);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::with_capacity((len - start) as usize);
    if file.take(n).read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let mut parts: Vec<Vec<u8>> = buf.split(|b| *b == b'\n').map(|s| s.to_vec()).collect();
    if start > 0 && !parts.is_empty() {
        parts.remove(0);
    }
    parts.retain(|p| !p.is_empty());
    parts
}

/// 把 `p` 规范成绝对路径并解析父目录里的符号链接；最后一段如果是符号链接本身不解析。
pub(crate) fn canonical_parent_join(p: &Path) -> Option<PathBuf> {
    let parent = p.parent()?;
    let name = p.file_name()?;
    let cp = std::fs::canonicalize(parent).ok()?;
    Some(cp.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_and_tilde() {
        assert_eq!(clip_title("  a\n\n b  c ", 80), "a b c");
        assert_eq!(clip_title("一二三四五", 3), "一二三");
        let home = Path::new("/Users/x");
        assert_eq!(tilde(home, "/Users/x/.codex"), "~/.codex");
        assert_eq!(tilde(home, "/Users/xy"), "/Users/xy");
        assert_eq!(tilde(home, "/Users/x"), "~");
    }

    #[test]
    fn partial_json_strings() {
        let hay = br#"{"role":"user","content":[{"type":"input_text","text":"a\"b\n\u4e2d"},{"type":"input_text","text":"second"},{"type":"input_image","image_url":"data:AAAA"#;
        assert_eq!(
            json_string_values(hay, "text"),
            vec!["a\"b\n中".to_string(), "second".to_string()]
        );
        // 截断在字符串中间：丢弃最后半个
        assert_eq!(
            json_string_values(br#"{"text":"ok"},{"text":"cut"#, "text"),
            vec!["ok"]
        );
    }

    #[test]
    fn human() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(7 * 1024 * 1024 * 1024), "7.0 GB");
    }

    #[test]
    fn id_is_16_hex() {
        let id = path_id("/a/b");
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn head_lines_skip_long() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.jsonl");
        let long = "x".repeat(5000);
        std::fs::write(&p, format!("a\n{long}\nb\nc")).unwrap();
        let mut got = Vec::new();
        for_each_head_line(&p, 1 << 20, 100, |l| {
            match l {
                HeadLine::Full(l) => got.push(String::from_utf8_lossy(l).into_owned()),
                HeadLine::Prefix(l) => got.push(format!("prefix:{}", l.len())),
            }
            true
        });
        assert_eq!(got, vec!["a", "prefix:100", "b", "c"]);
        let tail = tail_lines(&p, 3);
        assert_eq!(tail.len(), 1);
    }
}

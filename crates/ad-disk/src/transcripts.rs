//! 发现各工具的对话记录：读文件开头找 cwd 和标题，不读全文。

use crate::util::{
    clip_title, for_each_head_line, json_string_values, normalize_dir_string, read_first_line,
    system_time_secs, tail_lines, HeadLine,
};
use crate::walk::{dir_stats, Stats};
use rayon::prelude::*;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub(crate) const TITLE_CHARS: usize = 80;
/// 找标题时最多读文件开头这么多字节
const HEAD_BUDGET: usize = 4 * 1024 * 1024;
/// 单行超过这个长度不解析（多半是 base64 图片或大段工具输出）
const MAX_LINE: usize = 1024 * 1024;
/// Codex 第一行 session_meta 最多读 1MB
const CODEX_META_MAX: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct Transcript {
    /// "claude" | "codex" | "gemini"
    pub tool: &'static str,
    /// 对话记录文件
    pub path: PathBuf,
    pub session_id: String,
    /// 能确定时为项目目录；None = 未知
    pub cwd: Option<String>,
    pub title: Option<String>,
    pub stats: Stats,
    /// Codex 分支对话（文件名形如 `<主对话 id>_<分支 id>`）的主对话 id
    pub parent_id: Option<String>,
    /// Codex 内部子任务（如自动审查），不是用户直接发起的对话
    pub subagent: bool,
}

pub(crate) fn discover_all(home: &Path) -> Vec<Transcript> {
    let (mut claude, (codex, gemini)) = rayon::join(
        || discover_claude(home),
        || rayon::join(|| discover_codex(home), || discover_gemini(home, &[])),
    );
    // Gemini 的项目哈希可以用其他工具发现的项目路径来反查
    let mut known: Vec<String> = claude
        .iter()
        .chain(codex.iter())
        .filter_map(|t| t.cwd.clone())
        .collect();
    known.sort();
    known.dedup();
    let gemini = if gemini.iter().any(|t| t.cwd.is_none()) {
        discover_gemini(home, &known)
    } else {
        gemini
    };
    claude.extend(codex);
    claude.extend(gemini);
    claude
}

// ---------------------------------------------------------------- Claude

/// Claude Code 把项目路径编码成目录名：非字母数字一律换成 `-`
pub(crate) fn claude_encode(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

pub(crate) fn discover_claude(home: &Path) -> Vec<Transcript> {
    let root = home.join(".claude/projects");
    let Ok(rd) = fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut files: Vec<(PathBuf, String)> = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let enc = e.file_name().to_string_lossy().into_owned();
        if let Ok(inner) = fs::read_dir(&p) {
            for f in inner.flatten() {
                let fp = f.path();
                if fp.extension().and_then(|x| x.to_str()) == Some("jsonl")
                    && f.file_type().map(|t| t.is_file()).unwrap_or(false)
                {
                    files.push((fp, enc.clone()));
                }
            }
        }
    }
    files
        .par_iter()
        .map(|(p, enc)| parse_claude_transcript(p, enc))
        .collect()
}

pub(crate) fn parse_claude_transcript(path: &Path, enc_dir: &str) -> Transcript {
    let session_id = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = path.with_extension("");
    let extra_dir = if dir.is_dir() { Some(dir) } else { None };

    let mut head_cwds: Vec<String> = Vec::new();
    let mut title: Option<String> = None;
    let mut lines_seen = 0usize;
    for_each_head_line(path, HEAD_BUDGET, MAX_LINE, |line| {
        let HeadLine::Full(line) = line else {
            return true;
        };
        lines_seen += 1;
        let has_cwd = memchr::memmem::find(line, b"\"cwd\"").is_some();
        let is_user = memchr::memmem::find(line, b"\"type\":\"user\"").is_some();
        let wanted = (has_cwd && head_cwds.len() < 8) || (is_user && title.is_none());
        if !wanted {
            return title.is_none() || head_cwds.is_empty();
        }
        let Ok(v) = serde_json::from_slice::<Value>(line) else {
            return true;
        };
        if let Some(c) = v.get("cwd").and_then(|c| c.as_str()) {
            let c = normalize_dir_string(c);
            if !c.is_empty() && !head_cwds.contains(&c) {
                head_cwds.push(c);
            }
        }
        if title.is_none() && v.get("type").and_then(|t| t.as_str()) == Some("user") {
            title = claude_title_from_entry(&v);
        }
        // 标题和 cwd 都有了，再多看几十行收集 cwd 就停
        !(title.is_some() && !head_cwds.is_empty() && lines_seen > 60)
    });

    let mut tail_cwds: Vec<String> = Vec::new();
    for line in tail_lines(path, 256 * 1024).iter().rev() {
        if memchr::memmem::find(line, b"\"cwd\"").is_none() {
            continue;
        }
        if let Ok(v) = serde_json::from_slice::<Value>(line) {
            if let Some(c) = v.get("cwd").and_then(|c| c.as_str()) {
                let c = normalize_dir_string(c);
                if !c.is_empty() && !tail_cwds.contains(&c) {
                    tail_cwds.push(c);
                }
                if tail_cwds.len() >= 4 {
                    break;
                }
            }
        }
    }
    let cwd = resolve_claude_cwd(enc_dir, &tail_cwds, &head_cwds);

    if title.is_none() {
        title = extra_dir
            .as_ref()
            .and_then(|d| fs::read(d.join("custom-title.json")).ok())
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| {
                v.get("customTitle")
                    .and_then(|t| t.as_str())
                    .map(|s| s.to_string())
            })
            .map(|s| clip_title(&s, TITLE_CHARS))
            .filter(|s| !s.is_empty());
    }

    let mut stats = file_stats(path);
    if let Some(d) = &extra_dir {
        if let Some(s) = dir_stats(d) {
            stats.add(&s);
        }
    }
    Transcript {
        tool: "claude",
        path: path.to_path_buf(),
        session_id,
        cwd: Some(cwd),
        title,
        stats,
        parent_id: None,
        subagent: false,
    }
}

/// 目录名是 cwd 的有损编码。先找编码后和目录名一致的 cwd（也试它的上级目录，
/// 因为对话中途可能 cd 到子目录），都对不上就用最后出现的 cwd，再不行就粗略还原目录名。
pub(crate) fn resolve_claude_cwd(enc_dir: &str, tail: &[String], head: &[String]) -> String {
    for c in tail.iter().chain(head.iter()) {
        let mut p = Path::new(c);
        loop {
            let s = p.to_string_lossy();
            if claude_encode(&s) == enc_dir {
                return s.into_owned();
            }
            match p.parent() {
                Some(parent) if parent != p => p = parent,
                _ => break,
            }
        }
    }
    if let Some(c) = tail.first().or(head.first()) {
        return c.clone();
    }
    // 没有 cwd 字段：粗略还原（会把原本的 `-` 也当成 `/`）
    let guess = enc_dir.replace('-', "/");
    if guess.starts_with('/') {
        guess
    } else {
        format!("/{guess}")
    }
}

/// 从一条 Claude `type:user` 记录里取标题；不是真正的用户输入时返回 None。
pub(crate) fn claude_title_from_entry(v: &Value) -> Option<String> {
    if v.get("isMeta").and_then(|b| b.as_bool()) == Some(true)
        || v.get("isCompactSummary").and_then(|b| b.as_bool()) == Some(true)
        || v.get("isSidechain").and_then(|b| b.as_bool()) == Some(true)
    {
        return None;
    }
    let content = v.get("message")?.get("content")?;
    let mut texts: Vec<&str> = Vec::new();
    match content {
        Value::String(s) => texts.push(s),
        Value::Array(items) => {
            for it in items {
                match it.get("type").and_then(|t| t.as_str()) {
                    Some("tool_result") => return None,
                    Some("text") => {
                        if let Some(t) = it.get("text").and_then(|t| t.as_str()) {
                            texts.push(t);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => return None,
    }
    texts.into_iter().find_map(claude_text_title)
}

fn strip_leading_system_reminders(mut s: &str) -> &str {
    loop {
        let t = s.trim_start();
        if let Some(rest) = t.strip_prefix("<system-reminder>") {
            match rest.find("</system-reminder>") {
                Some(end) => s = &rest[end + "</system-reminder>".len()..],
                None => return "",
            }
        } else {
            return t;
        }
    }
}

fn tag_content<'a>(s: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = s.find(&open)? + open.len();
    let end = s[start..].find(&close)? + start;
    Some(&s[start..end])
}

fn claude_text_title(raw: &str) -> Option<String> {
    let t = strip_leading_system_reminders(raw).trim();
    if t.is_empty() || t.starts_with("Caveat:") || t.starts_with("[Request interrupted") {
        return None;
    }
    if t.starts_with("[Image: source") {
        return None;
    }
    if t.starts_with("<command-") {
        // 斜杠命令：带参数时参数就是用户的真实请求，用「/命令 参数」作标题
        let args = tag_content(t, "command-args").map(str::trim).unwrap_or("");
        if args.is_empty() {
            return None;
        }
        let name = tag_content(t, "command-name").map(str::trim).unwrap_or("");
        let s = if name.is_empty() {
            args.to_string()
        } else {
            format!("{name} {args}")
        };
        return Some(clip_title(&tidy_title(&s), TITLE_CHARS));
    }
    if t.starts_with('<') {
        return None;
    }
    let s = clip_title(&tidy_title(t), TITLE_CHARS);
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

// ---------------------------------------------------------------- Codex

pub(crate) fn discover_codex(home: &Path) -> Vec<Transcript> {
    let mut files = Vec::new();
    collect_jsonl(&home.join(".codex/sessions"), 4, &mut files);
    collect_jsonl(&home.join(".codex/archived_sessions"), 0, &mut files);
    files.par_iter().map(|p| parse_codex_rollout(p)).collect()
}

fn collect_jsonl(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        let p = e.path();
        if ft.is_dir() && depth > 0 {
            collect_jsonl(&p, depth - 1, out);
        } else if ft.is_file() && p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
            out.push(p);
        }
    }
}

const UUID_LEN: usize = 36;

fn looks_like_uuid(s: &str) -> bool {
    s.len() == UUID_LEN
        && s.chars().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

/// rollout-2026-10-01T20-52-56-<id>[_<分支 id>].jsonl → (主对话 id, 分支 id)
fn codex_ids_from_name(stem: &str) -> (Option<String>, Option<String>) {
    let (main, sub) = match stem.rsplit_once('_') {
        Some((a, b)) if looks_like_uuid(b) => (a, Some(b)),
        _ => (stem, None),
    };
    // 用 get 而不是直接切片：文件名里万一有多字节字符也不会 panic
    let id = main
        .len()
        .checked_sub(UUID_LEN)
        .and_then(|start| main.get(start..))
        .filter(|cand| looks_like_uuid(cand))
        .map(|cand| cand.to_string());
    (id, sub.map(|s| s.to_string()))
}

pub(crate) fn parse_codex_rollout(path: &Path) -> Transcript {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (main_id, branch_id) = codex_ids_from_name(&stem);
    let mut meta_id: Option<String> = None;
    let mut cwd = None;
    let mut subagent = false;
    if let Some(first) = read_first_line(path, CODEX_META_MAX) {
        if let Ok(v) = serde_json::from_slice::<Value>(&first) {
            if v.get("type").and_then(|t| t.as_str()) == Some("session_meta") {
                if let Some(p) = v.get("payload") {
                    if let Some(c) = p.get("cwd").and_then(|c| c.as_str()) {
                        let c = normalize_dir_string(c);
                        if !c.is_empty() {
                            cwd = Some(c);
                        }
                    }
                    if let Some(id) = p.get("id").and_then(|c| c.as_str()) {
                        if !id.is_empty() {
                            meta_id = Some(id.to_string());
                        }
                    }
                    subagent = p.get("source").and_then(|s| s.get("subagent")).is_some()
                        || p.get("thread_source")
                            .and_then(|s| s.as_str())
                            .is_some_and(|s| s.contains("review") || s.contains("subagent"));
                }
            }
        }
    }
    // 分支对话：自己的 id 是分支 id，主对话 id 记在 parent_id；普通对话以 session_meta 的 id 为准
    let (session_id, parent_id) = match branch_id {
        Some(b) => (b, meta_id.or(main_id)),
        None => (meta_id.or(main_id).unwrap_or_else(|| stem.clone()), None),
    };
    let mut title = None;
    let mut first_line = true;
    for_each_head_line(path, HEAD_BUDGET, MAX_LINE, |line| {
        if first_line {
            first_line = false;
            return true;
        }
        match line {
            HeadLine::Full(line) => {
                let is_user_msg = memchr::memmem::find(line, b"\"role\":\"user\"").is_some()
                    || memchr::memmem::find(line, b"\"user_message\"").is_some();
                if !is_user_msg {
                    return true;
                }
                if let Ok(v) = serde_json::from_slice::<Value>(line) {
                    title = codex_title_from_entry(&v);
                }
            }
            HeadLine::Prefix(prefix) => {
                // 超长的用户消息（带 base64 图片）：文字一般在图片前面，从开头部分里找
                if memchr::memmem::find(prefix, b"\"role\":\"user\"").is_some() {
                    title = json_string_values(prefix, "text")
                        .iter()
                        .find_map(|t| codex_text_title(t));
                }
            }
        }
        title.is_none()
    });
    Transcript {
        tool: "codex",
        path: path.to_path_buf(),
        session_id,
        cwd,
        title,
        stats: file_stats(path),
        parent_id,
        subagent,
    }
}

pub(crate) fn codex_title_from_entry(v: &Value) -> Option<String> {
    let p = v.get("payload")?;
    match p.get("type").and_then(|t| t.as_str()) {
        Some("message") if p.get("role").and_then(|r| r.as_str()) == Some("user") => {
            let items = p.get("content")?.as_array()?;
            items.iter().find_map(|it| {
                let ty = it.get("type").and_then(|t| t.as_str());
                if ty != Some("input_text") && ty != Some("text") {
                    return None;
                }
                codex_text_title(it.get("text")?.as_str()?)
            })
        }
        Some("user_message") => codex_text_title(p.get("message")?.as_str()?),
        _ => None,
    }
}

fn codex_text_title(raw: &str) -> Option<String> {
    let mut t = raw.trim();
    if let Some(idx) = t.find("## My request:") {
        t = t[idx + "## My request:".len()..].trim();
    } else if t.starts_with("# Files mentioned by the user") {
        return None;
    }
    if t.is_empty() || t.starts_with('<') || t.starts_with("# AGENTS.md instructions") {
        return None;
    }
    let s = clip_title(&tidy_title(&simplify_md_links(t)), TITLE_CHARS);
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// `[$skill](/path/SKILL.md)` → `$skill`
fn simplify_md_links(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('[') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(close) = after.find("](") {
            let label = &after[..close];
            let tail = &after[close + 2..];
            if let Some(end) = tail.find(')') {
                if !label.contains('\n') && !label.contains('[') {
                    out.push_str(label);
                    rest = &tail[end + 1..];
                    continue;
                }
            }
        }
        out.push('[');
        rest = after;
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------- Gemini

/// Gemini CLI 的项目哈希 = 项目路径的 sha256
pub(crate) fn gemini_hash(path: &str) -> String {
    hex::encode(Sha256::digest(path.as_bytes()))
}

pub(crate) fn discover_gemini(home: &Path, known_cwds: &[String]) -> Vec<Transcript> {
    let root = home.join(".gemini/tmp");
    let Ok(rd) = fs::read_dir(&root) else {
        return Vec::new();
    };
    // 哈希 → 路径
    let mut by_hash: HashMap<String, String> = HashMap::new();
    let mut add = |p: &str| {
        let p = normalize_dir_string(p);
        by_hash.insert(gemini_hash(&p), p);
    };
    add(&home.to_string_lossy());
    for c in known_cwds {
        add(c);
    }
    for f in ["projects.json", "trustedFolders.json"] {
        if let Ok(b) = fs::read(home.join(".gemini").join(f)) {
            if let Ok(v) = serde_json::from_slice::<Value>(&b) {
                let obj = v.get("projects").unwrap_or(&v);
                if let Some(m) = obj.as_object() {
                    for k in m.keys() {
                        add(k);
                    }
                }
            }
        }
    }
    let mut files: Vec<(PathBuf, Option<String>, String)> = Vec::new();
    for e in rd.flatten() {
        let d = e.path();
        if !d.is_dir() {
            continue;
        }
        let dir_name = e.file_name().to_string_lossy().into_owned();
        let root_marker = fs::read_to_string(d.join(".project_root"))
            .ok()
            .map(|s| normalize_dir_string(&s))
            .filter(|s| !s.is_empty());
        let cwd = root_marker.or_else(|| by_hash.get(&dir_name).cloned());
        if let Ok(chats) = fs::read_dir(d.join("chats")) {
            for c in chats.flatten() {
                let p = c.path();
                let name = c.file_name().to_string_lossy().into_owned();
                if name.starts_with("session-") && name.ends_with(".json") {
                    files.push((p, cwd.clone(), dir_name.clone()));
                }
            }
        }
    }
    files
        .par_iter()
        .map(|(p, cwd, _dir)| parse_gemini_chat(p, cwd.clone(), &by_hash))
        .collect()
}

pub(crate) fn parse_gemini_chat(
    path: &Path,
    cwd: Option<String>,
    by_hash: &HashMap<String, String>,
) -> Transcript {
    let stats = file_stats(path);
    let mut session_id = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut title = None;
    let mut cwd = cwd;
    // 单个会话文件一般不大；超过 16MB 不解析，只统计大小
    if stats.bytes < 16 * 1024 * 1024 {
        if let Ok(b) = fs::read(path) {
            if let Ok(v) = serde_json::from_slice::<Value>(&b) {
                if let Some(id) = v.get("sessionId").and_then(|s| s.as_str()) {
                    session_id = id.to_string();
                }
                if cwd.is_none() {
                    if let Some(h) = v.get("projectHash").and_then(|s| s.as_str()) {
                        cwd = by_hash.get(h).cloned();
                    }
                }
                if let Some(msgs) = v.get("messages").and_then(|m| m.as_array()) {
                    title = msgs.iter().find_map(|m| {
                        if m.get("type").and_then(|t| t.as_str()) != Some("user") {
                            return None;
                        }
                        let text = match m.get("content")? {
                            Value::String(s) => s.clone(),
                            Value::Array(parts) => parts
                                .iter()
                                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                                .collect::<Vec<_>>()
                                .join(" "),
                            _ => return None,
                        };
                        let t = text.trim();
                        if t.is_empty() || t.starts_with('<') {
                            return None;
                        }
                        Some(clip_title(&tidy_title(t), TITLE_CHARS))
                    });
                }
            }
        }
    }
    Transcript {
        tool: "gemini",
        path: path.to_path_buf(),
        session_id,
        cwd,
        title,
        stats,
        parent_id: None,
        subagent: false,
    }
}

// ---------------------------------------------------------------- common

/// 标题清理：去掉 Markdown 代码块标记，解码常见的 HTML 实体（如 `&#x20;`）
pub(crate) fn tidy_title(s: &str) -> String {
    let mut t = s.replace("```", " ");
    for (from, to) in [
        ("&nbsp;", " "),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&amp;", "&"),
    ] {
        t = t.replace(from, to);
    }
    // 数字实体 &#x20; / &#32;
    let mut out = String::with_capacity(t.len());
    let mut rest = t.as_str();
    while let Some(i) = rest.find("&#") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let (hex, digits_start) = match after.as_bytes().first() {
            Some(b'x') | Some(b'X') => (true, 1),
            _ => (false, 0),
        };
        let body = &after[digits_start..];
        let end = body.find(';').filter(|e| *e > 0 && *e <= 8);
        let decoded = end.and_then(|e| {
            let n = if hex {
                u32::from_str_radix(&body[..e], 16).ok()
            } else {
                body[..e].parse::<u32>().ok()
            };
            n.and_then(char::from_u32).map(|c| (c, e))
        });
        match decoded {
            Some((c, e)) => {
                out.push(c);
                rest = &body[e + 1..];
            }
            None => {
                out.push_str("&#");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

pub(crate) fn file_stats(path: &Path) -> Stats {
    match fs::symlink_metadata(path) {
        Ok(m) => Stats {
            bytes: m.blocks() * 512,
            files: 1,
            newest: m.modified().map(system_time_secs).unwrap_or(0),
        },
        Err(_) => Stats::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tidy() {
        assert_eq!(
            tidy_title("a&#x20;b &amp; ```markdown c"),
            "a b &  markdown c"
        );
        assert_eq!(tidy_title("x&#20013;y &#zz;"), "x中y &#zz;");
    }

    #[test]
    fn encode_matches_claude() {
        assert_eq!(
            claude_encode("/Users/alice/Library/Mobile Documents/iCloud~md~obsidian/Notes"),
            "-Users-alice-Library-Mobile-Documents-iCloud-md-obsidian-Notes"
        );
    }

    #[test]
    fn cwd_resolution_prefers_matching_encoding() {
        let enc = "-Users-a-proj-x";
        // 尾部 cwd 进了子目录，头部是另一个临时目录
        let tail = vec!["/Users/a/proj-x/videos/out".to_string()];
        let head = vec!["/tmp/scratch".to_string()];
        assert_eq!(resolve_claude_cwd(enc, &tail, &head), "/Users/a/proj-x");
        // 头部匹配
        let head2 = vec!["/Users/a/proj-x".to_string()];
        assert_eq!(resolve_claude_cwd(enc, &[], &head2), "/Users/a/proj-x");
        // 都不匹配：用最后出现的
        assert_eq!(resolve_claude_cwd(enc, &head, &[]), "/tmp/scratch");
        // 没有 cwd
        assert_eq!(resolve_claude_cwd("-Users-a-b", &[], &[]), "/Users/a/b");
    }

    #[test]
    fn claude_title_rules() {
        let skip = [
            json!({"type":"user","message":{"content":"<command-name>/clear</command-name><command-args></command-args>"}}),
            json!({"type":"user","message":{"content":"Caveat: The messages below were generated"}}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}),
            json!({"type":"user","isMeta":true,"message":{"content":[{"type":"text","text":"Base directory for this skill"}]}}),
            json!({"type":"user","message":{"content":"<system-reminder>only reminder</system-reminder>"}}),
            json!({"type":"user","message":{"content":"<local-command-stdout>x</local-command-stdout>"}}),
        ];
        for v in &skip {
            assert_eq!(claude_title_from_entry(v), None, "{v}");
        }
        let v = json!({"type":"user","message":{"content":[
            {"type":"text","text":"<system-reminder>\nscratch workspace\n</system-reminder>"},
            {"type":"text","text":"帮我安装remotion：npx create-video@latest"}]}});
        assert_eq!(
            claude_title_from_entry(&v).as_deref(),
            Some("帮我安装remotion：npx create-video@latest")
        );
        let v = json!({"type":"user","message":{"content":"<command-message>s</command-message>\n<command-name>/write-script</command-name>\n<command-args>改一下脚本</command-args>"}});
        assert_eq!(
            claude_title_from_entry(&v).as_deref(),
            Some("/write-script 改一下脚本")
        );
        let long = "长".repeat(200);
        let v = json!({"type":"user","message":{"content": long}});
        assert_eq!(claude_title_from_entry(&v).unwrap().chars().count(), 80);
    }

    #[test]
    fn claude_transcript_file() {
        let d = tempfile::tempdir().unwrap();
        let enc = claude_encode("/Users/a/my proj");
        let pd = d.path().join(&enc);
        fs::create_dir_all(pd.join("s1/subagents")).unwrap();
        fs::write(pd.join("s1/subagents/a.jsonl"), "{}\n").unwrap();
        let lines = [
            json!({"type":"queue-operation","operation":"enqueue"}).to_string(),
            json!({"type":"user","cwd":"/Users/a/my proj","message":{"content":"<command-name>/model</command-name><command-args></command-args>"}}).to_string(),
            json!({"type":"user","cwd":"/Users/a/my proj","message":{"content":[{"type":"tool_result","content":"x"}]}}).to_string(),
            json!({"type":"user","cwd":"/Users/a/my proj","message":{"content":"真正的问题"}}).to_string(),
        ];
        fs::write(pd.join("s1.jsonl"), lines.join("\n")).unwrap();
        let t = parse_claude_transcript(&pd.join("s1.jsonl"), &enc);
        assert_eq!(t.cwd.as_deref(), Some("/Users/a/my proj"));
        assert_eq!(t.title.as_deref(), Some("真正的问题"));
        assert_eq!(t.session_id, "s1");
        assert_eq!(t.stats.files, 2);
    }

    #[test]
    fn claude_custom_title_fallback() {
        let d = tempfile::tempdir().unwrap();
        let pd = d.path().join("-x");
        fs::create_dir_all(pd.join("s2")).unwrap();
        fs::write(
            pd.join("s2/custom-title.json"),
            r#"{"customTitle":"自定义标题"}"#,
        )
        .unwrap();
        fs::write(pd.join("s2.jsonl"), r#"{"type":"summary","cwd":"/x"}"#).unwrap();
        let t = parse_claude_transcript(&pd.join("s2.jsonl"), "-x");
        assert_eq!(t.title.as_deref(), Some("自定义标题"));
        assert_eq!(t.cwd.as_deref(), Some("/x"));
    }

    #[test]
    fn codex_rollout_parse() {
        let d = tempfile::tempdir().unwrap();
        let p = d
            .path()
            .join("rollout-2026-10-01T20-52-56-01a0e913-c976-74c2-b9a5-d94fbd9fc104.jsonl");
        let meta = json!({"type":"session_meta","payload":{"id":"01a0e913-c976-74c2-b9a5-d94fbd9fc104","cwd":"/Users/a/proj/","base_instructions":{"text":"x".repeat(30000)}}});
        let l2 = json!({"type":"response_item","payload":{"type":"message","role":"user","content":[
            {"type":"input_text","text":"# AGENTS.md instructions for /Users/a\n\n<INSTRUCTIONS>"},
            {"type":"input_text","text":"<environment_context>\n<cwd>/Users/a</cwd>"}]}});
        let l3 = json!({"type":"response_item","payload":{"type":"message","role":"user","content":[
            {"type":"input_text","text":"\n# Files mentioned by the user:\n\n## a.png: /tmp/a.png\n\n## My request:\n[$make-cover](/Users/a/.codex/skills/make-cover/SKILL.md) 做个封面\n"},
            {"type":"input_text","text":"<image name=[Image #1]>"}]}});
        fs::write(&p, format!("{meta}\n{l2}\n{l3}\n")).unwrap();
        let t = parse_codex_rollout(&p);
        assert_eq!(t.cwd.as_deref(), Some("/Users/a/proj"));
        assert_eq!(t.session_id, "01a0e913-c976-74c2-b9a5-d94fbd9fc104");
        assert_eq!(t.title.as_deref(), Some("$make-cover 做个封面"));
        assert!(t.parent_id.is_none());
    }

    #[test]
    fn codex_title_from_long_line_prefix() {
        let d = tempfile::tempdir().unwrap();
        let p = d
            .path()
            .join("rollout-2026-09-23T16-26-51-01a0cd5f-f0a0-7653-8d69-5368ad090697.jsonl");
        let meta = json!({"type":"session_meta","payload":{"id":"01a0cd5f-f0a0-7653-8d69-5368ad090697","cwd":"/a"}});
        let big = json!({"type":"response_item","payload":{"type":"message","role":"user","content":[
            {"type":"input_text","text":"# Files mentioned by the user:\n\n## x.png: /tmp/x.png\n\n## My request:\n把这些图做成视频"},
            {"type":"input_image","image_url": format!("data:image/png;base64,{}", "A".repeat(3_000_000))}]}});
        fs::write(&p, format!("{meta}\n{big}\n")).unwrap();
        let t = parse_codex_rollout(&p);
        assert_eq!(t.title.as_deref(), Some("把这些图做成视频"));
    }

    #[test]
    fn codex_subagent_name() {
        let (id, sub) = codex_ids_from_name(
            "rollout-2026-10-01T20-52-56-01a0e913-c976-74c2-b9a5-d94fbd9fc104_01a0f786-6bb1-7fd1-abff-7e2f6712bda0",
        );
        assert_eq!(id.as_deref(), Some("01a0e913-c976-74c2-b9a5-d94fbd9fc104"));
        assert_eq!(sub.as_deref(), Some("01a0f786-6bb1-7fd1-abff-7e2f6712bda0"));
    }

    #[test]
    fn codex_first_line_too_long_is_ignored() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("rollout-x.jsonl");
        let meta =
            json!({"type":"session_meta","payload":{"cwd":"/a","pad":"x".repeat(1_100_000)}});
        fs::write(&p, format!("{meta}\n")).unwrap();
        let t = parse_codex_rollout(&p);
        assert_eq!(t.cwd, None);
    }

    #[test]
    fn gemini_discovery() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        let proj = "/Users/a/gem";
        let h = gemini_hash(proj);
        let chats = home.join(".gemini/tmp").join(&h).join("chats");
        fs::create_dir_all(&chats).unwrap();
        fs::write(
            chats.join("session-2026-01-30T08-41-05d96d69.json"),
            json!({"sessionId":"g1","projectHash":h,"messages":[
                {"type":"info","content":"update"},
                {"type":"user","content":"Gemini 问题"}]})
            .to_string(),
        )
        .unwrap();
        let named = home.join(".gemini/tmp/named/chats");
        fs::create_dir_all(&named).unwrap();
        fs::write(
            home.join(".gemini/tmp/named/.project_root"),
            "/Users/a/named\n",
        )
        .unwrap();
        fs::write(
            named.join("session-1.json"),
            json!({"sessionId":"g2","messages":[{"type":"user","content":[{"text":"第二个"}]}]})
                .to_string(),
        )
        .unwrap();
        let mut ts = discover_gemini(home, &[proj.to_string()]);
        ts.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        assert_eq!(ts.len(), 2);
        assert_eq!(ts[0].cwd.as_deref(), Some(proj));
        assert_eq!(ts[0].title.as_deref(), Some("Gemini 问题"));
        assert_eq!(ts[1].cwd.as_deref(), Some("/Users/a/named"));
        assert_eq!(ts[1].title.as_deref(), Some("第二个"));
    }
}

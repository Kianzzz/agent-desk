//! 找出各数据源的日志文件（只 stat，不读内容）。

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use sha2::{Digest, Sha256};

use crate::Tool;

pub(crate) struct Found {
    pub tool: Tool,
    pub path: PathBuf,
    pub size: u64,
    pub mtime_ns: i64,
    /// Gemini：从所在目录推出来的项目路径
    pub gemini_project: Option<String>,
}

pub(crate) struct Source {
    pub tool: Tool,
    /// 主目录，用于展示
    pub root: PathBuf,
    pub errors: Vec<String>,
}

pub(crate) struct Discovery {
    pub files: Vec<Found>,
    pub sources: Vec<Source>,
    /// Gemini：sha256(项目路径) → 项目路径
    pub gemini_hashes: HashMap<String, String>,
}

fn stat(path: &Path) -> io::Result<(u64, i64)> {
    let md = fs::metadata(path)?;
    let mtime_ns = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    Ok((md.len(), mtime_ns))
}

fn read_dir_err(path: &Path, e: &io::Error) -> String {
    format!("无法读取目录 {}：{}", path.display(), e)
}

/// 递归找文件；不进入符号链接目录，避免循环。
fn walk(
    dir: &Path,
    depth: u32,
    accept: &dyn Fn(&str) -> bool,
    out: &mut Vec<PathBuf>,
    errors: &mut Vec<String>,
) {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return,
        Err(e) => {
            errors.push(read_dir_err(dir, &e));
            return;
        }
    };
    for ent in rd.flatten() {
        let Ok(ft) = ent.file_type() else { continue };
        let path = ent.path();
        if ft.is_dir() {
            if depth < 12 {
                walk(&path, depth + 1, accept, out, errors);
            }
            continue;
        }
        let name = ent.file_name();
        let Some(name) = name.to_str() else { continue };
        if !accept(name) {
            continue;
        }
        if ft.is_file() || (ft.is_symlink() && path.is_file()) {
            out.push(path);
        }
    }
}

fn push_found(tool: Tool, paths: Vec<PathBuf>, out: &mut Vec<Found>, errors: &mut Vec<String>) {
    for p in paths {
        match stat(&p) {
            Ok((size, mtime_ns)) => out.push(Found {
                tool,
                path: p,
                size,
                mtime_ns,
                gemini_project: None,
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => errors.push(format!("无法读取 {}：{}", p.display(), e)),
        }
    }
}

pub(crate) fn claude_roots(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".claude").join("projects"),
        home.join(".config").join("claude").join("projects"),
    ]
}

pub(crate) fn codex_roots(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".codex").join("sessions"),
        home.join(".codex").join("archived_sessions"),
    ]
}

pub(crate) fn gemini_root(home: &Path) -> PathBuf {
    home.join(".gemini").join("tmp")
}

fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// `~/.gemini/projects.json` 里登记过的项目路径，按 sha256 建索引。
fn gemini_hashes(home: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(text) = fs::read_to_string(home.join(".gemini").join("projects.json")) else {
        return map;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return map;
    };
    if let Some(projects) = v.get("projects").and_then(|p| p.as_object()) {
        for path in projects.keys() {
            map.insert(sha256_hex(path), path.clone());
        }
    }
    map
}

pub(crate) fn discover(home: &Path) -> Discovery {
    let mut files = Vec::new();
    let mut sources = Vec::new();

    // Claude
    {
        let mut errors = Vec::new();
        let mut paths = Vec::new();
        for root in claude_roots(home) {
            walk(
                &root,
                0,
                &|n| n.ends_with(".jsonl"),
                &mut paths,
                &mut errors,
            );
        }
        push_found(Tool::Claude, paths, &mut files, &mut errors);
        sources.push(Source {
            tool: Tool::Claude,
            root: claude_roots(home).remove(0),
            errors,
        });
    }

    // Codex
    {
        let mut errors = Vec::new();
        let mut paths = Vec::new();
        for root in codex_roots(home) {
            walk(
                &root,
                0,
                &|n| n.starts_with("rollout-") && n.ends_with(".jsonl"),
                &mut paths,
                &mut errors,
            );
        }
        push_found(Tool::Codex, paths, &mut files, &mut errors);
        sources.push(Source {
            tool: Tool::Codex,
            root: codex_roots(home).remove(0),
            errors,
        });
    }

    // Gemini
    let hashes = gemini_hashes(home);
    {
        let mut errors = Vec::new();
        let root = gemini_root(home);
        match fs::read_dir(&root) {
            Ok(rd) => {
                for ent in rd.flatten() {
                    let dir = ent.path();
                    let chats = dir.join("chats");
                    if !chats.is_dir() {
                        continue;
                    }
                    let name = ent.file_name().to_string_lossy().into_owned();
                    let project = fs::read_to_string(dir.join(".project_root"))
                        .ok()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .or_else(|| hashes.get(&name).cloned());
                    let mut paths = Vec::new();
                    walk(
                        &chats,
                        11,
                        &|n| {
                            n.starts_with("session-")
                                && (n.ends_with(".json") || n.ends_with(".jsonl"))
                        },
                        &mut paths,
                        &mut errors,
                    );
                    let start = files.len();
                    push_found(Tool::Gemini, paths, &mut files, &mut errors);
                    for f in &mut files[start..] {
                        f.gemini_project = project.clone();
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => errors.push(read_dir_err(&root, &e)),
        }
        sources.push(Source {
            tool: Tool::Gemini,
            root,
            errors,
        });
    }

    Discovery {
        files,
        sources,
        gemini_hashes: hashes,
    }
}

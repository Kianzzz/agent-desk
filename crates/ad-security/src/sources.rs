//! 找要扫的文件：技能目录、插件、项目目录、Codex 会话里记下的工作目录。

use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use crate::engine::Ctx;

/// 遍历时跳过的目录名。
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    ".trash",
    ".Trash",
    ".staging",
    "__pycache__",
    ".venv",
    "venv",
    "site-packages",
    ".mypy_cache",
    ".pytest_cache",
    ".next",
    "target",
];

fn skip_dir(name: &str) -> bool {
    SKIP_DIRS.contains(&name)
}

/// 一个技能根目录下的所有技能目录（含 SKILL.md 的目录）。技能可能嵌套在分组目录里
/// （比如 `skills/synced/<bucket>/<技能>`），往下最多找 `depth` 层。跟随符号链接。
pub(crate) fn skill_dirs(root: &Path, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    find_skill_dirs(root, depth, &mut out, &mut visited);
    out
}

fn find_skill_dirs(
    dir: &Path,
    depth: usize,
    out: &mut Vec<PathBuf>,
    visited: &mut HashSet<PathBuf>,
) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if skip_dir(name) || name == "marketplaces" || !p.is_dir() {
            continue;
        }
        let Ok(real) = fs::canonicalize(&p) else {
            continue;
        };
        if !visited.insert(real) {
            continue;
        }
        if p.join("SKILL.md").is_file() || p.join("skill.md").is_file() {
            out.push(p);
        } else if depth > 0 {
            find_skill_dirs(&p, depth - 1, out, visited);
        }
    }
}

/// 插件根目录（含 `.claude-plugin/plugin.json` 的目录）。
pub(crate) fn plugin_roots(root: &Path, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    find_plugin_roots(root, depth, &mut out);
    out
}

fn find_plugin_roots(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if dir.join(".claude-plugin").join("plugin.json").is_file() {
        out.push(dir.to_path_buf());
        return;
    }
    if depth == 0 {
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if skip_dir(name) || name == "marketplaces" || !p.is_dir() {
            continue;
        }
        find_plugin_roots(&p, depth - 1, out);
    }
}

/// 一个目录下的所有文件（递归、跟随符号链接、同一真实目录只进一次），跳过明显的
/// 二进制和依赖目录。
pub(crate) fn walk_files(dir: &Path, max_depth: usize, max_files: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    walk(dir, max_depth, max_files, &mut out, &mut visited);
    out
}

fn walk(
    dir: &Path,
    depth: usize,
    max: usize,
    out: &mut Vec<PathBuf>,
    visited: &mut HashSet<PathBuf>,
) {
    let Ok(real) = fs::canonicalize(dir) else {
        return;
    };
    if !visited.insert(real) {
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut entries: Vec<(PathBuf, fs::FileType)> = rd
        .flatten()
        .filter_map(|e| Some((e.path(), e.file_type().ok()?)))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    for (p, ft) in entries {
        if out.len() >= max {
            return;
        }
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        // 只有符号链接才需要再 stat 一次看它指向什么
        let (is_dir, is_file) = if ft.is_symlink() {
            match fs::metadata(&p) {
                Ok(m) => (m.is_dir(), m.is_file()),
                Err(_) => continue,
            }
        } else {
            (ft.is_dir(), ft.is_file())
        };
        if is_dir {
            if depth > 0 && !skip_dir(name) {
                walk(&p, depth - 1, max, out, visited);
            }
        } else if is_file && !skip_file(&p) {
            out.push(p);
        }
    }
}

/// 按扩展名就能判定不用读的文件。
pub(crate) fn skip_file(p: &Path) -> bool {
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if name == ".ds_store"
        || name == "package-lock.json"
        || name == "pnpm-lock.yaml"
        || name == "yarn.lock"
        || name == "cargo.lock"
        || name.ends_with(".min.js")
        || name.ends_with(".map")
    {
        return true;
    }
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    const BINARY: &[&str] = &[
        "png",
        "jpg",
        "jpeg",
        "gif",
        "webp",
        "bmp",
        "ico",
        "icns",
        "tif",
        "tiff",
        "heic",
        "svg",
        "pdf",
        "zip",
        "gz",
        "tgz",
        "bz2",
        "xz",
        "7z",
        "rar",
        "tar",
        "mp3",
        "mp4",
        "mov",
        "wav",
        "m4a",
        "aac",
        "flac",
        "ogg",
        "webm",
        "avi",
        "mkv",
        "ttf",
        "otf",
        "woff",
        "woff2",
        "eot",
        "pyc",
        "so",
        "dylib",
        "dll",
        "exe",
        "bin",
        "dat",
        "db",
        "sqlite",
        "sqlite3",
        "wasm",
        "jar",
        "class",
        "psd",
        "ai",
        "sketch",
        "fig",
        "key",
        "numbers",
        "pages",
        "docx",
        "xlsx",
        "pptx",
        "doc",
        "xls",
        "ppt",
        "lock",
        "node",
        "o",
        "a",
        "riv",
        "lottie",
        "glb",
        "gltf",
        "blend",
        "npy",
        "pt",
        "onnx",
        "safetensors",
        "parquet",
    ];
    BINARY.contains(&ext.as_str())
}

/// 文件属于哪种上下文：会执行的脚本、说明文字、还是数据配置。
pub(crate) fn file_ctx(p: &Path, text: &str) -> Ctx {
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if name == ".env" || name.starts_with(".env.") || name.ends_with(".env") {
        return Ctx::Data;
    }
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "md" | "markdown" | "mdx" | "txt" | "rst" | "adoc" | "html" | "htm" | "org" => Ctx::Doc,
        "sh" | "bash" | "zsh" | "fish" | "ksh" | "py" | "pyw" | "js" | "mjs" | "cjs" | "ts"
        | "mts" | "cts" | "jsx" | "tsx" | "rb" | "pl" | "php" | "ps1" | "psm1" | "bat" | "cmd"
        | "command" | "applescript" | "lua" | "swift" | "go" => Ctx::Exec,
        "json" | "jsonc" | "json5" | "yaml" | "yml" | "toml" | "ini" | "cfg" | "conf" | "xml"
        | "plist" | "csv" | "tsv" | "env" | "properties" => Ctx::Data,
        _ => {
            if text.starts_with("#!") {
                Ctx::Exec
            } else {
                Ctx::Doc
            }
        }
    }
}

pub(crate) fn is_markdown(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "md" | "markdown" | "mdx"
    )
}

/// `~/.codex/sessions/**/rollout-*.jsonl` 第一行 session_meta 里的 `payload.cwd`。只读第一行，
/// 最多 1MB。
pub(crate) fn codex_session_cwds(home: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_rollouts(&home.join(".codex").join("sessions"), 4, &mut files);
    let mut out = Vec::new();
    for f in files {
        let Ok(file) = fs::File::open(&f) else {
            continue;
        };
        let mut reader = BufReader::new(file.take(1 << 20));
        let mut line = Vec::new();
        if reader.read_until(b'\n', &mut line).is_err() {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("session_meta") {
            continue;
        }
        if let Some(cwd) = v.pointer("/payload/cwd").and_then(|c| c.as_str()) {
            out.push(PathBuf::from(cwd));
        }
    }
    out
}

fn collect_rollouts(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if depth > 0 {
                collect_rollouts(&p, depth - 1, out);
            }
        } else {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                out.push(p);
            }
        }
    }
}

/// 去重、去掉不存在的和 `$HOME` 本身。
pub(crate) fn dedupe_projects(home: &Path, dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    let home_real = fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for d in dirs {
        if !d.is_dir() {
            continue;
        }
        let Ok(real) = fs::canonicalize(&d) else {
            continue;
        };
        if real == home_real || real == Path::new("/") {
            continue;
        }
        if seen.insert(real.clone()) {
            out.push(real);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contexts() {
        assert_eq!(file_ctx(Path::new("a/SKILL.md"), ""), Ctx::Doc);
        assert_eq!(file_ctx(Path::new("a/run.sh"), ""), Ctx::Exec);
        assert_eq!(file_ctx(Path::new("a/tool"), "#!/bin/sh\n"), Ctx::Exec);
        assert_eq!(file_ctx(Path::new("a/.env"), ""), Ctx::Data);
        assert_eq!(file_ctx(Path::new("a/config.json"), ""), Ctx::Data);
        assert!(skip_file(Path::new("x/logo.png")));
    }
}

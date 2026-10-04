//! 找出各数据源的日志文件（只 stat，不读内容）。

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use sha2::{Digest, Sha256};

use crate::Tool;

/// 同一个工具可能有好几种存储格式，按这个分派给对应的解析函数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Claude,
    Codex,
    Gemini,
    /// Grok Build：`sessions/<cwd>/<id>/updates.jsonl`
    GrokUpdates,
    /// 社区版 grok-cli：`~/.grok/grok.db`
    GrokDevDb,
    /// OpenCode / Kilo CLI 的 SQLite
    OpencodeDb,
    /// OpenCode 旧版 `storage/message/**/*.json`（一个目录一条记录，路径指向会话目录）
    OpencodeJson,
    Qwen,
    CopilotDb,
    CopilotEvents,
    /// Cline / Roo Code / Kilo Code 扩展：`tasks/<id>/ui_messages.json`
    ClineTask,
    /// Cline CLI：`<id>.messages.json`
    ClineCli,
    KimiWire,
    KimiCode,
    Droid,
    Amp,
    Pi,
    OpenclawDb,
    Codebuddy,
    Crush,
    Goose,
}

pub(crate) struct Found {
    pub tool: Tool,
    pub kind: Kind,
    pub path: PathBuf,
    /// 用于判断要不要重读：SQLite 是主库加 `-wal` 的大小之和
    pub size: u64,
    /// 同上：多个文件时取最晚的修改时间
    pub mtime_ns: i64,
    /// 从所在目录推出来的项目路径（Gemini、Qwen 等）
    pub project: Option<String>,
}

pub(crate) struct Source {
    pub tool: Tool,
    /// 主目录，用于展示
    pub root: PathBuf,
    pub errors: Vec<String>,
}

impl Source {
    pub fn new(tool: Tool, root: PathBuf) -> Self {
        Source {
            tool,
            root,
            errors: Vec::new(),
        }
    }
}

pub(crate) struct Discovery {
    pub files: Vec<Found>,
    pub sources: Vec<Source>,
    /// Gemini：sha256(项目路径) → 项目路径
    pub gemini_hashes: HashMap<String, String>,
}

/// 主目录和环境变量。环境变量只在 `home` 就是当前用户的 `$HOME` 时生效：
/// 指向别的目录（比如测试用的临时目录）时，不让本机的 `GROK_HOME` 之类把扫描带到别处。
pub(crate) struct Ctx {
    pub home: PathBuf,
    vars: HashMap<String, String>,
}

impl Ctx {
    pub fn from_process(home: &Path) -> Ctx {
        let real_home = std::env::var_os("HOME").map(PathBuf::from);
        let vars = if real_home.as_deref() == Some(home) {
            std::env::vars().collect()
        } else {
            HashMap::new()
        };
        Ctx {
            home: home.to_path_buf(),
            vars,
        }
    }

    #[cfg(test)]
    pub fn with_vars(home: &Path, vars: &[(&str, &str)]) -> Ctx {
        Ctx {
            home: home.to_path_buf(),
            vars: vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    /// 非空的环境变量
    pub fn var(&self, name: &str) -> Option<&str> {
        self.vars
            .get(name)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
    }

    /// 环境变量里的路径；`~/` 开头的展开到主目录
    pub fn path_var(&self, name: &str) -> Option<PathBuf> {
        let v = self.var(name)?;
        Some(match v.strip_prefix("~/") {
            Some(rest) => self.home.join(rest),
            None if v == "~" => self.home.clone(),
            None => PathBuf::from(v),
        })
    }

    /// `$XDG_DATA_HOME`，默认 `~/.local/share`
    pub fn xdg_data(&self) -> PathBuf {
        self.path_var("XDG_DATA_HOME")
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| self.home.join(".local").join("share"))
    }

    /// `~/Library/Application Support`
    pub fn app_support(&self) -> PathBuf {
        self.home.join("Library").join("Application Support")
    }
}

pub(crate) fn stat(path: &Path) -> io::Result<(u64, i64)> {
    let md = fs::metadata(path)?;
    let mtime_ns = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    Ok((md.len(), mtime_ns))
}

/// 几个一起读的文件合成一个指纹：大小相加、修改时间取最晚的。第一个必须存在，其余可以没有。
pub(crate) fn stat_multi(main: &Path, extra: &[PathBuf]) -> io::Result<(u64, i64)> {
    let (mut size, mut mtime) = stat(main)?;
    for p in extra {
        if let Ok((s, m)) = stat(p) {
            size += s;
            mtime = mtime.max(m);
        }
    }
    Ok((size, mtime))
}

/// SQLite 主库加上 `-wal`：新写入的数据可能还只在 WAL 里，主库没变。
pub(crate) fn stat_db(path: &Path) -> io::Result<(u64, i64)> {
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    stat_multi(path, &[PathBuf::from(wal)])
}

pub(crate) fn read_dir_err(path: &Path, e: &io::Error) -> String {
    format!("无法读取目录 {}：{}", path.display(), e)
}

/// 递归找文件；不进入符号链接目录，避免循环。
pub(crate) fn walk(
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

pub(crate) fn push_found(
    tool: Tool,
    kind: Kind,
    paths: Vec<PathBuf>,
    out: &mut Vec<Found>,
    errors: &mut Vec<String>,
) {
    for p in paths {
        push_with(tool, kind, p, None, stat, out, errors);
    }
}

/// 用给定的 stat 函数（单文件、SQLite、多文件）登记一个数据文件；不存在就跳过。
pub(crate) fn push_with(
    tool: Tool,
    kind: Kind,
    path: PathBuf,
    project: Option<String>,
    stat_fn: impl Fn(&Path) -> io::Result<(u64, i64)>,
    out: &mut Vec<Found>,
    errors: &mut Vec<String>,
) {
    match stat_fn(&path) {
        Ok((size, mtime_ns)) => out.push(Found {
            tool,
            kind,
            path,
            size,
            mtime_ns,
            project,
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => errors.push(format!("无法读取 {}：{}", path.display(), e)),
    }
}

/// 列出目录下的子目录；目录不存在不算错误。
pub(crate) fn subdirs(dir: &Path, errors: &mut Vec<String>) -> Vec<PathBuf> {
    match fs::read_dir(dir) {
        Ok(rd) => {
            let mut v: Vec<PathBuf> = rd
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path())
                .collect();
            v.sort();
            v
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) if e.kind() == io::ErrorKind::NotADirectory => Vec::new(),
        Err(e) => {
            errors.push(read_dir_err(dir, &e));
            Vec::new()
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

pub(crate) fn discover(ctx: &Ctx) -> Discovery {
    let home = ctx.home.as_path();
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
        push_found(Tool::Claude, Kind::Claude, paths, &mut files, &mut errors);
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
        push_found(Tool::Codex, Kind::Codex, paths, &mut files, &mut errors);
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
                    push_found(Tool::Gemini, Kind::Gemini, paths, &mut files, &mut errors);
                    for f in &mut files[start..] {
                        f.project = project.clone();
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

    for f in crate::tools::DISCOVERERS {
        sources.push(f(ctx, &mut files));
    }

    Discovery {
        files,
        sources,
        gemini_hashes: hashes,
    }
}

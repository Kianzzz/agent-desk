//! 「按项目」：把对话记录按项目目录（cwd）归类，统计项目文件夹和里面可清理的产物。

use crate::locations::scratch_display_name;
use crate::transcripts::Transcript;
use crate::util::{path_id, path_string, rfc3339, DAY, IN_USE_SECS};
use crate::walk::{dir_stats, Stats};
use crate::{DiskItem, ProjectUsage, Safety};
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const R_RESUME: &str = "删除后这段对话无法再继续（resume），用量统计不受影响";

pub(crate) struct ProjectEnv<'a> {
    pub home: &'a Path,
    pub now: i64,
    pub include_folders: bool,
    pub claude_tmp: Option<&'a Path>,
}

/// 构建产物的种类
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArtifactKind {
    NodeModules,
    RustTarget,
    JsBuild,
    FrameworkCache,
    PyVenv,
    PyCache,
    PytestCache,
    Gradle,
}

impl ArtifactKind {
    fn label(self, name: &str) -> String {
        match self {
            ArtifactKind::NodeModules if name == "node_modules" => "node_modules 依赖".into(),
            ArtifactKind::NodeModules => format!("node_modules 副本 {name}"),
            ArtifactKind::RustTarget => "Rust 编译产物 target".into(),
            ArtifactKind::JsBuild => format!("前端构建产物 {name}"),
            ArtifactKind::FrameworkCache => format!("框架构建缓存 {name}"),
            ArtifactKind::PyVenv => format!("Python 虚拟环境 {name}"),
            ArtifactKind::PyCache => "Python 缓存 __pycache__".into(),
            ArtifactKind::PytestCache => "pytest 缓存".into(),
            ArtifactKind::Gradle => "Gradle 缓存".into(),
        }
    }
    fn rebuild(self) -> &'static str {
        match self {
            ArtifactKind::NodeModules => "可以用 npm install 重新生成",
            ArtifactKind::RustTarget => "可以用 cargo build 重新生成",
            ArtifactKind::JsBuild | ArtifactKind::FrameworkCache => "可以重新构建生成",
            ArtifactKind::PyVenv => "可以重新创建虚拟环境并安装依赖",
            ArtifactKind::PyCache | ArtifactKind::PytestCache | ArtifactKind::Gradle => {
                "运行时会自动重新生成"
            }
        }
    }
}

/// 判断目录 `dir`（名字 `name`，所在目录里的文件名集合 `siblings`）是不是构建产物
pub(crate) fn artifact_kind(
    dir: &Path,
    name: &str,
    sibling_has: impl Fn(&str) -> bool,
) -> Option<ArtifactKind> {
    match name {
        // 也包括改了名的副本，如 node_modules.bak、xxx-node_modules-backup-20260910
        n if n.contains("node_modules") => Some(ArtifactKind::NodeModules),
        "target" if sibling_has("Cargo.toml") => Some(ArtifactKind::RustTarget),
        "dist" | "build" if sibling_has("package.json") => Some(ArtifactKind::JsBuild),
        ".next" | ".nuxt" | ".turbo" => Some(ArtifactKind::FrameworkCache),
        ".venv" | "venv" if dir.join("pyvenv.cfg").is_file() => Some(ArtifactKind::PyVenv),
        "__pycache__" => Some(ArtifactKind::PyCache),
        ".pytest_cache" => Some(ArtifactKind::PytestCache),
        ".gradle" => Some(ArtifactKind::Gradle),
        _ => None,
    }
}

#[derive(Debug, Default)]
pub(crate) struct FolderScan {
    pub stats: Stats,
    /// 不含产物的最新修改时间
    pub newest_own: i64,
    pub artifacts: Vec<(PathBuf, ArtifactKind, Stats)>,
    pub worktrees: Vec<(PathBuf, Stats)>,
}

impl FolderScan {
    fn merge(&mut self, o: FolderScan) {
        self.stats.add(&o.stats);
        self.newest_own = self.newest_own.max(o.newest_own);
        self.artifacts.extend(o.artifacts);
        self.worktrees.extend(o.worktrees);
    }
}

/// 遍历项目文件夹：并行、不跟随链接；遇到产物和 worktree 只算大小不再往里走；
/// `exclude` 里的目录（其他项目）整个跳过。
pub(crate) fn scan_folder(root: &Path, exclude: &HashSet<PathBuf>) -> FolderScan {
    scan_dir(root, root, exclude)
}

fn scan_dir(root: &Path, dir: &Path, exclude: &HashSet<PathBuf>) -> FolderScan {
    use std::os::unix::fs::MetadataExt;
    let mut out = FolderScan::default();
    if let Ok(m) = fs::symlink_metadata(dir) {
        out.stats.bytes += m.blocks() * 512;
        let t = m.modified().map(crate::util::system_time_secs).unwrap_or(0);
        out.stats.newest = t;
        out.newest_own = t;
    }
    let entries: Vec<fs::DirEntry> = match fs::read_dir(dir) {
        Ok(rd) => rd.flatten().collect(),
        Err(_) => return out,
    };
    let names: HashSet<String> = entries
        .iter()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let at_root = dir == root;
    let subs: Vec<FolderScan> = entries
        .par_iter()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            let mut s = FolderScan::default();
            if !m.is_dir() {
                s.stats = Stats {
                    bytes: m.blocks() * 512,
                    files: 1,
                    newest: m.modified().map(crate::util::system_time_secs).unwrap_or(0),
                };
                s.newest_own = s.stats.newest;
                return Some(s);
            }
            if exclude.contains(&p) {
                return None;
            }
            if at_root && name == ".worktrees" {
                s.merge(worktrees_in(&p));
                return Some(s);
            }
            if at_root && name == ".claude" {
                // .claude/worktrees/* 是 worktree，其余照常遍历
                let wt = p.join("worktrees");
                let mut ex = exclude.clone();
                if wt.is_dir() {
                    s.merge(worktrees_in(&wt));
                    ex.insert(wt);
                }
                s.merge(scan_dir(root, &p, &ex));
                return Some(s);
            }
            if let Some(kind) = artifact_kind(&p, &name, |n| names.contains(n)) {
                let st = dir_stats(&p).unwrap_or_default();
                s.stats = st;
                s.artifacts.push((p, kind, st));
                return Some(s);
            }
            Some(scan_dir(root, &p, exclude))
        })
        .collect();
    for s in subs {
        out.merge(s);
    }
    out
}

fn worktrees_in(dir: &Path) -> FolderScan {
    let mut out = FolderScan::default();
    let Ok(rd) = fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let p = e.path();
        let st = dir_stats(&p).unwrap_or_default();
        out.stats.add(&st);
        out.worktrees.push((p, st));
    }
    out
}

/// worktree 的未提交改动：Some(true) 干净，Some(false) 有改动，None 无法确认
pub(crate) fn worktree_clean(path: &Path) -> Option<bool> {
    let git = if Path::new("/usr/bin/git").exists() {
        "/usr/bin/git"
    } else {
        "git"
    };
    let mut child = Command::new(git)
        .arg("-C")
        .arg(path)
        .args(["status", "--porcelain"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut out = String::new();
                use std::io::Read;
                child.stdout.take()?.read_to_string(&mut out).ok()?;
                return Some(out.trim().is_empty());
            }
            Ok(None) => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    }
}

/// 不遍历的项目目录：主目录本身、根目录、主目录的上级、~/Library 下面
pub(crate) fn folder_walkable(home: &Path, cwd: &Path) -> bool {
    if cwd == Path::new("/") || home.starts_with(cwd) {
        return false;
    }
    !cwd.starts_with(home.join("Library"))
}

pub(crate) fn is_scratch_workspace(home: &Path, cwd: &Path) -> bool {
    cwd.starts_with(home.join("Library/Application Support/Claude/scratch-workspaces"))
}

fn transcript_item(t: &Transcript, now: i64, project_exists: Option<bool>) -> DiskItem {
    let age_days = if t.stats.newest > 0 {
        (now - t.stats.newest).max(0) / DAY
    } else {
        0
    };
    let in_use = t.stats.newest > 0 && now - t.stats.newest < IN_USE_SECS;
    let (safety, reason) = if in_use {
        (
            Safety::Protected,
            format!("这段对话正在进行中（30 分钟内还在写入），不能删除。{R_RESUME}"),
        )
    } else if age_days < 14 {
        (
            Safety::Keep,
            format!("最近 14 天内的对话，可能还会继续。{R_RESUME}"),
        )
    } else if project_exists == Some(false) && age_days >= 30 {
        (
            Safety::Safe,
            format!("项目文件夹已经不在了，而且 30 天以上没动过。{R_RESUME}"),
        )
    } else {
        (
            Safety::Review,
            format!("{age_days} 天前的对话，确认不再需要再删。{R_RESUME}"),
        )
    };
    let (label, category) = match t.tool {
        "claude" => ("Claude Code 对话", "claude_transcripts"),
        "codex" if t.subagent => ("Codex 子任务对话", "codex_sessions"),
        "codex" if t.parent_id.is_some() => ("Codex 分支对话", "codex_sessions"),
        "codex" => ("Codex 对话", "codex_sessions"),
        _ => ("Gemini 对话", "gemini_chats"),
    };
    let path_s = path_string(&t.path);
    DiskItem {
        id: path_id(&path_s),
        path: path_s,
        label: label.to_string(),
        category: category.to_string(),
        tool: t.tool.to_string(),
        project: t.cwd.clone(),
        size_bytes: t.stats.bytes,
        file_count: t.stats.files,
        modified: rfc3339(t.stats.newest),
        safety,
        reason,
        in_use,
        title: t.title.clone(),
        children: Vec::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn simple_item(
    path: &Path,
    label: String,
    category: &str,
    tool: &str,
    project: &str,
    stats: Stats,
    safety: Safety,
    reason: String,
    now: i64,
) -> DiskItem {
    let in_use = stats.newest > 0 && now - stats.newest < IN_USE_SECS;
    let path_s = path_string(path);
    DiskItem {
        id: path_id(&path_s),
        path: path_s,
        label,
        category: category.to_string(),
        tool: tool.to_string(),
        project: Some(project.to_string()),
        size_bytes: stats.bytes,
        file_count: stats.files,
        modified: rfc3339(stats.newest),
        safety,
        reason,
        in_use,
        title: None,
        children: Vec::new(),
    }
}

struct Group<'a> {
    cwd: Option<String>,
    /// Gemini 认不出项目时用的占位键
    key: String,
    transcripts: Vec<&'a Transcript>,
}

pub(crate) fn build_projects(
    penv: &ProjectEnv,
    transcripts: &[Transcript],
    scratch_items: &HashMap<String, DiskItem>,
) -> Vec<ProjectUsage> {
    // 分组
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    for t in transcripts {
        let key = match &t.cwd {
            Some(c) => c.clone(),
            None => format!("\u{0}gemini-unknown:{}", gemini_dir_of(t)),
        };
        groups
            .entry(key.clone())
            .or_insert_with(|| Group {
                cwd: t.cwd.clone(),
                key,
                transcripts: Vec::new(),
            })
            .transcripts
            .push(t);
    }

    let home = penv.home;
    // 会被遍历的项目目录（用来在嵌套时互相排除）
    let walk_set: HashSet<PathBuf> = groups
        .values()
        .filter_map(|g| g.cwd.as_ref())
        .map(PathBuf::from)
        .filter(|p| penv.include_folders && p.is_dir() && folder_walkable(home, p))
        .collect();

    // AI 数据里能对应到会话的部分：Claude 临时目录、Codex 生成的图片
    let mut session_extras: HashMap<String, Vec<(PathBuf, &'static str)>> = HashMap::new();
    if let Some(tmp) = penv.claude_tmp {
        if let Ok(rd) = fs::read_dir(tmp) {
            for enc in rd.flatten() {
                if !enc.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                if let Ok(inner) = fs::read_dir(enc.path()) {
                    for s in inner.flatten() {
                        if s.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                            let id = s.file_name().to_string_lossy().into_owned();
                            session_extras
                                .entry(id)
                                .or_default()
                                .push((s.path(), "temp"));
                        }
                    }
                }
            }
        }
    }
    if let Ok(rd) = fs::read_dir(home.join(".codex/generated_images")) {
        for e in rd.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let id = e.file_name().to_string_lossy().into_owned();
                session_extras
                    .entry(id)
                    .or_default()
                    .push((e.path(), "codex_images"));
            }
        }
    }

    // 每份 AI 数据只归给一段对话：会话 id 相同的优先，其次是以它为主对话的分支
    let by_id: HashMap<&str, &Transcript> = transcripts
        .iter()
        .map(|t| (t.session_id.as_str(), t))
        .collect();
    let mut extras_by_transcript: HashMap<PathBuf, Vec<(PathBuf, &'static str)>> = HashMap::new();
    for (id, extras) in session_extras {
        let owner = by_id.get(id.as_str()).copied().or_else(|| {
            transcripts
                .iter()
                .find(|t| t.parent_id.as_deref() == Some(id.as_str()))
        });
        if let Some(t) = owner {
            extras_by_transcript
                .entry(t.path.clone())
                .or_default()
                .extend(extras);
        }
    }

    let groups: Vec<Group> = groups.into_values().collect();
    let mut out: Vec<ProjectUsage> = groups
        .par_iter()
        .map(|g| build_one(penv, g, &walk_set, &extras_by_transcript, scratch_items))
        .collect();

    disambiguate_names(&mut out);
    out.sort_by(|a, b| {
        b.total_bytes
            .cmp(&a.total_bytes)
            .then(a.project.cmp(&b.project))
    });
    out
}

fn gemini_dir_of(t: &Transcript) -> String {
    t.path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn build_one(
    penv: &ProjectEnv,
    g: &Group,
    walk_set: &HashSet<PathBuf>,
    extras_by_transcript: &HashMap<PathBuf, Vec<(PathBuf, &'static str)>>,
    scratch_items: &HashMap<String, DiskItem>,
) -> ProjectUsage {
    let home = penv.home;
    let now = penv.now;
    let (project, exists_opt, display_name) = match &g.cwd {
        Some(c) => {
            let p = Path::new(c);
            let exists = p.is_dir();
            let name = if p == home {
                "主目录".to_string()
            } else if is_scratch_workspace(home, p) {
                scratch_display_name(c)
            } else {
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| c.clone())
            };
            (c.clone(), Some(exists), name)
        }
        None => {
            let dir = g.key.rsplit(':').next().unwrap_or("").to_string();
            let short: String = dir.chars().take(8).collect();
            (
                home.join(".gemini/tmp")
                    .join(&dir)
                    .to_string_lossy()
                    .into_owned(),
                None,
                format!("Gemini 未识别的项目 {short}"),
            )
        }
    };
    let exists = exists_opt.unwrap_or(false);

    let mut transcripts: Vec<DiskItem> = g
        .transcripts
        .iter()
        .map(|t| transcript_item(t, now, exists_opt))
        .collect();
    transcripts.sort_by(|a, b| b.modified.cmp(&a.modified));
    let transcripts_bytes: u64 = transcripts.iter().map(|t| t.size_bytes).sum();
    let last_active_secs = g
        .transcripts
        .iter()
        .map(|t| t.stats.newest)
        .max()
        .unwrap_or(0);
    let mut tools: Vec<String> = g.transcripts.iter().map(|t| t.tool.to_string()).collect();
    tools.sort();
    tools.dedup();

    let cwd_path = g.cwd.as_ref().map(PathBuf::from);
    let mut artifacts: Vec<DiskItem> = Vec::new();
    let mut folder_bytes: Option<u64> = None;
    let mut activity = last_active_secs;

    if let Some(cp) = &cwd_path {
        if exists && is_scratch_workspace(home, cp) {
            // 临时工作区：算它自己的大小，本身也列为可清理项
            if let Some(item) = scratch_items.get(&project) {
                folder_bytes = Some(item.size_bytes);
                let mut it = item.clone();
                it.children.clear();
                artifacts.push(it);
            } else if let Some(st) = dir_stats(cp) {
                folder_bytes = Some(st.bytes);
            }
        } else if exists && walk_set.contains(cp) {
            let exclude: HashSet<PathBuf> = walk_set
                .iter()
                .filter(|p| *p != cp && p.starts_with(cp))
                .cloned()
                .collect();
            let scan = scan_folder(cp, &exclude);
            folder_bytes = Some(scan.stats.bytes);
            activity = activity.max(scan.newest_own);
            let inactive = activity > 0 && (now - activity) >= 30 * DAY;
            artifacts.extend(artifact_items(&project, &scan, inactive, now));
            let wts: Vec<DiskItem> = scan
                .worktrees
                .par_iter()
                .map(|(p, st)| worktree_item(&project, p, *st, now))
                .collect();
            artifacts.extend(wts);
        }
    }

    // 能对应到这个项目里各个会话的 AI 数据（临时文件、生成的图片），不在项目文件夹里
    for t in &g.transcripts {
        if let Some(extras) = extras_by_transcript.get(&t.path) {
            for (p, kind) in extras {
                let Some(st) = dir_stats(p) else { continue };
                if st.bytes == 0 {
                    continue;
                }
                let title = t
                    .title
                    .clone()
                    .unwrap_or_else(|| t.session_id.chars().take(8).collect());
                let item = match *kind {
                    "temp" => {
                        let age = (now - st.newest).max(0) / DAY;
                        let in_use = now - st.newest < IN_USE_SECS;
                        let (safety, reason) = if in_use {
                            (Safety::Keep, "这段对话正在使用这些临时文件".to_string())
                        } else if age >= 3 {
                            (
                                Safety::Safe,
                                "Claude Code 对话时产生的临时文件，已经 3 天以上没动，对话结束后一般就不需要了".to_string(),
                            )
                        } else {
                            (
                                Safety::Review,
                                "最近对话产生的临时文件，如果对话已经结束一般可以删".to_string(),
                            )
                        };
                        simple_item(
                            p,
                            format!("对话临时文件 · {title}"),
                            "temp",
                            "claude",
                            &project,
                            st,
                            safety,
                            reason,
                            now,
                        )
                    }
                    _ => simple_item(
                        p,
                        format!("生成的图片 · {title}"),
                        "codex_images",
                        "codex",
                        &project,
                        st,
                        Safety::Review,
                        "这段对话里 AI 生成的图片，确认没用再删".to_string(),
                        now,
                    ),
                };
                artifacts.push(item);
            }
        }
    }
    artifacts.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));

    // 合计：对话记录 + 项目文件夹 + 不在项目文件夹里的可清理项
    let outside: u64 = artifacts
        .iter()
        .filter(|a| match &cwd_path {
            Some(cp) if folder_bytes.is_some() => !Path::new(&a.path).starts_with(cp),
            _ => true,
        })
        .map(|a| a.size_bytes)
        .sum();
    let total_bytes = transcripts_bytes + folder_bytes.unwrap_or(0) + outside;

    ProjectUsage {
        project,
        display_name,
        exists,
        last_active: rfc3339(last_active_secs),
        sessions: transcripts.len() as u32,
        tools,
        transcripts_bytes,
        folder_bytes,
        artifacts,
        transcripts,
        total_bytes,
    }
}

fn artifact_items(project: &str, scan: &FolderScan, inactive: bool, now: i64) -> Vec<DiskItem> {
    let mut out = Vec::new();
    let mut pycache: Vec<DiskItem> = Vec::new();
    let mut pystats = Stats::default();
    for (p, kind, st) in &scan.artifacts {
        // 太小的不值得单独列（__pycache__ 例外，会汇总）
        if *kind != ArtifactKind::PyCache && st.bytes < crate::locations::LIST_MIN {
            continue;
        }
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (safety, reason) = if inactive {
            (
                Safety::Safe,
                format!("项目 30 天以上没有活动，{}", kind.rebuild()),
            )
        } else {
            (
                Safety::Review,
                format!(
                    "项目最近还在用，删了下次运行要重新{}",
                    if matches!(kind, ArtifactKind::NodeModules | ArtifactKind::PyVenv) {
                        "安装"
                    } else {
                        "生成"
                    }
                ),
            )
        };
        let mut label = kind.label(&name);
        if let Some(rel) = p.parent().and_then(|par| par.strip_prefix(project).ok()) {
            let rel = rel.to_string_lossy();
            if !rel.is_empty() {
                label = format!("{label}（{rel}）");
            }
        }
        let item = simple_item(
            p,
            label,
            "build_artifact",
            "other",
            project,
            *st,
            safety,
            reason,
            now,
        );
        if *kind == ArtifactKind::PyCache {
            pystats.add(st);
            pycache.push(item);
        } else {
            out.push(item);
        }
    }
    if !pycache.is_empty() {
        // __pycache__ 汇总成一项；路径是项目目录本身（不能整体删除），具体路径在 children 里
        let safety = pycache[0].safety;
        let reason = format!(
            "{} 个 __pycache__ 目录的合计，{}；请展开后删除",
            pycache.len(),
            if inactive {
                "项目 30 天以上没有活动，运行时会自动重新生成"
            } else {
                "项目最近还在用，删了运行时会自动重新生成"
            }
        );
        pycache.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
        let mut agg = simple_item(
            Path::new(project),
            format!("Python 缓存 __pycache__（{} 个）", pycache.len()),
            "build_artifact",
            "other",
            project,
            pystats,
            safety,
            reason,
            now,
        );
        agg.id = path_id(&format!("{project}#__pycache__"));
        agg.children = pycache;
        out.push(agg);
    }
    out
}

fn worktree_item(project: &str, p: &Path, st: Stats, now: i64) -> DiskItem {
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (safety, reason) = match worktree_clean(p) {
        Some(true) => (
            Safety::Review,
            "git worktree，没有未提交的改动；删除后可在主仓库运行 git worktree prune 清理记录"
                .to_string(),
        ),
        Some(false) => (
            Safety::Keep,
            "git worktree，有未提交的改动，删除会丢失".to_string(),
        ),
        None => (
            Safety::Review,
            "git worktree，无法确认有没有未提交的改动，删前请先检查".to_string(),
        ),
    };
    simple_item(
        p,
        format!("worktree {name}"),
        "worktree",
        "other",
        project,
        st,
        safety,
        reason,
        now,
    )
}

fn disambiguate_names(list: &mut [ProjectUsage]) {
    let mut count: HashMap<String, usize> = HashMap::new();
    for p in list.iter() {
        *count.entry(p.display_name.clone()).or_default() += 1;
    }
    for p in list.iter_mut() {
        if count.get(&p.display_name).copied().unwrap_or(0) > 1 {
            if p.display_name.starts_with("临时工作区") {
                // scratch-2026-09-30-430995 → 用末尾的编号区分
                let name = Path::new(&p.project)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if let Some(tag) = name.rsplit('-').next() {
                    p.display_name = format!("{} #{tag}", p.display_name);
                    continue;
                }
            }
            let parent = Path::new(&p.project)
                .parent()
                .and_then(|x| x.file_name())
                .map(|n| n.to_string_lossy().into_owned());
            if let Some(parent) = parent {
                p.display_name = format!("{}（{parent}）", p.display_name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn artifact_detection_needs_evidence() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("proj");
        fs::create_dir_all(root.join("node_modules/a/node_modules/b")).unwrap();
        fs::write(root.join("node_modules/a/index.js"), "x").unwrap();
        // target 旁边没有 Cargo.toml：不是产物
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("target/x"), "x").unwrap();
        // rust 子项目
        fs::create_dir_all(root.join("crate1/target/debug")).unwrap();
        fs::write(root.join("crate1/Cargo.toml"), "[package]").unwrap();
        fs::write(root.join("crate1/target/debug/bin"), vec![0u8; 5000]).unwrap();
        // dist 需要 package.json
        fs::create_dir_all(root.join("web/dist")).unwrap();
        fs::write(root.join("web/package.json"), "{}").unwrap();
        fs::create_dir_all(root.join("docs/dist")).unwrap();
        // venv 需要 pyvenv.cfg
        fs::create_dir_all(root.join(".venv")).unwrap();
        fs::write(root.join(".venv/pyvenv.cfg"), "home=/x").unwrap();
        fs::create_dir_all(root.join("venv")).unwrap();
        // __pycache__
        fs::create_dir_all(root.join("pkg/__pycache__")).unwrap();
        fs::create_dir_all(root.join("pkg/sub/__pycache__")).unwrap();
        // worktree
        fs::create_dir_all(root.join(".claude/worktrees/wt1")).unwrap();
        fs::write(root.join(".claude/settings.json"), "{}").unwrap();
        // 嵌套的其他项目
        fs::create_dir_all(root.join("other/node_modules")).unwrap();
        // 改了名的 node_modules 备份：整体算一项，不再往里找 dist
        fs::create_dir_all(root.join(".x-node_modules-backup/pkg/dist")).unwrap();
        fs::write(root.join(".x-node_modules-backup/pkg/package.json"), "{}").unwrap();

        let mut ex = HashSet::new();
        ex.insert(root.join("other"));
        let scan = scan_folder(&root, &ex);
        let mut kinds: Vec<(String, ArtifactKind)> = scan
            .artifacts
            .iter()
            .map(|(p, k, _)| {
                (
                    p.strip_prefix(&root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    *k,
                )
            })
            .collect();
        kinds.sort_by(|a, b| a.0.cmp(&b.0));
        let paths: Vec<&str> = kinds.iter().map(|k| k.0.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                ".venv",
                ".x-node_modules-backup",
                "crate1/target",
                "node_modules",
                "pkg/__pycache__",
                "pkg/sub/__pycache__",
                "web/dist"
            ]
        );
        assert_eq!(scan.worktrees.len(), 1);
        assert!(scan.worktrees[0].0.ends_with(".claude/worktrees/wt1"));
        // 嵌套 node_modules 不单独列出
        assert!(!paths.iter().any(|p| p.contains("a/node_modules")));
        assert!(scan.stats.bytes > 0);

        // 小于 1MB 的产物不单独列出：这里只有 crate1/target 不够大，先把它撑大
        let mut scan = scan;
        for a in scan.artifacts.iter_mut() {
            a.2.bytes = 2 * crate::locations::LIST_MIN;
        }
        let items = artifact_items(&path_string(&root), &scan, true, crate::util::now_secs());
        let py = items
            .iter()
            .find(|i| i.label.contains("__pycache__"))
            .unwrap();
        assert_eq!(py.children.len(), 2);
        assert_eq!(py.path, path_string(&root));
        assert!(items.iter().all(|i| i.safety == Safety::Safe));
        let nm = items
            .iter()
            .find(|i| i.label.contains("node_modules"))
            .unwrap();
        assert!(nm.reason.contains("npm install"));
        let items = artifact_items(&path_string(&root), &scan, false, crate::util::now_secs());
        assert!(items.iter().all(|i| i.safety == Safety::Review));
    }

    #[test]
    fn walkable_rules() {
        let home = Path::new("/Users/a");
        assert!(!folder_walkable(home, Path::new("/Users/a")));
        assert!(!folder_walkable(home, Path::new("/")));
        assert!(!folder_walkable(home, Path::new("/Users")));
        assert!(!folder_walkable(
            home,
            Path::new("/Users/a/Library/Mobile Documents/x")
        ));
        assert!(folder_walkable(home, Path::new("/Users/a/code/x")));
    }

    fn set_mtime(p: &Path, secs: i64) {
        let t = std::time::UNIX_EPOCH + Duration::from_secs(secs as u64);
        let f = fs::File::open(p).unwrap();
        f.set_times(fs::FileTimes::new().set_modified(t).set_accessed(t))
            .unwrap();
    }

    fn tr(tool: &'static str, cwd: Option<&str>, newest: i64) -> Transcript {
        Transcript {
            tool,
            path: PathBuf::from(format!("/t/{tool}-{newest}.jsonl")),
            session_id: format!("s{newest}"),
            cwd: cwd.map(|s| s.to_string()),
            title: Some("标题".into()),
            stats: Stats {
                bytes: 100,
                files: 1,
                newest,
            },
            parent_id: None,
            subagent: false,
        }
    }

    #[test]
    fn transcript_safety() {
        let now = 1_000 * DAY;
        let t = tr("claude", Some("/gone"), now - 60);
        assert_eq!(
            transcript_item(&t, now, Some(false)).safety,
            Safety::Protected
        );
        let t = tr("claude", Some("/gone"), now - 3 * DAY);
        assert_eq!(transcript_item(&t, now, Some(false)).safety, Safety::Keep);
        let t = tr("claude", Some("/gone"), now - 40 * DAY);
        let it = transcript_item(&t, now, Some(false));
        assert_eq!(it.safety, Safety::Safe);
        assert!(it.reason.contains("项目文件夹已经不在了"));
        assert!(it.reason.contains("resume"));
        assert_eq!(transcript_item(&t, now, Some(true)).safety, Safety::Review);
        assert_eq!(transcript_item(&t, now, None).safety, Safety::Review);
        let t = tr("claude", Some("/gone"), now - 20 * DAY);
        assert_eq!(transcript_item(&t, now, Some(false)).safety, Safety::Review);
    }

    #[test]
    fn grouping_and_names() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        let a = home.join("work/app");
        let b = home.join("other/app");
        fs::create_dir_all(a.join("node_modules")).unwrap();
        fs::write(a.join("node_modules/x.js"), vec![1u8; 2 * 1024 * 1024]).unwrap();
        fs::create_dir_all(&b).unwrap();
        let now = crate::util::now_secs();
        // 项目文件夹本身也很久没动
        set_mtime(&a, now - 100 * DAY);
        let a_s = path_string(&a);
        let b_s = path_string(&b);
        let ts = vec![
            tr("claude", Some(&a_s), now - 100 * DAY),
            tr("codex", Some(&a_s), now - 90 * DAY),
            tr("codex", Some(&b_s), now - 2 * DAY),
            tr("claude", Some(&path_string(&home)), now - 2 * DAY),
        ];
        let penv = ProjectEnv {
            home: &home,
            now,
            include_folders: true,
            claude_tmp: None,
        };
        let ps = build_projects(&penv, &ts, &HashMap::new());
        assert_eq!(ps.len(), 3);
        let pa = ps.iter().find(|p| p.project == a_s).unwrap();
        assert_eq!(pa.sessions, 2);
        assert_eq!(pa.tools, vec!["claude", "codex"]);
        assert_eq!(pa.display_name, "app（work）");
        assert!(pa.folder_bytes.unwrap() >= 10_000);
        assert_eq!(pa.artifacts.len(), 1);
        // 项目文件只有 node_modules，最近活动看对话（90 天前）→ Safe
        assert_eq!(pa.artifacts[0].safety, Safety::Safe);
        assert_eq!(
            pa.total_bytes,
            pa.transcripts_bytes + pa.folder_bytes.unwrap()
        );
        let ph = ps.iter().find(|p| p.display_name == "主目录").unwrap();
        assert_eq!(ph.folder_bytes, None);
        // 不遍历时
        let penv2 = ProjectEnv {
            include_folders: false,
            ..penv
        };
        let ps2 = build_projects(&penv2, &ts, &HashMap::new());
        assert!(ps2
            .iter()
            .all(|p| p.folder_bytes.is_none() && p.artifacts.is_empty()));
    }
}

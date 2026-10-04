//! 技能：扫描各根目录、内容哈希、同名分组，以及移动式停用 / 启用、移到废纸篓。

use anyhow::{anyhow, bail, Context, Result};
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::frontmatter;
use crate::plugins::PluginInfo;
use crate::store::{append_trash_log, inventory_dir, DisabledSkill, DisabledStore, TrashRecord};
use crate::util::{
    atomic_write, backup_file, canon, now_rfc3339, path_str, read_toml_doc, stable_id,
    time_to_rfc3339,
};
use crate::{SkillEntry, SkillGroup, SkillRoot};

/// 超过这个大小的文件只哈希大小和修改时间
const BIG_FILE: u64 = 5 * 1024 * 1024;
const SCRIPT_EXTS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "fish",
    "command",
    "py",
    "js",
    "mjs",
    "cjs",
    "ts",
    "mts",
    "cts",
    "tsx",
    "jsx",
    "rb",
    "pl",
    "php",
    "lua",
    "ps1",
    "applescript",
    "swift",
    "go",
];
const CHATCUT_MANIFEST: &str = ".chatcut-desktop-skills.json";

#[derive(Debug, Clone)]
pub(crate) struct RootDef {
    pub id: String,
    pub label: String,
    pub dirs: Vec<PathBuf>,
    pub manageable: bool,
    /// 插件根目录：插件是否启用
    pub plugin_enabled: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub id: String,
    pub root_id: String,
    pub root_label: String,
    pub dir_name: String,
    /// 原位置（停用后条目已不在这里）
    pub path: PathBuf,
    /// 条目现在实际所在的位置（启用时等于 path，停用时是存档位置）
    pub location: PathBuf,
    pub is_symlink: bool,
    /// 解析符号链接后的真实目录
    pub real_path: PathBuf,
    pub archived: Option<DisabledSkill>,
    pub manageable: bool,
    pub plugin_enabled: bool,
    /// Codex 自己在 config.toml 的 `[[skills.config]]` 里停用了
    pub native_disabled: bool,
    pub synced_by: Option<String>,
}

pub(crate) fn find_skill_md(dir: &Path) -> Option<PathBuf> {
    let direct = dir.join("SKILL.md");
    if direct.is_file() {
        return Some(direct);
    }
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| {
            e.file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case("skill.md")
                && e.path().is_file()
        })
        .map(|e| e.path())
}

pub(crate) fn root_defs(home: &Path, plugins: &[PluginInfo]) -> Vec<RootDef> {
    let mut v = vec![
        RootDef {
            id: "claude".into(),
            label: "Claude Code".into(),
            dirs: vec![home.join(".claude").join("skills")],
            manageable: true,
            plugin_enabled: true,
        },
        RootDef {
            id: "codex".into(),
            label: "Codex".into(),
            dirs: vec![home.join(".codex").join("skills")],
            manageable: true,
            plugin_enabled: true,
        },
        RootDef {
            id: "agents".into(),
            label: "共享 .agents".into(),
            dirs: vec![home.join(".agents").join("skills")],
            manageable: true,
            plugin_enabled: true,
        },
        RootDef {
            id: "gemini".into(),
            label: "Gemini".into(),
            dirs: vec![home.join(".gemini").join("skills")],
            manageable: true,
            plugin_enabled: true,
        },
    ];
    for p in plugins {
        if p.skill_dirs.is_empty() {
            continue;
        }
        let id = format!("plugin:{}", p.entry.name);
        if v.iter().any(|r| r.id == id) {
            continue;
        }
        v.push(RootDef {
            id,
            label: format!("插件 {}", p.entry.name),
            dirs: p.skill_dirs.clone(),
            manageable: false,
            plugin_enabled: p.entry.enabled,
        });
    }
    v
}

pub(crate) fn skill_id(root_id: &str, dir_name: &str) -> String {
    stable_id(&["skill", root_id, dir_name])
}

fn chatcut_synced(dir: &Path) -> HashSet<String> {
    fs::read_to_string(dir.join(CHATCUT_MANIFEST))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| {
            v.get("entries")
                .and_then(|e| e.as_object())
                .map(|m| m.keys().cloned().collect())
        })
        .unwrap_or_default()
}

/// Codex `[[skills.config]] path = ".../SKILL.md" enabled = false`
fn codex_disabled_skill_files(home: &Path) -> HashSet<PathBuf> {
    let Ok(doc) = read_toml_doc(&home.join(".codex").join("config.toml")) else {
        return HashSet::new();
    };
    let Some(arr) = doc
        .get("skills")
        .and_then(|s| s.get("config"))
        .and_then(|c| c.as_array_of_tables())
    else {
        return HashSet::new();
    };
    arr.iter()
        .filter(|t| t.get("enabled").and_then(|e| e.as_bool()) == Some(false))
        .filter_map(|t| t.get("path").and_then(|p| p.as_str()).map(PathBuf::from))
        .flat_map(|p| [canon(&p), p])
        .collect()
}

fn is_native_disabled(
    disabled_files: &HashSet<PathBuf>,
    skill_dir: &Path,
    real_dir: &Path,
) -> bool {
    if disabled_files.is_empty() {
        return false;
    }
    let names = ["SKILL.md", "skill.md", "Skill.md"];
    names.iter().any(|n| {
        disabled_files.contains(&skill_dir.join(n)) || disabled_files.contains(&real_dir.join(n))
    })
}

pub(crate) fn candidates(
    home: &Path,
    store: &DisabledStore,
    plugins: &[PluginInfo],
    warnings: &mut Vec<String>,
) -> (Vec<RootDef>, Vec<Candidate>) {
    let roots = root_defs(home, plugins);
    let codex_disabled = codex_disabled_skill_files(home);
    let mut out: Vec<Candidate> = Vec::new();
    for root in &roots {
        let mut synced = HashSet::new();
        for dir in &root.dirs {
            synced.extend(chatcut_synced(dir));
        }
        let mut items: Vec<Candidate> = Vec::new();
        for dir in &root.dirs {
            let Ok(rd) = fs::read_dir(dir) else { continue };
            for e in rd.flatten() {
                let dir_name = e.file_name().to_string_lossy().into_owned();
                if dir_name.starts_with('.') {
                    continue;
                }
                let path = e.path();
                let Ok(lmeta) = fs::symlink_metadata(&path) else {
                    continue;
                };
                let is_symlink = lmeta.file_type().is_symlink();
                if is_symlink {
                    if fs::metadata(&path).is_err() {
                        warnings.push(format!(
                            "技能链接已失效（指向的目录不存在）：{}",
                            path.display()
                        ));
                        continue;
                    }
                    if !path.is_dir() {
                        continue;
                    }
                } else if !lmeta.is_dir() {
                    continue;
                }
                if find_skill_md(&path).is_none() {
                    continue;
                }
                let real_path = canon(&path);
                let native_disabled = is_native_disabled(&codex_disabled, &path, &real_path);
                items.push(Candidate {
                    id: skill_id(&root.id, &dir_name),
                    root_id: root.id.clone(),
                    root_label: root.label.clone(),
                    synced_by: synced.contains(&dir_name).then(|| "ChatCut".to_string()),
                    dir_name,
                    location: path.clone(),
                    path,
                    is_symlink,
                    real_path,
                    archived: None,
                    manageable: root.manageable,
                    plugin_enabled: root.plugin_enabled,
                    native_disabled,
                });
            }
        }
        items.sort_by(|a, b| a.dir_name.cmp(&b.dir_name));
        out.extend(items);
    }

    // 被本工具停用的技能
    for a in &store.skills {
        let Some(root) = roots.iter().find(|r| r.id == a.root_id) else {
            continue;
        };
        let archived = PathBuf::from(&a.archived_path);
        if fs::symlink_metadata(&archived).is_err() {
            warnings.push(format!(
                "技能「{}」的停用存档不见了（{}），无法恢复。",
                a.dir_name, a.archived_path
            ));
            continue;
        }
        let original = PathBuf::from(&a.original_path);
        let real_path = if a.is_symlink {
            // 相对链接要按原来的上级目录解析
            let target = a.link_target.as_ref().map(PathBuf::from);
            match (target, original.parent()) {
                (Some(t), Some(parent)) if t.is_relative() => canon(&parent.join(t)),
                (Some(t), _) => canon(&t),
                _ => canon(&archived),
            }
        } else {
            canon(&archived)
        };
        let mut id = a.id.clone();
        if out.iter().any(|c| c.id == id) {
            warnings.push(format!(
                "技能「{}」停用后又出现在原位置（{}），可能被 ChatCut 等工具重新同步回来了。",
                a.dir_name, a.original_path
            ));
            id = stable_id(&["skill", &a.root_id, &a.dir_name, "disabled"]);
        } else if a.synced_by.is_some() {
            warnings.push(format!(
                "技能「{}」是 {} 同步进来的，已停用；{} 下次同步时可能会把它放回原位置。",
                a.dir_name,
                a.synced_by.as_deref().unwrap_or("其他工具"),
                a.synced_by.as_deref().unwrap_or("该工具")
            ));
        }
        out.push(Candidate {
            id,
            root_id: a.root_id.clone(),
            root_label: root.label.clone(),
            dir_name: a.dir_name.clone(),
            path: original.clone(),
            location: archived,
            is_symlink: a.is_symlink,
            native_disabled: is_native_disabled(&codex_disabled, &original, &real_path),
            real_path,
            archived: Some(a.clone()),
            manageable: root.manageable,
            plugin_enabled: true,
            synced_by: a.synced_by.clone(),
        });
    }
    (roots, out)
}

#[derive(Debug, Clone, Default)]
struct DirStats {
    size: u64,
    files: u32,
    modified: Option<SystemTime>,
    hash: String,
    has_scripts: bool,
    /// 文件清单（相对路径、大小、修改时间）的指纹，用来复用缓存里的内容哈希
    fingerprint: String,
}

/// 内容哈希缓存：`<state_dir>/inventory/hash-cache.json`。文件清单没变就不再读文件内容。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct HashCache {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    dirs: HashMap<String, CachedHash>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CachedHash {
    fp: String,
    hash: String,
}

const HASH_CACHE_VERSION: u32 = 1;

fn hash_cache_path(state_dir: &Path) -> PathBuf {
    inventory_dir(state_dir).join("hash-cache.json")
}

fn load_hash_cache(state_dir: &Path) -> HashCache {
    fs::read_to_string(hash_cache_path(state_dir))
        .ok()
        .and_then(|t| serde_json::from_str::<HashCache>(&t).ok())
        .filter(|c| c.version == HASH_CACHE_VERSION)
        .unwrap_or_default()
}

fn mtime_nanos(t: Option<SystemTime>) -> u128 {
    t.and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos())
}

fn dir_stats(dir: &Path, cached: Option<&CachedHash>) -> DirStats {
    let mut files: Vec<(String, PathBuf, u64, Option<SystemTime>)> = Vec::new();
    let mut st = DirStats::default();
    for e in walkdir::WalkDir::new(dir)
        .follow_links(true)
        .into_iter()
        .flatten()
    {
        if !e.file_type().is_file() {
            continue;
        }
        let Ok(meta) = e.metadata() else { continue };
        let size = meta.len();
        let mtime = meta.modified().ok();
        st.size += size;
        st.files += 1;
        if let Some(m) = mtime {
            if st.modified.is_none_or(|cur| m > cur) {
                st.modified = Some(m);
            }
        }
        if !st.has_scripts {
            if let Some(ext) = e.path().extension().and_then(|x| x.to_str()) {
                let ext = ext.to_ascii_lowercase();
                st.has_scripts = SCRIPT_EXTS.contains(&ext.as_str());
            }
        }
        let rel = e
            .path()
            .strip_prefix(dir)
            .unwrap_or(e.path())
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        if rel == ".DS_Store" || rel.ends_with("/.DS_Store") {
            continue;
        }
        files.push((rel, e.path().to_path_buf(), size, mtime));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut fp = Sha256::new();
    for (rel, _, size, mtime) in &files {
        fp.update(rel.as_bytes());
        fp.update([0u8]);
        fp.update(size.to_le_bytes());
        fp.update(mtime_nanos(*mtime).to_le_bytes());
    }
    st.fingerprint = hex::encode(fp.finalize());
    if let Some(c) = cached.filter(|c| c.fp == st.fingerprint) {
        st.hash = c.hash.clone();
        return st;
    }

    let mut h = Sha256::new();
    for (rel, path, size, mtime) in &files {
        h.update(rel.as_bytes());
        h.update([0u8]);
        h.update(size.to_le_bytes());
        if *size > BIG_FILE {
            let secs = mtime
                .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            h.update(secs.to_le_bytes());
        } else if let Ok(mut f) = fs::File::open(path) {
            std::io::copy(&mut f, &mut h).ok();
        }
        h.update([0xffu8]);
    }
    st.hash = hex::encode(h.finalize());
    st
}

pub(crate) fn scan(
    home: &Path,
    state_dir: &Path,
    store: &DisabledStore,
    plugins: &[PluginInfo],
    warnings: &mut Vec<String>,
) -> (Vec<SkillRoot>, Vec<SkillEntry>, Vec<SkillGroup>) {
    let (roots, cands) = candidates(home, store, plugins, warnings);

    let unique: Vec<PathBuf> = {
        let mut seen = HashSet::new();
        cands
            .iter()
            .filter(|c| seen.insert(c.real_path.clone()))
            .map(|c| c.real_path.clone())
            .collect()
    };
    let cache = load_hash_cache(state_dir);
    let stats: HashMap<PathBuf, (DirStats, frontmatter::Frontmatter)> = unique
        .par_iter()
        .map(|p| {
            let fm = find_skill_md(p)
                .and_then(|f| fs::read(f).ok())
                .map(|b| frontmatter::parse(&String::from_utf8_lossy(&b)))
                .unwrap_or_default();
            let cached = cache.dirs.get(&path_str(p));
            (p.clone(), (dir_stats(p, cached), fm))
        })
        .collect();

    // 只在有变化时写回缓存；写失败不影响扫描结果
    let new_cache = HashCache {
        version: HASH_CACHE_VERSION,
        dirs: stats
            .iter()
            .map(|(p, (st, _))| {
                (
                    path_str(p),
                    CachedHash {
                        fp: st.fingerprint.clone(),
                        hash: st.hash.clone(),
                    },
                )
            })
            .collect(),
    };
    let changed = new_cache.dirs.len() != cache.dirs.len()
        || new_cache.dirs.iter().any(|(k, v)| {
            cache
                .dirs
                .get(k)
                .is_none_or(|c| c.fp != v.fp || c.hash != v.hash)
        });
    if changed {
        if let Ok(data) = serde_json::to_vec(&new_cache) {
            atomic_write(&hash_cache_path(state_dir), &data).ok();
        }
    }

    let skills: Vec<SkillEntry> = cands
        .iter()
        .map(|c| {
            let (st, fm) = stats.get(&c.real_path).cloned().unwrap_or_default();
            let enabled = c.archived.is_none() && !c.native_disabled && c.plugin_enabled;
            SkillEntry {
                id: c.id.clone(),
                name: fm.name.clone().unwrap_or_else(|| c.dir_name.clone()),
                description: fm.description.clone().unwrap_or_default(),
                root_id: c.root_id.clone(),
                root_label: c.root_label.clone(),
                path: path_str(&c.path),
                real_path: path_str(&c.real_path),
                is_symlink: c.is_symlink,
                size_bytes: st.size,
                file_count: st.files,
                modified: st.modified.map(time_to_rfc3339).unwrap_or_default(),
                content_hash: st.hash.clone(),
                enabled,
                manageable: c.manageable,
                has_scripts: st.has_scripts,
                dir_name: c.dir_name.clone(),
                synced_by: c.synced_by.clone(),
                archived_path: c.archived.as_ref().map(|a| a.archived_path.clone()),
            }
        })
        .collect();

    let skill_roots = roots
        .iter()
        .map(|r| SkillRoot {
            id: r.id.clone(),
            label: r.label.clone(),
            path: r.dirs.first().map(|d| path_str(d)).unwrap_or_default(),
            exists: r.dirs.iter().any(|d| d.is_dir()),
            count: skills.iter().filter(|s| s.root_id == r.id).count() as u32,
        })
        .collect();

    let groups = group_skills(&skills);
    (skill_roots, skills, groups)
}

pub(crate) fn group_skills(skills: &[SkillEntry]) -> Vec<SkillGroup> {
    let mut order: Vec<String> = Vec::new();
    let mut by_name: HashMap<String, Vec<&SkillEntry>> = HashMap::new();
    for s in skills {
        let e = by_name.entry(s.name.clone()).or_default();
        if e.is_empty() {
            order.push(s.name.clone());
        }
        e.push(s);
    }
    order
        .into_iter()
        .filter_map(|name| {
            let list = by_name.remove(&name)?;
            if list.len() < 2 {
                return None;
            }
            let same_real = list.iter().all(|s| s.real_path == list[0].real_path);
            let same_hash = list
                .iter()
                .all(|s| !s.content_hash.is_empty() && s.content_hash == list[0].content_hash);
            Some(SkillGroup {
                name,
                entry_ids: list.iter().map(|s| s.id.clone()).collect(),
                identical: same_real || same_hash,
            })
        })
        .collect()
}

// ───────────────────────────── 开关 / 删除 ─────────────────────────────

fn find_candidate(home: &Path, store: &DisabledStore, id: &str) -> Result<Candidate> {
    let plugins = crate::plugins::scan_plugins(home, &mut Vec::new());
    let (_, cands) = candidates(home, store, &plugins, &mut Vec::new());
    cands
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| anyhow!("找不到这个技能，可能已经被移动或删除了，请刷新后再试。"))
}

/// 移动条目本身（目录或符号链接，不跟随链接）
fn move_entry(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).with_context(|| format!("无法创建目录 {}", parent.display()))?;
    }
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(18) => {
            // 跨磁盘：复制过去，再把原件移到废纸篓（不永久删除）
            let meta = fs::symlink_metadata(from)?;
            if meta.file_type().is_symlink() {
                let target = fs::read_link(from)?;
                std::os::unix::fs::symlink(&target, to)?;
                // 符号链接已在新位置原样重建，这里只是完成“移动”
                fs::remove_file(from)?;
            } else {
                copy_dir(from, to)?;
                system_trash(from)?;
            }
            Ok(())
        }
        Err(e) => {
            Err(e).with_context(|| format!("无法移动 {} 到 {}", from.display(), to.display()))
        }
    }
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for e in fs::read_dir(from)?.flatten() {
        let src = e.path();
        let dst = to.join(e.file_name());
        let ft = fs::symlink_metadata(&src)?.file_type();
        if ft.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&src)?, &dst)?;
        } else if ft.is_dir() {
            copy_dir(&src, &dst)?;
        } else {
            fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

pub(crate) fn set_enabled(home: &Path, state_dir: &Path, id: &str, enabled: bool) -> Result<()> {
    let mut store = DisabledStore::load(state_dir)?;
    let c = find_candidate(home, &store, id)?;
    if !c.manageable {
        bail!(
            "「{}」是插件里的技能，不能单独开关，请在插件管理里开关整个插件。",
            c.dir_name
        );
    }
    match (enabled, c.archived.clone()) {
        (false, Some(_)) => Ok(()),
        (false, None) if c.native_disabled => Ok(()),
        (false, None) => disable(state_dir, &mut store, &c),
        (true, None) if c.native_disabled => clear_codex_skill_config(home, state_dir, &c),
        (true, None) => Ok(()),
        (true, Some(a)) => {
            enable(state_dir, &mut store, &c, &a)?;
            if c.native_disabled {
                clear_codex_skill_config(home, state_dir, &c)?;
            }
            Ok(())
        }
    }
}

fn disable(state_dir: &Path, store: &mut DisabledStore, c: &Candidate) -> Result<()> {
    let dest_dir = inventory_dir(state_dir)
        .join("disabled-skills")
        .join(&c.root_id);
    let mut dest = dest_dir.join(&c.dir_name);
    if fs::symlink_metadata(&dest).is_ok() {
        let ts = chrono::Local::now().format("%Y%m%d-%H%M%S%.3f");
        dest = dest_dir.join(format!("{}.{ts}", c.dir_name));
    }
    let link_target = if c.is_symlink {
        fs::read_link(&c.path).ok().map(|t| path_str(&t))
    } else {
        None
    };
    move_entry(&c.path, &dest)?;
    store.skills.retain(|s| s.id != c.id);
    store.skills.push(DisabledSkill {
        id: c.id.clone(),
        root_id: c.root_id.clone(),
        dir_name: c.dir_name.clone(),
        original_path: path_str(&c.path),
        archived_path: path_str(&dest),
        is_symlink: c.is_symlink,
        link_target,
        synced_by: c.synced_by.clone(),
        disabled_at: now_rfc3339(),
    });
    if let Err(e) = store.save(state_dir) {
        move_entry(&dest, &c.path).ok();
        return Err(e);
    }
    Ok(())
}

fn enable(
    state_dir: &Path,
    store: &mut DisabledStore,
    c: &Candidate,
    a: &DisabledSkill,
) -> Result<()> {
    let original = PathBuf::from(&a.original_path);
    if fs::symlink_metadata(&original).is_ok() {
        bail!(
            "原位置已经有同名技能「{}」（{}），没有覆盖。请先处理掉重名的那个再启用。",
            a.dir_name,
            original.display()
        );
    }
    move_entry(&c.location, &original)?;
    store.skills.retain(|s| s.id != a.id);
    if let Err(e) = store.save(state_dir) {
        move_entry(&original, &c.location).ok();
        return Err(e);
    }
    Ok(())
}

/// 删掉 Codex config.toml 里停用这个技能的 `[[skills.config]]` 条目
fn clear_codex_skill_config(home: &Path, state_dir: &Path, c: &Candidate) -> Result<()> {
    let path = home.join(".codex").join("config.toml");
    let mut doc = read_toml_doc(&path)?;
    let mut targets: HashSet<PathBuf> = HashSet::new();
    for base in [&c.path, &c.real_path] {
        for n in ["SKILL.md", "skill.md", "Skill.md"] {
            targets.insert(base.join(n));
            targets.insert(canon(&base.join(n)));
        }
    }
    let Some(skills) = doc.get_mut("skills").and_then(|s| s.as_table_like_mut()) else {
        return Ok(());
    };
    let Some(arr) = skills
        .get_mut("config")
        .and_then(|c| c.as_array_of_tables_mut())
    else {
        return Ok(());
    };
    let before = arr.len();
    arr.retain(|t| {
        let p = t.get("path").and_then(|p| p.as_str()).map(PathBuf::from);
        let off = t.get("enabled").and_then(|e| e.as_bool()) == Some(false);
        !(off && p.is_some_and(|p| targets.contains(&p) || targets.contains(&canon(&p))))
    });
    if arr.len() == before {
        return Ok(());
    }
    if arr.is_empty() {
        skills.remove("config");
    }
    backup_file(home, state_dir, &path)?;
    atomic_write(&path, doc.to_string().as_bytes())
}

pub(crate) fn system_trash(p: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        let mut ctx = trash::TrashContext::default();
        // NSFileManager：不需要控制 Finder 的权限，且只移动链接本身、不会跟随符号链接
        ctx.set_delete_method(DeleteMethod::NsFileManager);
        ctx.delete(p).map_err(|e| anyhow!("移到废纸篓失败：{e}"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        trash::delete(p).map_err(|e| anyhow!("移到废纸篓失败：{e}"))
    }
}

pub(crate) fn trash_with(
    home: &Path,
    state_dir: &Path,
    id: &str,
    trasher: &dyn Fn(&Path) -> Result<()>,
) -> Result<()> {
    let mut store = DisabledStore::load(state_dir)?;
    let c = find_candidate(home, &store, id)?;
    if !c.manageable {
        bail!(
            "「{}」是插件里的技能，不能单独删除，请在插件管理里卸载整个插件。",
            c.dir_name
        );
    }
    trasher(&c.location)?;
    if c.archived.is_some() {
        store.skills.retain(|s| s.id != c.id);
        store.save(state_dir)?;
    }
    append_trash_log(
        state_dir,
        TrashRecord {
            id: c.id.clone(),
            name: c.dir_name.clone(),
            root_id: c.root_id.clone(),
            original_path: path_str(&c.path),
            is_symlink: c.is_symlink,
            trashed_at: now_rfc3339(),
        },
    )
    .ok();
    Ok(())
}

pub(crate) fn read_skill_md(dir: &Path) -> Result<String> {
    let f =
        find_skill_md(dir).ok_or_else(|| anyhow!("这个目录里没有 SKILL.md：{}", dir.display()))?;
    let bytes = fs::read(&f).with_context(|| format!("无法读取 {}", f.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

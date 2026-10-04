//! Claude Code 插件：`~/.claude/plugins/installed_plugins.json`、`synced/<bucket>/manifest.json`，
//! 加上 settings.json 的 `enabledPlugins` 和 `blocklist.json`。

use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::util::{path_str, stable_id};
use crate::PluginEntry;

/// 扫描插件时顺带收集的内容，供 MCP / 技能 / 钩子使用。
#[derive(Debug, Clone)]
pub(crate) struct PluginInfo {
    pub entry: PluginEntry,
    /// 放技能子目录的目录（通常是 `<root>/skills`）
    pub skill_dirs: Vec<PathBuf>,
    pub mcp: Vec<(String, Value)>,
    pub mcp_path: PathBuf,
    /// 事件 → matcher 组数组
    pub hooks: Map<String, Value>,
    pub hooks_path: PathBuf,
}

fn read_json(p: &Path) -> Option<Value> {
    let text = fs::read_to_string(p).ok()?;
    serde_json::from_str(&text).ok()
}

/// 合并 user / user-local 的 `enabledPlugins`（local 覆盖 user）。
fn enabled_map(home: &Path) -> HashMap<String, bool> {
    let mut map = HashMap::new();
    for f in ["settings.json", "settings.local.json"] {
        if let Some(Value::Object(ep)) =
            read_json(&home.join(".claude").join(f)).and_then(|v| v.get("enabledPlugins").cloned())
        {
            for (k, v) in ep {
                if let Some(b) = v.as_bool() {
                    map.insert(k, b);
                }
            }
        }
    }
    map
}

fn blocklist(plugins_dir: &Path) -> Vec<String> {
    read_json(&plugins_dir.join("blocklist.json"))
        .and_then(|v| v.get("plugins").and_then(|p| p.as_array()).cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|p| p.get("plugin").and_then(|s| s.as_str()).map(str::to_string))
        .collect()
}

struct Candidate {
    key: String,
    name: String,
    marketplace: Option<String>,
    version: Option<String>,
    root: PathBuf,
    synced: bool,
}

pub(crate) fn scan_plugins(home: &Path, warnings: &mut Vec<String>) -> Vec<PluginInfo> {
    let plugins_dir = home.join(".claude").join("plugins");
    if !plugins_dir.is_dir() {
        return Vec::new();
    }
    let enabled = enabled_map(home);
    let blocked = blocklist(&plugins_dir);
    let mut cands: Vec<Candidate> = Vec::new();

    // 1. 正式安装的插件
    let installed_path = plugins_dir.join("installed_plugins.json");
    if installed_path.exists() {
        match read_json(&installed_path) {
            Some(v) => {
                if let Some(Value::Object(plugins)) = v.get("plugins") {
                    for (key, info) in plugins {
                        // v2：数组（按 scope 多份）；v1：对象
                        let item = match info {
                            Value::Array(a) => a
                                .iter()
                                .find(|x| x.get("scope").and_then(|s| s.as_str()) == Some("user"))
                                .or_else(|| a.first())
                                .cloned(),
                            Value::Object(_) => Some(info.clone()),
                            _ => None,
                        };
                        let Some(item) = item else { continue };
                        let Some(install) = item.get("installPath").and_then(|s| s.as_str()) else {
                            continue;
                        };
                        let (name, mkt) = split_key(key);
                        cands.push(Candidate {
                            key: key.clone(),
                            name,
                            marketplace: mkt,
                            version: item
                                .get("version")
                                .and_then(|s| s.as_str())
                                .map(str::to_string),
                            root: PathBuf::from(install),
                            synced: false,
                        });
                    }
                }
            }
            None => warnings.push(format!("无法解析插件清单：{}", installed_path.display())),
        }
    }

    // 2. 从 Claude 账号同步下来的插件
    let synced = plugins_dir.join("synced");
    if let Ok(rd) = fs::read_dir(&synced) {
        let mut buckets: Vec<PathBuf> = rd
            .flatten()
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        buckets.sort();
        for bucket in buckets {
            let Some(manifest) = read_json(&bucket.join("manifest.json")) else {
                continue;
            };
            let Some(list) = manifest.get("plugins").and_then(|p| p.as_array()) else {
                continue;
            };
            for p in list {
                let Some(name) = p.get("name").and_then(|s| s.as_str()) else {
                    continue;
                };
                let gen = p.get("generation").and_then(|g| g.as_u64());
                let mut root = bucket.join(name);
                if let Some(g) = gen {
                    let gdir = bucket.join(format!("{name}~g{g}"));
                    if gdir.is_dir() || !root.is_dir() {
                        root = gdir;
                    }
                }
                if !root.is_dir() {
                    continue;
                }
                let mkt = p
                    .get("marketplaceName")
                    .and_then(|s| s.as_str())
                    .map(str::to_string);
                let key = match &mkt {
                    Some(m) => format!("{name}@{m}"),
                    None => name.to_string(),
                };
                if cands.iter().any(|c| c.key == key) {
                    continue;
                }
                cands.push(Candidate {
                    key,
                    name: name.to_string(),
                    marketplace: mkt,
                    version: p
                        .get("version")
                        .and_then(|s| s.as_str())
                        .map(str::to_string),
                    root,
                    synced: true,
                });
            }
        }
    }

    cands
        .into_iter()
        .map(|c| {
            let is_blocked = blocked.contains(&c.key);
            let on = !is_blocked
                && match enabled.get(&c.key) {
                    Some(b) => *b,
                    // 同步插件默认启用；正式安装的插件要在 enabledPlugins 里打开
                    None => c.synced,
                };
            build_info(c, on)
        })
        .collect()
}

fn split_key(key: &str) -> (String, Option<String>) {
    match key.rsplit_once('@') {
        Some((n, m)) if !n.is_empty() => (n.to_string(), Some(m.to_string())),
        _ => (key.to_string(), None),
    }
}

/// plugin.json 里组件路径字段：字符串或字符串数组。
fn manifest_paths(root: &Path, v: Option<&Value>) -> Vec<PathBuf> {
    match v {
        Some(Value::String(s)) => vec![root.join(s)],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str())
            .map(|s| root.join(s))
            .collect(),
        _ => Vec::new(),
    }
}

fn count_md(paths: &[PathBuf]) -> u32 {
    let mut n = 0;
    for p in paths {
        if p.is_file() {
            n += u32::from(p.extension().is_some_and(|e| e == "md"));
            continue;
        }
        for e in walkdir::WalkDir::new(p)
            .follow_links(true)
            .max_depth(6)
            .into_iter()
            .flatten()
        {
            if e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "md") {
                n += 1;
            }
        }
    }
    n
}

fn hooks_object(v: &Value) -> Map<String, Value> {
    match v.get("hooks") {
        Some(Value::Object(m)) => m.clone(),
        _ => match v {
            Value::Object(m) if !m.contains_key("hooks") => m
                .iter()
                .filter(|(_, x)| x.is_array())
                .map(|(k, x)| (k.clone(), x.clone()))
                .collect(),
            _ => Map::new(),
        },
    }
}

pub(crate) fn mcp_servers_of(v: &Value) -> Vec<(String, Value)> {
    let obj = match v.get("mcpServers") {
        Some(Value::Object(m)) => m,
        _ => match v.as_object() {
            Some(m) => m,
            None => return Vec::new(),
        },
    };
    obj.iter()
        .filter(|(_, x)| x.is_object())
        .map(|(k, x)| (k.clone(), x.clone()))
        .collect()
}

pub(crate) fn count_hooks(hooks: &Map<String, Value>) -> u32 {
    hooks
        .values()
        .filter_map(|groups| groups.as_array())
        .flatten()
        .map(|g| {
            g.get("hooks")
                .and_then(|h| h.as_array())
                .map_or(0, |a| a.len() as u32)
        })
        .sum()
}

fn has_skill_md(dir: &Path) -> bool {
    crate::skills::find_skill_md(dir).is_some()
}

fn build_info(c: Candidate, enabled: bool) -> PluginInfo {
    let root = c.root;
    let manifest =
        read_json(&root.join(".claude-plugin").join("plugin.json")).unwrap_or(Value::Null);

    let mut skill_dirs = vec![root.join("skills")];
    skill_dirs.extend(manifest_paths(&root, manifest.get("skills")));
    skill_dirs.retain(|p| p.is_dir());
    skill_dirs.dedup();
    let skills: u32 = skill_dirs
        .iter()
        .map(|d| {
            fs::read_dir(d)
                .map(|rd| {
                    rd.flatten()
                        .filter(|e| e.path().is_dir() && has_skill_md(&e.path()))
                        .count() as u32
                })
                .unwrap_or(0)
        })
        .sum();

    let mut cmd_paths = vec![root.join("commands")];
    cmd_paths.extend(manifest_paths(&root, manifest.get("commands")));
    cmd_paths.retain(|p| p.exists());
    let mut agent_paths = vec![root.join("agents")];
    agent_paths.extend(manifest_paths(&root, manifest.get("agents")));
    agent_paths.retain(|p| p.exists());

    let mut hooks_path = root.join("hooks").join("hooks.json");
    let mut hooks = read_json(&hooks_path)
        .map(|v| hooks_object(&v))
        .unwrap_or_default();
    match manifest.get("hooks") {
        Some(Value::Object(_)) => {
            for (k, v) in hooks_object(manifest.get("hooks").unwrap_or(&Value::Null)) {
                hooks.entry(k).or_insert(v);
            }
            // 两处都有时保留 hooks.json 作为路径
            if !hooks_path.exists() {
                hooks_path = root.join(".claude-plugin").join("plugin.json");
            }
        }
        Some(Value::String(s)) => {
            let p = root.join(s);
            if let Some(v) = read_json(&p) {
                for (k, v) in hooks_object(&v) {
                    hooks.entry(k).or_insert(v);
                }
                hooks_path = p;
            }
        }
        _ => {}
    }

    let mut mcp_path = root.join(".mcp.json");
    let mut mcp = read_json(&mcp_path)
        .map(|v| mcp_servers_of(&v))
        .unwrap_or_default();
    match manifest.get("mcpServers") {
        Some(Value::Object(m)) => {
            for (k, v) in m {
                if v.is_object() && !mcp.iter().any(|(n, _)| n == k) {
                    mcp.push((k.clone(), v.clone()));
                }
            }
            if !root.join(".mcp.json").exists() {
                mcp_path = root.join(".claude-plugin").join("plugin.json");
            }
        }
        Some(Value::String(s)) => {
            let p = root.join(s);
            if let Some(v) = read_json(&p) {
                for (k, v) in mcp_servers_of(&v) {
                    if !mcp.iter().any(|(n, _)| *n == k) {
                        mcp.push((k, v));
                    }
                }
                mcp_path = p;
            }
        }
        _ => {}
    }

    let version = manifest
        .get("version")
        .and_then(|s| s.as_str())
        .map(str::to_string)
        .or(c.version);

    let entry = PluginEntry {
        id: stable_id(&["plugin", &c.key]),
        name: c.name,
        marketplace: c.marketplace,
        version,
        enabled,
        path: path_str(&root),
        skills,
        mcp_servers: mcp.len() as u32,
        hooks: count_hooks(&hooks),
        commands: count_md(&cmd_paths),
        agents: count_md(&agent_paths),
    };
    PluginInfo {
        entry,
        skill_dirs,
        mcp,
        mcp_path,
        hooks,
        hooks_path,
    }
}

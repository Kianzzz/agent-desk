//! 钩子：Claude Code settings / 项目 / 插件，Codex hooks.json 和 notify，Gemini settings。

use anyhow::{anyhow, bail, Result};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::mcp::{codex_config_path, gemini_settings_path, known_projects, read_claude_json};
use crate::plugins::PluginInfo;
use crate::store::{ArchiveFormat, DisabledHook, DisabledStore};
use crate::util::{
    atomic_write, backup_file, canon, client_key, now_rfc3339, path_str, read_toml_doc, stable_id,
    toml_from_snippet, toml_insert_at, toml_snippet, JsonDoc,
};
use crate::{Client, HookEntry};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HookOrigin {
    Json,
    CodexNotify,
    Archived,
}

#[derive(Debug, Clone)]
pub(crate) struct HookRecord {
    pub entry: HookEntry,
    pub origin: HookOrigin,
}

struct Src<'a> {
    client: Client,
    scope: &'a str,
    scope_path: Option<&'a str>,
    config_path: &'a Path,
    manageable: bool,
    enabled: bool,
    /// Gemini 的 timeout 是毫秒
    timeout_ms: bool,
}

/// 遍历结果里的一条钩子及其位置
pub(crate) struct Found<'v> {
    pub id: String,
    pub event: String,
    pub matcher: Option<String>,
    pub event_index: usize,
    pub group_index: usize,
    pub hook_index: usize,
    pub hook: &'v Value,
}

fn hook_command(h: &Value) -> String {
    for k in ["command", "url", "prompt"] {
        if let Some(s) = h.get(k).and_then(|v| v.as_str()) {
            return s.to_string();
        }
    }
    h.get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string()
}

fn matcher_of(group: &Value) -> Option<String> {
    group
        .get("matcher")
        .and_then(|m| m.as_str())
        .filter(|m| !m.is_empty())
        .map(str::to_string)
}

fn hook_id(src: &Src, event: &str, matcher: Option<&str>, command: &str, occ: usize) -> String {
    let occ = occ.to_string();
    stable_id(&[
        "hook",
        client_key(src.client),
        src.scope,
        src.scope_path.unwrap_or(""),
        &path_str(src.config_path),
        event,
        matcher.unwrap_or(""),
        command,
        &occ,
    ])
}

fn walk<'v>(hooks: &'v Map<String, Value>, src: &Src) -> Vec<Found<'v>> {
    let mut out = Vec::new();
    let mut occ: HashMap<(String, Option<String>, String), usize> = HashMap::new();
    for (ei, (event, groups)) in hooks.iter().enumerate() {
        let Some(groups) = groups.as_array() else {
            continue;
        };
        for (gi, g) in groups.iter().enumerate() {
            let matcher = matcher_of(g);
            let Some(list) = g.get("hooks").and_then(|h| h.as_array()) else {
                continue;
            };
            for (hi, h) in list.iter().enumerate() {
                let command = hook_command(h);
                let n = occ
                    .entry((event.clone(), matcher.clone(), command.clone()))
                    .or_insert(0);
                let id = hook_id(src, event, matcher.as_deref(), &command, *n);
                *n += 1;
                out.push(Found {
                    id,
                    event: event.clone(),
                    matcher: matcher.clone(),
                    event_index: ei,
                    group_index: gi,
                    hook_index: hi,
                    hook: h,
                });
            }
        }
    }
    out
}

fn timeout_of(h: &Value, ms: bool) -> Option<u32> {
    let n = h.get("timeout").and_then(|t| t.as_f64())?;
    let secs = if ms { (n / 1000.0).ceil() } else { n };
    (secs >= 0.0).then_some(secs as u32)
}

fn push_hooks(
    out: &mut Vec<HookRecord>,
    hooks: &Map<String, Value>,
    src: &Src,
    origin: HookOrigin,
) {
    for f in walk(hooks, src) {
        out.push(HookRecord {
            entry: HookEntry {
                id: f.id,
                client: src.client,
                scope: src.scope.to_string(),
                scope_path: src.scope_path.map(str::to_string),
                event: f.event,
                matcher: f.matcher,
                command: hook_command(f.hook),
                timeout_sec: timeout_of(f.hook, src.timeout_ms),
                enabled: src.enabled,
                manageable: src.manageable,
                config_path: path_str(src.config_path),
            },
            origin,
        });
    }
}

fn read_json_value(p: &Path, warnings: &mut Vec<String>) -> Option<Value> {
    if !p.is_file() {
        return None;
    }
    match JsonDoc::read(p) {
        Ok(d) => Some(d.value),
        Err(e) => {
            warnings.push(format!("{e:#}"));
            None
        }
    }
}

fn hooks_map(v: &Value) -> Option<&Map<String, Value>> {
    v.get("hooks").and_then(|h| h.as_object())
}

fn shell_join(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if a.is_empty()
                || a.chars()
                    .any(|c| c.is_whitespace() || "'\"\\$`".contains(c))
            {
                format!("'{}'", a.replace('\'', "'\\''"))
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn notify_id(config_path: &Path) -> String {
    stable_id(&[
        "hook",
        "codex",
        "user",
        "",
        &path_str(config_path),
        "notify",
    ])
}

pub(crate) fn scan_records(
    home: &Path,
    store: &DisabledStore,
    plugins: &[PluginInfo],
    warnings: &mut Vec<String>,
) -> Vec<HookRecord> {
    let mut out = Vec::new();

    // Claude Code 用户级
    for f in ["settings.json", "settings.local.json"] {
        let p = home.join(".claude").join(f);
        if let Some(v) = read_json_value(&p, warnings) {
            if v.get("disableAllHooks").and_then(|b| b.as_bool()) == Some(true) {
                warnings.push(format!(
                    "Claude Code 设置里打开了 disableAllHooks（{}），所有钩子当前都不会运行。",
                    p.display()
                ));
            }
            if let Some(h) = hooks_map(&v) {
                let src = Src {
                    client: Client::ClaudeCode,
                    scope: "user",
                    scope_path: None,
                    config_path: &p,
                    manageable: true,
                    enabled: true,
                    timeout_ms: false,
                };
                push_hooks(&mut out, h, &src, HookOrigin::Json);
            }
        }
    }

    // Claude Code 项目级（只读）
    if let Ok(Some(cj)) = read_claude_json(home) {
        let home_c = canon(home);
        let mut seen = std::collections::HashSet::new();
        for proj in known_projects(&cj) {
            let pc = canon(Path::new(&proj));
            if pc == home_c || !seen.insert(pc) {
                continue;
            }
            for (f, scope) in [
                ("settings.json", "project"),
                ("settings.local.json", "local"),
            ] {
                let p = Path::new(&proj).join(".claude").join(f);
                if let Some(v) = read_json_value(&p, warnings) {
                    if let Some(h) = hooks_map(&v) {
                        let src = Src {
                            client: Client::ClaudeCode,
                            scope,
                            scope_path: Some(&proj),
                            config_path: &p,
                            manageable: false,
                            enabled: true,
                            timeout_ms: false,
                        };
                        push_hooks(&mut out, h, &src, HookOrigin::Json);
                    }
                }
            }
        }
    }

    // 插件（只读）
    for p in plugins {
        let src = Src {
            client: Client::ClaudeCode,
            scope: "plugin",
            scope_path: Some(&p.entry.name),
            config_path: &p.hooks_path,
            manageable: false,
            enabled: p.entry.enabled,
            timeout_ms: false,
        };
        push_hooks(&mut out, &p.hooks, &src, HookOrigin::Json);
    }

    // Codex hooks.json
    let codex_hooks = home.join(".codex").join("hooks.json");
    let mut codex_hook_count = 0;
    if let Some(v) = read_json_value(&codex_hooks, warnings) {
        if let Some(h) = hooks_map(&v) {
            let before = out.len();
            let src = Src {
                client: Client::Codex,
                scope: "user",
                scope_path: None,
                config_path: &codex_hooks,
                manageable: true,
                enabled: true,
                timeout_ms: false,
            };
            push_hooks(&mut out, h, &src, HookOrigin::Json);
            codex_hook_count = out.len() - before;
        }
    }

    // Codex notify
    let codex_cfg = codex_config_path(home);
    if codex_cfg.is_file() {
        match read_toml_doc(&codex_cfg) {
            Ok(doc) => {
                if codex_hook_count > 0
                    && doc
                        .get("features")
                        .and_then(|f| f.get("hooks"))
                        .and_then(|h| h.as_bool())
                        == Some(false)
                {
                    warnings.push("Codex 配置里关闭了钩子功能（features.hooks = false），hooks.json 里的钩子当前不会运行。".into());
                }
                if let Some(arr) = doc.get("notify").and_then(|n| n.as_array()) {
                    let args: Vec<String> = arr
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect();
                    out.push(HookRecord {
                        entry: HookEntry {
                            id: notify_id(&codex_cfg),
                            client: Client::Codex,
                            scope: "user".into(),
                            scope_path: None,
                            event: "notify".into(),
                            matcher: None,
                            command: shell_join(&args),
                            timeout_sec: None,
                            enabled: true,
                            manageable: true,
                            config_path: path_str(&codex_cfg),
                        },
                        origin: HookOrigin::CodexNotify,
                    });
                }
            }
            Err(e) => warnings.push(format!("{e:#}")),
        }
    }

    // Gemini
    let gemini = gemini_settings_path(home);
    if let Some(v) = read_json_value(&gemini, warnings) {
        if let Some(h) = hooks_map(&v) {
            let src = Src {
                client: Client::Gemini,
                scope: "user",
                scope_path: None,
                config_path: &gemini,
                manageable: true,
                enabled: true,
                timeout_ms: true,
            };
            push_hooks(&mut out, h, &src, HookOrigin::Json);
        }
    }

    // 被本工具停用的
    for a in &store.hooks {
        let mut id = a.id.clone();
        if out.iter().any(|r| r.entry.id == id) {
            id = stable_id(&["hook", &a.id, "disabled"]);
        }
        let (command, timeout_sec) = match a.format {
            ArchiveFormat::Toml => {
                let args = a
                    .hook
                    .as_str()
                    .and_then(|s| toml_from_snippet(s, &[], "notify").ok())
                    .and_then(|(_, item)| item.as_array().cloned())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                (shell_join(&args), None)
            }
            _ => (
                hook_command(&a.hook),
                timeout_of(&a.hook, a.client == Client::Gemini),
            ),
        };
        out.push(HookRecord {
            entry: HookEntry {
                id,
                client: a.client,
                scope: a.scope.clone(),
                scope_path: None,
                event: a.event.clone(),
                matcher: a.matcher.clone(),
                command,
                timeout_sec,
                enabled: false,
                manageable: true,
                config_path: a.config_path.clone(),
            },
            origin: HookOrigin::Archived,
        });
    }
    out
}

// ───────────────────────────── 开关 ─────────────────────────────

pub(crate) fn set_enabled(home: &Path, state_dir: &Path, id: &str, enabled: bool) -> Result<()> {
    let mut store = DisabledStore::load(state_dir)?;
    let plugins = crate::plugins::scan_plugins(home, &mut Vec::new());
    let rec = scan_records(home, &store, &plugins, &mut Vec::new())
        .into_iter()
        .find(|r| r.entry.id == id)
        .ok_or_else(|| anyhow!("找不到这个钩子，可能已经被删除了，请刷新后再试。"))?;
    if !rec.entry.manageable {
        bail!("项目和插件里的钩子只能在对应的项目或插件里修改，这里不能开关。");
    }
    match (enabled, rec.origin) {
        (false, HookOrigin::Archived)
        | (true, HookOrigin::Json)
        | (true, HookOrigin::CodexNotify) => Ok(()),
        (false, HookOrigin::Json) => json_disable(home, state_dir, &mut store, &rec.entry),
        (false, HookOrigin::CodexNotify) => notify_disable(home, state_dir, &mut store, &rec.entry),
        (true, HookOrigin::Archived) => {
            let a = store
                .hooks
                .iter()
                .find(|h| h.id == id || stable_id(&["hook", &h.id, "disabled"]) == id)
                .cloned()
                .ok_or_else(|| anyhow!("找不到停用存档"))?;
            match a.format {
                ArchiveFormat::Toml => notify_enable(home, state_dir, &mut store, &a),
                _ => json_enable(home, state_dir, &mut store, &a),
            }
        }
    }
}

fn json_disable(
    home: &Path,
    state_dir: &Path,
    store: &mut DisabledStore,
    e: &HookEntry,
) -> Result<()> {
    let path = PathBuf::from(&e.config_path);
    let mut doc = JsonDoc::read(&path)?;
    let src = Src {
        client: e.client,
        scope: &e.scope,
        scope_path: e.scope_path.as_deref(),
        config_path: &path,
        manageable: true,
        enabled: true,
        timeout_ms: false,
    };
    let (ei, gi, hi, event, matcher) = {
        let hooks = hooks_map(&doc.value)
            .ok_or_else(|| anyhow!("配置里已经没有这个钩子了，请刷新后再试。"))?;
        let f = walk(hooks, &src)
            .into_iter()
            .find(|f| f.id == e.id)
            .ok_or_else(|| anyhow!("配置里已经没有这个钩子了，请刷新后再试。"))?;
        (
            f.event_index,
            f.group_index,
            f.hook_index,
            f.event,
            f.matcher,
        )
    };
    let root = doc
        .value
        .as_object_mut()
        .ok_or_else(|| anyhow!("配置文件格式不对"))?;
    let root_keys: Vec<String> = root.keys().cloned().collect();
    let hooks_obj = root
        .get_mut("hooks")
        .and_then(|h| h.as_object_mut())
        .ok_or_else(|| anyhow!("配置文件格式不对"))?;
    let ev_keys: Vec<String> = hooks_obj.keys().cloned().collect();
    let groups = hooks_obj
        .get_mut(&event)
        .and_then(|g| g.as_array_mut())
        .ok_or_else(|| anyhow!("配置文件格式不对"))?;
    let group = groups
        .get_mut(gi)
        .and_then(|g| g.as_object_mut())
        .ok_or_else(|| anyhow!("配置文件格式不对"))?;
    let list = group
        .get_mut("hooks")
        .and_then(|h| h.as_array_mut())
        .ok_or_else(|| anyhow!("配置文件格式不对"))?;
    let hook = list.remove(hi);
    let list_empty = list.is_empty();
    let mut skeleton = group.clone();
    skeleton.shift_remove("hooks");
    if list_empty {
        groups.remove(gi);
    }
    if groups.is_empty() {
        hooks_obj.shift_remove(&event);
    }
    let mut hooks_key_index = None;
    if hooks_obj.is_empty() {
        hooks_key_index = root_keys.iter().position(|k| k == "hooks");
        root.shift_remove("hooks");
    }
    let archive = DisabledHook {
        id: e.id.clone(),
        client: e.client,
        scope: e.scope.clone(),
        config_path: e.config_path.clone(),
        format: ArchiveFormat::Json,
        event,
        matcher,
        hook,
        group: Value::Object(skeleton),
        event_index: ei,
        group_index: gi,
        hook_index: hi,
        hooks_key_index,
        prev_key: ei.checked_sub(1).and_then(|i| ev_keys.get(i).cloned()),
        next_key: ev_keys.get(ei + 1).cloned(),
        disabled_at: now_rfc3339(),
    };
    backup_file(home, state_dir, &path)?;
    store.hooks.retain(|h| h.id != e.id);
    store.hooks.push(archive);
    store.save(state_dir)?;
    if let Err(err) = doc.write(&path) {
        store.hooks.retain(|h| h.id != e.id);
        store.save(state_dir).ok();
        return Err(err);
    }
    Ok(())
}

fn json_enable(
    home: &Path,
    state_dir: &Path,
    store: &mut DisabledStore,
    a: &DisabledHook,
) -> Result<()> {
    let path = PathBuf::from(&a.config_path);
    let mut doc = if path.exists() {
        JsonDoc::read(&path)?
    } else {
        JsonDoc::parse("{\n}\n")?
    };
    let root = doc
        .value
        .as_object_mut()
        .ok_or_else(|| anyhow!("配置文件格式不对"))?;
    if !root.get("hooks").is_some_and(|h| h.is_object()) {
        let idx = a.hooks_key_index.unwrap_or(root.len()).min(root.len());
        root.shift_insert(idx, "hooks".into(), Value::Object(Map::new()));
    }
    let hooks_obj = root
        .get_mut("hooks")
        .and_then(|h| h.as_object_mut())
        .ok_or_else(|| anyhow!("配置文件格式不对"))?;
    if !hooks_obj.get(&a.event).is_some_and(|g| g.is_array()) {
        let keys: Vec<String> = hooks_obj.keys().cloned().collect();
        let idx = a
            .prev_key
            .as_ref()
            .and_then(|p| keys.iter().position(|k| k == p).map(|i| i + 1))
            .or_else(|| {
                a.next_key
                    .as_ref()
                    .and_then(|n| keys.iter().position(|k| k == n))
            })
            .unwrap_or(a.event_index.min(keys.len()));
        hooks_obj.shift_insert(idx, a.event.clone(), Value::Array(Vec::new()));
    }
    let groups = hooks_obj
        .get_mut(&a.event)
        .and_then(|g| g.as_array_mut())
        .ok_or_else(|| anyhow!("配置文件格式不对"))?;
    let same =
        |g: &Value| matcher_of(g) == a.matcher && g.get("hooks").is_some_and(|h| h.is_array());
    let target = if groups.get(a.group_index).is_some_and(same) {
        Some(a.group_index)
    } else {
        groups.iter().position(same)
    };
    match target {
        Some(gi) => {
            let list = groups[gi]
                .get_mut("hooks")
                .and_then(|h| h.as_array_mut())
                .ok_or_else(|| anyhow!("配置文件格式不对"))?;
            let hi = a.hook_index.min(list.len());
            list.insert(hi, a.hook.clone());
        }
        None => {
            let mut g = a.group.as_object().cloned().unwrap_or_default();
            if let Some(m) = &a.matcher {
                if !g.contains_key("matcher") {
                    g.insert("matcher".into(), Value::String(m.clone()));
                }
            }
            g.insert("hooks".into(), Value::Array(vec![a.hook.clone()]));
            let gi = a.group_index.min(groups.len());
            groups.insert(gi, Value::Object(g));
        }
    }
    backup_file(home, state_dir, &path)?;
    doc.write(&path)?;
    store.hooks.retain(|h| h.id != a.id);
    store.save(state_dir)
}

fn notify_disable(
    home: &Path,
    state_dir: &Path,
    store: &mut DisabledStore,
    e: &HookEntry,
) -> Result<()> {
    let path = PathBuf::from(&e.config_path);
    let mut doc = read_toml_doc(&path)?;
    let root = doc.as_table_mut();
    let keys: Vec<String> = root.iter().map(|(k, _)| k.to_string()).collect();
    let idx = keys
        .iter()
        .position(|k| k == "notify")
        .ok_or_else(|| anyhow!("Codex 配置里已经没有 notify 了"))?;
    let (key, item) = root
        .remove_entry("notify")
        .ok_or_else(|| anyhow!("Codex 配置里已经没有 notify 了"))?;
    let snippet = toml_snippet(&[], &key, &item);
    let archive = DisabledHook {
        id: e.id.clone(),
        client: Client::Codex,
        scope: "user".into(),
        config_path: e.config_path.clone(),
        format: ArchiveFormat::Toml,
        event: "notify".into(),
        matcher: None,
        hook: Value::String(snippet),
        group: Value::Null,
        event_index: idx,
        group_index: 0,
        hook_index: 0,
        hooks_key_index: None,
        prev_key: idx.checked_sub(1).and_then(|i| keys.get(i).cloned()),
        next_key: keys.get(idx + 1).cloned(),
        disabled_at: now_rfc3339(),
    };
    backup_file(home, state_dir, &path)?;
    store.hooks.retain(|h| h.id != e.id);
    store.hooks.push(archive);
    store.save(state_dir)?;
    if let Err(err) = atomic_write(&path, doc.to_string().as_bytes()) {
        store.hooks.retain(|h| h.id != e.id);
        store.save(state_dir).ok();
        return Err(err);
    }
    Ok(())
}

fn notify_enable(
    home: &Path,
    state_dir: &Path,
    store: &mut DisabledStore,
    a: &DisabledHook,
) -> Result<()> {
    let path = PathBuf::from(&a.config_path);
    let mut doc = if path.exists() {
        read_toml_doc(&path)?
    } else {
        toml_edit::DocumentMut::new()
    };
    if doc.contains_key("notify") {
        bail!("Codex 配置里已经有 notify 了，没有覆盖。");
    }
    let snippet = a.hook.as_str().ok_or_else(|| anyhow!("存档内容已损坏"))?;
    let (key, item) = toml_from_snippet(snippet, &[], "notify")?;
    toml_insert_at(
        doc.as_table_mut(),
        key,
        item,
        a.prev_key.as_deref(),
        a.next_key.as_deref(),
    );
    backup_file(home, state_dir, &path)?;
    atomic_write(&path, doc.to_string().as_bytes())?;
    store.hooks.retain(|h| h.id != a.id);
    store.save(state_dir)
}

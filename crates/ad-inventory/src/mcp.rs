//! MCP 服务：扫描各客户端配置，以及可撤销的开关。

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::cli::ClaudeCli;
use crate::plugins::{mcp_servers_of, PluginInfo};
use crate::store::{ArchiveFormat, DisabledMcp, DisabledStore};
use crate::util::{
    atomic_write, backup_file, canon, client_key, client_label, now_rfc3339, path_str,
    read_toml_doc, stable_id, toml_from_snippet, toml_insert_at, toml_item_to_json, toml_snippet,
    JsonDoc,
};
use crate::{Client, McpServer};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    /// 配置文件里现有的条目；`native_disabled` 表示客户端自己的停用标记（如 Codex 的 `enabled = false`）
    Live { native_disabled: bool },
    /// 被本工具停用、存在存档里的条目
    Archived,
}

#[derive(Debug, Clone)]
pub(crate) struct McpRecord {
    pub server: McpServer,
    pub origin: Origin,
}

struct Loc<'a> {
    client: Client,
    scope: &'a str,
    scope_path: Option<&'a str>,
    config_path: &'a Path,
    manageable: bool,
}

pub(crate) fn mcp_id(client: Client, scope: &str, scope_path: Option<&str>, name: &str) -> String {
    stable_id(&[
        "mcp",
        client_key(client),
        scope,
        scope_path.unwrap_or(""),
        name,
    ])
}

pub(crate) fn claude_json_path(home: &Path) -> PathBuf {
    home.join(".claude.json")
}
pub(crate) fn desktop_dir(home: &Path) -> PathBuf {
    home.join("Library")
        .join("Application Support")
        .join("Claude")
}
pub(crate) fn desktop_config_path(home: &Path) -> PathBuf {
    desktop_dir(home).join("claude_desktop_config.json")
}
pub(crate) fn codex_config_path(home: &Path) -> PathBuf {
    home.join(".codex").join("config.toml")
}
pub(crate) fn gemini_settings_path(home: &Path) -> PathBuf {
    home.join(".gemini").join("settings.json")
}
pub(crate) fn cursor_config_path(home: &Path) -> PathBuf {
    home.join(".cursor").join("mcp.json")
}

/// `~/.claude.json` 只取需要的字段，其余（历史、缓存）跳过。
#[derive(Deserialize, Default)]
pub(crate) struct ClaudeJson {
    #[serde(rename = "mcpServers", default)]
    pub mcp_servers: Option<Value>,
    #[serde(default)]
    pub projects: Option<BTreeMap<String, ClaudeProject>>,
}

#[derive(Deserialize, Default)]
pub(crate) struct ClaudeProject {
    #[serde(rename = "mcpServers", default)]
    pub mcp_servers: Option<Value>,
    #[serde(rename = "disabledMcpjsonServers", default)]
    pub disabled_mcpjson: Option<Value>,
}

pub(crate) fn read_claude_json(home: &Path) -> Result<Option<ClaudeJson>> {
    let p = claude_json_path(home);
    let text = match fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("无法读取 {}", p.display())),
    };
    let cj: ClaudeJson =
        serde_json::from_str(&text).with_context(|| format!("无法解析 {}", p.display()))?;
    Ok(Some(cj))
}

/// 已知项目目录（`~/.claude.json` 的 projects 里仍存在的目录）
pub(crate) fn known_projects(cj: &ClaudeJson) -> Vec<String> {
    cj.projects
        .as_ref()
        .map(|p| {
            p.keys()
                .filter(|k| Path::new(k).is_dir())
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn str_list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| match x {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn obj_keys(v: Option<&Value>) -> Vec<String> {
    v.and_then(|x| x.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

fn detect_package(command: Option<&str>, args: &[String]) -> Option<String> {
    let cmd = command?;
    let base = Path::new(cmd).file_name()?.to_str()?;
    let base = base.trim_end_matches(".cmd").trim_end_matches(".exe");
    let rest: &[String] = match base {
        "npx" | "bunx" | "uvx" | "pnpx" => args,
        "pnpm" | "yarn" if args.first().is_some_and(|a| a == "dlx") => &args[1..],
        _ => return None,
    };
    rest.iter().find(|a| !a.starts_with('-')).cloned()
}

fn url_transport(url: &str) -> &'static str {
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .trim_end_matches('/');
    if path.ends_with("/sse") {
        "sse"
    } else {
        "http"
    }
}

/// 先看 `type`；没有就按 url 判断（以 /sse 结尾为 sse，否则 http）；有 command 或都没有时为 stdio。
fn transport_of(ty: Option<&str>, url: Option<&str>) -> String {
    match ty.map(|t| t.to_ascii_lowercase()) {
        Some(t) if t == "stdio" => return "stdio".into(),
        Some(t) if t == "sse" => return "sse".into(),
        Some(t) if t == "http" || t.starts_with("streamable") => return "http".into(),
        _ => {}
    }
    url.map_or("stdio", url_transport).into()
}

/// Claude Code / 桌面版 / Gemini / Cursor 的 JSON 条目
fn from_json(loc: &Loc, name: &str, v: &Value, enabled: bool) -> McpServer {
    let command = v
        .get("command")
        .and_then(|s| s.as_str())
        .map(str::to_string);
    let args = str_list(v.get("args"));
    let ty = v
        .get("type")
        .or_else(|| v.get("transport"))
        .and_then(|s| s.as_str());
    let http_url = v.get("httpUrl").and_then(|s| s.as_str());
    let plain_url = v
        .get("url")
        .or_else(|| v.get("serverUrl"))
        .and_then(|s| s.as_str());
    let url = http_url.or(plain_url).map(str::to_string);
    let transport = if loc.client == Client::Gemini && ty.is_none() && url.is_some() {
        // Gemini：httpUrl 是可流式 HTTP，url 是 SSE
        if http_url.is_some() { "http" } else { "sse" }.to_string()
    } else {
        transport_of(ty, url.as_deref())
    };
    let package = detect_package(command.as_deref(), &args);
    McpServer {
        id: mcp_id(loc.client, loc.scope, loc.scope_path, name),
        client: loc.client,
        name: name.to_string(),
        scope: loc.scope.to_string(),
        scope_path: loc.scope_path.map(str::to_string),
        transport,
        command,
        args,
        url,
        env_keys: obj_keys(v.get("env")),
        header_keys: obj_keys(v.get("headers")),
        enabled,
        manageable: loc.manageable,
        config_path: path_str(loc.config_path),
        package,
    }
}

/// Codex `[mcp_servers.<name>]`（已转成 JSON）
fn from_codex(loc: &Loc, name: &str, v: &Value, enabled: bool) -> McpServer {
    let command = v
        .get("command")
        .and_then(|s| s.as_str())
        .map(str::to_string);
    let args = str_list(v.get("args"));
    let url = v.get("url").and_then(|s| s.as_str()).map(str::to_string);
    let transport = transport_of(None, url.as_deref());
    let mut env_keys = obj_keys(v.get("env"));
    for k in str_list(v.get("env_vars")) {
        if !env_keys.contains(&k) {
            env_keys.push(k);
        }
    }
    let mut header_keys = obj_keys(v.get("http_headers"));
    for k in obj_keys(v.get("env_http_headers")) {
        if !header_keys.contains(&k) {
            header_keys.push(k);
        }
    }
    if v.get("bearer_token_env_var").is_some()
        && !header_keys
            .iter()
            .any(|h| h.eq_ignore_ascii_case("authorization"))
    {
        header_keys.push("Authorization".into());
    }
    let package = detect_package(command.as_deref(), &args);
    McpServer {
        id: mcp_id(loc.client, loc.scope, loc.scope_path, name),
        client: loc.client,
        name: name.to_string(),
        scope: loc.scope.to_string(),
        scope_path: loc.scope_path.map(str::to_string),
        transport,
        command,
        args,
        url,
        env_keys,
        header_keys,
        enabled,
        manageable: loc.manageable,
        config_path: path_str(loc.config_path),
        package,
    }
}

fn json_native_disabled(v: &Value) -> bool {
    v.get("disabled").and_then(|b| b.as_bool()) == Some(true)
}

fn gemini_excluded(home: &Path) -> Vec<String> {
    fs::read_to_string(gemini_settings_path(home))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .map(|v| str_list(v.get("mcp").and_then(|m| m.get("excluded"))))
        .unwrap_or_default()
}

fn push_json_file(
    out: &mut Vec<McpRecord>,
    warnings: &mut Vec<String>,
    client: Client,
    path: &Path,
    excluded: &[String],
) {
    if !path.exists() {
        return;
    }
    let doc = match JsonDoc::read(path) {
        Ok(d) => d,
        Err(e) => {
            warnings.push(format!(
                "{} 的 MCP 配置读取失败：{e:#}",
                client_label(client)
            ));
            return;
        }
    };
    let Some(servers) = doc.value.get("mcpServers").and_then(|m| m.as_object()) else {
        return;
    };
    let loc = Loc {
        client,
        scope: "user",
        scope_path: None,
        config_path: path,
        manageable: true,
    };
    for (name, v) in servers {
        let native_disabled = json_native_disabled(v) || excluded.contains(name);
        out.push(McpRecord {
            server: from_json(&loc, name, v, !native_disabled),
            origin: Origin::Live { native_disabled },
        });
    }
}

fn scan_claude_code(
    home: &Path,
    plugins: &[PluginInfo],
    out: &mut Vec<McpRecord>,
    warnings: &mut Vec<String>,
) {
    let cj_path = claude_json_path(home);
    let cj = match read_claude_json(home) {
        Ok(Some(c)) => c,
        Ok(None) => ClaudeJson::default(),
        Err(e) => {
            warnings.push(format!("{e:#}"));
            ClaudeJson::default()
        }
    };
    if let Some(Value::Object(m)) = &cj.mcp_servers {
        let loc = Loc {
            client: Client::ClaudeCode,
            scope: "user",
            scope_path: None,
            config_path: &cj_path,
            manageable: true,
        };
        for (name, v) in m {
            out.push(McpRecord {
                server: from_json(&loc, name, v, true),
                origin: Origin::Live {
                    native_disabled: false,
                },
            });
        }
    }
    let mut seen_project_files = HashSet::new();
    if let Some(projects) = &cj.projects {
        for (proj, p) in projects {
            let exists = Path::new(proj).is_dir();
            if let Some(Value::Object(m)) = &p.mcp_servers {
                let loc = Loc {
                    client: Client::ClaudeCode,
                    scope: "local",
                    scope_path: Some(proj),
                    config_path: &cj_path,
                    // local 级要在项目目录里执行 claude 命令，目录不在了就不能开关
                    manageable: exists,
                };
                for (name, v) in m {
                    out.push(McpRecord {
                        server: from_json(&loc, name, v, true),
                        origin: Origin::Live {
                            native_disabled: false,
                        },
                    });
                }
            }
            if !exists {
                continue;
            }
            let mcp_file = Path::new(proj).join(".mcp.json");
            if !mcp_file.is_file() || !seen_project_files.insert(canon(&mcp_file)) {
                continue;
            }
            match fs::read_to_string(&mcp_file)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            {
                Some(v) => {
                    let disabled = str_list(p.disabled_mcpjson.as_ref());
                    let loc = Loc {
                        client: Client::ClaudeCode,
                        scope: "project",
                        scope_path: Some(proj),
                        config_path: &mcp_file,
                        manageable: false,
                    };
                    for (name, sv) in mcp_servers_of(&v) {
                        let on = !disabled.contains(&name);
                        out.push(McpRecord {
                            server: from_json(&loc, &name, &sv, on),
                            origin: Origin::Live {
                                native_disabled: !on,
                            },
                        });
                    }
                }
                None => warnings.push(format!("无法解析项目 MCP 配置：{}", mcp_file.display())),
            }
        }
    }
    for p in plugins.iter().filter(|p| p.entry.enabled) {
        let loc = Loc {
            client: Client::ClaudeCode,
            scope: "plugin",
            scope_path: Some(&p.entry.name),
            config_path: &p.mcp_path,
            manageable: false,
        };
        for (name, v) in &p.mcp {
            out.push(McpRecord {
                server: from_json(&loc, name, v, true),
                origin: Origin::Live {
                    native_disabled: false,
                },
            });
        }
    }
}

/// 桌面扩展（只读列出）
fn scan_desktop_extensions(home: &Path, out: &mut Vec<McpRecord>) {
    let dir = desktop_dir(home);
    let ext_dir = dir.join("Claude Extensions");
    let settings_dir = dir.join("Claude Extensions Settings");
    let Ok(rd) = fs::read_dir(&ext_dir) else {
        return;
    };
    let mut dirs: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for d in dirs {
        let manifest_path = d.join("manifest.json");
        let Some(m) = fs::read_to_string(&manifest_path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        else {
            continue;
        };
        let ext_id = d
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = m
            .get("display_name")
            .or_else(|| m.get("name"))
            .and_then(|s| s.as_str())
            .unwrap_or(&ext_id)
            .to_string();
        let cfg = m
            .get("server")
            .and_then(|s| s.get("mcp_config"))
            .cloned()
            .unwrap_or(Value::Null);
        let enabled = fs::read_to_string(settings_dir.join(format!("{ext_id}.json")))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v.get("isEnabled").and_then(|b| b.as_bool()))
            .unwrap_or(true);
        let loc = Loc {
            client: Client::ClaudeDesktop,
            scope: "plugin",
            scope_path: Some(&ext_id),
            config_path: &manifest_path,
            manageable: false,
        };
        let mut s = from_json(&loc, &name, &cfg, enabled);
        if s.command.is_none() && s.url.is_none() {
            s.transport = "stdio".into();
        }
        out.push(McpRecord {
            server: s,
            origin: Origin::Live {
                native_disabled: !enabled,
            },
        });
    }
}

fn scan_codex(home: &Path, out: &mut Vec<McpRecord>, warnings: &mut Vec<String>) {
    let path = codex_config_path(home);
    if !path.exists() {
        return;
    }
    let doc = match read_toml_doc(&path) {
        Ok(d) => d,
        Err(e) => {
            warnings.push(format!("Codex 配置读取失败：{e:#}"));
            return;
        }
    };
    let Some(servers) = doc.get("mcp_servers").and_then(|i| i.as_table_like()) else {
        return;
    };
    let loc = Loc {
        client: Client::Codex,
        scope: "user",
        scope_path: None,
        config_path: &path,
        manageable: true,
    };
    for (name, item) in servers.iter() {
        let v = toml_item_to_json(item);
        let native_disabled = v.get("enabled").and_then(|b| b.as_bool()) == Some(false);
        out.push(McpRecord {
            server: from_codex(&loc, name, &v, !native_disabled),
            origin: Origin::Live { native_disabled },
        });
    }
}

fn archived_server(a: &DisabledMcp) -> Option<McpServer> {
    let config_path = PathBuf::from(&a.config_path);
    let manageable = match (a.client, a.scope.as_str()) {
        (Client::ClaudeCode, "local") => a
            .scope_path
            .as_deref()
            .is_some_and(|p| Path::new(p).is_dir()),
        _ => true,
    };
    let loc = Loc {
        client: a.client,
        scope: &a.scope,
        scope_path: a.scope_path.as_deref(),
        config_path: &config_path,
        manageable,
    };
    let mut s = match a.format {
        ArchiveFormat::Json | ArchiveFormat::ClaudeCli => {
            from_json(&loc, &a.name, &a.config, false)
        }
        ArchiveFormat::Toml => {
            let snippet = a.config.as_str()?;
            let (_, item) = toml_from_snippet(snippet, &["mcp_servers"], &a.name).ok()?;
            from_codex(&loc, &a.name, &toml_item_to_json(&item), false)
        }
    };
    s.id = a.id.clone();
    Some(s)
}

pub(crate) fn scan_records(
    home: &Path,
    store: &DisabledStore,
    plugins: &[PluginInfo],
    warnings: &mut Vec<String>,
) -> Vec<McpRecord> {
    let mut out = Vec::new();
    scan_claude_code(home, plugins, &mut out, warnings);
    push_json_file(
        &mut out,
        warnings,
        Client::ClaudeDesktop,
        &desktop_config_path(home),
        &[],
    );
    scan_desktop_extensions(home, &mut out);
    scan_codex(home, &mut out, warnings);
    push_json_file(
        &mut out,
        warnings,
        Client::Gemini,
        &gemini_settings_path(home),
        &gemini_excluded(home),
    );
    push_json_file(
        &mut out,
        warnings,
        Client::Cursor,
        &cursor_config_path(home),
        &[],
    );

    for a in &store.mcp {
        if out.iter().any(|r| r.server.id == a.id) {
            warnings.push(format!(
                "{} 的 MCP 服务「{}」已经重新出现在配置里，本工具保存的停用存档没有用到。再次停用时会覆盖这份存档。",
                client_label(a.client),
                a.name
            ));
            continue;
        }
        match archived_server(a) {
            Some(s) => out.push(McpRecord {
                server: s,
                origin: Origin::Archived,
            }),
            None => warnings.push(format!(
                "MCP 服务「{}」的停用存档已损坏，无法恢复。",
                a.name
            )),
        }
    }
    out
}

// ───────────────────────────── 开关 ─────────────────────────────

pub(crate) fn set_enabled(
    home: &Path,
    state_dir: &Path,
    id: &str,
    enabled: bool,
    cli: &dyn ClaudeCli,
) -> Result<()> {
    let mut store = DisabledStore::load(state_dir)?;
    let plugins = crate::plugins::scan_plugins(home, &mut Vec::new());
    let recs = scan_records(home, &store, &plugins, &mut Vec::new());
    let rec = recs
        .into_iter()
        .find(|r| r.server.id == id)
        .ok_or_else(|| anyhow!("找不到这个 MCP 服务，可能已经被删除了，请刷新后再试。"))?;
    let s = &rec.server;
    if !s.manageable {
        match s.scope.as_str() {
            "project" => bail!(
                "「{}」是项目里 .mcp.json 配置的服务，只能在项目里修改，这里不能开关。",
                s.name
            ),
            "plugin" => bail!(
                "「{}」是插件或扩展自带的服务，请在插件管理里开关整个插件。",
                s.name
            ),
            "local" => bail!(
                "「{}」所在的项目目录已经不存在，无法通过 claude 命令开关。",
                s.name
            ),
            _ => bail!("「{}」不能在这里开关。", s.name),
        }
    }
    match (enabled, rec.origin) {
        (false, Origin::Archived)
        | (
            false,
            Origin::Live {
                native_disabled: true,
            },
        ) => Ok(()),
        (
            true,
            Origin::Live {
                native_disabled: false,
            },
        ) => Ok(()),
        (
            true,
            Origin::Live {
                native_disabled: true,
            },
        ) => clear_native_disabled(home, state_dir, s),
        (false, Origin::Live { .. }) => match s.client {
            Client::ClaudeCode => cli_disable(home, state_dir, &mut store, s, cli),
            Client::Codex => toml_disable(home, state_dir, &mut store, s),
            _ => json_disable(home, state_dir, &mut store, s),
        },
        (true, Origin::Archived) => {
            let a = store
                .mcp
                .iter()
                .find(|m| m.id == id)
                .cloned()
                .ok_or_else(|| anyhow!("找不到停用存档"))?;
            match a.format {
                ArchiveFormat::ClaudeCli => cli_enable(home, state_dir, &mut store, &a, cli),
                ArchiveFormat::Toml => toml_enable(home, state_dir, &mut store, &a),
                ArchiveFormat::Json => json_enable(home, state_dir, &mut store, &a),
            }
        }
    }
}

fn archive_entry(
    s: &McpServer,
    format: ArchiveFormat,
    config: Value,
    keys: &[String],
    idx: usize,
) -> DisabledMcp {
    DisabledMcp {
        id: s.id.clone(),
        client: s.client,
        scope: s.scope.clone(),
        scope_path: s.scope_path.clone(),
        name: s.name.clone(),
        config_path: s.config_path.clone(),
        format,
        config,
        prev_key: idx.checked_sub(1).and_then(|i| keys.get(i).cloned()),
        next_key: keys.get(idx + 1).cloned(),
        index: idx,
        disabled_at: now_rfc3339(),
    }
}

fn save_archive(state_dir: &Path, store: &mut DisabledStore, entry: DisabledMcp) -> Result<()> {
    store.mcp.retain(|m| m.id != entry.id);
    store.mcp.push(entry);
    store.save(state_dir)
}

fn drop_archive(state_dir: &Path, store: &mut DisabledStore, id: &str) -> Result<()> {
    store.mcp.retain(|m| m.id != id);
    store.save(state_dir)
}

fn insert_index(keys: &[String], prev: Option<&str>, next: Option<&str>, index: usize) -> usize {
    prev.and_then(|p| keys.iter().position(|k| k == p).map(|i| i + 1))
        .or_else(|| next.and_then(|n| keys.iter().position(|k| k == n)))
        .unwrap_or(index.min(keys.len()))
}

fn json_disable(
    home: &Path,
    state_dir: &Path,
    store: &mut DisabledStore,
    s: &McpServer,
) -> Result<()> {
    let path = PathBuf::from(&s.config_path);
    let mut doc = JsonDoc::read(&path)?;
    let servers = doc
        .value
        .get_mut("mcpServers")
        .and_then(|m| m.as_object_mut())
        .ok_or_else(|| anyhow!("配置文件里没有 mcpServers"))?;
    let keys: Vec<String> = servers.keys().cloned().collect();
    let idx = keys
        .iter()
        .position(|k| *k == s.name)
        .ok_or_else(|| anyhow!("配置里已经没有「{}」了，请刷新后再试。", s.name))?;
    let raw = servers.shift_remove(&s.name).unwrap_or(Value::Null);
    backup_file(home, state_dir, &path)?;
    save_archive(
        state_dir,
        store,
        archive_entry(s, ArchiveFormat::Json, raw, &keys, idx),
    )?;
    if let Err(e) = doc.write(&path) {
        drop_archive(state_dir, store, &s.id).ok();
        return Err(e);
    }
    Ok(())
}

fn json_enable(
    home: &Path,
    state_dir: &Path,
    store: &mut DisabledStore,
    a: &DisabledMcp,
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
        .ok_or_else(|| anyhow!("配置文件格式不对：{}", path.display()))?;
    if !root.get("mcpServers").is_some_and(|v| v.is_object()) {
        root.insert("mcpServers".into(), Value::Object(Default::default()));
    }
    let servers = root
        .get_mut("mcpServers")
        .and_then(|m| m.as_object_mut())
        .ok_or_else(|| anyhow!("配置文件格式不对"))?;
    if servers.contains_key(&a.name) {
        bail!(
            "配置里已经有同名的服务「{}」，没有覆盖。请先处理重名后再启用。",
            a.name
        );
    }
    let keys: Vec<String> = servers.keys().cloned().collect();
    let idx = insert_index(&keys, a.prev_key.as_deref(), a.next_key.as_deref(), a.index);
    servers.shift_insert(idx, a.name.clone(), a.config.clone());
    backup_file(home, state_dir, &path)?;
    doc.write(&path)?;
    drop_archive(state_dir, store, &a.id)
}

fn toml_disable(
    home: &Path,
    state_dir: &Path,
    store: &mut DisabledStore,
    s: &McpServer,
) -> Result<()> {
    let path = PathBuf::from(&s.config_path);
    let mut doc = read_toml_doc(&path)?;
    let table = doc
        .get_mut("mcp_servers")
        .and_then(|i| i.as_table_mut())
        .ok_or_else(|| anyhow!("Codex 配置里的 mcp_servers 格式特殊，暂不支持自动停用。"))?;
    let keys: Vec<String> = table.iter().map(|(k, _)| k.to_string()).collect();
    let idx = keys
        .iter()
        .position(|k| *k == s.name)
        .ok_or_else(|| anyhow!("配置里已经没有「{}」了，请刷新后再试。", s.name))?;
    let (key, item) = table
        .remove_entry(&s.name)
        .ok_or_else(|| anyhow!("删除条目失败"))?;
    let snippet = toml_snippet(&["mcp_servers"], &key, &item);
    backup_file(home, state_dir, &path)?;
    save_archive(
        state_dir,
        store,
        archive_entry(s, ArchiveFormat::Toml, Value::String(snippet), &keys, idx),
    )?;
    if let Err(e) = atomic_write(&path, doc.to_string().as_bytes()) {
        drop_archive(state_dir, store, &s.id).ok();
        return Err(e);
    }
    Ok(())
}

fn toml_enable(
    home: &Path,
    state_dir: &Path,
    store: &mut DisabledStore,
    a: &DisabledMcp,
) -> Result<()> {
    let path = PathBuf::from(&a.config_path);
    let mut doc = if path.exists() {
        read_toml_doc(&path)?
    } else {
        toml_edit::DocumentMut::new()
    };
    let snippet = a.config.as_str().ok_or_else(|| anyhow!("存档内容已损坏"))?;
    let (key, item) = toml_from_snippet(snippet, &["mcp_servers"], &a.name)?;
    if doc.get("mcp_servers").is_none() {
        let mut t = toml_edit::Table::new();
        t.set_implicit(true);
        doc.insert("mcp_servers", toml_edit::Item::Table(t));
    }
    let table = doc
        .get_mut("mcp_servers")
        .and_then(|i| i.as_table_mut())
        .ok_or_else(|| anyhow!("Codex 配置里的 mcp_servers 格式特殊，暂不支持自动启用。"))?;
    if table.contains_key(&a.name) {
        bail!("Codex 配置里已经有同名的服务「{}」，没有覆盖。", a.name);
    }
    toml_insert_at(
        table,
        key,
        item,
        a.prev_key.as_deref(),
        a.next_key.as_deref(),
    );
    backup_file(home, state_dir, &path)?;
    atomic_write(&path, doc.to_string().as_bytes())?;
    drop_archive(state_dir, store, &a.id)
}

/// 客户端自己的停用标记：Codex `enabled = false`、JSON `"disabled": true`、Gemini `mcp.excluded`
fn clear_native_disabled(home: &Path, state_dir: &Path, s: &McpServer) -> Result<()> {
    let path = PathBuf::from(&s.config_path);
    match s.client {
        Client::Codex => {
            let mut doc = read_toml_doc(&path)?;
            let entry = doc
                .get_mut("mcp_servers")
                .and_then(|i| i.as_table_like_mut())
                .and_then(|t| t.get_mut(&s.name))
                .and_then(|i| i.as_table_like_mut())
                .ok_or_else(|| anyhow!("配置里已经没有「{}」了", s.name))?;
            entry.remove("enabled");
            backup_file(home, state_dir, &path)?;
            atomic_write(&path, doc.to_string().as_bytes())
        }
        Client::ClaudeDesktop | Client::Gemini | Client::Cursor => {
            let mut doc = JsonDoc::read(&path)?;
            let mut changed = false;
            if let Some(e) = doc
                .value
                .get_mut("mcpServers")
                .and_then(|m| m.get_mut(&s.name))
                .and_then(|e| e.as_object_mut())
            {
                changed |= e.shift_remove("disabled").is_some();
            }
            if let Some(Value::Array(ex)) =
                doc.value.get_mut("mcp").and_then(|m| m.get_mut("excluded"))
            {
                let before = ex.len();
                ex.retain(|x| x.as_str() != Some(&s.name));
                changed |= ex.len() != before;
            }
            if !changed {
                return Ok(());
            }
            backup_file(home, state_dir, &path)?;
            doc.write(&path)
        }
        Client::ClaudeCode => bail!(
            "「{}」被 Claude Code 自己停用了，请在 Claude Code 里用 /mcp 打开。",
            s.name
        ),
    }
}

/// 从 `~/.claude.json` 里取出某个服务的原始配置
fn claude_raw(
    home: &Path,
    scope: &str,
    scope_path: Option<&str>,
    name: &str,
) -> Result<Option<Value>> {
    let Some(cj) = read_claude_json(home)? else {
        return Ok(None);
    };
    let servers = match scope {
        "user" => cj.mcp_servers,
        "local" => scope_path
            .and_then(|p| cj.projects.as_ref().and_then(|ps| ps.get(p)))
            .and_then(|p| p.mcp_servers.clone()),
        _ => None,
    };
    Ok(servers.and_then(|m| m.get(name).cloned()))
}

/// 错误信息里不能带出密钥：把配置里的 env / headers 值打码
fn sanitize(text: &str, config: &Value) -> String {
    let mut out: String = text.trim().lines().take(3).collect::<Vec<_>>().join(" ");
    for key in ["env", "headers"] {
        if let Some(m) = config.get(key).and_then(|v| v.as_object()) {
            for v in m.values().filter_map(|v| v.as_str()) {
                if v.chars().count() >= 4 {
                    let masked: String = v.chars().take(4).collect::<String>() + "…";
                    out = out.replace(v, &masked);
                }
            }
        }
    }
    if out.chars().count() > 300 {
        out = out.chars().take(300).collect::<String>() + "…";
    }
    out
}

fn cli_cwd(home: &Path, scope: &str, scope_path: Option<&str>) -> Result<PathBuf> {
    match scope {
        "local" => {
            let p = PathBuf::from(scope_path.ok_or_else(|| anyhow!("缺少项目路径"))?);
            if !p.is_dir() {
                bail!("项目目录已经不存在：{}", p.display());
            }
            Ok(p)
        }
        _ => Ok(home.to_path_buf()),
    }
}

fn cli_disable(
    home: &Path,
    state_dir: &Path,
    store: &mut DisabledStore,
    s: &McpServer,
    cli: &dyn ClaudeCli,
) -> Result<()> {
    if s.scope != "user" && s.scope != "local" {
        bail!("「{}」不能在这里开关。", s.name);
    }
    let raw = claude_raw(home, &s.scope, s.scope_path.as_deref(), &s.name)?
        .ok_or_else(|| anyhow!("配置里已经没有「{}」了，请刷新后再试。", s.name))?;
    let cwd = cli_cwd(home, &s.scope, s.scope_path.as_deref())?;
    backup_file(home, state_dir, &claude_json_path(home))?;
    save_archive(
        state_dir,
        store,
        archive_entry(s, ArchiveFormat::ClaudeCli, raw.clone(), &[], 0),
    )?;
    let args: Vec<String> = vec![
        "mcp".into(),
        "remove".into(),
        s.name.clone(),
        "-s".into(),
        s.scope.clone(),
    ];
    let out = match cli.run(&args, &cwd) {
        Ok(o) => o,
        Err(e) => {
            drop_archive(state_dir, store, &s.id).ok();
            return Err(e);
        }
    };
    if !out.success {
        drop_archive(state_dir, store, &s.id).ok();
        let msg = if out.stderr.trim().is_empty() {
            &out.stdout
        } else {
            &out.stderr
        };
        bail!("停用失败，claude 命令返回：{}", sanitize(msg, &raw));
    }
    if claude_raw(home, &s.scope, s.scope_path.as_deref(), &s.name)?.is_some() {
        drop_archive(state_dir, store, &s.id).ok();
        bail!(
            "claude 命令执行完了，但配置里仍然有「{}」，请稍后重试。",
            s.name
        );
    }
    Ok(())
}

fn cli_enable(
    home: &Path,
    state_dir: &Path,
    store: &mut DisabledStore,
    a: &DisabledMcp,
    cli: &dyn ClaudeCli,
) -> Result<()> {
    if claude_raw(home, &a.scope, a.scope_path.as_deref(), &a.name)?.is_some() {
        bail!("Claude Code 里已经有同名的服务「{}」，没有覆盖。", a.name);
    }
    let cwd = cli_cwd(home, &a.scope, a.scope_path.as_deref())?;
    let json = serde_json::to_string(&a.config)?;
    backup_file(home, state_dir, &claude_json_path(home))?;
    let args: Vec<String> = vec![
        "mcp".into(),
        "add-json".into(),
        a.name.clone(),
        json,
        "-s".into(),
        a.scope.clone(),
    ];
    let out = cli.run(&args, &cwd)?;
    if !out.success {
        let msg = if out.stderr.trim().is_empty() {
            &out.stdout
        } else {
            &out.stderr
        };
        bail!("启用失败，claude 命令返回：{}", sanitize(msg, &a.config));
    }
    if claude_raw(home, &a.scope, a.scope_path.as_deref(), &a.name)?.is_none() {
        bail!(
            "claude 命令执行完了，但配置里没有出现「{}」，请稍后重试。",
            a.name
        );
    }
    drop_archive(state_dir, store, &a.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_detection() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            detect_package(Some("npx"), &a(&["-y", "@playwright/mcp@latest"])).as_deref(),
            Some("@playwright/mcp@latest")
        );
        assert_eq!(
            detect_package(Some("/opt/homebrew/bin/uvx"), &a(&["mcp-server-git"])).as_deref(),
            Some("mcp-server-git")
        );
        assert_eq!(
            detect_package(Some("pnpm"), &a(&["dlx", "--silent", "foo"])).as_deref(),
            Some("foo")
        );
        assert_eq!(detect_package(Some("pnpm"), &a(&["run", "foo"])), None);
        assert_eq!(detect_package(Some("node"), &a(&["server.js"])), None);
    }

    #[test]
    fn transports() {
        assert_eq!(transport_of(None, Some("https://x.com/sse")), "sse");
        assert_eq!(transport_of(None, Some("https://x.com/sse/?a=1")), "sse");
        assert_eq!(transport_of(None, Some("https://x.com/mcp")), "http");
        assert_eq!(transport_of(Some("sse"), Some("https://x.com/mcp")), "sse");
        assert_eq!(
            transport_of(Some("http"), Some("https://x.com/sse")),
            "http"
        );
        assert_eq!(transport_of(None, None), "stdio");
    }

    #[test]
    fn sanitize_masks_secrets() {
        let cfg = serde_json::json!({"env": {"API_KEY": "sk-abcdef123456"}});
        let s = sanitize("error: bad value sk-abcdef123456 here", &cfg);
        assert!(!s.contains("sk-abcdef123456"));
        assert!(s.contains("sk-a…"));
    }
}

//! Cline / Roo Code / Kilo Code 扩展（同一套任务目录格式），以及 Cline 新版的 SDK 会话：
//! - 扩展：`~/Library/Application Support/<编辑器>/User/globalStorage/<扩展 id>/tasks/<任务 id>/ui_messages.json`，
//!   扩展 id 分别是 `saoudrizwan.claude-dev`、`rooveterinaryinc.roo-cline`、`kilocode.kilo-code`；
//!   Cline 的 JetBrains / 独立版写在 `~/.cline/data/tasks/`（`$CLINE_DATA_DIR`、`$CLINE_DIR/data`）。
//!   `ui_messages.json` 里 `say:"api_req_started"` 的 `text` 是 JSON 字符串：`tokensIn`、`tokensOut`、
//!   `cacheWrites`、`cacheReads`、`cost`；同一条先写占位再原地补上用量，没有用量的跳过。
//!   `subagent_usage`（子代理汇总）和 `deleted_api_reqs`（回滚检查点时被删请求的汇总）也是真实用量，一起计。
//! - Cline SDK 会话（Cline CLI、4.x 扩展）：`~/.cline/data/sessions/<id>/<id>.messages.json`
//!   （子代理是同目录下的 `<代理>.messages.json`），`messages[].metrics.{inputTokens,outputTokens,
//!   cacheReadTokens,cacheWriteTokens,cost}`，`inputTokens` 含缓存读写。
//!
//! `tokensIn` 含不含缓存因来源而异：Cline 里 OpenAI 兼容一类的提供方含缓存读，其余（Anthropic、
//! OpenRouter、Cline 自家等）不含；Roo / Kilo 从 3.29.5 起总是含缓存读写，更早的 Anthropic 协议不含——
//! 用 `tokensIn >= 缓存读 + 缓存写` 来判断。

use std::collections::HashMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::discover::{self, Ctx, Found, Kind, Source};
use crate::record::{clean_title, fnv64, num_to_ms, parse_time_str, usd_to_cost, FileEntry, Rec};
use crate::Tool;

/// macOS 上常见的 VS Code 系编辑器
const EDITORS: [&str; 10] = [
    "Code",
    "Code - Insiders",
    "Cursor",
    "Windsurf",
    "VSCodium",
    "Positron",
    "Antigravity",
    "Kiro",
    "Trae",
    "Void",
];

pub(crate) fn extension_id(tool: Tool) -> &'static str {
    match tool {
        Tool::Roo => "rooveterinaryinc.roo-cline",
        Tool::Kilo => "kilocode.kilo-code",
        _ => "saoudrizwan.claude-dev",
    }
}

/// Cline 独立版 / CLI 的数据目录
fn cline_data(ctx: &Ctx) -> PathBuf {
    ctx.path_var("CLINE_DATA_DIR")
        .or_else(|| ctx.path_var("CLINE_DIR").map(|d| d.join("data")))
        .unwrap_or_else(|| ctx.home.join(".cline").join("data"))
}

fn cline_sessions(ctx: &Ctx) -> PathBuf {
    ctx.path_var("CLINE_SESSION_DATA_DIR")
        .unwrap_or_else(|| cline_data(ctx).join("sessions"))
}

/// 各编辑器下这个扩展的 globalStorage 目录（第一个是展示用的默认路径）
fn storage_roots(ctx: &Ctx, tool: Tool) -> Vec<PathBuf> {
    let id = extension_id(tool);
    let mut v: Vec<PathBuf> = EDITORS
        .iter()
        .map(|e| {
            ctx.app_support()
                .join(e)
                .join("User/globalStorage")
                .join(id)
        })
        .collect();
    if tool == Tool::Cline {
        v.push(cline_data(ctx));
    }
    v
}

/// Cline 的 `state/taskHistory.json`：任务 id → 任务开始时的工作目录
fn task_history_cwds(root: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(bytes) = std::fs::read(root.join("state").join("taskHistory.json")) else {
        return map;
    };
    let Ok(Value::Array(items)) = serde_json::from_slice::<Value>(&bytes) else {
        return map;
    };
    for it in items {
        if let (Some(id), Some(cwd)) = (
            it.get("id").and_then(Value::as_str),
            it.get("cwdOnTaskInitialization").and_then(Value::as_str),
        ) {
            if !cwd.is_empty() {
                map.insert(id.to_string(), cwd.to_string());
            }
        }
    }
    map
}

/// 扩展的任务目录。Kilo 的 CLI 部分在 `opencode` 模块。
pub(crate) fn discover_tasks(ctx: &Ctx, tool: Tool, out: &mut Vec<Found>, src: &mut Source) {
    for root in storage_roots(ctx, tool) {
        let tasks = root.join("tasks");
        let dirs = discover::subdirs(&tasks, &mut src.errors);
        if dirs.is_empty() {
            continue;
        }
        let cwds = task_history_cwds(&root);
        for d in dirs {
            let p = d.join("ui_messages.json");
            if !p.is_file() {
                continue;
            }
            let id = d.file_name().map(|s| s.to_string_lossy().into_owned());
            let project = id.and_then(|id| cwds.get(&id).cloned());
            discover::push_with(
                tool,
                Kind::ClineTask,
                p,
                project,
                discover::stat,
                out,
                &mut src.errors,
            );
        }
    }
}

pub(crate) fn discover_cline(ctx: &Ctx, out: &mut Vec<Found>) -> Source {
    let mut src = Source::new(Tool::Cline, storage_roots(ctx, Tool::Cline).remove(0));
    discover_tasks(ctx, Tool::Cline, out, &mut src);
    for d in discover::subdirs(&cline_sessions(ctx), &mut src.errors) {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        let mut files: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(".messages.json"))
            })
            .collect();
        files.sort();
        discover::push_found(Tool::Cline, Kind::ClineCli, files, out, &mut src.errors);
    }
    src
}

pub(crate) fn discover_roo(ctx: &Ctx, out: &mut Vec<Found>) -> Source {
    let mut src = Source::new(Tool::Roo, storage_roots(ctx, Tool::Roo).remove(0));
    discover_tasks(ctx, Tool::Roo, out, &mut src);
    src
}

// ---------- 任务目录 ----------

#[derive(Deserialize)]
struct UiMessage {
    ts: Option<Value>,
    say: Option<String>,
    text: Option<String>,
    #[serde(rename = "modelInfo")]
    model_info: Option<ModelInfo>,
    #[serde(rename = "contextCondense")]
    context_condense: Option<Condense>,
}

#[derive(Deserialize, Clone)]
struct ModelInfo {
    #[serde(rename = "providerId")]
    provider_id: Option<String>,
    #[serde(rename = "modelId")]
    model_id: Option<String>,
}

#[derive(Deserialize)]
struct Condense {
    cost: Option<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReqInfo {
    tokens_in: Option<u64>,
    tokens_out: Option<u64>,
    cache_writes: Option<u64>,
    cache_reads: Option<u64>,
    cost: Option<f64>,
}

/// Cline 里 `tokensIn` 是原始 `prompt_tokens`（含缓存读）的提供方
const CLINE_INCLUSIVE: [&str; 11] = [
    "openai",
    "litellm",
    "xai",
    "zai",
    "qwen",
    "fireworks",
    "baseten",
    "cerebras",
    "doubao",
    "lmstudio",
    "hicap",
];

/// `task_metadata.json` 的 `model_usage`：(时间, 模型)，模型或模式变化时才追加一条
fn model_usage(task_dir: &Path) -> Vec<(i64, String)> {
    let Ok(bytes) = std::fs::read(task_dir.join("task_metadata.json")) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_slice::<Value>(&bytes) else {
        return Vec::new();
    };
    let mut out: Vec<(i64, String)> = v
        .get("model_usage")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let ts = m.get("ts").and_then(Value::as_f64).and_then(num_to_ms)?;
            let id = m.get("model_id").and_then(Value::as_str)?;
            (!id.is_empty()).then(|| (ts, id.to_string()))
        })
        .collect();
    out.sort_by_key(|x| x.0);
    out
}

/// Roo / Kilo：用户消息的环境信息里有 `<model>…</model>`，带时间
fn env_models(task_dir: &Path) -> Vec<(i64, String)> {
    #[derive(Deserialize)]
    struct Hist {
        role: Option<String>,
        ts: Option<Value>,
        content: Option<Value>,
    }
    let Ok(bytes) = std::fs::read(task_dir.join("api_conversation_history.json")) else {
        return Vec::new();
    };
    let Ok(items) = serde_json::from_slice::<Vec<Hist>>(&bytes) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for h in items {
        if h.role.as_deref() != Some("user") {
            continue;
        }
        let Some(ts) = h.ts.as_ref().and_then(crate::record::parse_time_value) else {
            continue;
        };
        let texts: Vec<&str> = match &h.content {
            Some(Value::String(s)) => vec![s.as_str()],
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect(),
            _ => Vec::new(),
        };
        for t in texts {
            if let Some(i) = t.rfind("<model>") {
                let rest = &t[i + "<model>".len()..];
                if let Some(j) = rest.find("</model>") {
                    let m = rest[..j].trim();
                    if !m.is_empty() {
                        out.push((ts, m.to_string()));
                    }
                }
            }
        }
    }
    out.sort_by_key(|x| x.0);
    out
}

/// 环境信息里的工作目录：`# Current Working Directory (<路径>) Files`（Roo 是 `Workspace`）
fn env_cwd(task_dir: &Path) -> Option<String> {
    let mut buf = Vec::new();
    std::fs::File::open(task_dir.join("api_conversation_history.json"))
        .ok()?
        .take(512 * 1024)
        .read_to_end(&mut buf)
        .ok()?;
    for pat in [
        &b"# Current Working Directory ("[..],
        b"# Current Workspace Directory (",
    ] {
        if let Some(i) = memchr::memmem::find(&buf, pat) {
            let rest = &buf[i + pat.len()..];
            let end = memchr::memmem::find(rest, b") Files")?;
            let p = std::str::from_utf8(&rest[..end]).ok()?.trim();
            if p.starts_with('/') {
                return Some(p.to_string());
            }
        }
    }
    None
}

/// Roo 新版每个任务目录有 `history_item.json`，里面有工作目录
fn history_item_workspace(task_dir: &Path) -> Option<String> {
    let v: Value =
        serde_json::from_slice(&std::fs::read(task_dir.join("history_item.json")).ok()?).ok()?;
    v.get("workspace")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn model_at(list: &[(i64, String)], ts: i64) -> Option<&str> {
    list.iter()
        .rev()
        .find(|(t, _)| *t <= ts)
        .or_else(|| list.first())
        .map(|(_, m)| m.as_str())
}

pub(crate) fn parse_task(
    path: &Path,
    tool: Tool,
    project: Option<String>,
    entry: &mut FileEntry,
) -> io::Result<()> {
    let task_dir = path.parent().unwrap_or(Path::new("."));
    let task_id = task_dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let msgs: Vec<UiMessage> = serde_json::from_slice(&std::fs::read(path)?)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let roo_like = tool != Tool::Cline;
    let session = entry.session_idx(&task_id);
    let project = project
        .or_else(|| roo_like.then(|| history_item_workspace(task_dir)).flatten())
        .or_else(|| env_cwd(task_dir));
    entry.sessions[session as usize].project = project;

    let usage_models = if roo_like {
        env_models(task_dir)
    } else {
        model_usage(task_dir)
    };
    let prefix: &[u8] = match tool {
        Tool::Roo => b"roo",
        Tool::Kilo => b"kilo-ext",
        _ => b"cline",
    };
    let mut current: Option<ModelInfo> = None;
    let mut keys = entry.key_index();
    for m in msgs {
        let Some(ts) = m.ts.as_ref().and_then(crate::record::parse_time_value) else {
            continue;
        };
        if m.model_info
            .as_ref()
            .and_then(|i| i.model_id.as_ref())
            .is_some()
        {
            current = m.model_info.clone();
        }
        let say = m.say.as_deref().unwrap_or("");
        if say == "task" {
            if let Some(t) = m.text.as_deref().and_then(clean_title) {
                entry.sessions[session as usize].set_title(t, false, ts);
            }
            continue;
        }
        let (tokens, cost) = match say {
            "api_req_started" | "api_req_finished" | "subagent_usage" | "deleted_api_reqs" => {
                let Some(info) = m
                    .text
                    .as_deref()
                    .and_then(|t| serde_json::from_str::<ReqInfo>(t).ok())
                else {
                    continue;
                };
                // 占位行（还在请求或已在恢复时作废）没有任何用量字段
                if info.tokens_in.is_none()
                    && info.tokens_out.is_none()
                    && info.cache_reads.is_none()
                    && info.cache_writes.is_none()
                    && info.cost.is_none()
                {
                    continue;
                }
                let tin = info.tokens_in.unwrap_or(0);
                let cr = info.cache_reads.unwrap_or(0);
                let cw = info.cache_writes.unwrap_or(0);
                let input = if roo_like {
                    if tin >= cr + cw {
                        tin - cr - cw
                    } else {
                        tin
                    }
                } else {
                    let provider = m
                        .model_info
                        .as_ref()
                        .or(current.as_ref())
                        .and_then(|i| i.provider_id.as_deref())
                        .unwrap_or("");
                    if CLINE_INCLUSIVE.contains(&provider) && tin >= cr {
                        tin - cr
                    } else {
                        tin
                    }
                };
                (
                    [input, info.tokens_out.unwrap_or(0), cr, cw],
                    info.cost.unwrap_or(0.0),
                )
            }
            // Roo：压缩上下文那次调用只记了费用
            "condense_context" => match m.context_condense.and_then(|c| c.cost) {
                Some(c) if c > 0.0 => ([0; 4], c),
                _ => continue,
            },
            _ => continue,
        };
        let model = m
            .model_info
            .as_ref()
            .and_then(|i| i.model_id.clone())
            .or_else(|| model_at(&usage_models, ts).map(str::to_string))
            .or_else(|| current.as_ref().and_then(|i| i.model_id.clone()))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        let [input, output, cr, cw] = tokens;
        let rec = Rec {
            ts,
            // 同一任务在多个编辑器目录里出现时只算一次
            key: fnv64(&[
                prefix,
                task_id.as_bytes(),
                &ts.to_le_bytes(),
                say.as_bytes(),
            ]),
            model: entry.model_idx(&model),
            session,
            input,
            output,
            cache_read: cr,
            cache_write_5m: cw,
            known_cost: usd_to_cost(cost),
            ..Default::default()
        };
        if !rec.is_empty() || rec.known_cost > 0 {
            entry.push_dedup(&mut keys, rec);
        }
    }
    Ok(())
}

// ---------- Cline SDK 会话 ----------

#[derive(Deserialize)]
#[serde(untagged)]
enum SdkFile {
    Obj {
        #[serde(rename = "sessionId")]
        session_id: Option<String>,
        #[serde(default)]
        messages: Vec<SdkMsg>,
    },
    Arr(Vec<SdkMsg>),
}

#[derive(Deserialize)]
struct SdkMsg {
    id: Option<String>,
    role: Option<String>,
    ts: Option<Value>,
    content: Option<Value>,
    #[serde(rename = "modelInfo")]
    model_info: Option<SdkModel>,
    metrics: Option<SdkMetrics>,
}

#[derive(Deserialize)]
struct SdkModel {
    id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SdkMetrics {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_tokens: u64,
    #[serde(default)]
    cache_write_tokens: u64,
    cost: Option<f64>,
}

#[derive(Deserialize, Default)]
struct Manifest {
    model: Option<String>,
    cwd: Option<String>,
    workspace_root: Option<String>,
    started_at: Option<String>,
    prompt: Option<String>,
    metadata: Option<Value>,
}

pub(crate) fn parse_sdk(path: &Path, entry: &mut FileEntry) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    // 子代理文件也归到所在目录（根会话）
    let root_id = dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let manifest: Manifest = std::fs::read(dir.join(format!("{root_id}.json")))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let file: SdkFile = serde_json::from_slice(&std::fs::read(path)?)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let (file_sid, msgs) = match file {
        SdkFile::Obj {
            session_id,
            messages,
        } => (session_id, messages),
        SdkFile::Arr(m) => (None, m),
    };
    let file_sid = file_sid.unwrap_or_else(|| {
        path.file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".messages.json"))
            .unwrap_or_default()
            .to_string()
    });
    let meta = manifest.metadata.as_ref();
    // 从旧版任务接续过来的会话：最后一条种子消息带着整个旧任务的合计（没有时间），旧任务目录里已经算过
    let migrated = meta.is_some_and(|m| {
        m.get("legacyTask").is_some_and(|v| !v.is_null())
            || m.get("migratedFromLegacyTask")
                .is_some_and(|v| v.as_bool() != Some(false) && !v.is_null())
    });
    let session = entry.session_idx(&root_id);
    {
        let s = &mut entry.sessions[session as usize];
        s.project = manifest
            .cwd
            .clone()
            .or(manifest.workspace_root.clone())
            .filter(|c| !c.is_empty());
        let title = meta
            .and_then(|m| m.get("title"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or(manifest.prompt.clone());
        if let Some(t) = title.as_deref().and_then(clean_title) {
            s.set_title(t, false, 0);
        }
    }
    let started = manifest.started_at.as_deref().and_then(parse_time_str);
    let mut keys = entry.key_index();
    for (i, m) in msgs.into_iter().enumerate() {
        if m.role.as_deref() == Some("user") {
            if entry.sessions[session as usize].title.is_none() {
                if let Some(t) = m
                    .content
                    .as_ref()
                    .and_then(crate::claude::title_from_content)
                {
                    entry.sessions[session as usize].set_title(t, false, 0);
                }
            }
            continue;
        }
        let Some(x) = m.metrics else { continue };
        let own_ts = m.ts.as_ref().and_then(crate::record::parse_time_value);
        if migrated && own_ts.is_none() {
            continue;
        }
        let Some(ts) = own_ts.or(started) else {
            continue;
        };
        let model = m
            .model_info
            .and_then(|mi| mi.id)
            .or(manifest.model.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        let ident = m.id.unwrap_or_else(|| format!("#{i}"));
        let rec = Rec {
            ts,
            key: fnv64(&[b"cline-sdk", file_sid.as_bytes(), ident.as_bytes()]),
            model: entry.model_idx(&model),
            session,
            input: x
                .input_tokens
                .saturating_sub(x.cache_read_tokens)
                .saturating_sub(x.cache_write_tokens),
            output: x.output_tokens,
            cache_read: x.cache_read_tokens,
            cache_write_5m: x.cache_write_tokens,
            known_cost: x.cost.map(usd_to_cost).unwrap_or(0),
            ..Default::default()
        };
        if !rec.is_empty() {
            entry.push_dedup(&mut keys, rec);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::COST_UNITS_PER_USD;

    fn home() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tools/cline-home")
    }

    fn parse_found(f: &Found) -> FileEntry {
        let mut e = FileEntry::new(f.tool, String::new(), 0, 0);
        match f.kind {
            Kind::ClineTask => parse_task(&f.path, f.tool, f.project.clone(), &mut e).unwrap(),
            _ => parse_sdk(&f.path, &mut e).unwrap(),
        }
        e
    }

    #[test]
    fn cline_extension_task() {
        let ctx = Ctx::with_vars(&home(), &[]);
        let mut found = Vec::new();
        let src = discover_cline(&ctx, &mut found);
        assert!(src.errors.is_empty());
        let tasks: Vec<&Found> = found.iter().filter(|f| f.kind == Kind::ClineTask).collect();
        // Code 和 Cursor 里各有一份同一个任务
        assert_eq!(tasks.len(), 2);
        let a = parse_found(tasks[0]);
        let b = parse_found(tasks[1]);
        assert_eq!(
            a.recs.iter().map(|r| r.key).collect::<Vec<_>>(),
            b.recs.iter().map(|r| r.key).collect::<Vec<_>>()
        );
        let s = &a.sessions[0];
        assert_eq!(s.id, "1764899999000");
        assert_eq!(s.title.as_deref(), Some("Fix bug"));
        assert_eq!(s.project.as_deref(), Some("/Users/alice/repo"));
        // 占位行跳过；取消但有用量的照算；子代理汇总、回滚删掉的请求都算
        assert_eq!(a.recs.len(), 5);
        let r = &a.recs[0];
        assert_eq!((r.input, r.output, r.cache_write_5m), (12, 340, 15000));
        assert_eq!(r.known_cost as f64 / COST_UNITS_PER_USD, 0.0614);
        assert_eq!(a.models[r.model as usize], "claude-sonnet-4-5");
        // 没有 modelInfo 的行：按 task_metadata 的 model_usage 找当时的模型
        assert_eq!(a.models[a.recs[1].model as usize], "claude-sonnet-4-5");
        // OpenAI 兼容提供方：tokensIn 含缓存读，要减掉
        let x = &a.recs[4];
        assert_eq!((x.input, x.cache_read), (100, 900));
        assert_eq!(a.models[x.model as usize], "qwen3-coder");
    }

    #[test]
    fn roo_task_input_semantics() {
        let ctx = Ctx::with_vars(&home(), &[]);
        let mut found = Vec::new();
        let src = discover_roo(&ctx, &mut found);
        assert!(src.errors.is_empty());
        assert_eq!(found.len(), 1);
        let e = parse_found(&found[0]);
        assert_eq!(e.sessions[0].project.as_deref(), Some("/Users/alice/roo"));
        assert_eq!(e.recs.len(), 3);
        // 新口径：tokensIn 含缓存读写
        assert_eq!(e.recs[0].input, 12);
        // 旧 Anthropic 口径：tokensIn 不含缓存
        assert_eq!(e.recs[1].input, 12);
        // 压缩上下文：只有费用
        assert_eq!(e.recs[2].input + e.recs[2].output, 0);
        assert_eq!(e.recs[2].known_cost as f64 / COST_UNITS_PER_USD, 0.02);
        // 模型来自环境信息里的 <model>，按时间对应
        assert_eq!(e.models[e.recs[0].model as usize], "claude-sonnet-4-5");
        assert_eq!(e.models[e.recs[1].model as usize], "gpt-5.1");
    }

    #[test]
    fn cline_sdk_sessions() {
        let ctx = Ctx::with_vars(&home(), &[]);
        let mut found = Vec::new();
        discover_cline(&ctx, &mut found);
        let mut sdk: Vec<&Found> = found.iter().filter(|f| f.kind == Kind::ClineCli).collect();
        sdk.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(sdk.len(), 3);
        let lead = parse_found(
            sdk.iter()
                .find(|f| f.path.ends_with("S1/S1.messages.json"))
                .unwrap(),
        );
        assert_eq!(
            lead.sessions[0].project.as_deref(),
            Some("/Users/alice/repo")
        );
        assert_eq!(lead.sessions[0].title.as_deref(), Some("Inspect repo"));
        assert_eq!(lead.recs.len(), 1);
        assert_eq!((lead.recs[0].input, lead.recs[0].cache_read), (50, 15000));
        let sub = parse_found(
            sdk.iter()
                .find(|f| f.path.ends_with("S1/agent1.messages.json"))
                .unwrap(),
        );
        assert_eq!(sub.sessions[0].id, "S1");
        assert_eq!(sub.recs.len(), 1);
        assert_eq!(sub.models, vec!["claude-sonnet-4-5"]);
        // 从旧任务接续的会话：没有时间的合计种子不计
        let mig = parse_found(
            sdk.iter()
                .find(|f| f.path.ends_with("S2/S2.messages.json"))
                .unwrap(),
        );
        assert_eq!(mig.recs.len(), 1);
        assert_eq!(mig.recs[0].output, 7);
    }

    #[test]
    fn missing_dirs_are_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Ctx::with_vars(dir.path(), &[]);
        let mut found = Vec::new();
        let src = discover_cline(&ctx, &mut found);
        assert!(found.is_empty() && src.errors.is_empty());
        assert!(src
            .root
            .ends_with("Code/User/globalStorage/saoudrizwan.claude-dev"));
        assert!(discover_roo(&ctx, &mut found).errors.is_empty());
        let ctx = Ctx::with_vars(dir.path(), &[("CLINE_DIR", "/c")]);
        assert_eq!(cline_sessions(&ctx), PathBuf::from("/c/data/sessions"));
    }
}

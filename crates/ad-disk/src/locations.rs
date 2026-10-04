//! 「按位置」：各 AI 工具的数据目录，按实际子目录拆开并给出能不能删的判断。

use crate::util::{path_id, path_string, rfc3339, DAY, IN_USE_SECS};
use crate::walk::{walk_tree, Node, Stats};
use crate::{DiskItem, Safety};
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 子项至少这么大才单独列出
pub(crate) const LIST_MIN: u64 = 1024 * 1024;
/// 位置树保留的深度
const TREE_DEPTH: usize = 5;

const R_CACHE: &str = "缓存，删除后会自动重建";
const R_UNKNOWN_DIR: &str = "未识别的目录，不确定用途";
const R_UNKNOWN_FILE: &str = "未识别的文件，不确定用途";
const R_ROOT: &str = "这是整个数据目录，里面有配置和登录信息，不能整体删除；请展开后删除具体项目";
const R_WEB_STORAGE: &str = "网页存储（可能含登录状态），删了可能需要重新登录";
const R_TRANSCRIPTS_TAIL: &str = "删除后这段对话无法再继续（resume），用量统计不受影响";
const R_DB: &str = "Codex 正在使用的数据库，直接删除会丢历史或损坏数据";
const R_CONFIG: &str = "配置或登录凭据，删了需要重新设置或登录";
const R_DEPS_REDOWNLOAD: &str = "MCP/浏览器自动化用的依赖，删了下次使用会重新下载";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocId {
    ClaudeCode,
    ClaudeDesktop,
    ClaudeCli,
    ClaudeTmp,
    Codex,
    CodexDesktop,
    ChatCut,
    ChatCutCli,
    Gemini,
    LibCaches,
    DotCache,
    Npx,
}

pub(crate) struct LocDef {
    pub id: LocId,
    pub root: PathBuf,
    pub label: &'static str,
    pub tool: &'static str,
    /// 虚拟分组：只看这些子项（其余不是 AI 工具的，不管）
    pub only: Option<&'static [&'static str]>,
}

const LIB_CACHES_AI: &[&str] = &[
    "com.anthropic.claudefordesktop",
    "com.anthropic.claudefordesktop.ShipIt",
    "claude-cli-nodejs",
    "Codex",
    "com.openai.codex",
    "com.openai.sky.CUAService",
    "com.openai.sky.CUAService.cli",
    "io.chatcut.desktop",
    "io.chatcut.desktop.ShipIt",
    "chatcut-desktop-updater",
    "ms-playwright",
    "ms-playwright-mcp",
];

const DOT_CACHE_AI: &[&str] = &[
    "uv",
    "codex-runtimes",
    "hyperframes",
    "chrome-devtools-mcp",
    "claude",
];

pub(crate) struct Env {
    pub home: PathBuf,
    /// Claude Code 的临时目录 /private/tmp/claude-<uid>（只在扫描真实 $HOME 时启用）
    pub claude_tmp: Option<PathBuf>,
    pub now: i64,
}

impl Env {
    pub fn new(home: &Path) -> Env {
        let home = home.to_path_buf();
        let claude_tmp = if is_real_home(&home) {
            // SAFETY: getuid 没有副作用
            let uid = unsafe { libc::getuid() };
            let p = PathBuf::from(format!("/private/tmp/claude-{uid}"));
            p.is_dir().then_some(p)
        } else {
            None
        };
        Env {
            home,
            claude_tmp,
            now: crate::util::now_secs(),
        }
    }
}

fn is_real_home(home: &Path) -> bool {
    let Some(h) = std::env::var_os("HOME") else {
        return false;
    };
    let a = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    let b = std::fs::canonicalize(&h).unwrap_or_else(|_| PathBuf::from(&h));
    a == b
}

pub(crate) fn location_defs(env: &Env) -> Vec<LocDef> {
    let h = &env.home;
    let lib = h.join("Library");
    let mut v = vec![
        LocDef {
            id: LocId::ClaudeCode,
            root: h.join(".claude"),
            label: "Claude Code",
            tool: "claude",
            only: None,
        },
        LocDef {
            id: LocId::ClaudeDesktop,
            root: lib.join("Application Support/Claude"),
            label: "Claude 桌面版",
            tool: "claude",
            only: None,
        },
        LocDef {
            id: LocId::ClaudeCli,
            root: h.join(".local/share/claude"),
            label: "Claude Code 程序",
            tool: "claude",
            only: None,
        },
    ];
    if let Some(t) = &env.claude_tmp {
        v.push(LocDef {
            id: LocId::ClaudeTmp,
            root: t.clone(),
            label: "Claude Code 临时文件",
            tool: "claude",
            only: None,
        });
    }
    v.extend([
        LocDef {
            id: LocId::Codex,
            root: h.join(".codex"),
            label: "Codex",
            tool: "codex",
            only: None,
        },
        LocDef {
            id: LocId::CodexDesktop,
            root: lib.join("Application Support/Codex"),
            label: "Codex 桌面版",
            tool: "codex",
            only: None,
        },
        LocDef {
            id: LocId::ChatCut,
            root: lib.join("Application Support/ChatCut"),
            label: "ChatCut",
            tool: "chatcut",
            only: None,
        },
        LocDef {
            id: LocId::ChatCutCli,
            root: h.join(".chatcut"),
            label: "ChatCut 命令行",
            tool: "chatcut",
            only: None,
        },
        LocDef {
            id: LocId::Gemini,
            root: h.join(".gemini"),
            label: "Gemini",
            tool: "gemini",
            only: None,
        },
        LocDef {
            id: LocId::LibCaches,
            root: lib.join("Caches"),
            label: "AI 应用的系统缓存",
            tool: "other",
            only: Some(LIB_CACHES_AI),
        },
        LocDef {
            id: LocId::DotCache,
            root: h.join(".cache"),
            label: "AI 工具的下载缓存",
            tool: "other",
            only: Some(DOT_CACHE_AI),
        },
        LocDef {
            id: LocId::Npx,
            root: h.join(".npm/_npx"),
            label: "npx 临时工具",
            tool: "other",
            only: None,
        },
    ]);
    v
}

// ---------------------------------------------------------------- 上下文

#[derive(Default)]
pub(crate) struct Ctx {
    pub now: i64,
    /// Claude 项目编码目录名 → cwd
    pub claude_enc: HashMap<String, String>,
    /// 会话 id → (标题, cwd)
    pub sessions: HashMap<String, (Option<String>, Option<String>)>,
    /// cwd → 第一个有标题的对话
    pub title_by_cwd: HashMap<String, String>,
    /// Gemini tmp 下的目录名 → cwd
    pub gemini_dirs: HashMap<String, String>,
    /// ~/.local/bin/claude 指向的版本目录名
    pub current_claude_cli: Option<String>,
}

impl Ctx {
    fn session_title(&self, id: &str) -> Option<String> {
        self.sessions.get(id).and_then(|(t, _)| t.clone())
    }
    fn session_cwd(&self, id: &str) -> Option<String> {
        self.sessions.get(id).and_then(|(_, c)| c.clone())
    }
}

pub(crate) fn current_claude_cli(home: &Path) -> Option<String> {
    let target = std::fs::read_link(home.join(".local/bin/claude")).ok()?;
    let parent = target.parent()?;
    if parent.file_name()? == "versions" {
        Some(target.file_name()?.to_string_lossy().into_owned())
    } else {
        None
    }
}

// ---------------------------------------------------------------- 分类结果

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Expand {
    No,
    Children,
    /// 跳过中间层，把第 n 层的后代当作子项
    Flatten(usize),
}

#[derive(Debug, Clone)]
pub(crate) struct Class {
    pub label: String,
    pub category: &'static str,
    pub tool: Option<&'static str>,
    pub safety: Safety,
    pub reason: String,
    pub expand: Expand,
    pub project: Option<String>,
    pub title: Option<String>,
}

fn cl(
    label: impl Into<String>,
    category: &'static str,
    safety: Safety,
    reason: impl Into<String>,
) -> Class {
    Class {
        label: label.into(),
        category,
        tool: None,
        safety,
        reason: reason.into(),
        expand: Expand::No,
        project: None,
        title: None,
    }
}

impl Class {
    fn expand(mut self, e: Expand) -> Self {
        self.expand = e;
        self
    }
    fn tool(mut self, t: &'static str) -> Self {
        self.tool = Some(t);
        self
    }
    fn project(mut self, p: Option<String>) -> Self {
        self.project = p;
        self
    }
}

fn cache(label: impl Into<String>) -> Class {
    cl(label, "cache", Safety::Safe, R_CACHE)
}

fn unknown(name: &str, node: &Node) -> Class {
    let reason = if node.is_dir {
        R_UNKNOWN_DIR
    } else {
        R_UNKNOWN_FILE
    };
    cl(name, "other", Safety::Review, reason)
}

fn protected_config(name: &str) -> Class {
    cl(name, "config", Safety::Protected, R_CONFIG)
}

fn age_days(ctx: &Ctx, node: &Node) -> i64 {
    if node.stats.newest <= 0 {
        return 0;
    }
    (ctx.now - node.stats.newest).max(0) / DAY
}

fn in_use(ctx: &Ctx, node: &Node) -> bool {
    node.stats.newest > 0 && ctx.now - node.stats.newest < IN_USE_SECS
}

pub(crate) fn is_db_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let base = n
        .trim_end_matches("-wal")
        .trim_end_matches("-shm")
        .trim_end_matches("-journal");
    base.ends_with(".sqlite") || base.ends_with(".sqlite3") || base.ends_with(".db")
}

pub(crate) fn is_config_name(name: &str) -> bool {
    matches!(
        name,
        "auth.json"
            | ".credentials.json"
            | "credentials.json"
            | "config.toml"
            | "config.json"
            | "settings.json"
            | "settings.local.json"
            | "oauth_creds.json"
            | "google_accounts.json"
            | "mcp-oauth-tokens-v2.json"
            | "mcp-oauth-tokens.json"
            | "auth-token.json"
            | "claude_desktop_config.json"
            | ".claude.json"
            | "installation_id"
            | "Local State"
            | "Preferences"
    ) || name.ends_with(".key")
        || name.ends_with(".pem")
}

fn short_id(s: &str) -> String {
    s.chars().take(8).collect()
}

fn folder_name(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string())
}

/// 版本号比较："2.1.286" > "2.1.284"
fn version_key(s: &str) -> Vec<u64> {
    s.split(|c: char| !c.is_ascii_digit())
        .filter(|x| !x.is_empty())
        .map(|x| x.parse().unwrap_or(0))
        .collect()
}

fn newest_version(siblings: &[Node]) -> Option<&str> {
    siblings
        .iter()
        .filter(|n| n.is_dir && n.name.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .max_by_key(|n| version_key(&n.name))
        .map(|n| n.name.as_str())
}

/// 备份类：30 天以上 Safe，否则 Review
fn backup_rule(ctx: &Ctx, label: impl Into<String>, node: &Node, what: &str) -> Class {
    if age_days(ctx, node) >= 30 {
        cl(
            label,
            "skill_backups",
            Safety::Safe,
            format!("{what}，已经 30 天以上没动过，一般用不到了"),
        )
    } else {
        cl(
            label,
            "skill_backups",
            Safety::Review,
            format!("{what}，最近 30 天内的，确认新版本没问题再删"),
        )
    }
}

// ---------------------------------------------------------------- 浏览器数据（Chromium/Electron）

const CHROMIUM_GPU: &[&str] = &[
    "GPUCache",
    "DawnCache",
    "DawnWebGPUCache",
    "DawnGraphiteCache",
    "GraphiteDawnCache",
    "GrShaderCache",
    "ShaderCache",
    "GPUPersistentCache",
];

const CHROMIUM_COMPONENTS: &[&str] = &[
    "component_crx_cache",
    "extensions_crx_cache",
    "OptimizationGuideModelsManifest",
    "OptimizationHints",
    "optimization_guide_model_store",
    "OnDeviceHeadSuggestModel",
    "Safe Browsing",
    "CertificateRevocation",
    "Subresource Filter",
    "PKIMetadata",
    "Crowd Deny",
    "FileTypePolicies",
    "MEIPreload",
    "SafetyTips",
    "ZxcvbnData",
    "ActorSafetyLists",
    "AmountExtractionHeuristicRegexes",
    "WasmTtsEngine",
    "WidevineCdm",
    "segmentation_platform",
    "TrustTokenKeyCommitments",
    "FirstPartySetsPreloaded",
    "SSLErrorAssistant",
    "CaptchaProviders",
    "CookieReadinessList",
    "AutofillStates",
    "hyphen-data",
    "Dictionaries",
];

const CHROMIUM_STORAGE: &[&str] = &[
    "Local Storage",
    "Session Storage",
    "IndexedDB",
    "WebStorage",
    "Cookies",
    "File System",
    "SharedStorage",
    "databases",
    "Extension State",
    "Local Extension Settings",
    "Extensions",
    "Sync Data",
    "Web Data",
    "Login Data",
    "History",
    "Storage",
];

/// 浏览器数据目录里认识的条目；不认识返回 None
fn chromium(name: &str) -> Option<Class> {
    let c = match name {
        "Cache" => cache("网页缓存"),
        "Code Cache" => cache("脚本缓存"),
        "Crashpad" | "BrowserMetrics" | "BrowserMetrics-spare.pma" => cl(
            "崩溃报告和统计",
            "cache",
            Safety::Safe,
            "崩溃报告和运行统计，删了没有影响",
        ),
        "Shared Dictionary" | "blob_storage" | "VideoDecodeStats" => cache(name),
        "Service Worker" => cl(
            "网页离线缓存",
            "cache",
            Safety::Review,
            "网页的离线缓存，删了网页会重新下载，一般不影响登录",
        ),
        "Partitions" => {
            cl("网页分区存储", "browser_data", Safety::Keep, R_WEB_STORAGE).expand(Expand::Children)
        }
        "Default" => cl(
            "浏览器配置",
            "browser_data",
            Safety::Keep,
            "浏览器配置（含登录状态），删了需要重新登录",
        )
        .expand(Expand::Children),
        n if CHROMIUM_GPU.contains(&n) => cache("显卡缓存"),
        n if CHROMIUM_COMPONENTS.contains(&n) => cl(
            format!("浏览器组件 {n}"),
            "cache",
            Safety::Safe,
            "浏览器自动下载的组件，删了会重新下载",
        ),
        n if CHROMIUM_STORAGE.contains(&n) => cl(n, "browser_data", Safety::Keep, R_WEB_STORAGE),
        n if n.starts_with("Profile ") => cl(
            n,
            "browser_data",
            Safety::Keep,
            "浏览器配置（含登录状态），删了需要重新登录",
        )
        .expand(Expand::Children),
        _ => return None,
    };
    Some(c)
}

/// rel 相对于浏览器数据根目录。顶层不认识的返回 None，更深层不认识的算作浏览器数据。
fn browser_profile(rel: &[&str]) -> Option<Class> {
    match rel {
        [] => None,
        [name] => chromium(name),
        ["Partitions", part] => Some(
            cl(
                format!("网页分区 {part}"),
                "browser_data",
                Safety::Keep,
                R_WEB_STORAGE,
            )
            .expand(Expand::Children),
        ),
        ["Partitions", _, rest @ ..] => {
            Some(browser_profile(rest).unwrap_or_else(|| browser_other(rest)))
        }
        [first, rest @ ..] if *first == "Default" || first.starts_with("Profile ") => {
            Some(browser_profile(rest).unwrap_or_else(|| browser_other(rest)))
        }
        _ => None,
    }
}

fn browser_other(rel: &[&str]) -> Class {
    let name = rel.last().copied().unwrap_or("");
    cl(
        name,
        "browser_data",
        Safety::Keep,
        "浏览器的其他数据，不建议单独删除",
    )
}

// ---------------------------------------------------------------- 各位置规则

pub(crate) fn root_class(loc: &LocDef) -> Class {
    match loc.id {
        LocId::Npx => cl(
            loc.label,
            "cache",
            Safety::Safe,
            "npx 临时下载的命令行工具（很多 MCP 服务靠它运行），删了下次使用会重新下载",
        )
        .expand(Expand::Children),
        _ => cl(loc.label, "location", Safety::Protected, R_ROOT).expand(Expand::Children),
    }
}

/// 给位置下的一个条目分类。rel 是相对位置根目录的各段路径。返回 None 表示不是 AI 工具的东西，跳过。
pub(crate) fn classify(
    ctx: &Ctx,
    loc: LocId,
    path: &Path,
    rel: &[&str],
    node: &Node,
    siblings: &[Node],
) -> Option<Class> {
    let c = match loc {
        LocId::ClaudeCode => claude_code(ctx, rel, node),
        LocId::ClaudeDesktop => claude_desktop(ctx, path, rel, node, siblings),
        LocId::ClaudeCli => claude_cli(ctx, rel, node, siblings),
        LocId::ClaudeTmp => claude_tmp(ctx, rel, node),
        LocId::Codex => codex(ctx, rel, node),
        LocId::CodexDesktop => codex_desktop(rel, node),
        LocId::ChatCut => chatcut(rel, node),
        LocId::ChatCutCli => match rel {
            ["cache"] => cl(
                "命令行工具缓存",
                "cache",
                Safety::Safe,
                "ChatCut 命令行工具的缓存（如 ffmpeg），删了用到时会重新下载",
            ),
            [name, ..] if is_config_name(name) => protected_config(name),
            [.., name] => unknown(name, node),
            [] => return None,
        },
        LocId::Gemini => gemini(ctx, rel, node),
        LocId::LibCaches => return lib_caches(rel),
        LocId::DotCache => return dot_cache(ctx, rel, node),
        LocId::Npx => npx(path, rel),
    };
    Some(c)
}

fn claude_code(ctx: &Ctx, rel: &[&str], node: &Node) -> Class {
    match rel {
        ["projects"] => cl(
            "对话记录",
            "claude_transcripts",
            Safety::Review,
            format!("Claude Code 的对话记录。{R_TRANSCRIPTS_TAIL}；每段对话的建议见「按项目」"),
        )
        .expand(Expand::Children),
        ["projects", enc] => {
            let cwd = ctx.claude_enc.get(*enc).cloned();
            let name = cwd
                .as_deref()
                .map(folder_name)
                .unwrap_or_else(|| enc.to_string());
            cl(
                format!("对话记录 · {name}"),
                "claude_transcripts",
                Safety::Review,
                format!("这个项目的 Claude Code 对话记录。{R_TRANSCRIPTS_TAIL}"),
            )
            .project(cwd)
        }
        ["file-history"] => file_history_rule(ctx, "文件改动快照".to_string(), node),
        ["file-history", sess] => {
            let label = match ctx.session_title(sess) {
                Some(t) => format!("改动快照 · {t}"),
                None => format!("改动快照 · {}", short_id(sess)),
            };
            file_history_rule(ctx, label, node).project(ctx.session_cwd(sess))
        }
        ["shell-snapshots" | "debug" | "paste-cache" | "image-cache" | "cache" | "statsig"
        | "logs"] => cache(rel[0]),
        ["telemetry"] => cl(
            "未发送的统计数据",
            "cache",
            Safety::Safe,
            "没发送成功的使用统计，删了没有影响",
        ),
        ["todos"] => cl(
            "旧对话的待办清单",
            "cache",
            Safety::Safe,
            "以前对话里的待办清单，删了没有影响",
        ),
        ["plugins"] => cl(
            "插件",
            "user_data",
            Safety::Keep,
            "已安装的插件和插件市场，删了需要重新安装",
        ),
        ["skills"] => cl(
            "技能",
            "user_data",
            Safety::Keep,
            "你安装或编写的技能，删了就没了",
        ),
        [name] if is_backup_dir(name) => {
            backup_rule(ctx, *name, node, "技能或配置的旧备份").expand(Expand::Children)
        }
        [parent, name] if is_backup_dir(parent) => {
            backup_rule(ctx, *name, node, "技能或配置的旧备份")
        }
        ["sessions"] => cl(
            "正在运行的会话",
            "config",
            Safety::Protected,
            "Claude Code 正在运行的会话信息",
        ),
        ["history.jsonl"] => cl(
            "输入历史",
            "user_data",
            Safety::Keep,
            "你在 Claude Code 里输入过的内容（按上箭头能翻到）",
        ),
        ["ide" | "session-env" | "chrome" | "state" | "hooks" | "agents" | "commands"
        | "output-styles"] => cl(rel[0], "config", Safety::Keep, "Claude Code 的设置和扩展"),
        [name] if is_config_name(name) || name.ends_with(".json") => protected_config(name),
        [.., name] => unknown(name, node),
        [] => unknown("", node),
    }
}

fn is_backup_dir(name: &str) -> bool {
    name.starts_with("skills-backup") || name == "skill-backups" || name == "backups"
}

fn file_history_rule(ctx: &Ctx, label: String, node: &Node) -> Class {
    let c = if age_days(ctx, node) >= 30 {
        cl(
            label,
            "file_history",
            Safety::Safe,
            "Claude Code 改文件前留的快照，用来撤回改动；已经 30 天以上，一般用不到了",
        )
    } else {
        cl(
            label,
            "file_history",
            Safety::Review,
            "Claude Code 改文件前留的快照，用来撤回改动；最近 30 天内的，删了就不能撤回这些改动",
        )
    };
    c.expand(Expand::Children)
}

fn claude_desktop(ctx: &Ctx, path: &Path, rel: &[&str], node: &Node, siblings: &[Node]) -> Class {
    const R_VM: &str = "Claude 桌面版运行沙箱用的虚拟机镜像，删除后下次使用会重新下载";
    match rel {
        ["vm_bundles"] => {
            cl("沙箱虚拟机", "claude_vm", Safety::Review, R_VM).expand(Expand::Children)
        }
        ["vm_bundles", name] => {
            let label = match *name {
                "claudevm.bundle" => "虚拟机镜像",
                "warm" => "预热的虚拟机镜像",
                n => n,
            };
            cl(label, "claude_vm", Safety::Review, R_VM)
        }
        ["scratch-workspaces"] => cl(
            "临时工作区",
            "claude_scratch",
            Safety::Review,
            "桌面版在你没选项目文件夹时建的临时工作区，按工作区分别判断",
        )
        .expand(Expand::Flatten(3)),
        ["scratch-workspaces", ..] => scratch_workspace(ctx, path, node),
        ["claude-code"] => cl(
            "内置 Claude Code 程序",
            "program",
            Safety::Review,
            "桌面版自带的 Claude Code 程序，按版本分别判断",
        )
        .expand(Expand::Children),
        ["claude-code-vm"] => cl(
            "沙箱里的 Claude Code 程序",
            "program",
            Safety::Review,
            "桌面版沙箱里用的 Claude Code 程序，按版本分别判断",
        )
        .expand(Expand::Children),
        ["claude-code" | "claude-code-vm", ver] => {
            version_rule(ver, newest_version(siblings), "Claude Code")
        }
        ["local-agent-mode-sessions"] => cl(
            "Cowork 会话",
            "user_data",
            Safety::Keep,
            "Cowork（本地代理）任务的会话数据，删了这些任务的记录会丢失",
        ),
        ["claude-code-sessions"] => cl(
            "桌面版会话列表",
            "user_data",
            Safety::Keep,
            "桌面版 Claude Code 的会话列表",
        ),
        ["git-shadow"] => cl(
            "git 工作区记录",
            "user_data",
            Safety::Keep,
            "桌面版管理的 git 工作区记录",
        ),
        ["sentry"] => cl("崩溃报告", "cache", Safety::Safe, "崩溃报告，删了没有影响"),
        [name] if is_config_name(name) || name.ends_with(".json") => protected_config(name),
        _ => match browser_profile(rel) {
            Some(c) => c,
            None => unknown(rel.last().copied().unwrap_or(""), node),
        },
    }
}

fn version_rule(ver: &str, newest: Option<&str>, what: &str) -> Class {
    if Some(ver) == newest {
        cl(
            format!("{what} {ver}"),
            "program",
            Safety::Keep,
            "正在使用的最新版本",
        )
    } else {
        cl(
            format!("{what} {ver}（旧版本）"),
            "program",
            Safety::Safe,
            format!("旧版本的 {what}，已经被新版本取代，删了不影响使用"),
        )
    }
}

/// 「scratch-2026-09-27-ea6ad3」→「09-27」
fn scratch_date(name: &str) -> Option<String> {
    let rest = name.strip_prefix("scratch-")?;
    let parts: Vec<&str> = rest.split('-').collect();
    if parts.len() >= 3 && parts[0].len() == 4 && parts[1].len() == 2 && parts[2].len() == 2 {
        Some(format!("{}-{}", parts[1], parts[2]))
    } else {
        None
    }
}

pub(crate) fn scratch_display_name(path: &str) -> String {
    let name = folder_name(path);
    match scratch_date(&name) {
        Some(d) => format!("临时工作区 {d}"),
        None => format!("临时工作区 {}", short_id(&name)),
    }
}

fn scratch_workspace(ctx: &Ctx, path: &Path, node: &Node) -> Class {
    let p = path_string(path);
    let mut label = scratch_display_name(&p);
    if let Some(t) = ctx.title_by_cwd.get(&p) {
        label = format!("{label} · {t}");
    }
    let c = if in_use(ctx, node) {
        cl(
            label,
            "claude_scratch",
            Safety::Keep,
            "这个临时工作区正在使用",
        )
    } else if age_days(ctx, node) > 7 {
        cl(
            label,
            "claude_scratch",
            Safety::Safe,
            "超过 7 天没动的临时工作区，里面是当时对话生成的文件（图片、视频等），确认没有要留的再删",
        )
    } else {
        cl(
            label,
            "claude_scratch",
            Safety::Keep,
            "最近 7 天内用过的临时工作区，可能还会继续用",
        )
    };
    c.project(Some(p))
}

fn claude_cli(ctx: &Ctx, rel: &[&str], node: &Node, siblings: &[Node]) -> Class {
    match rel {
        ["versions"] => cl(
            "命令行程序版本",
            "program",
            Safety::Review,
            "Claude Code 命令行程序的各个版本，按版本分别判断",
        )
        .expand(Expand::Children),
        ["versions", ver] => {
            let current = ctx
                .current_claude_cli
                .as_deref()
                .or_else(|| newest_version(siblings));
            version_rule(ver, current, "Claude Code")
        }
        [.., name] => unknown(name, node),
        [] => unknown("", node),
    }
}

fn tmp_age_rule(ctx: &Ctx, label: String, node: &Node) -> Class {
    if in_use(ctx, node) {
        cl(label, "temp", Safety::Keep, "这段对话正在使用这些临时文件")
    } else if age_days(ctx, node) >= 3 {
        cl(
            label,
            "temp",
            Safety::Safe,
            "Claude Code 对话时产生的临时文件，已经 3 天以上没动，对话结束后一般就不需要了",
        )
    } else {
        cl(
            label,
            "temp",
            Safety::Review,
            "最近对话产生的临时文件，如果对话已经结束一般可以删",
        )
    }
}

fn claude_tmp(ctx: &Ctx, rel: &[&str], node: &Node) -> Class {
    match rel {
        [enc] if enc.starts_with('-') => {
            let cwd = ctx.claude_enc.get(*enc).cloned();
            let name = cwd
                .as_deref()
                .map(folder_name)
                .unwrap_or_else(|| enc.to_string());
            tmp_age_rule(ctx, format!("临时文件 · {name}"), node)
                .expand(Expand::Children)
                .project(cwd)
        }
        [enc, sess] if enc.starts_with('-') => {
            let label = match ctx.session_title(sess) {
                Some(t) => format!("对话临时文件 · {t}"),
                None => format!("对话临时文件 · {}", short_id(sess)),
            };
            let cwd = ctx
                .session_cwd(sess)
                .or_else(|| ctx.claude_enc.get(*enc).cloned());
            tmp_age_rule(ctx, label, node).project(cwd)
        }
        ["bash-edit-diff"] => cache("命令改动对比缓存"),
        ["bundled-skills"] => cl(
            "内置技能解压缓存",
            "cache",
            Safety::Safe,
            "Claude Code 解压出来的内置技能，会自动重建",
        ),
        [.., name] => tmp_age_rule(ctx, name.to_string(), node),
        [] => unknown("", node),
    }
}

fn codex(ctx: &Ctx, rel: &[&str], node: &Node) -> Class {
    const R_SESS: &str = "Codex 的对话记录";
    match rel {
        ["sessions"] => cl(
            "对话记录",
            "codex_sessions",
            Safety::Review,
            format!("{R_SESS}。{R_TRANSCRIPTS_TAIL}；每段对话的建议见「按项目」"),
        )
        .expand(Expand::Flatten(2)),
        ["sessions", y, m] => cl(
            format!("{y} 年 {} 月的对话", m.trim_start_matches('0')),
            "codex_sessions",
            Safety::Review,
            format!("{R_SESS}。{R_TRANSCRIPTS_TAIL}"),
        ),
        ["sessions", ..] => cl(
            "对话记录",
            "codex_sessions",
            Safety::Review,
            format!("{R_SESS}。{R_TRANSCRIPTS_TAIL}"),
        ),
        ["archived_sessions"] => cl(
            "已归档的对话",
            "codex_sessions",
            Safety::Review,
            format!("已归档的 Codex 对话。{R_TRANSCRIPTS_TAIL}"),
        ),
        ["generated_images"] => cl(
            "生成的图片",
            "codex_images",
            Safety::Review,
            "AI 生成的图片，确认没用再删",
        )
        .expand(Expand::Children),
        ["generated_images", id] => {
            let (label, reason) = match ctx.sessions.get(*id) {
                Some((t, _)) => (
                    format!("生成的图片 · {}", t.clone().unwrap_or_else(|| short_id(id))),
                    "这段对话里 AI 生成的图片，确认没用再删",
                ),
                None => (
                    format!("生成的图片 · 对话 {}", short_id(id)),
                    "AI 生成的图片，对应的对话记录已经不在了，确认没用再删",
                ),
            };
            cl(label, "codex_images", Safety::Review, reason).project(ctx.session_cwd(id))
        }
        ["skill-backups"] => backup_rule(ctx, "技能备份", node, "Codex 技能的旧备份").expand(Expand::Children),
        ["skill-backups", name] => backup_rule(ctx, *name, node, "Codex 技能的旧备份"),
        ["local-deps"] => cl(
            "技能依赖",
            "dependency",
            Safety::Review,
            "Codex 为技能安装的依赖包，删了下次用到时需要重新安装",
        ),
        [".tmp"] => cl(
            "内部临时目录",
            "temp",
            Safety::Review,
            "Codex 的内部临时目录（插件市场副本、临时 git 仓库），其中自带插件市场还被配置引用，不建议整体删除",
        )
        .expand(Expand::Children),
        [".tmp", "bundled-marketplaces"] => cl(
            "自带插件市场",
            "program",
            Safety::Keep,
            "Codex 自带的插件市场，配置里正在引用",
        ),
        [".tmp", "marketplaces" | "plugins"] => cl(
            format!("插件市场副本 {}", rel[1]),
            "temp",
            Safety::Review,
            "从网上同步的插件市场副本，删了会重新下载，期间插件可能暂时不可用",
        ),
        [".tmp", name] => cl(*name, "temp", Safety::Review, "Codex 的临时文件"),
        ["plugins"] => cl("插件", "user_data", Safety::Keep, "已安装的 Codex 插件，删了需要重新安装"),
        ["skills" | "vendor_imports"] => cl(rel[0], "user_data", Safety::Keep, "你的技能，删了就没了"),
        ["memories" | "memories_v2"] => cl("记忆", "user_data", Safety::Keep, "Codex 记住的关于你的信息"),
        ["cache"] => cache("缓存"),
        ["log" | "logs"] => cl("日志", "cache", Safety::Safe, "日志，删了不影响使用"),
        ["computer-use"] => cl("电脑操控组件", "program", Safety::Keep, "Codex 的电脑操控组件"),
        [".chatgpt-projects"] => cl(
            "ChatGPT 项目文件",
            "user_data",
            Safety::Review,
            "ChatGPT 项目同步到本地的文件",
        ),
        ["visualizations"] => cl(
            "可视化文件",
            "user_data",
            Safety::Review,
            "Codex 生成的图表和可视化文件，确认没用再删",
        ),
        ["worktrees"] => cl("工作区", "worktree", Safety::Review, "Codex 创建的 git 工作区，删前确认没有未提交的改动"),
        ["sqlite"] => cl("数据库目录", "database", Safety::Protected, R_DB),
        [name] if is_db_name(name) => cl(*name, "database", Safety::Protected, R_DB),
        ["history.jsonl"] => cl("输入历史", "user_data", Safety::Keep, "你在 Codex 里输入过的内容"),
        ["session_index.jsonl"] => cl("对话索引", "user_data", Safety::Keep, "Codex 的对话列表索引"),
        [name] if is_config_name(name) => protected_config(name),
        [name] if name.contains("global-state") => cl(*name, "config", Safety::Keep, "Codex 桌面版的界面状态"),
        [.., name] => unknown(name, node),
        [] => unknown("", node),
    }
}

fn codex_desktop(rel: &[&str], node: &Node) -> Class {
    match rel {
        ["codex-browser-app" | "executor-plugins"] => {
            cl(rel[0], "program", Safety::Keep, "Codex 桌面版的组件")
        }
        ["sentry"] => cl("崩溃报告", "cache", Safety::Safe, "崩溃报告，删了没有影响"),
        [name] if is_config_name(name) || name.ends_with(".json") || is_db_name(name) => {
            protected_config(name)
        }
        _ => match browser_profile(rel) {
            Some(c) => c,
            None => unknown(rel.last().copied().unwrap_or(""), node),
        },
    }
}

fn chatcut(rel: &[&str], node: &Node) -> Class {
    const R_BACKUP: &str =
        "ChatCut 工程的自动备份（定时快照）。确认工程没问题后可以删，删了就不能回到旧版本";
    match rel {
        ["project-backups"] => {
            cl("工程自动备份", "backup", Safety::Review, R_BACKUP).expand(Expand::Children)
        }
        ["project-backups", id] => cl(
            format!("工程备份 {}", short_id(id)),
            "backup",
            Safety::Review,
            R_BACKUP,
        ),
        ["projects"] => cl(
            "剪辑工程",
            "user_data",
            Safety::Keep,
            "ChatCut 的剪辑工程，删了工程就没了",
        ),
        ["acp-agents"] => cl(
            "AI 助手运行组件",
            "dependency",
            Safety::Review,
            "ChatCut 内置 AI 助手的运行组件，删了下次使用会重新下载",
        ),
        ["acp-workspaces"] => cl(
            "AI 助手工作目录",
            "user_data",
            Safety::Review,
            "ChatCut 里 AI 助手的工作目录，可能有生成的文件",
        ),
        ["asset-preview"] => cl(
            "素材预览缓存",
            "cache",
            Safety::Safe,
            "素材预览图缓存，会自动重建",
        ),
        ["font_cache"] => cache("字体缓存"),
        ["logs"] => cl("日志", "cache", Safety::Safe, "日志，删了不影响使用"),
        ["agent-projects"] => cl(
            "AI 助手项目",
            "user_data",
            Safety::Keep,
            "ChatCut AI 助手的项目数据",
        ),
        [name] if is_db_name(name) => cl(
            *name,
            "database",
            Safety::Protected,
            "ChatCut 正在使用的数据库，直接删除会丢数据",
        ),
        [name] if is_config_name(name) || name.ends_with(".json") => protected_config(name),
        _ => match browser_profile(rel) {
            Some(c) => c,
            None => unknown(rel.last().copied().unwrap_or(""), node),
        },
    }
}

fn gemini(ctx: &Ctx, rel: &[&str], node: &Node) -> Class {
    match rel {
        ["tmp"] => cl(
            "对话记录和临时文件",
            "gemini_chats",
            Safety::Review,
            "Gemini CLI 的对话记录、日志和临时文件",
        )
        .expand(Expand::Children),
        ["tmp", "bin"] => cl(
            "下载的工具",
            "cache",
            Safety::Safe,
            "Gemini 下载的工具（ripgrep），删了会自动重新下载",
        ),
        ["tmp", d] => {
            let cwd = ctx.gemini_dirs.get(*d).cloned();
            let name = cwd
                .as_deref()
                .map(folder_name)
                .unwrap_or_else(|| short_id(d));
            cl(
                format!("对话记录 · {name}"),
                "gemini_chats",
                Safety::Review,
                format!("Gemini CLI 在这个项目里的对话记录和日志。{R_TRANSCRIPTS_TAIL}"),
            )
            .project(cwd)
        }
        ["antigravity-browser-profile"] => cl(
            "Antigravity 浏览器数据",
            "browser_data",
            Safety::Keep,
            "Antigravity 内置浏览器的数据（含登录状态），删了需要重新登录",
        )
        .expand(Expand::Children),
        ["antigravity-browser-profile", rest @ ..] => {
            browser_profile(rest).unwrap_or_else(|| browser_other(rest))
        }
        ["antigravity"] => cl(
            "Antigravity 数据",
            "user_data",
            Safety::Keep,
            "Antigravity 的对话和知识库",
        ),
        ["history" | "skills" | "extensions"] => {
            cl(rel[0], "user_data", Safety::Keep, "Gemini 的历史和扩展")
        }
        [name] if is_config_name(name) || name.ends_with(".json") => protected_config(name),
        [.., name] => unknown(name, node),
        [] => unknown("", node),
    }
}

fn lib_caches(rel: &[&str]) -> Option<Class> {
    const R_PW_MCP: &str =
        "Playwright MCP 用的浏览器配置（可能含网站登录状态），删了下次会重新创建，需要重新登录";
    let c = match rel {
        ["com.anthropic.claudefordesktop"] => cache("Claude 桌面版缓存").tool("claude"),
        ["com.anthropic.claudefordesktop.ShipIt"] => cl(
            "Claude 桌面版更新缓存",
            "cache",
            Safety::Safe,
            "更新时下载的临时文件，删了没有影响",
        )
        .tool("claude"),
        ["claude-cli-nodejs"] => cl(
            "Claude Code 的 MCP 日志",
            "cache",
            Safety::Safe,
            "Claude Code 记录的 MCP 日志，删了没有影响",
        )
        .tool("claude"),
        ["Codex" | "com.openai.codex"] => cache("Codex 桌面版缓存").tool("codex"),
        ["com.openai.sky.CUAService" | "com.openai.sky.CUAService.cli"] => {
            cache("Codex 电脑操控服务的缓存").tool("codex")
        }
        ["io.chatcut.desktop"] => cache("ChatCut 缓存").tool("chatcut"),
        ["io.chatcut.desktop.ShipIt"] => cl(
            "ChatCut 更新缓存",
            "cache",
            Safety::Safe,
            "更新时下载的临时文件，删了没有影响",
        )
        .tool("chatcut"),
        ["chatcut-desktop-updater"] => cl(
            "ChatCut 更新安装包",
            "cache",
            Safety::Safe,
            "ChatCut 下载的更新安装包，更新装好后就用不到了",
        )
        .tool("chatcut"),
        ["ms-playwright"] => cl(
            "Playwright 浏览器内核",
            "dependency",
            Safety::Safe,
            R_DEPS_REDOWNLOAD,
        ),
        ["ms-playwright-mcp"] => cl(
            "Playwright MCP 浏览器配置",
            "browser_data",
            Safety::Review,
            R_PW_MCP,
        )
        .expand(Expand::Children),
        ["ms-playwright-mcp", profile] => cl(
            format!("浏览器配置 {profile}"),
            "browser_data",
            Safety::Review,
            R_PW_MCP,
        )
        .expand(Expand::Children),
        ["ms-playwright-mcp", _, rest @ ..] => {
            browser_profile(rest).unwrap_or_else(|| browser_other(rest))
        }
        [first, ..] if LIB_CACHES_AI.contains(first) => cache(rel.last().copied().unwrap_or(first)),
        _ => return None,
    };
    Some(c)
}

fn dot_cache(ctx: &Ctx, rel: &[&str], node: &Node) -> Option<Class> {
    const R_HF: &str = "HyperFrames 视频技能下载的模型、字体和浏览器，删了下次使用会重新下载";
    let c = match rel {
        ["uv"] => cl("uv 依赖缓存", "dependency", Safety::Safe, R_DEPS_REDOWNLOAD),
        ["codex-runtimes"] => cl("Codex 运行环境", "program", Safety::Review, "Codex 的运行环境，按目录分别判断")
            .tool("codex")
            .expand(Expand::Children),
        ["codex-runtimes", "codex-primary-runtime"] => cl(
            "Codex 当前运行环境",
            "program",
            Safety::Keep,
            "Codex 正在使用的运行环境（配置里引用了它）",
        )
        .tool("codex"),
        ["codex-runtimes", name] if name.starts_with("codex-runtime-install-") => {
            let label = format!("安装残留 {name}");
            if !in_use(ctx, node) && age_days(ctx, node) >= 1 {
                cl(label, "temp", Safety::Safe, "安装运行环境时留下的临时目录，安装完成后就没用了")
            } else {
                cl(label, "temp", Safety::Review, "安装运行环境时用的临时目录，可能正在安装")
            }
            .tool("codex")
        }
        ["codex-runtimes", name] => cl(*name, "program", Safety::Review, "Codex 的运行环境文件").tool("codex"),
        ["hyperframes"] => cl("HyperFrames 缓存", "dependency", Safety::Safe, R_HF).expand(Expand::Children),
        ["hyperframes", name] => {
            let label = match *name {
                "whisper" => "语音识别模型",
                "chrome" => "渲染用浏览器",
                "fonts" => "字体",
                "background-removal" => "抠图模型",
                "optional" => "可选组件",
                n => n,
            };
            cl(label, "dependency", Safety::Safe, R_HF)
        }
        ["chrome-devtools-mcp"] => cl(
            "Chrome DevTools MCP 浏览器配置",
            "browser_data",
            Safety::Review,
            "Chrome DevTools MCP 用的浏览器配置（可能含网站登录状态），删了下次会重新创建，需要重新登录",
        ),
        ["claude"] => cache("Claude 缓存").tool("claude"),
        [first, ..] if DOT_CACHE_AI.contains(first) => cache(rel.last().copied().unwrap_or(first)),
        _ => return None,
    };
    Some(c)
}

fn npx(path: &Path, rel: &[&str]) -> Class {
    const R: &str = "npx 临时下载的命令行工具，删了下次使用会重新下载";
    match rel {
        [hash] => {
            let pkgs = npx_packages(path);
            let label = if pkgs.is_empty() {
                format!("npx 工具 {}", short_id(hash))
            } else {
                format!("npx 工具 · {}", pkgs.join("、"))
            };
            cl(label, "cache", Safety::Safe, R)
        }
        _ => cl(rel.last().copied().unwrap_or(""), "cache", Safety::Safe, R),
    }
}

fn npx_packages(dir: &Path) -> Vec<String> {
    let Ok(b) = std::fs::read(dir.join("package.json")) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&b) else {
        return Vec::new();
    };
    v.get("dependencies")
        .and_then(|d| d.as_object())
        .map(|m| m.keys().take(3).cloned().collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------- 组装

pub(crate) fn walk_location(loc: &LocDef) -> Option<Node> {
    match loc.only {
        None => {
            if !loc.root.is_dir() {
                return None;
            }
            walk_tree(&loc.root, TREE_DEPTH)
        }
        Some(names) => {
            let kids: Vec<Node> = names
                .par_iter()
                .filter_map(|n| walk_tree(&loc.root.join(n), TREE_DEPTH - 1))
                .collect();
            if kids.is_empty() {
                return None;
            }
            let mut stats = Stats::default();
            for k in &kids {
                stats.add(&k.stats);
            }
            Some(Node {
                name: loc
                    .root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                is_dir: true,
                stats,
                children: kids,
            })
        }
    }
}

pub(crate) fn build_location(ctx: &Ctx, loc: &LocDef, node: &Node) -> DiskItem {
    let class = root_class(loc);
    build_item(ctx, loc, &loc.root, &[], node, class)
}

fn build_item(
    ctx: &Ctx,
    loc: &LocDef,
    path: &Path,
    rel: &[String],
    node: &Node,
    class: Class,
) -> DiskItem {
    let mut children: Vec<DiskItem> = Vec::new();
    let mut push_child =
        |child_path: PathBuf, child_rel: Vec<String>, child: &Node, siblings: &[Node]| {
            if child.stats.bytes < LIST_MIN {
                return;
            }
            let r: Vec<&str> = child_rel.iter().map(|s| s.as_str()).collect();
            if let Some(c) = classify(ctx, loc.id, &child_path, &r, child, siblings) {
                children.push(build_item(ctx, loc, &child_path, &child_rel, child, c));
            }
        };
    match class.expand {
        Expand::No => {}
        Expand::Children => {
            for ch in &node.children {
                let mut r = rel.to_vec();
                r.push(ch.name.clone());
                push_child(path.join(&ch.name), r, ch, &node.children);
            }
        }
        Expand::Flatten(depth) => {
            let mut found: Vec<(PathBuf, Vec<String>, &Node, &[Node])> = Vec::new();
            collect_at_depth(node, path, rel, depth, &mut found);
            for (p, r, n, sib) in found {
                push_child(p, r, n, sib);
            }
        }
    }
    children.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));

    let used = in_use(ctx, node);
    let mut reason = class.reason;
    if used && matches!(class.safety, Safety::Safe | Safety::Review) {
        reason.push_str("（最近 30 分钟内还在写入，可能正在使用，建议先退出对应程序再删）");
    }
    let path_s = path_string(path);
    DiskItem {
        id: path_id(&path_s),
        path: path_s,
        label: class.label,
        category: class.category.to_string(),
        tool: class.tool.unwrap_or(loc.tool).to_string(),
        project: class.project,
        size_bytes: node.stats.bytes,
        file_count: node.stats.files,
        modified: rfc3339(node.stats.newest),
        safety: class.safety,
        reason,
        in_use: used,
        title: class.title,
        children,
    }
}

fn collect_at_depth<'a>(
    node: &'a Node,
    path: &Path,
    rel: &[String],
    depth: usize,
    out: &mut Vec<(PathBuf, Vec<String>, &'a Node, &'a [Node])>,
) {
    for ch in &node.children {
        let mut r = rel.to_vec();
        r.push(ch.name.clone());
        let p = path.join(&ch.name);
        if depth <= 1 {
            out.push((p, r, ch, &node.children));
        } else if ch.is_dir {
            collect_at_depth(ch, &p, &r, depth - 1, out);
        }
    }
}

/// 对单个路径重新分类（移到废纸篓前的校验用）。返回 (位置, 分类)；
/// 不在任何已知位置里返回 None；在虚拟分组里但不是 AI 工具的，返回 Some((loc, None))。
pub(crate) fn classify_single<'a>(
    ctx: &Ctx,
    defs: &'a [LocDef],
    canon_roots: &[Option<PathBuf>],
    path: &Path,
    node: &Node,
) -> Option<(&'a LocDef, Option<Class>)> {
    for (loc, root) in defs.iter().zip(canon_roots) {
        let Some(root) = root else { continue };
        let Ok(rest) = path.strip_prefix(root) else {
            continue;
        };
        let parts: Vec<String> = rest
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        if parts.is_empty() {
            return Some((loc, Some(root_class(loc))));
        }
        let mut current = root_class(loc);
        let mut k = 0usize;
        loop {
            let step = match current.expand {
                Expand::No => return Some((loc, Some(current))),
                Expand::Children => 1,
                Expand::Flatten(d) => d,
            };
            if k + step > parts.len() {
                // 落在被拍平的中间层：沿用上一级的判断
                return Some((loc, Some(current)));
            }
            k += step;
            let rel: Vec<&str> = parts[..k].iter().map(|s| s.as_str()).collect();
            let is_last = k == parts.len();
            let sub_path = root.join(parts[..k].iter().collect::<PathBuf>());
            let siblings = sibling_nodes(&sub_path);
            let probe;
            let n = if is_last {
                node
            } else {
                probe = Node {
                    name: parts[k - 1].clone(),
                    is_dir: true,
                    stats: Stats {
                        bytes: 0,
                        files: 0,
                        newest: 0,
                    },
                    children: Vec::new(),
                };
                &probe
            };
            match classify(ctx, loc.id, &sub_path, &rel, n, &siblings) {
                Some(c) => current = c,
                None => return Some((loc, None)),
            }
            if is_last {
                return Some((loc, Some(current)));
            }
        }
    }
    None
}

fn sibling_nodes(path: &Path) -> Vec<Node> {
    let Some(parent) = path.parent() else {
        return Vec::new();
    };
    let Ok(rd) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    rd.flatten()
        .map(|e| Node {
            name: e.file_name().to_string_lossy().into_owned(),
            is_dir: e.file_type().map(|t| t.is_dir()).unwrap_or(false),
            stats: Stats::default(),
            children: Vec::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str, is_dir: bool, newest: i64) -> Node {
        Node {
            name: name.to_string(),
            is_dir,
            stats: Stats {
                bytes: 10 * LIST_MIN,
                files: 1,
                newest,
            },
            children: Vec::new(),
        }
    }

    fn ctx(now: i64) -> Ctx {
        Ctx {
            now,
            ..Default::default()
        }
    }

    fn class_of(c: &Ctx, loc: LocId, rel: &[&str], n: &Node) -> Class {
        classify(c, loc, Path::new("/x"), rel, n, &[]).unwrap()
    }

    #[test]
    fn cache_and_vm_rules() {
        let now = 10_000 * DAY;
        let c = ctx(now);
        let old = node("Cache", true, now - 40 * DAY);
        assert_eq!(
            class_of(&c, LocId::ClaudeDesktop, &["Cache"], &old).safety,
            Safety::Safe
        );
        assert_eq!(
            class_of(&c, LocId::ClaudeDesktop, &["Code Cache"], &old).safety,
            Safety::Safe
        );
        let vm = class_of(&c, LocId::ClaudeDesktop, &["vm_bundles"], &old);
        assert_eq!(vm.safety, Safety::Review);
        assert!(vm.reason.contains("虚拟机镜像"));
        assert_eq!(
            class_of(&c, LocId::ClaudeDesktop, &["Partitions"], &old).safety,
            Safety::Keep
        );
        // 分区里面的缓存可以删
        assert_eq!(
            class_of(
                &c,
                LocId::ClaudeDesktop,
                &["Partitions", "p1", "Cache"],
                &old
            )
            .safety,
            Safety::Safe
        );
        assert_eq!(
            class_of(
                &c,
                LocId::ClaudeDesktop,
                &["Partitions", "p1", "IndexedDB"],
                &old
            )
            .safety,
            Safety::Keep
        );
        let unk = class_of(&c, LocId::ClaudeDesktop, &["weird-dir"], &old);
        assert_eq!(unk.safety, Safety::Review);
        assert_eq!(unk.reason, R_UNKNOWN_DIR);
    }

    #[test]
    fn scratch_workspace_rules() {
        let now = 10_000 * DAY;
        let c = ctx(now);
        let rel = ["scratch-workspaces", "a", "b", "scratch-2026-09-27-ea6ad3"];
        let p = Path::new("/h/scratch-workspaces/a/b/scratch-2026-09-27-ea6ad3");
        let old = node(rel[3], true, now - 8 * DAY);
        let cls = classify(&c, LocId::ClaudeDesktop, p, &rel, &old, &[]).unwrap();
        assert_eq!(cls.safety, Safety::Safe);
        assert!(cls.label.starts_with("临时工作区 09-27"));
        let recent = node(rel[3], true, now - 2 * DAY);
        assert_eq!(
            class_of(&c, LocId::ClaudeDesktop, &rel, &recent).safety,
            Safety::Keep
        );
        let busy = node(rel[3], true, now - 60);
        assert_eq!(
            class_of(&c, LocId::ClaudeDesktop, &rel, &busy).safety,
            Safety::Keep
        );
    }

    #[test]
    fn codex_rules() {
        let now = 10_000 * DAY;
        let c = ctx(now);
        let n = node("x", false, now - DAY);
        for db in [
            "thread_history_1.sqlite",
            "logs_2.sqlite",
            "state_5.sqlite-wal",
            "x.db",
        ] {
            let cls = class_of(&c, LocId::Codex, &[db], &n);
            assert_eq!(cls.safety, Safety::Protected, "{db}");
            assert_eq!(cls.category, "database");
        }
        assert_eq!(
            class_of(&c, LocId::Codex, &["auth.json"], &n).safety,
            Safety::Protected
        );
        assert_eq!(
            class_of(&c, LocId::Codex, &["config.toml"], &n).safety,
            Safety::Protected
        );
        assert_eq!(
            class_of(&c, LocId::Codex, &["generated_images"], &n).safety,
            Safety::Review
        );
        assert_eq!(
            class_of(&c, LocId::Codex, &["local-deps"], &n).safety,
            Safety::Review
        );
        let old = node("b", true, now - 31 * DAY);
        assert_eq!(
            class_of(&c, LocId::Codex, &["skill-backups", "b"], &old).safety,
            Safety::Safe
        );
        let fresh = node("b", true, now - 3 * DAY);
        assert_eq!(
            class_of(&c, LocId::Codex, &["skill-backups", "b"], &fresh).safety,
            Safety::Review
        );
        let s = class_of(&c, LocId::Codex, &["sessions"], &n);
        assert_eq!(s.expand, Expand::Flatten(2));
        assert!(s.reason.contains("resume"));
    }

    #[test]
    fn claude_code_rules() {
        let now = 10_000 * DAY;
        let c = ctx(now);
        let old = node("x", true, now - 31 * DAY);
        let fresh = node("x", true, now - DAY);
        assert_eq!(
            class_of(&c, LocId::ClaudeCode, &["file-history"], &old).safety,
            Safety::Safe
        );
        assert_eq!(
            class_of(&c, LocId::ClaudeCode, &["file-history"], &fresh).safety,
            Safety::Review
        );
        assert_eq!(
            class_of(&c, LocId::ClaudeCode, &["projects"], &fresh).safety,
            Safety::Review
        );
        assert_eq!(
            class_of(&c, LocId::ClaudeCode, &["shell-snapshots"], &fresh).safety,
            Safety::Safe
        );
        assert_eq!(
            class_of(&c, LocId::ClaudeCode, &[".credentials.json"], &fresh).safety,
            Safety::Protected
        );
        assert_eq!(
            class_of(&c, LocId::ClaudeCode, &["skills"], &fresh).safety,
            Safety::Keep
        );
    }

    #[test]
    fn versions_newest_kept() {
        let c = ctx(10_000 * DAY);
        let sibs = vec![node("2.1.284", true, 0), node("2.1.286", true, 0)];
        let a = classify(
            &c,
            LocId::ClaudeDesktop,
            Path::new("/x"),
            &["claude-code", "2.1.284"],
            &sibs[0],
            &sibs,
        )
        .unwrap();
        let b = classify(
            &c,
            LocId::ClaudeDesktop,
            Path::new("/x"),
            &["claude-code", "2.1.286"],
            &sibs[1],
            &sibs,
        )
        .unwrap();
        assert_eq!(a.safety, Safety::Safe);
        assert_eq!(b.safety, Safety::Keep);
    }

    #[test]
    fn virtual_groups_skip_non_ai() {
        let c = ctx(10_000 * DAY);
        let n = node("x", true, 0);
        assert!(classify(
            &c,
            LocId::LibCaches,
            Path::new("/x"),
            &["com.apple.Safari"],
            &n,
            &[]
        )
        .is_none());
        assert_eq!(
            classify(
                &c,
                LocId::LibCaches,
                Path::new("/x"),
                &["ms-playwright"],
                &n,
                &[]
            )
            .unwrap()
            .safety,
            Safety::Safe
        );
        assert_eq!(
            classify(&c, LocId::DotCache, Path::new("/x"), &["uv"], &n, &[])
                .unwrap()
                .safety,
            Safety::Safe
        );
        assert!(classify(&c, LocId::DotCache, Path::new("/x"), &["random"], &n, &[]).is_none());
    }

    #[test]
    fn build_tree_lists_big_children() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        let codex = home.join(".codex");
        std::fs::create_dir_all(codex.join("cache")).unwrap();
        std::fs::create_dir_all(codex.join("sessions/2026/09/01")).unwrap();
        std::fs::write(codex.join("cache/a"), vec![0u8; 2 * 1024 * 1024]).unwrap();
        std::fs::write(
            codex.join("sessions/2026/09/01/rollout-a.jsonl"),
            vec![b'x'; 2 * 1024 * 1024],
        )
        .unwrap();
        std::fs::write(codex.join("logs_2.sqlite"), vec![0u8; 2 * 1024 * 1024]).unwrap();
        std::fs::write(codex.join("config.toml"), "x").unwrap();
        let env = Env::new(home);
        let defs = location_defs(&env);
        let loc = defs.iter().find(|l| l.id == LocId::Codex).unwrap();
        let node = walk_location(loc).unwrap();
        let c = ctx(crate::util::now_secs());
        let item = build_location(&c, loc, &node);
        assert_eq!(item.safety, Safety::Protected);
        let names: Vec<&str> = item.children.iter().map(|c| c.label.as_str()).collect();
        assert!(names.contains(&"缓存"), "{names:?}");
        assert!(names.contains(&"logs_2.sqlite"));
        let sessions = item
            .children
            .iter()
            .find(|c| c.category == "codex_sessions")
            .unwrap();
        assert_eq!(sessions.children.len(), 1);
        assert_eq!(sessions.children[0].label, "2026 年 9 月的对话");
        // 刚写的文件：在用
        assert!(item.in_use);
    }
}

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use ad_disk::{AssessInput, Assessment, DiskReport, DiskScanOptions, TrashResult};
use ad_inventory::Inventory;
use ad_security::{ScanOptions, ScanReport};
use ad_usage::UsageSnapshot;

use crate::jobs;
use crate::settings::Settings;
use crate::state::AppState;
use crate::windows;

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(err)
}

// ---------- 用量 ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageState {
    pub snapshot: Option<UsageSnapshot>,
    pub error: Option<String>,
}

#[tauri::command]
pub fn get_usage(state: State<'_, AppState>) -> UsageState {
    UsageState {
        snapshot: state.usage.read().unwrap().clone(),
        error: state.usage_error.read().unwrap().clone(),
    }
}

#[tauri::command]
pub async fn refresh_usage(app: AppHandle) -> CmdResult<UsageState> {
    let a = app.clone();
    blocking(move || jobs::refresh_usage(&a)).await?;
    Ok(get_usage(app.state::<AppState>()))
}

#[tauri::command]
pub async fn update_pricing(app: AppHandle) -> CmdResult<usize> {
    let a = app.clone();
    let n = blocking(move || {
        let state = a.state::<AppState>();
        let mut engine = state.engine.lock().unwrap();
        engine.update_pricing_from_network()
    })
    .await?
    .map_err(err)?;
    let a = app.clone();
    blocking(move || jobs::refresh_usage(&a)).await?;
    Ok(n)
}

// ---------- 账号与额度 ----------

#[tauri::command]
pub fn get_accounts(state: State<'_, AppState>) -> Vec<ad_quota::Account> {
    let quotas = state.usage.read().unwrap().as_ref().map(|s| s.quotas.clone()).unwrap_or_default();
    ad_quota::accounts(&state.home, &state.state_dir, &quotas)
}

#[tauri::command]
pub fn claude_bridge_status(state: State<'_, AppState>) -> ad_quota::bridge::BridgeStatus {
    ad_quota::bridge::status(&state.home, &state.state_dir)
}

/// 接入或断开 Claude Code 状态栏（用来拿 Claude 的 5 小时和每周额度）。
#[tauri::command]
pub fn set_claude_bridge(app: AppHandle, enabled: bool) -> CmdResult<ad_quota::bridge::BridgeStatus> {
    let state = app.state::<AppState>();
    if enabled {
        ad_quota::bridge::install(&state.home, &state.state_dir).map_err(err)?;
    } else {
        ad_quota::bridge::uninstall(&state.home, &state.state_dir).map_err(err)?;
    }
    let _ = app.emit("usage-updated", ());
    Ok(ad_quota::bridge::status(&state.home, &state.state_dir))
}

// ---------- MCP / 技能 / 钩子 ----------

#[tauri::command]
pub async fn get_inventory(state: State<'_, AppState>) -> CmdResult<Inventory> {
    let (home, dir) = (state.home.clone(), state.state_dir.clone());
    blocking(move || ad_inventory::scan(&home, &dir)).await
}

/// kind: "skill" | "mcp" | "hook"
#[tauri::command]
pub async fn set_enabled(state: State<'_, AppState>, kind: String, id: String, enabled: bool) -> CmdResult<()> {
    let (home, dir) = (state.home.clone(), state.state_dir.clone());
    blocking(move || match kind.as_str() {
        "skill" => ad_inventory::set_skill_enabled(&home, &dir, &id, enabled),
        "mcp" => ad_inventory::set_mcp_enabled(&home, &dir, &id, enabled),
        "hook" => ad_inventory::set_hook_enabled(&home, &dir, &id, enabled),
        other => Err(anyhow::anyhow!("未知类型：{other}")),
    })
    .await?
    .map_err(err)
}

#[tauri::command]
pub async fn trash_skill(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let (home, dir) = (state.home.clone(), state.state_dir.clone());
    blocking(move || ad_inventory::trash_skill(&home, &dir, &id)).await?.map_err(err)
}

#[tauri::command]
pub async fn read_skill_md(path: String) -> CmdResult<String> {
    blocking(move || ad_inventory::read_skill_md(std::path::Path::new(&path))).await?.map_err(err)
}

// ---------- 安全 ----------

#[tauri::command]
pub fn get_security(state: State<'_, AppState>) -> Option<ScanReport> {
    state.security.read().unwrap().clone()
}

#[tauri::command]
pub async fn run_security_scan(app: AppHandle) -> CmdResult<ScanReport> {
    let a = app.clone();
    let report = blocking(move || {
        let state = a.state::<AppState>();
        let report = ad_security::scan(&ScanOptions { home: state.home.clone(), extra_project_dirs: vec![] });
        *state.security.write().unwrap() = Some(report.clone());
        report
    })
    .await?;
    let _ = app.emit("security-updated", ());
    Ok(report)
}

// ---------- 磁盘 ----------

#[tauri::command]
pub fn get_disk(state: State<'_, AppState>) -> Option<DiskReport> {
    state.disk.read().unwrap().clone()
}

#[tauri::command]
pub async fn run_disk_scan(app: AppHandle, include_projects: bool) -> CmdResult<DiskReport> {
    let a = app.clone();
    blocking(move || jobs::scan_disk(&a, include_projects)).await?.map_err(err)
}

#[tauri::command]
pub async fn assess_items(items: Vec<AssessInput>) -> CmdResult<Vec<Assessment>> {
    blocking(move || ad_disk::assess_with_ai(&items)).await?.map_err(err)
}

#[tauri::command]
pub async fn trash_paths(app: AppHandle, paths: Vec<String>) -> CmdResult<Vec<TrashResult>> {
    let a = app.clone();
    blocking(move || {
        // 先把对话记录里的用量收进缓存，删掉原文件后统计不丢
        jobs::refresh_usage(&a);
        let state = a.state::<AppState>();
        let results = ad_disk::move_to_trash(&state.home, &paths);
        if results.iter().any(|r| r.ok) {
            let include_projects = state
                .disk
                .read()
                .unwrap()
                .as_ref()
                .map(|d| d.projects.iter().any(|p| p.folder_bytes.is_some()))
                .unwrap_or(false);
            let home = state.home.clone();
            if let Ok(report) = ad_disk::scan(&DiskScanOptions { home, include_project_folders: include_projects }) {
                *state.disk.write().unwrap() = Some(report);
                let _ = a.emit("disk-updated", ());
            }
        }
        results
    })
    .await
}

// ---------- 设置与窗口 ----------

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings.read().unwrap().clone()
}

#[tauri::command]
pub fn save_settings(app: AppHandle, settings: Settings) -> CmdResult<Settings> {
    let state = app.state::<AppState>();
    let old = state.settings.read().unwrap().clone();
    settings.save(&state.state_dir).map_err(err)?;
    *state.settings.write().unwrap() = settings.clone();

    if old.theme != settings.theme {
        windows::apply_theme(&app);
    }
    if old.edge_enabled != settings.edge_enabled || old.edge_side != settings.edge_side {
        windows::ensure_edge(&app);
    }
    if old.tray_enabled != settings.tray_enabled {
        crate::tray::ensure_tray(&app);
    }
    crate::tray::update_title(&app);
    if old.launch_at_login != settings.launch_at_login {
        use tauri_plugin_autostart::ManagerExt;
        let launcher = app.autolaunch();
        let r = if settings.launch_at_login { launcher.enable() } else { launcher.disable() };
        r.map_err(err)?;
    }
    let _ = app.emit("settings-updated", ());
    Ok(settings)
}

#[tauri::command]
pub fn open_main(app: AppHandle, page: Option<String>) {
    windows::collapse_edge(&app);
    windows::show_main(&app, page.as_deref());
}

/// 边缘卡片和菜单栏卡片按内容调整高度。
#[tauri::command]
pub fn set_panel_height(app: AppHandle, label: String, height: f64) {
    windows::set_panel_height(&app, &label, height);
}

#[tauri::command]
pub fn hide_tray_panel(app: AppHandle) {
    if let Some(w) = app.get_webview_window(windows::TRAY) {
        let _ = w.hide();
    }
}

fn expand_home(path: &str) -> std::path::PathBuf {
    match path.strip_prefix('~') {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest.trim_start_matches('/')),
        None => std::path::PathBuf::from(path),
    }
}

/// 在访达里显示
#[tauri::command]
pub fn reveal_path(path: String) -> CmdResult<()> {
    std::process::Command::new("open").arg("-R").arg(expand_home(&path)).spawn().map_err(err)?;
    Ok(())
}

/// 用默认程序打开（文件夹在访达里打开）
#[tauri::command]
pub fn open_path(path: String) -> CmdResult<()> {
    std::process::Command::new("open").arg(expand_home(&path)).spawn().map_err(err)?;
    Ok(())
}

#[tauri::command]
pub fn quit_app(app: AppHandle) {
    app.exit(0);
}

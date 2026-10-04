//! 后台任务：定时刷新用量，启动后跑一次安全扫描和快速磁盘扫描。

use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

use ad_disk::{DiskReport, DiskScanOptions};
use ad_security::ScanOptions;

use crate::state::AppState;

pub fn refresh_usage(app: &AppHandle) {
    let state = app.state::<AppState>();
    let result = state.engine.lock().unwrap().refresh();
    match result {
        Ok(snap) => {
            *state.usage.write().unwrap() = Some(snap);
            *state.usage_error.write().unwrap() = None;
        }
        Err(e) => {
            *state.usage_error.write().unwrap() = Some(format!("读取用量失败：{e}"));
        }
    }
    crate::tray::update_title(app);
    let _ = app.emit("usage-updated", ());
}

pub fn scan_disk(app: &AppHandle, include_projects: bool) -> anyhow::Result<DiskReport> {
    let state = app.state::<AppState>();
    let _guard = state.disk_busy.lock().unwrap();
    let report = ad_disk::scan(&DiskScanOptions { home: state.home.clone(), include_project_folders: include_projects })?;
    *state.disk.write().unwrap() = Some(report.clone());
    let _ = app.emit("disk-updated", ());
    Ok(report)
}

pub fn spawn_background(app: AppHandle) {
    let a = app.clone();
    std::thread::spawn(move || loop {
        refresh_usage(&a);
        let secs = a.state::<AppState>().settings.read().unwrap().refresh_seconds.max(15);
        std::thread::sleep(Duration::from_secs(secs));
    });

    let a = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(4));
        let state = a.state::<AppState>();
        let report = ad_security::scan(&ScanOptions { home: state.home.clone(), extra_project_dirs: vec![] });
        *state.security.write().unwrap() = Some(report);
        let _ = a.emit("security-updated", ());
    });

    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(20));
        loop {
            // 用户跑过含项目文件夹的完整扫描，就保持同样的范围，免得把详细结果覆盖掉
            let include_projects = app
                .state::<AppState>()
                .disk
                .read()
                .unwrap()
                .as_ref()
                .map(|d| d.projects.iter().any(|p| p.folder_bytes.is_some()))
                .unwrap_or(false);
            if let Err(e) = scan_disk(&app, include_projects) {
                eprintln!("磁盘扫描失败: {e}");
            }
            std::thread::sleep(Duration::from_secs(6 * 3600));
        }
    });
}

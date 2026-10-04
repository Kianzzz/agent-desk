mod commands;
mod jobs;
mod settings;
mod state;
mod tray;
mod windows;

use tauri::{Manager, WindowEvent};

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            windows::show_main(app, None);
        }))
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, None))
        .manage(state::AppState::new())
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            let handle = app.handle().clone();
            tray::ensure_tray(&handle);
            windows::ensure_edge(&handle);
            windows::spawn_edge_watcher(handle.clone());
            jobs::spawn_background(handle.clone());
            // 边缘细条和菜单栏图标都关掉时，至少把主面板打开，免得找不到程序
            let s = handle.state::<state::AppState>().settings.read().unwrap().clone();
            if !s.edge_enabled && !s.tray_enabled {
                windows::show_main(&handle, None);
            }
            Ok(())
        })
        .on_window_event(|window, event| match (window.label(), event) {
            (windows::MAIN, WindowEvent::CloseRequested { api, .. }) => {
                api.prevent_close();
                let _ = window.hide();
                windows::on_main_closed(window.app_handle());
            }
            (windows::TRAY, WindowEvent::Focused(false)) => windows::hide_tray_on_blur(window),
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_usage,
            commands::refresh_usage,
            commands::update_pricing,
            commands::get_accounts,
            commands::claude_bridge_status,
            commands::set_claude_bridge,
            commands::get_inventory,
            commands::set_enabled,
            commands::trash_skill,
            commands::read_skill_md,
            commands::get_security,
            commands::run_security_scan,
            commands::get_disk,
            commands::run_disk_scan,
            commands::assess_items,
            commands::trash_paths,
            commands::get_settings,
            commands::save_settings,
            commands::open_main,
            commands::set_panel_height,
            commands::hide_tray_panel,
            commands::reveal_path,
            commands::open_path,
            commands::quit_app,
        ])
        .build(tauri::generate_context!())
        .expect("启动 Agent Desk 失败")
        .run(|app, event| match event {
            // ⌘Q 和关掉最后一个窗口都只收起主面板，留在菜单栏；真正退出走菜单里的「退出」
            tauri::RunEvent::ExitRequested { api, code: None, .. } => {
                api.prevent_exit();
                if let Some(w) = app.get_webview_window(windows::MAIN) {
                    let _ = w.hide();
                }
                windows::on_main_closed(app);
            }
            // 程序已经在运行时再次打开它（访达、启动台、Spotlight）：显示主面板
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => windows::show_main(app, None),
            _ => {}
        });
}

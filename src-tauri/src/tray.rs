use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

use crate::state::AppState;
use crate::windows;

const TRAY_ID: &str = "agent-desk";

pub fn ensure_tray(app: &AppHandle) {
    let enabled = app.state::<AppState>().settings.read().unwrap().tray_enabled;
    if !enabled {
        let _ = app.remove_tray_by_id(TRAY_ID);
        return;
    }
    if app.tray_by_id(TRAY_ID).is_some() {
        return;
    }
    if let Err(e) = build(app) {
        eprintln!("创建菜单栏图标失败: {e}");
    }
    update_title(app);
}

fn build(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "打开主面板", true, None::<&str>)?;
    let refresh = MenuItem::with_id(app, "refresh", "立即刷新用量", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "设置…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出 Agent Desk", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[&open, &refresh, &PredefinedMenuItem::separator(app)?, &settings, &PredefinedMenuItem::separator(app)?, &quit],
    )?;
    let icon = Image::from_bytes(include_bytes!("../icons/tray@2x.png"))?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .icon_as_template(true)
        .tooltip("Agent Desk")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => windows::show_main(app, None),
            "settings" => windows::show_main(app, Some("settings")),
            "refresh" => {
                let a = app.clone();
                std::thread::spawn(move || crate::jobs::refresh_usage(&a));
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, rect, .. } = event {
                windows::toggle_tray_panel(tray.app_handle(), rect);
            }
        })
        .build(app)?;
    Ok(())
}

/// 菜单栏图标旁边的文字：今天的 token 数。
pub fn update_title(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else { return };
    let state = app.state::<AppState>();
    let settings = state.settings.read().unwrap().clone();
    let totals = state.today_totals();
    let title = settings
        .tray_show_cost
        .then_some(totals)
        .flatten()
        .map(|(t, _)| crate::settings::format_tokens(t));
    let _ = tray.set_title(title.as_deref());
    let tip = match totals {
        Some((t, c)) => format!("Agent Desk · 今天 {} tokens（≈ {}）", crate::settings::format_tokens(t), settings.format_money(c)),
        None => "Agent Desk · 正在读取用量…".to_string(),
    };
    let _ = tray.set_tooltip(Some(tip));
}

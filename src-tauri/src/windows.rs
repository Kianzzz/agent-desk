//! 三个窗口：主面板（main）、屏幕边缘细条（edge）、菜单栏弹出卡片（tray）。
//!
//! 边缘细条的悬停不靠网页里的 mouseenter：窗口不在前台时 WKWebView 收不到鼠标移动事件。
//! 改为后台线程每 80ms 读一次全局光标位置，自己判断进出；收起时窗口对鼠标透明，不挡别的程序。

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, Rect, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};

use crate::state::AppState;

pub const MAIN: &str = "main";
pub const EDGE: &str = "edge";
pub const TRAY: &str = "tray";

const EDGE_STRIP_W: f64 = 6.0;
const EDGE_STRIP_H: f64 = 132.0;
const EDGE_PANEL_W: f64 = 272.0;
const TRAY_W: f64 = 280.0;

static EDGE_EXPANDED: AtomicBool = AtomicBool::new(false);
/// 卡片高度（逻辑像素），由网页量好内容后告诉后端，窗口贴合内容。
static EDGE_PANEL_H: AtomicU32 = AtomicU32::new(440);
static TRAY_H: AtomicU32 = AtomicU32::new(440);

fn edge_panel_h() -> f64 {
    EDGE_PANEL_H.load(Ordering::Relaxed) as f64
}

fn tray_h() -> f64 {
    TRAY_H.load(Ordering::Relaxed) as f64
}

/// 网页量出卡片内容高度后调用；正展开或正显示时立即调整窗口。
pub fn set_panel_height(app: &AppHandle, label: &str, height: f64) {
    let h = height.clamp(160.0, 680.0).round() as u32;
    match label {
        EDGE => {
            if EDGE_PANEL_H.swap(h, Ordering::Relaxed) != h && EDGE_EXPANDED.load(Ordering::SeqCst) {
                if let Some(win) = app.get_webview_window(EDGE) {
                    let right = app.state::<AppState>().settings.read().unwrap().edge_side != "left";
                    if let Some(g) = monitor_geometry(&win) {
                        apply_rect(&win, edge_rect(&g, right, true));
                    }
                }
            }
        }
        TRAY => {
            if TRAY_H.swap(h, Ordering::Relaxed) != h {
                if let Some(win) = app.get_webview_window(TRAY) {
                    if win.is_visible().unwrap_or(false) {
                        let scale = win.scale_factor().unwrap_or(2.0);
                        let _ = win.set_size(PhysicalSize::new((TRAY_W * scale).round() as u32, (h as f64 * scale).round() as u32));
                    }
                }
            }
        }
        _ => {}
    }
}
/// 菜单栏卡片因失去焦点而隐藏的时刻（毫秒）。点图标时卡片会先失焦隐藏、再收到点击，
/// 不记下来的话会立刻又弹出来。
static TRAY_HIDDEN_AT: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn hide_tray_on_blur(win: &tauri::Window) {
    if win.is_visible().unwrap_or(false) {
        TRAY_HIDDEN_AT.store(now_ms(), Ordering::SeqCst);
        let _ = win.hide();
    }
}

fn current_theme(app: &AppHandle) -> Option<tauri::Theme> {
    app.state::<AppState>().settings.read().unwrap().native_theme()
}

/// 设置里换了外观后，让已打开的窗口跟着换（标题栏按钮、滚动条、表单控件）。
pub fn apply_theme(app: &AppHandle) {
    let theme = current_theme(app);
    for win in app.webview_windows().values() {
        let _ = win.set_theme(theme);
    }
}

pub fn show_main(app: &AppHandle, page: Option<&str>) {
    let win = match app.get_webview_window(MAIN) {
        Some(w) => w,
        None => match WebviewWindowBuilder::new(app, MAIN, WebviewUrl::App("index.html".into()))
            .title("Agent Desk")
            .theme(current_theme(app))
            .inner_size(1200.0, 800.0)
            .min_inner_size(980.0, 640.0)
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true)
            .accept_first_mouse(true)
            .center()
            .visible(false)
            .build()
        {
            Ok(w) => w,
            Err(e) => {
                eprintln!("创建主窗口失败: {e}");
                return;
            }
        },
    };
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    let _ = win.show();
    let _ = win.unminimize();
    let _ = win.set_focus();
    if let Some(p) = page {
        let _ = win.emit("navigate", p.to_string());
    }
    if let Some(t) = app.get_webview_window(TRAY) {
        let _ = t.hide();
    }
}

/// 主窗口关闭时只隐藏，并退回「无 Dock 图标」模式。
pub fn on_main_closed(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

fn overlay_builder<'a>(app: &'a AppHandle, label: &'a str) -> WebviewWindowBuilder<'a, tauri::Wry, AppHandle> {
    WebviewWindowBuilder::new(app, label, WebviewUrl::App("index.html".into()))
        .theme(current_theme(app))
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .visible_on_all_workspaces(true)
        .accept_first_mouse(true)
        .focused(false)
        .visible(false)
}

// ---------- 边缘细条 ----------

struct Geometry {
    origin_x: f64,
    origin_y: f64,
    width: f64,
    height: f64,
    scale: f64,
}

fn monitor_geometry(win: &WebviewWindow) -> Option<Geometry> {
    let m = win.primary_monitor().ok().flatten().or_else(|| win.current_monitor().ok().flatten())?;
    let area = m.work_area();
    Some(Geometry {
        origin_x: area.position.x as f64,
        origin_y: area.position.y as f64,
        width: area.size.width as f64,
        height: area.size.height as f64,
        scale: m.scale_factor(),
    })
}

/// 收起和展开时窗口的物理坐标矩形 (x, y, w, h)。
fn edge_rect(g: &Geometry, right: bool, expanded: bool) -> (f64, f64, f64, f64) {
    let (w, h) = if expanded {
        (EDGE_PANEL_W * g.scale, edge_panel_h() * g.scale)
    } else {
        (EDGE_STRIP_W * g.scale, EDGE_STRIP_H * g.scale)
    };
    let center_y = g.origin_y + g.height * 0.42;
    let y = (center_y - h / 2.0).clamp(g.origin_y, g.origin_y + g.height - h);
    let x = if right { g.origin_x + g.width - w } else { g.origin_x };
    (x, y, w, h)
}

fn apply_rect(win: &WebviewWindow, r: (f64, f64, f64, f64)) {
    let _ = win.set_size(PhysicalSize::new(r.2.round() as u32, r.3.round() as u32));
    let _ = win.set_position(PhysicalPosition::new(r.0.round() as i32, r.1.round() as i32));
}

pub fn ensure_edge(app: &AppHandle) {
    let state = app.state::<AppState>();
    let settings = state.settings.read().unwrap().clone();
    if !settings.edge_enabled {
        if let Some(w) = app.get_webview_window(EDGE) {
            let _ = w.close();
        }
        return;
    }
    let win = match app.get_webview_window(EDGE) {
        Some(w) => w,
        None => match overlay_builder(app, EDGE).shadow(false).inner_size(EDGE_STRIP_W, EDGE_STRIP_H).build() {
            Ok(w) => w,
            Err(e) => {
                eprintln!("创建边缘窗口失败: {e}");
                return;
            }
        },
    };
    if let Some(g) = monitor_geometry(&win) {
        apply_rect(&win, edge_rect(&g, settings.edge_side != "left", false));
    }
    EDGE_EXPANDED.store(false, Ordering::SeqCst);
    let _ = win.set_ignore_cursor_events(true);
    let _ = win.show();
    let _ = win.emit("edge-expanded", false);
}

fn set_edge_expanded(app: &AppHandle, win: &WebviewWindow, expanded: bool) {
    if EDGE_EXPANDED.swap(expanded, Ordering::SeqCst) == expanded {
        return;
    }
    let right = app.state::<AppState>().settings.read().unwrap().edge_side != "left";
    if let Some(g) = monitor_geometry(win) {
        apply_rect(win, edge_rect(&g, right, expanded));
    }
    let _ = win.set_ignore_cursor_events(!expanded);
    let _ = win.emit("edge-expanded", expanded);
}

/// 网页里点了按钮之后主动收起（比如点「打开面板」）。
pub fn collapse_edge(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(EDGE) {
        set_edge_expanded(app, &win, false);
    }
}

pub fn spawn_edge_watcher(app: AppHandle) {
    std::thread::spawn(move || {
        let mut hot_since: Option<Instant> = None;
        let mut away_since: Option<Instant> = None;
        loop {
            std::thread::sleep(Duration::from_millis(80));
            let Some(win) = app.get_webview_window(EDGE) else {
                hot_since = None;
                away_since = None;
                std::thread::sleep(Duration::from_millis(500));
                continue;
            };
            let Ok(cursor) = app.cursor_position() else { continue };
            let Some(g) = monitor_geometry(&win) else { continue };
            let right = app.state::<AppState>().settings.read().unwrap().edge_side != "left";
            let expanded = EDGE_EXPANDED.load(Ordering::SeqCst);

            if !expanded {
                // 光标贴到屏幕边缘、并且在细条的高度范围内
                let (_, sy, _, sh) = edge_rect(&g, right, false);
                let edge_hit = if right {
                    cursor.x >= g.origin_x + g.width - 4.0 * g.scale
                } else {
                    cursor.x <= g.origin_x + 4.0 * g.scale
                };
                let in_band = cursor.y >= sy - 8.0 * g.scale && cursor.y <= sy + sh + 8.0 * g.scale;
                if edge_hit && in_band {
                    let since = *hot_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= Duration::from_millis(140) {
                        set_edge_expanded(&app, &win, true);
                        hot_since = None;
                        away_since = None;
                    }
                } else {
                    hot_since = None;
                }
            } else {
                let (x, y, w, h) = edge_rect(&g, right, true);
                let margin = 14.0 * g.scale;
                let inside = cursor.x >= x - margin
                    && cursor.x <= x + w + margin
                    && cursor.y >= y - margin
                    && cursor.y <= y + h + margin;
                if inside {
                    away_since = None;
                } else {
                    let since = *away_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= Duration::from_millis(380) {
                        set_edge_expanded(&app, &win, false);
                        away_since = None;
                    }
                }
            }
        }
    });
}

// ---------- 菜单栏弹出卡片 ----------

pub fn toggle_tray_panel(app: &AppHandle, icon_rect: Rect) {
    let win = match app.get_webview_window(TRAY) {
        Some(w) => w,
        None => match overlay_builder(app, TRAY).shadow(false).inner_size(TRAY_W, tray_h()).build() {
            Ok(w) => w,
            Err(e) => {
                eprintln!("创建菜单栏卡片失败: {e}");
                return;
            }
        },
    };
    if win.is_visible().unwrap_or(false) {
        let _ = win.hide();
        return;
    }
    if now_ms().saturating_sub(TRAY_HIDDEN_AT.load(Ordering::SeqCst)) < 300 {
        return;
    }
    let scale = win
        .current_monitor()
        .ok()
        .flatten()
        .map(|m| m.scale_factor())
        .unwrap_or(2.0);
    let pos = icon_rect.position.to_physical::<f64>(scale);
    let size = icon_rect.size.to_physical::<f64>(scale);
    let w = TRAY_W * scale;
    let mut x = pos.x + size.width / 2.0 - w / 2.0;
    if let Ok(Some(m)) = win.current_monitor() {
        let mx = m.position().x as f64;
        let mw = m.size().width as f64;
        x = x.clamp(mx + 8.0 * scale, mx + mw - w - 8.0 * scale);
    }
    // 卡片上方本身留了 6px 透明边距
    let y = pos.y + size.height;
    let _ = win.set_size(PhysicalSize::new(w.round() as u32, (tray_h() * scale).round() as u32));
    let _ = win.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
    let _ = win.show();
    let _ = win.set_focus();
    let _ = win.emit("tray-shown", ());
}

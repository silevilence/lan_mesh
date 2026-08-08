use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{
    App, AppHandle, Emitter, Manager, State, WindowEvent,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

pub(crate) struct TraySettings {
    close_to_tray: AtomicBool,
}

impl Default for TraySettings {
    fn default() -> Self {
        Self {
            close_to_tray: AtomicBool::new(true),
        }
    }
}

pub(crate) fn setup(app: &mut App) -> tauri::Result<()> {
    app.manage(TraySettings::default());
    let open_main = MenuItem::with_id(app, "open-main-window", "打开主界面", true, None::<&str>)?;
    let exit = MenuItem::with_id(app, "exit-application", "退出程序", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open_main, &exit])?;
    let icon = app
        .default_window_icon()
        .cloned()
        .expect("the application bundle must provide an icon");
    TrayIconBuilder::with_id("lan-mesh-tray")
        .icon(icon)
        .tooltip("LAN Mesh")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id() {
            id if id == "open-main-window" => show_main_window(app),
            id if id == "exit-application" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } | TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                }
            ) {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;

    let window = app
        .get_webview_window("main")
        .expect("the application must provide the main window");
    let event_window = window.clone();
    window.on_window_event(move |event| match event {
        WindowEvent::CloseRequested { api, .. } => {
            let settings = event_window.state::<TraySettings>();
            if settings.close_to_tray.load(Ordering::Relaxed) {
                api.prevent_close();
                hide_main_window(&event_window);
            }
        }
        WindowEvent::Resized(_) if event_window.is_minimized().unwrap_or(false) => {
            hide_main_window(&event_window);
        }
        _ => {}
    });
    Ok(())
}

#[tauri::command]
pub(crate) fn set_close_to_tray(settings: State<'_, TraySettings>, enabled: bool) {
    settings.close_to_tray.store(enabled, Ordering::Relaxed);
}

#[tauri::command]
pub(crate) fn open_main_window(app: AppHandle) {
    show_main_window(&app);
}

#[tauri::command]
pub(crate) fn is_main_window_visible(app: AppHandle) -> bool {
    app.get_webview_window("main")
        .and_then(|window| window.is_visible().ok())
        .unwrap_or(false)
}

fn hide_main_window(window: &tauri::WebviewWindow) {
    let _ = window.hide();
    let _ = window.emit("mesh://tray-hidden", ());
}

pub(crate) fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

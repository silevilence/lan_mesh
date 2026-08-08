use std::sync::atomic::{AtomicBool, Ordering};

use notify_rust::{Notification, NotificationResponse};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

pub(crate) struct NotificationSettings {
    enabled: AtomicBool,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            enabled: AtomicBool::new(true),
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NotificationTarget {
    group_id: String,
    device_id: Option<String>,
}

impl NotificationTarget {
    pub(crate) fn new(group_id: String, device_id: Option<String>) -> Self {
        Self {
            group_id,
            device_id,
        }
    }
}

pub(crate) fn setup(app: &mut tauri::App) {
    app.manage(NotificationSettings::default());
}

#[tauri::command]
pub(crate) fn set_notifications_enabled(settings: State<'_, NotificationSettings>, enabled: bool) {
    settings.enabled.store(enabled, Ordering::Relaxed);
}

pub(crate) fn notify_when_hidden(
    app: &AppHandle,
    title: &str,
    body: &str,
    target: NotificationTarget,
) {
    if !app
        .state::<NotificationSettings>()
        .enabled
        .load(Ordering::Relaxed)
        || app
            .get_webview_window("main")
            .is_some_and(|window| window.is_visible().unwrap_or(false))
    {
        return;
    }

    let app = app.clone();
    let title = title.to_string();
    let body = body.to_string();
    std::thread::spawn(move || {
        let mut notification = Notification::new();
        notification
            .summary(&title)
            .body(&body)
            .app_id("dev.lanmesh.desktop");
        let Ok(handle) = notification.show() else {
            return;
        };
        let _ = handle.wait_for_response(move |response: &NotificationResponse| {
            if response == &NotificationResponse::Default {
                crate::tray::show_main_window(&app);
                let _ = app.emit("mesh://notification-opened", target);
            }
        });
    });
}

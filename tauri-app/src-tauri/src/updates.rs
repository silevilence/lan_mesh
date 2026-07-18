use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_updater::{Update, UpdaterExt};

#[derive(Default)]
pub(crate) struct PendingUpdate(pub(crate) Mutex<Option<Update>>);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateMetadata {
    version: String,
    current_version: String,
    date: Option<String>,
    body: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateProgress {
    downloaded: u64,
    content_length: Option<u64>,
    finished: bool,
}

#[tauri::command]
pub(crate) async fn check_update(
    app: AppHandle,
    pending_update: State<'_, PendingUpdate>,
) -> Result<Option<UpdateMetadata>, String> {
    let update = app
        .updater()
        .map_err(|err| err.to_string())?
        .check()
        .await
        .map_err(|err| err.to_string())?;

    let metadata = update.as_ref().map(|update| UpdateMetadata {
        version: update.version.clone(),
        current_version: update.current_version.clone(),
        date: update.date.map(|date| date.to_string()),
        body: update.body.clone(),
    });

    *pending_update
        .0
        .lock()
        .map_err(|_| "pending update state is unavailable".to_string())? = update;
    Ok(metadata)
}

#[tauri::command]
pub(crate) async fn install_update(
    app: AppHandle,
    pending_update: State<'_, PendingUpdate>,
) -> Result<(), String> {
    let update = pending_update
        .0
        .lock()
        .map_err(|_| "pending update state is unavailable".to_string())?
        .take()
        .ok_or_else(|| "no pending update; check for updates first".to_string())?;

    let downloaded = Arc::new(AtomicU64::new(0));
    let progress_app = app.clone();
    let finished_app = app.clone();
    let progress_downloaded = downloaded.clone();
    update
        .download_and_install(
            move |chunk_length, content_length| {
                let downloaded = progress_downloaded
                    .fetch_add(chunk_length as u64, Ordering::Relaxed)
                    + chunk_length as u64;
                let _ = progress_app.emit(
                    "mesh://update-progress",
                    UpdateProgress {
                        downloaded,
                        content_length,
                        finished: false,
                    },
                );
            },
            move || {
                let _ = finished_app.emit(
                    "mesh://update-progress",
                    UpdateProgress {
                        downloaded: downloaded.load(Ordering::Relaxed),
                        content_length: None,
                        finished: true,
                    },
                );
            },
        )
        .await
        .map_err(|err| err.to_string())
}

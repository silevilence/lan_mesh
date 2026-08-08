use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_updater::{Update, UpdaterExt};

#[derive(Default)]
pub(crate) struct PendingUpdate(pub(crate) Mutex<Option<Update>>);

pub(crate) struct UpdateSettings(pub(crate) Mutex<UpdateSettingsData>);

pub(crate) struct UpdateSettingsData {
    pub(crate) retain_installer: bool,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self(Mutex::new(UpdateSettingsData {
            retain_installer: true,
        }))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateMetadata {
    version: String,
    current_version: String,
    date: Option<String>,
    body: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RetainedUpdatePackage {
    pub(crate) version: String,
    pub(crate) file_name: String,
    pub(crate) total_size: u64,
    pub(crate) sha256: String,
    #[serde(skip)]
    pub(crate) path: PathBuf,
}

#[derive(Deserialize, Serialize)]
struct RetainedUpdateRecord {
    version: String,
    file_name: String,
    storage_name: String,
    total_size: u64,
    sha256: String,
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
    settings: State<'_, UpdateSettings>,
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
    let bytes = update
        .download(
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
        .map_err(|err| err.to_string())?;

    let retain_installer = settings
        .0
        .lock()
        .map_err(|_| "update settings are unavailable".to_string())?
        .retain_installer;
    if retain_installer {
        store_retained_update_package(&app, &update, &bytes).await?;
    }
    update.install(&bytes).map_err(|err| err.to_string())
}

#[tauri::command]
pub(crate) fn set_retain_installer(
    settings: State<'_, UpdateSettings>,
    enabled: bool,
) -> Result<(), String> {
    settings
        .0
        .lock()
        .map_err(|_| "update settings are unavailable".to_string())?
        .retain_installer = enabled;
    Ok(())
}

#[tauri::command]
pub(crate) async fn get_retained_update_package(
    app: AppHandle,
) -> Result<Option<RetainedUpdatePackage>, String> {
    retained_update_package(&app).await
}

pub(crate) async fn retained_update_package(
    app: &AppHandle,
) -> Result<Option<RetainedUpdatePackage>, String> {
    let directory = retained_updates_dir(app)?;
    let metadata_path = directory.join("retained-update.json");
    let record = match tokio::fs::read(&metadata_path).await {
        Ok(contents) => serde_json::from_slice::<RetainedUpdateRecord>(&contents)
            .map_err(|err| format!("failed to read retained update metadata: {err}"))?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("failed to read retained update metadata: {err}")),
    };
    let path = directory.join(&record.storage_name);
    if !path.is_file() {
        return Ok(None);
    }
    Ok(Some(RetainedUpdatePackage {
        version: record.version,
        file_name: record.file_name,
        total_size: record.total_size,
        sha256: record.sha256,
        path,
    }))
}

async fn store_retained_update_package(
    app: &AppHandle,
    update: &Update,
    bytes: &[u8],
) -> Result<(), String> {
    let directory = retained_updates_dir(app)?;
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|err| format!("failed to create retained update directory: {err}"))?;
    let file_name = update_file_name(update);
    let storage_name = format!("retained-{}", safe_file_name(&file_name));
    let path = directory.join(&storage_name);
    tokio::fs::write(&path, bytes)
        .await
        .map_err(|err| format!("failed to retain update package: {err}"))?;
    let sha256 = lan_mesh_core::sha256_file(&path)
        .await
        .map_err(|err| format!("failed to hash retained update package: {err}"))?;
    let metadata_path = directory.join("retained-update.json");
    if let Ok(previous) = tokio::fs::read(&metadata_path).await {
        if let Ok(previous) = serde_json::from_slice::<RetainedUpdateRecord>(&previous) {
            let old_path = directory.join(previous.storage_name);
            if old_path != path {
                let _ = tokio::fs::remove_file(old_path).await;
            }
        }
    }
    let record = RetainedUpdateRecord {
        version: update.version.clone(),
        file_name,
        storage_name,
        total_size: bytes.len() as u64,
        sha256,
    };
    tokio::fs::write(
        metadata_path,
        serde_json::to_vec_pretty(&record)
            .map_err(|err| format!("failed to serialize retained update metadata: {err}"))?,
    )
    .await
    .map_err(|err| format!("failed to save retained update metadata: {err}"))
}

fn retained_updates_dir(app: &AppHandle) -> Result<PathBuf, String> {
    use tauri::Manager;

    app.path()
        .app_local_data_dir()
        .map(|path| path.join("updates"))
        .map_err(|err| format!("failed to resolve retained update directory: {err}"))
}

fn update_file_name(update: &Update) -> String {
    update
        .download_url
        .path_segments()
        .and_then(|segments| segments.last())
        .filter(|name| !name.is_empty())
        .map(safe_file_name)
        .unwrap_or_else(|| format!("LAN-Mesh-{}-update.zip", update.version))
}

fn safe_file_name(value: &str) -> String {
    Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(|name| name.replace(['\\', '/', ':', '*', '?', '"', '<', '>', '|'], "_"))
        .unwrap_or_else(|| "update-package.zip".to_string())
}

pub(crate) fn is_newer_version(version: &str) -> bool {
    let received = semver::Version::parse(version.trim_start_matches('v'));
    let current = semver::Version::parse(env!("CARGO_PKG_VERSION"));
    matches!((received, current), (Ok(received), Ok(current)) if received > current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_strictly_newer_semantic_version_is_installable() {
        let current = semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
        let newer = format!("{}.{}.{}", current.major, current.minor, current.patch + 1);

        assert!(is_newer_version(&newer));
        assert!(!is_newer_version(&current.to_string()));
        assert!(!is_newer_version("not-a-version"));
    }
}

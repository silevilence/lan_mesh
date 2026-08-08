use crate::{
    ids::{err_string, id},
    state::AppState,
    transfers::send_file_from_client,
    views::SharedUpdatePackageResponse,
};
use lan_mesh_core::{FileId, GroupId, MessageTarget};
use std::{
    ffi::OsStr,
    fs::File,
    io,
    path::{Path, PathBuf},
    process::Command,
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

pub(crate) async fn share_retained_update_package(
    app: &AppHandle,
    state: &AppState,
    group_ids: Vec<GroupId>,
) -> Result<Vec<SharedUpdatePackageResponse>, String> {
    let package = retained_update_package(app)
        .await?
        .ok_or_else(|| "请先检查更新获取安装包".to_string())?;
    let mut clients = Vec::with_capacity(group_ids.len());
    {
        let active_clients = state.clients.lock().await;
        for group_id in group_ids {
            let client = active_clients
                .get(&group_id)
                .cloned()
                .ok_or_else(|| "所选群组当前不可连接".to_string())?;
            clients.push(client);
        }
    }

    let mut shared = Vec::with_capacity(clients.len());
    for client in clients {
        let transfer = send_file_from_client(
            app,
            &state.sent_files,
            &client,
            package.path.to_string_lossy().into_owned(),
            MessageTarget::Broadcast,
            None,
            Some(&package.version),
        )
        .await?;
        shared.push(SharedUpdatePackageResponse {
            group_id: id(client.group_id.0),
            file_id: transfer.file_id,
        });
    }
    Ok(shared)
}

pub(crate) async fn install_received_update_package(
    app: &AppHandle,
    state: &AppState,
    file_id: FileId,
) -> Result<(), String> {
    let package = state
        .received_update_packages
        .lock()
        .await
        .get(&file_id)
        .cloned()
        .ok_or_else(|| "更新包元数据不存在".to_string())?;
    let path = package
        .path
        .ok_or_else(|| "更新包尚未完成或未通过校验".to_string())?;
    if !is_newer_version(&package.metadata.version) {
        return Err(format!(
            "更新包版本 {} 不高于当前版本 {}",
            package.metadata.version,
            env!("CARGO_PKG_VERSION")
        ));
    }
    tokio::task::spawn_blocking(move || run_received_installer(&path))
        .await
        .map_err(err_string)??;
    app.exit(0);
    Ok(())
}

fn run_received_installer(path: &Path) -> Result<(), String> {
    let installer = if path.extension() == Some(OsStr::new("zip")) {
        extract_installer(path)?
    } else {
        path.to_path_buf()
    };
    match installer.extension().and_then(OsStr::to_str) {
        Some("exe") => Command::new(&installer)
            .spawn()
            .map_err(|err| format!("无法启动更新安装包: {err}"))?,
        Some("msi") => Command::new("msiexec.exe")
            .args(["/i", &installer.to_string_lossy(), "/promptrestart"])
            .spawn()
            .map_err(|err| format!("无法启动 MSI 安装包: {err}"))?,
        _ => return Err("更新包不包含可运行的 Windows 安装程序".to_string()),
    };
    Ok(())
}

fn extract_installer(path: &Path) -> Result<PathBuf, String> {
    let file = File::open(path).map_err(|err| format!("无法打开更新包: {err}"))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|err| format!("更新包不是有效 ZIP: {err}"))?;
    let output_dir = std::env::temp_dir().join(format!("LAN Mesh-update-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&output_dir).map_err(|err| format!("无法创建安装目录: {err}"))?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|err| format!("无法读取更新包: {err}"))?;
        let Some(name) = entry.enclosed_name() else {
            continue;
        };
        let is_installer = matches!(
            name.extension().and_then(OsStr::to_str),
            Some("exe") | Some("msi")
        );
        if !is_installer || name.components().count() != 1 {
            continue;
        }
        let output = output_dir.join(
            name.file_name()
                .unwrap_or_else(|| OsStr::new("update-installer")),
        );
        let mut target = File::create(&output).map_err(|err| format!("无法创建安装程序: {err}"))?;
        io::copy(&mut entry, &mut target).map_err(|err| format!("无法解压安装程序: {err}"))?;
        return Ok(output);
    }
    Err("更新包不包含可运行的 Windows 安装程序".to_string())
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

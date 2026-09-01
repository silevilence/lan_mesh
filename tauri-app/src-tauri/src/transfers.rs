use crate::{
    DEFAULT_TTL,
    groups::GroupRuntime,
    ids::{err_string, id},
    state::{SentFile, SentFiles},
    views::{SendFileResponse, TransferProgressEvent},
};
use lan_mesh_core::{FileChunkReader, FileId, MessageTarget, update_package_message};
use std::path::Path;
use tauri::{AppHandle, Emitter};

pub(crate) async fn send_file_from_client(
    app: &AppHandle,
    sent_files: &SentFiles,
    runtime: &GroupRuntime,
    path: String,
    target: MessageTarget,
    sender_nickname: Option<String>,
    update_version: Option<&str>,
) -> Result<SendFileResponse, String> {
    let file_id = FileId::new();
    let mut reader = FileChunkReader::open(
        &path,
        file_id,
        runtime.group_id(),
        runtime.session().device_id(),
        target.clone(),
        DEFAULT_TTL,
    )
    .await
    .map_err(err_string)?
    .with_sender_nickname(sender_nickname.clone());
    let chunk_count = reader.chunk_count();
    let total_size = reader.total_size();
    let file_name = Path::new(&path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToString::to_string);
    let mut done_chunks = 0;
    if let Some(version) = update_version {
        runtime
            .session()
            .route_message(update_package_message(
                file_id,
                version.to_string(),
                file_name
                    .clone()
                    .unwrap_or_else(|| "update-package.zip".to_string()),
                total_size,
                reader.sha256().to_string(),
                runtime.group_id(),
                runtime.session().device_id(),
                target.clone(),
                DEFAULT_TTL,
            ))
            .await
            .map_err(err_string)?;
    }
    sent_files.lock().await.insert(
        file_id,
        SentFile {
            path,
            target: target.clone(),
            sender_nickname: sender_nickname.clone(),
        },
    );

    while let Some(message) = reader.next_message().await.map_err(err_string)? {
        runtime
            .session()
            .route_message(message)
            .await
            .map_err(err_string)?;
        let chunk_index = done_chunks;
        done_chunks += 1;
        let _ = app.emit(
            "mesh://transfer-progress",
            TransferProgressEvent {
                group_id: id(runtime.group_id().0),
                file_id: id(file_id.0),
                file_name: file_name.clone(),
                sender_nickname: sender_nickname.clone(),
                direction: "outgoing",
                chunk_index,
                chunk_count,
                done_chunks,
                total_size,
                status: if done_chunks >= chunk_count {
                    "done"
                } else {
                    "running"
                },
                path: None,
                error: None,
                from: None,
                target_device_id: None,
            },
        );
    }

    Ok(SendFileResponse {
        file_id: id(file_id.0),
        chunk_count,
        total_size,
    })
}

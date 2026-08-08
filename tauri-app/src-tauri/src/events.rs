use crate::{
    DEFAULT_TTL,
    ids::{err_string, id, neighbor},
    notifications::{NotificationTarget, notify_when_hidden},
    persistence::{GroupAvailability, GroupStore},
    state::{ReceivedFiles, ReceivedUpdatePackage, ReceivedUpdatePackages, SentFiles},
    views::{
        MemberEvent, MessageEvent, NeighborEvent, TransferProgressEvent, UpdatePackageEvent,
        UpdatePackageReadyEvent,
    },
};
use lan_mesh_core::{
    DeviceRole, FileAssemblyStatus, FileChunkPayload, FileResumeRequestPayload, GroupId, Message,
    MessageHeader, MessageTarget, Session, SessionEvent, resend_file_chunks,
};
use std::path::PathBuf;
use tauri::{AppHandle, Emitter};

pub(crate) async fn forward_events(
    app: AppHandle,
    session: Session,
    group_id: GroupId,
    sent_files: SentFiles,
    received_files: ReceivedFiles,
    received_update_packages: ReceivedUpdatePackages,
    groups: GroupStore,
) {
    let mut events = session.subscribe();
    while let Ok(event) = events.recv().await {
        emit_event(
            &app,
            &session,
            group_id,
            &sent_files,
            &received_files,
            &received_update_packages,
            &groups,
            event,
        )
        .await;
    }
}

async fn emit_event(
    app: &AppHandle,
    session: &Session,
    group_id: GroupId,
    sent_files: &SentFiles,
    received_files: &ReceivedFiles,
    received_update_packages: &ReceivedUpdatePackages,
    groups: &GroupStore,
    event: SessionEvent,
) {
    match event {
        SessionEvent::NeighborOnline {
            neighbor_id,
            peer_addr,
        } => {
            sync_leaf_availability(app, session, group_id, groups, true).await;
            let _ = app.emit(
                "mesh://neighbor-online",
                NeighborEvent {
                    group_id: id(group_id.0),
                    neighbor_id: neighbor(neighbor_id),
                    peer_addr: peer_addr.to_string(),
                },
            );
        }
        SessionEvent::NeighborOffline {
            neighbor_id,
            peer_addr,
        } => {
            sync_leaf_availability(app, session, group_id, groups, false).await;
            let _ = app.emit(
                "mesh://neighbor-offline",
                NeighborEvent {
                    group_id: id(group_id.0),
                    neighbor_id: neighbor(neighbor_id),
                    peer_addr: peer_addr.to_string(),
                },
            );
        }
        SessionEvent::MessageReceived {
            neighbor_id,
            message,
        } => {
            emit_message_side_events(
                app,
                group_id,
                received_files,
                received_update_packages,
                &message,
            )
            .await;
            if let Message::FileResumeRequest { payload, .. } = &message {
                let _ = resend_saved_chunks(app, session, group_id, sent_files, payload).await;
            }
            let _ = app.emit(
                "mesh://message-received",
                MessageEvent {
                    group_id: id(group_id.0),
                    neighbor_id: neighbor(neighbor_id),
                    message,
                },
            );
        }
    }
}

async fn sync_leaf_availability(
    app: &AppHandle,
    session: &Session,
    group_id: GroupId,
    groups: &GroupStore,
    connected: bool,
) {
    if session.role() != DeviceRole::Leaf {
        return;
    }
    let result = groups
        .set_status(
            group_id,
            if connected {
                GroupAvailability::Connected
            } else {
                GroupAvailability::Unreachable
            },
            (!connected).then(|| "与 Relay 的连接已断开".to_string()),
        )
        .await;
    if result.is_ok() {
        let _ = app.emit("mesh://groups-changed", ());
    }
}

pub(crate) async fn resend_saved_chunks(
    app: &AppHandle,
    session: &Session,
    group_id: GroupId,
    sent_files: &SentFiles,
    request: &FileResumeRequestPayload,
) -> Result<usize, String> {
    let sent = sent_files
        .lock()
        .await
        .get(&request.file_id)
        .cloned()
        .ok_or_else(|| "file is not available for resume".to_string())?;
    let messages = resend_file_chunks(
        &sent.path,
        request,
        group_id,
        session.device_id(),
        sent.target,
        DEFAULT_TTL,
    )
    .await
    .map_err(err_string)?;
    let resent_chunks = messages.len();

    for mut message in messages {
        if let Message::FileChunk { payload, .. } = &mut message {
            payload.sender_nickname = sent.sender_nickname.clone();
            let _ = app.emit(
                "mesh://transfer-progress",
                TransferProgressEvent {
                    group_id: id(group_id.0),
                    file_id: id(payload.file_id.0),
                    file_name: Some(payload.file_name.clone()),
                    sender_nickname: payload.sender_nickname.clone(),
                    direction: "outgoing",
                    chunk_index: payload.chunk_index,
                    chunk_count: payload.chunk_count,
                    done_chunks: payload.chunk_index + 1,
                    total_size: payload.total_size,
                    status: if payload.chunk_index + 1 >= payload.chunk_count {
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
        session.route_message(message).await.map_err(err_string)?;
    }

    Ok(resent_chunks)
}

async fn emit_message_side_events(
    app: &AppHandle,
    group_id: GroupId,
    received_files: &ReceivedFiles,
    received_update_packages: &ReceivedUpdatePackages,
    message: &Message,
) {
    match message {
        Message::MemberChanged { payload, .. } => {
            let _ = app.emit(
                "mesh://member-changed",
                MemberEvent {
                    group_id: id(group_id.0),
                    device_id: id(payload.device_id.0),
                    change: payload.change,
                },
            );
        }
        Message::FileChunk { header, payload } => {
            let mut event = if update_package_matches(received_update_packages, payload).await {
                receive_file_chunk(received_files, payload).await
            } else {
                incoming_progress(
                    payload,
                    payload.chunk_index + 1,
                    "failed",
                    None,
                    Some("更新包元数据与文件分片不一致"),
                )
            };
            event.group_id = id(group_id.0);
            event.from = Some(id(header.source_device_id.0));
            event.target_device_id = target_device_id(header);
            if event.status == "done" {
                notify_when_hidden(
                    app,
                    "LAN Mesh 文件已接收",
                    &payload.file_name,
                    NotificationTarget::new(id(group_id.0), target_device_id(header)),
                );
                if let Some(path) = event.path.as_ref().map(PathBuf::from) {
                    let package = {
                        let mut packages = received_update_packages.lock().await;
                        if let Some(package) = packages.get_mut(&payload.file_id) {
                            package.path = Some(path.clone());
                            Some(package.metadata.clone())
                        } else {
                            None
                        }
                    };
                    if let Some(package) = package {
                        let _ = app.emit(
                            "mesh://update-package-ready",
                            UpdatePackageReadyEvent {
                                group_id: id(group_id.0),
                                file_id: id(payload.file_id.0),
                                version: package.version,
                                file_name: package.file_name,
                                source_device_id: id(package.source_device_id.0),
                            },
                        );
                    }
                }
            }
            let _ = app.emit("mesh://transfer-progress", event);
        }
        Message::UpdatePackage { header, payload } => {
            if payload.source_device_id != header.source_device_id {
                return;
            }
            received_update_packages.lock().await.insert(
                payload.file_id,
                ReceivedUpdatePackage {
                    metadata: payload.clone(),
                    path: None,
                },
            );
            let _ = app.emit(
                "mesh://update-package",
                UpdatePackageEvent {
                    group_id: id(group_id.0),
                    file_id: id(payload.file_id.0),
                    version: payload.version.clone(),
                    file_name: payload.file_name.clone(),
                    total_size: payload.total_size,
                    sha256: payload.sha256.clone(),
                    source_device_id: id(payload.source_device_id.0),
                },
            );
        }
        Message::Text { header, payload } => {
            notify_when_hidden(
                app,
                "LAN Mesh 新消息",
                &payload.content,
                NotificationTarget::new(id(group_id.0), target_device_id(header)),
            );
        }
        _ => {}
    }
}

async fn update_package_matches(
    received_update_packages: &ReceivedUpdatePackages,
    payload: &FileChunkPayload,
) -> bool {
    let packages = received_update_packages.lock().await;
    let Some(package) = packages.get(&payload.file_id) else {
        return true;
    };
    package.metadata.file_name == payload.file_name
        && package.metadata.total_size == payload.total_size
        && package.metadata.sha256 == payload.sha256
}

async fn receive_file_chunk(
    received_files: &ReceivedFiles,
    payload: &FileChunkPayload,
) -> TransferProgressEvent {
    let path = received_file_path(payload);
    let mut files = received_files.lock().await;
    if let std::collections::hash_map::Entry::Vacant(entry) = files.entry(payload.file_id) {
        if let Some(parent) = path.parent() {
            if let Err(err) = tokio::fs::create_dir_all(parent).await {
                return incoming_progress(
                    payload,
                    payload.chunk_index + 1,
                    "failed",
                    None,
                    Some(err),
                );
            }
        }
        match lan_mesh_core::FileAssembler::create(
            &path,
            payload.file_id,
            payload.chunk_count,
            payload.total_size,
            payload.sha256.clone(),
        )
        .await
        {
            Ok(assembler) => {
                entry.insert(assembler);
            }
            Err(err) => {
                return incoming_progress(
                    payload,
                    payload.chunk_index + 1,
                    "failed",
                    None,
                    Some(err),
                );
            }
        }
    }

    let Some(assembler) = files.get_mut(&payload.file_id) else {
        return incoming_progress(
            payload,
            payload.chunk_index + 1,
            "failed",
            None,
            Some("missing assembler"),
        );
    };
    match assembler.push_chunk(payload).await {
        Ok(FileAssemblyStatus::Incomplete { missing_chunks }) => incoming_progress(
            payload,
            payload.chunk_count - missing_chunks.len() as u32,
            "running",
            None,
            None::<String>,
        ),
        Ok(FileAssemblyStatus::Complete { path }) => {
            files.remove(&payload.file_id);
            incoming_progress(
                payload,
                payload.chunk_count,
                "done",
                Some(path),
                None::<String>,
            )
        }
        Ok(FileAssemblyStatus::HashMismatch { expected, actual }) => {
            files.remove(&payload.file_id);
            incoming_progress(
                payload,
                payload.chunk_index + 1,
                "failed",
                None,
                Some(format!(
                    "hash mismatch: expected {expected}, actual {actual}"
                )),
            )
        }
        Err(err) => incoming_progress(payload, payload.chunk_index + 1, "failed", None, Some(err)),
    }
}

fn incoming_progress(
    payload: &FileChunkPayload,
    done_chunks: u32,
    status: &'static str,
    path: Option<PathBuf>,
    error: Option<impl ToString>,
) -> TransferProgressEvent {
    TransferProgressEvent {
        group_id: String::new(),
        file_id: id(payload.file_id.0),
        file_name: Some(payload.file_name.clone()),
        sender_nickname: payload.sender_nickname.clone(),
        direction: "incoming",
        chunk_index: payload.chunk_index,
        chunk_count: payload.chunk_count,
        done_chunks,
        total_size: payload.total_size,
        status,
        path: path.map(|path| path.to_string_lossy().to_string()),
        error: error.map(|err| err.to_string()),
        from: None,
        target_device_id: None,
    }
}

fn received_file_path(payload: &FileChunkPayload) -> PathBuf {
    std::env::temp_dir()
        .join("LAN Mesh")
        .join(id(payload.file_id.0))
        .join(safe_file_name(&payload.file_name))
}

fn target_device_id(header: &MessageHeader) -> Option<String> {
    match &header.target {
        MessageTarget::Device { device_id } => Some(id(device_id.0)),
        MessageTarget::Broadcast => None,
    }
}

fn safe_file_name(value: &str) -> String {
    let name = PathBuf::from(value)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("received-file")
        .trim()
        .to_string();
    if name.is_empty() {
        "received-file".to_string()
    } else {
        name.replace(['\\', '/', ':', '*', '?', '"', '<', '>', '|'], "_")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lan_mesh_core::FileId;

    #[tokio::test]
    async fn receive_file_chunk_assembles_one_file() {
        let received_files = Default::default();
        let payload = FileChunkPayload {
            file_id: FileId::new(),
            file_name: "hello.txt".to_string(),
            sender_nickname: None,
            chunk_index: 0,
            chunk_count: 1,
            total_size: 5,
            sha256: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".to_string(),
            data: b"hello".to_vec(),
        };

        let event = receive_file_chunk(&received_files, &payload).await;

        assert_eq!(event.status, "done");
        let path = event.path.unwrap();
        assert!(path.ends_with("hello.txt"));
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"hello");
        let _ = tokio::fs::remove_file(path).await;
    }
}

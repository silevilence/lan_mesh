use crate::{
    DEFAULT_TTL, DISCOVERY_PORT,
    events::resend_saved_chunks,
    ids::{
        duration_ms, err_string, id, parse_device_id, parse_file_id, parse_group_id,
        parse_optional_ip, parse_or_new_device_id, parse_or_new_group_id, role_name,
    },
    network::{announcement_targets, network_interfaces, parse_socket_addr},
    state::{AppState, ClientSession, SentFile, current_session, install_session},
    views::{
        ConnectionStatus, MemberView, NeighborView, NetworkInterfaceView, RelayAnnouncementView,
        ResumeFileResponse, SendFileResponse, SessionResponse, TransferProgressEvent, relay_view,
        route_view, session_response,
    },
};
use lan_mesh_core::{
    DeviceId, DeviceRole, FileChunkReader, FileId, FileResumeRequestPayload, GroupId,
    MessageTarget, Session, file_resume_request_message,
};
use std::{net::SocketAddr, path::Path, process::Command, time::Duration};
use tauri::{AppHandle, Emitter, State};

#[tauri::command]
pub(crate) async fn create_group(
    app: AppHandle,
    state: State<'_, AppState>,
    device_id: Option<String>,
    group_id: Option<String>,
    group_name: Option<String>,
    bind_addr: String,
) -> Result<SessionResponse, String> {
    let device_id = parse_or_new_device_id(device_id)?;
    let group_id = parse_or_new_group_id(group_id)?;
    let bind_addr = parse_socket_addr(&bind_addr)?;
    let (session, local_addr) = Session::create_group(device_id, group_id, bind_addr)
        .await
        .map_err(err_string)?;
    let group_name = group_name
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "LAN Mesh".to_string());
    let mut started = false;
    let mut last_err = None;
    for (announce_bind, tcp_addr) in announcement_targets(local_addr) {
        match session
            .start_relay_announcement(
                announce_bind,
                SocketAddr::from(([255, 255, 255, 255], DISCOVERY_PORT)),
                tcp_addr,
                group_name.clone(),
                Duration::from_secs(2),
            )
            .await
        {
            Ok(_) => started = true,
            Err(err) => last_err = Some(err),
        }
    }
    if !started {
        return Err(last_err
            .map(err_string)
            .unwrap_or_else(|| "failed to start relay announcement".to_string()));
    }

    install_session(&app, &state, ClientSession { session, group_id }).await;

    Ok(session_response(
        device_id,
        group_id,
        DeviceRole::Relay,
        Some(local_addr.to_string()),
        None,
    ))
}

#[tauri::command]
pub(crate) async fn discover_relays(
    bind_addr: String,
    duration_ms: Option<u64>,
) -> Result<Vec<RelayAnnouncementView>, String> {
    let session = Session::new(DeviceId::new(), GroupId::new(), DeviceRole::Leaf);
    let result = session
        .discover_relays(
            parse_socket_addr(&bind_addr)?,
            Duration::from_millis(duration_ms.unwrap_or(1000)),
        )
        .await
        .map_err(err_string)?
        .into_iter()
        .map(relay_view)
        .collect();
    session.destroy().await;
    Ok(result)
}

#[tauri::command]
pub(crate) async fn join_group(
    app: AppHandle,
    state: State<'_, AppState>,
    device_id: Option<String>,
    group_id: String,
    relay_addr: String,
    local_ip: Option<String>,
) -> Result<SessionResponse, String> {
    let device_id = parse_or_new_device_id(device_id)?;
    let group_id = parse_group_id(&group_id)?;
    let relay_addr = parse_socket_addr(&relay_addr)?;
    let local_ip = parse_optional_ip(local_ip)?;
    let (session, neighbor_id) = Session::join_group(device_id, group_id, relay_addr, local_ip)
        .await
        .map_err(err_string)?;

    install_session(&app, &state, ClientSession { session, group_id }).await;

    Ok(session_response(
        device_id,
        group_id,
        DeviceRole::Leaf,
        None,
        Some(neighbor_id),
    ))
}

#[tauri::command]
pub(crate) async fn close_session(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(task) = state.event_task.lock().await.take() {
        task.abort();
    }
    let client = state.client.lock().await.take();
    if let Some(client) = client {
        client.session.destroy().await;
    }
    state.sent_files.lock().await.clear();
    state.received_files.lock().await.clear();
    Ok(())
}

#[tauri::command]
pub(crate) async fn send_group_text(
    state: State<'_, AppState>,
    content: String,
) -> Result<String, String> {
    current_session(&state)
        .await?
        .session
        .send_group_message(content)
        .await
        .map(|message_id| id(message_id.0))
        .map_err(err_string)
}

#[tauri::command]
pub(crate) async fn send_direct_text(
    state: State<'_, AppState>,
    target_device_id: String,
    content: String,
) -> Result<String, String> {
    let target_device_id = parse_device_id(&target_device_id)?;
    current_session(&state)
        .await?
        .session
        .send_direct_message(target_device_id, content)
        .await
        .map(|message_id| id(message_id.0))
        .map_err(err_string)
}

#[tauri::command]
pub(crate) async fn send_file(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    target_device_id: Option<String>,
) -> Result<SendFileResponse, String> {
    let client = current_session(&state).await?;
    let file_id = FileId::new();
    let target = match target_device_id
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        Some(value) => MessageTarget::Device {
            device_id: parse_device_id(value)?,
        },
        None => MessageTarget::Broadcast,
    };
    let mut reader = FileChunkReader::open(
        &path,
        file_id,
        client.group_id,
        client.session.device_id(),
        target.clone(),
        DEFAULT_TTL,
    )
    .await
    .map_err(err_string)?;
    let chunk_count = reader.chunk_count();
    let total_size = reader.total_size();
    let file_name = Path::new(&path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_string());
    let mut done_chunks = 0;
    state.sent_files.lock().await.insert(
        file_id,
        SentFile {
            path,
            target: target.clone(),
        },
    );

    while let Some(message) = reader.next_message().await.map_err(err_string)? {
        client
            .session
            .route_message(message)
            .await
            .map_err(err_string)?;
        let chunk_index = done_chunks;
        done_chunks += 1;
        let _ = app.emit(
            "mesh://transfer-progress",
            TransferProgressEvent {
                file_id: id(file_id.0),
                file_name: file_name.clone(),
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

#[tauri::command]
pub(crate) async fn resume_file_transfer(
    app: AppHandle,
    state: State<'_, AppState>,
    file_id: String,
    missing_chunks: Vec<u32>,
) -> Result<ResumeFileResponse, String> {
    let client = current_session(&state).await?;
    let request = FileResumeRequestPayload {
        file_id: parse_file_id(&file_id)?,
        missing_chunks,
    };
    let resent_chunks = resend_saved_chunks(
        &app,
        &client.session,
        client.group_id,
        &state.sent_files,
        &request,
    )
    .await?;
    Ok(ResumeFileResponse {
        file_id,
        resent_chunks,
    })
}

#[tauri::command]
pub(crate) async fn request_file_resume(
    state: State<'_, AppState>,
    file_id: String,
    missing_chunks: Vec<u32>,
    target_device_id: Option<String>,
) -> Result<String, String> {
    let client = current_session(&state).await?;
    let target = match target_device_id
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        Some(value) => MessageTarget::Device {
            device_id: parse_device_id(value)?,
        },
        None => MessageTarget::Broadcast,
    };
    let message = file_resume_request_message(
        parse_file_id(&file_id)?,
        missing_chunks,
        client.group_id,
        client.session.device_id(),
        target,
        DEFAULT_TTL,
    );
    client
        .session
        .route_message(message)
        .await
        .map_err(err_string)?;
    Ok(file_id)
}

#[tauri::command]
pub(crate) async fn get_members(state: State<'_, AppState>) -> Result<Vec<MemberView>, String> {
    Ok(current_session(&state)
        .await?
        .session
        .members()
        .await
        .into_iter()
        .map(|member| MemberView {
            device_id: id(member.device_id.0),
            online: member.online,
            last_seen_ms: duration_ms(member.last_seen_elapsed),
        })
        .collect())
}

#[tauri::command]
pub(crate) async fn get_connection_status(
    state: State<'_, AppState>,
) -> Result<ConnectionStatus, String> {
    let client = current_session(&state).await?;
    let neighbors = client
        .session
        .neighbors()
        .await
        .into_iter()
        .map(|item| NeighborView {
            neighbor_id: crate::ids::neighbor(item.neighbor_id),
            peer_addr: item.peer_addr.to_string(),
            last_active_ms: duration_ms(item.last_active_elapsed),
        })
        .collect();
    let routes = client
        .session
        .routes()
        .await
        .into_iter()
        .map(route_view)
        .collect();

    Ok(ConnectionStatus {
        device_id: id(client.session.device_id().0),
        group_id: id(client.group_id.0),
        role: role_name(client.session.role()),
        neighbors,
        routes,
    })
}

#[tauri::command]
pub(crate) fn list_network_interfaces() -> Vec<NetworkInterfaceView> {
    network_interfaces()
}

#[tauri::command]
pub(crate) async fn pick_file() -> Result<String, String> {
    tokio::task::spawn_blocking(|| {
        #[cfg(target_os = "windows")]
        {
            let script = r#"Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.OpenFileDialog; if ($d.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) { $d.FileName }"#;
            let output = Command::new("powershell")
                .args(["-NoProfile", "-STA", "-Command", script])
                .output()
                .map_err(err_string)?;
            if !output.status.success() {
                return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
            }
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if path.is_empty() {
                Err("未选择文件".to_string())
            } else {
                Ok(path)
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            Err("当前平台未实现文件选择，请手动填写绝对路径".to_string())
        }
    })
    .await
    .map_err(err_string)?
}

#[tauri::command]
pub(crate) async fn save_file_as(
    path: String,
    file_name: Option<String>,
) -> Result<String, String> {
    let destination = pick_save_path(file_name.unwrap_or_else(|| {
        Path::new(&path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("received-file")
            .to_string()
    }))
    .await?;
    tokio::fs::copy(&path, &destination)
        .await
        .map_err(err_string)?;
    Ok(destination)
}

async fn pick_save_path(file_name: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        #[cfg(target_os = "windows")]
        {
            let script = r#"Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.SaveFileDialog; $d.FileName = [Environment]::GetEnvironmentVariable('LAN_MESH_FILE_NAME'); if ($d.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) { $d.FileName }"#;
            let output = Command::new("powershell")
                .env("LAN_MESH_FILE_NAME", file_name)
                .args(["-NoProfile", "-STA", "-Command", script])
                .output()
                .map_err(err_string)?;
            if !output.status.success() {
                return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
            }
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if path.is_empty() {
                Err("未选择保存位置".to_string())
            } else {
                Ok(path)
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            Err("当前平台未实现另存为".to_string())
        }
    })
    .await
    .map_err(err_string)?
}

use crate::{
    DEFAULT_TTL,
    events::resend_saved_chunks,
    groups::{CreateRelayRequest, JoinLeafRequest},
    ids::{
        duration_ms, err_string, id, parse_device_id, parse_file_id, parse_group_id,
        parse_optional_ip, parse_or_new_device_id, parse_or_new_group_id, role_name,
    },
    network::{discovery_bind_addrs, network_interfaces, parse_socket_addr},
    state::AppState,
    views::{
        ConnectionStatus, MemberView, NeighborView, NetworkInterfaceView, ProbeRelayResponse,
        RelayAnnouncementView, ResumeFileResponse, SavedGroupView, SendFileResponse,
        SessionResponse, ShareUpdatePackagesResponse, relay_view, route_view, saved_group_view,
        session_response,
    },
};
use lan_mesh_core::{
    DeviceId, DeviceRole, FileResumeRequestPayload, GroupId, MessageTarget, Session,
    file_resume_request_message,
};
use std::{
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};
use tauri::{AppHandle, State};
use tokio::{net::TcpSocket, task::JoinSet, time::timeout};

#[tauri::command]
pub(crate) async fn create_group(
    state: State<'_, AppState>,
    device_id: Option<String>,
    group_id: Option<String>,
    group_name: Option<String>,
    bind_addr: String,
) -> Result<SessionResponse, String> {
    let device_id = parse_or_new_device_id(device_id)?;
    let group_id = parse_or_new_group_id(group_id)?;
    let bind_addr = parse_socket_addr(&bind_addr)?;
    let group_name = group_name
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "LAN Mesh".to_string());
    let started = state
        .groups
        .create_relay(CreateRelayRequest {
            device_id: Some(device_id),
            group_id,
            group_name,
            bind_addr,
        })
        .await?;

    Ok(session_response(
        started.snapshot.device_id,
        started.snapshot.group_id,
        started.snapshot.role,
        started.snapshot.bind_addr,
        started.neighbor_id,
    ))
}

#[tauri::command]
pub(crate) async fn discover_relays(
    bind_addr: String,
    duration_ms: Option<u64>,
) -> Result<Vec<RelayAnnouncementView>, String> {
    let duration = Duration::from_millis(duration_ms.unwrap_or(1000));
    let mut scans = JoinSet::new();
    for bind_addr in discovery_bind_addrs(parse_socket_addr(&bind_addr)?) {
        scans.spawn(async move {
            let session = Session::new(DeviceId::new(), GroupId::new(), DeviceRole::Leaf);
            let result = session.discover_relays(bind_addr, duration).await;
            session.destroy().await;
            result
        });
    }

    let mut relays = std::collections::HashMap::new();
    while let Some(result) = scans.join_next().await {
        let result = result.map_err(err_string)?.map_err(err_string)?;
        for relay in result {
            relays.insert((relay.device_id, relay.tcp_addr), relay);
        }
    }
    Ok(relays.into_values().map(relay_view).collect())
}

#[tauri::command]
pub(crate) async fn join_group(
    state: State<'_, AppState>,
    device_id: Option<String>,
    group_id: String,
    group_name: Option<String>,
    relay_addr: String,
    local_ip: Option<String>,
) -> Result<SessionResponse, String> {
    let group_id = parse_group_id(&group_id)?;
    let device_id = parse_or_new_device_id(device_id)?;
    let relay_addr = parse_socket_addr(&relay_addr)?;
    let local_ip = parse_optional_ip(local_ip)?;
    let group_name = group_name
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "LAN Mesh".to_string());
    let started = state
        .groups
        .join_leaf(JoinLeafRequest {
            device_id: Some(device_id),
            group_id,
            group_name,
            relay_addr,
            local_ip,
        })
        .await?;

    Ok(session_response(
        started.snapshot.device_id,
        started.snapshot.group_id,
        started.snapshot.role,
        started.snapshot.bind_addr,
        started.neighbor_id,
    ))
}

#[tauri::command]
pub(crate) async fn close_session(
    state: State<'_, AppState>,
    group_id: String,
) -> Result<(), String> {
    state.groups.remove(parse_group_id(&group_id)?).await
}

#[tauri::command]
pub(crate) async fn list_saved_groups(
    state: State<'_, AppState>,
) -> Result<Vec<SavedGroupView>, String> {
    Ok(state
        .groups
        .list()
        .await
        .into_iter()
        .map(saved_group_view)
        .collect())
}

#[tauri::command]
pub(crate) async fn activate_saved_group(
    state: State<'_, AppState>,
    group_id: String,
) -> Result<SavedGroupView, String> {
    let group_id = parse_group_id(&group_id)?;
    activate_persisted_group(&state, group_id).await
}

async fn activate_persisted_group(
    state: &AppState,
    group_id: GroupId,
) -> Result<SavedGroupView, String> {
    state.groups.runtime(group_id).await?;
    state.groups.snapshot(group_id).await.map(saved_group_view)
}

#[tauri::command]
pub(crate) async fn retry_saved_group(
    state: State<'_, AppState>,
    group_id: String,
) -> Result<SavedGroupView, String> {
    let group_id = parse_group_id(&group_id)?;
    state
        .groups
        .retry(group_id)
        .await
        .map(|started| saved_group_view(started.snapshot))
}

#[tauri::command]
pub(crate) async fn delete_saved_group(
    state: State<'_, AppState>,
    group_id: String,
) -> Result<(), String> {
    state.groups.remove(parse_group_id(&group_id)?).await
}

#[tauri::command]
pub(crate) async fn send_group_text(
    state: State<'_, AppState>,
    group_id: String,
    content: String,
    sender_nickname: Option<String>,
) -> Result<String, String> {
    state
        .groups
        .runtime(parse_group_id(&group_id)?)
        .await?
        .session()
        .send_group_message_with_nickname(content, clean_nickname(sender_nickname))
        .await
        .map(|message_id| id(message_id.0))
        .map_err(err_string)
}

#[tauri::command]
pub(crate) async fn send_direct_text(
    state: State<'_, AppState>,
    group_id: String,
    target_device_id: String,
    content: String,
    sender_nickname: Option<String>,
) -> Result<String, String> {
    let target_device_id = parse_device_id(&target_device_id)?;
    state
        .groups
        .runtime(parse_group_id(&group_id)?)
        .await?
        .session()
        .send_direct_message_with_nickname(
            target_device_id,
            content,
            clean_nickname(sender_nickname),
        )
        .await
        .map(|message_id| id(message_id.0))
        .map_err(err_string)
}

#[tauri::command]
pub(crate) async fn announce_nickname(
    state: State<'_, AppState>,
    group_id: String,
    nickname: Option<String>,
) -> Result<(), String> {
    state
        .groups
        .runtime(parse_group_id(&group_id)?)
        .await?
        .session()
        .announce_nickname(clean_nickname(nickname))
        .await
        .map_err(err_string)
}

#[tauri::command]
pub(crate) async fn send_file(
    app: AppHandle,
    state: State<'_, AppState>,
    group_id: String,
    path: String,
    target_device_id: Option<String>,
    sender_nickname: Option<String>,
) -> Result<SendFileResponse, String> {
    let runtime = state.groups.runtime(parse_group_id(&group_id)?).await?;
    let target = match target_device_id
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        Some(value) => MessageTarget::Device {
            device_id: parse_device_id(value)?,
        },
        None => MessageTarget::Broadcast,
    };
    crate::transfers::send_file_from_client(
        &app,
        &state.sent_files,
        &runtime,
        path,
        target,
        clean_nickname(sender_nickname),
        None,
    )
    .await
}

#[tauri::command]
pub(crate) async fn resume_file_transfer(
    app: AppHandle,
    state: State<'_, AppState>,
    group_id: String,
    file_id: String,
    missing_chunks: Vec<u32>,
) -> Result<ResumeFileResponse, String> {
    let runtime = state.groups.runtime(parse_group_id(&group_id)?).await?;
    let request = FileResumeRequestPayload {
        file_id: parse_file_id(&file_id)?,
        missing_chunks,
    };
    let resent_chunks = resend_saved_chunks(
        &app,
        runtime.session(),
        runtime.group_id(),
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
    group_id: String,
    file_id: String,
    missing_chunks: Vec<u32>,
    target_device_id: Option<String>,
) -> Result<String, String> {
    let runtime = state.groups.runtime(parse_group_id(&group_id)?).await?;
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
        runtime.group_id(),
        runtime.session().device_id(),
        target,
        DEFAULT_TTL,
    );
    runtime
        .session()
        .route_message(message)
        .await
        .map_err(err_string)?;
    Ok(file_id)
}

#[tauri::command]
pub(crate) async fn get_members(
    state: State<'_, AppState>,
    group_id: String,
) -> Result<Vec<MemberView>, String> {
    Ok(state
        .groups
        .runtime(parse_group_id(&group_id)?)
        .await?
        .session()
        .members()
        .await
        .into_iter()
        .map(|member| MemberView {
            device_id: id(member.device_id.0),
            online: member.online,
            nickname: member.nickname,
            last_seen_ms: duration_ms(member.last_seen_elapsed),
        })
        .collect())
}

#[tauri::command]
pub(crate) async fn get_connection_status(
    state: State<'_, AppState>,
    group_id: String,
) -> Result<ConnectionStatus, String> {
    let runtime = state.groups.runtime(parse_group_id(&group_id)?).await?;
    let neighbors = runtime
        .session()
        .neighbors()
        .await
        .into_iter()
        .map(|item| NeighborView {
            neighbor_id: crate::ids::neighbor(item.neighbor_id),
            peer_addr: item.peer_addr.to_string(),
            last_active_ms: duration_ms(item.last_active_elapsed),
        })
        .collect();
    let routes = runtime
        .session()
        .routes()
        .await
        .into_iter()
        .map(route_view)
        .collect();

    Ok(ConnectionStatus {
        device_id: id(runtime.session().device_id().0),
        group_id: id(runtime.group_id().0),
        role: role_name(runtime.session().role()),
        neighbors,
        routes,
    })
}

#[tauri::command]
pub(crate) fn list_network_interfaces() -> Vec<NetworkInterfaceView> {
    network_interfaces()
}

#[tauri::command]
pub(crate) async fn probe_relay_addr(
    relay_addrs: Vec<String>,
    local_ips: Vec<String>,
    timeout_ms: Option<u64>,
) -> Result<ProbeRelayResponse, String> {
    let timeout_duration = Duration::from_millis(timeout_ms.unwrap_or(250).clamp(50, 2000));
    let relay_addrs: Vec<_> = relay_addrs
        .into_iter()
        .filter_map(|value| parse_socket_addr(&value).ok())
        .collect();
    if relay_addrs.is_empty() {
        return Err("分享码里没有有效的 Relay 地址".to_string());
    }

    let mut local_ips: Vec<_> = local_ips
        .into_iter()
        .filter_map(|value| value.parse::<IpAddr>().ok())
        .collect();
    local_ips.sort();
    local_ips.dedup();

    for relay_addr in relay_addrs {
        for local_ip in &local_ips {
            if can_connect(relay_addr, Some(*local_ip), timeout_duration).await {
                return Ok(ProbeRelayResponse {
                    relay_addr: relay_addr.to_string(),
                    local_ip: Some(local_ip.to_string()),
                });
            }
        }
        if can_connect(relay_addr, None, timeout_duration).await {
            return Ok(ProbeRelayResponse {
                relay_addr: relay_addr.to_string(),
                local_ip: None,
            });
        }
    }

    Err("分享码中的地址当前都连不上".to_string())
}

async fn can_connect(
    addr: SocketAddr,
    local_ip: Option<IpAddr>,
    timeout_duration: Duration,
) -> bool {
    let socket = match addr {
        SocketAddr::V4(_) => TcpSocket::new_v4(),
        SocketAddr::V6(_) => TcpSocket::new_v6(),
    };
    let Ok(socket) = socket else {
        return false;
    };
    if let Some(local_ip) = local_ip
        && (local_ip.is_ipv4() != addr.is_ipv4()
            || socket.bind(SocketAddr::new(local_ip, 0)).is_err())
    {
        return false;
    }
    timeout(timeout_duration, socket.connect(addr))
        .await
        .is_ok_and(|result| result.is_ok())
}

#[tauri::command]
pub(crate) async fn pick_file() -> Result<String, String> {
    tokio::task::spawn_blocking(|| {
        rfd::FileDialog::new()
            .pick_file()
            .map(path_string)
            .ok_or_else(|| "未选择文件".to_string())
    })
    .await
    .map_err(err_string)?
}

#[tauri::command]
pub(crate) async fn save_temp_file(file_name: String, bytes: Vec<u8>) -> Result<String, String> {
    let path = std::env::temp_dir()
        .join("LAN Mesh")
        .join("pasted")
        .join(format!(
            "{}-{}",
            uuid::Uuid::new_v4(),
            safe_file_name(&file_name)
        ));
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(err_string)?;
    }
    tokio::fs::write(&path, bytes).await.map_err(err_string)?;
    Ok(path_string(path))
}

#[tauri::command]
pub(crate) fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[tauri::command]
pub(crate) async fn share_retained_update_package(
    app: AppHandle,
    state: State<'_, AppState>,
    group_ids: Vec<String>,
) -> Result<ShareUpdatePackagesResponse, String> {
    if group_ids.is_empty() {
        return Err("请选择至少一个群组".to_string());
    }
    let group_ids = group_ids
        .into_iter()
        .map(|group_id| parse_group_id(&group_id))
        .collect::<Result<_, _>>()?;
    crate::updates::share_retained_update_package(&app, &state, group_ids).await
}

#[tauri::command]
pub(crate) async fn install_received_update_package(
    app: AppHandle,
    state: State<'_, AppState>,
    file_id: String,
) -> Result<(), String> {
    let file_id = parse_file_id(&file_id)?;
    crate::updates::install_received_update_package(&app, &state, file_id).await
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
        rfd::FileDialog::new()
            .set_file_name(file_name)
            .save_file()
            .map(path_string)
            .ok_or_else(|| "未选择保存位置".to_string())
    })
    .await
    .map_err(err_string)?
}

fn path_string(path: std::path::PathBuf) -> String {
    path.to_string_lossy().into_owned()
}

fn clean_nickname(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().chars().take(24).collect::<String>())
        .filter(|value| !value.is_empty())
}

fn safe_file_name(value: &str) -> String {
    let name = PathBuf::from(value)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("pasted-file")
        .trim()
        .to_string();
    if name.is_empty() {
        "pasted-file".to_string()
    } else {
        name.replace(['\\', '/', ':', '*', '?', '"', '<', '>', '|'], "_")
    }
}

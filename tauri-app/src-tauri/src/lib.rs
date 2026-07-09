use lan_mesh_core::{
    DeviceId, DeviceRole, FileChunkReader, FileId, GroupId, MemberChange, Message, MessageTarget,
    NeighborId, RelayAnnouncement, RouteSnapshot, Session, SessionEvent,
};
use serde::Serialize;
use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};
use tauri::{AppHandle, Emitter, State};
use tokio::{sync::Mutex, task::JoinHandle};
use uuid::Uuid;

const DEFAULT_TTL: u8 = 8;

#[derive(Default)]
struct AppState {
    client: Mutex<Option<ClientSession>>,
    event_task: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Clone)]
struct ClientSession {
    session: Session,
    group_id: GroupId,
}

#[derive(Serialize)]
struct SessionResponse {
    device_id: String,
    group_id: String,
    role: &'static str,
    bind_addr: Option<String>,
    neighbor_id: Option<String>,
}

#[derive(Serialize)]
struct RelayAnnouncementView {
    device_id: String,
    group_id: String,
    group_name: String,
    tcp_addr: String,
    timestamp_ms: u64,
}

#[derive(Serialize)]
struct MemberView {
    device_id: String,
    online: bool,
    last_seen_ms: u64,
}

#[derive(Serialize)]
struct NeighborView {
    neighbor_id: String,
    peer_addr: String,
    last_active_ms: u64,
}

#[derive(Serialize)]
struct RouteView {
    target_device_id: String,
    next_hop: String,
    path: Vec<String>,
    last_updated_ms: u64,
}

#[derive(Serialize)]
struct ConnectionStatus {
    device_id: String,
    group_id: String,
    role: &'static str,
    neighbors: Vec<NeighborView>,
    routes: Vec<RouteView>,
}

#[derive(Clone, Serialize)]
struct NeighborEvent {
    neighbor_id: String,
    peer_addr: String,
}

#[derive(Clone, Serialize)]
struct MessageEvent {
    neighbor_id: String,
    message: Message,
}

#[derive(Clone, Serialize)]
struct MemberEvent {
    device_id: String,
    change: MemberChange,
}

#[derive(Clone, Serialize)]
struct TransferProgressEvent {
    file_id: String,
    direction: &'static str,
    chunk_index: u32,
    chunk_count: u32,
    done_chunks: u32,
    total_size: u64,
}

#[derive(Serialize)]
struct SendFileResponse {
    file_id: String,
    chunk_count: u32,
    total_size: u64,
}

#[tauri::command]
async fn create_group(
    app: AppHandle,
    state: State<'_, AppState>,
    device_id: Option<String>,
    group_id: Option<String>,
    bind_addr: String,
) -> Result<SessionResponse, String> {
    let device_id = parse_or_new_device_id(device_id)?;
    let group_id = parse_or_new_group_id(group_id)?;
    let bind_addr = parse_socket_addr(&bind_addr)?;
    let (session, local_addr) = Session::create_group(device_id, group_id, bind_addr)
        .await
        .map_err(err_string)?;

    install_session(&app, &state, ClientSession { session, group_id }).await;

    Ok(SessionResponse {
        device_id: id(device_id.0),
        group_id: id(group_id.0),
        role: role_name(DeviceRole::Relay),
        bind_addr: Some(local_addr.to_string()),
        neighbor_id: None,
    })
}

#[tauri::command]
async fn discover_relays(
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
async fn join_group(
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

    Ok(SessionResponse {
        device_id: id(device_id.0),
        group_id: id(group_id.0),
        role: role_name(DeviceRole::Leaf),
        bind_addr: None,
        neighbor_id: Some(neighbor(neighbor_id)),
    })
}

#[tauri::command]
async fn send_group_text(state: State<'_, AppState>, content: String) -> Result<String, String> {
    current_session(&state)
        .await?
        .session
        .send_group_message(content)
        .await
        .map(|message_id| id(message_id.0))
        .map_err(err_string)
}

#[tauri::command]
async fn send_direct_text(
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
async fn send_file(
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
        path,
        file_id,
        client.group_id,
        client.session.device_id(),
        target,
        DEFAULT_TTL,
    )
    .await
    .map_err(err_string)?;
    let chunk_count = reader.chunk_count();
    let total_size = reader.total_size();
    let mut done_chunks = 0;

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
                direction: "outgoing",
                chunk_index,
                chunk_count,
                done_chunks,
                total_size,
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
async fn get_members(state: State<'_, AppState>) -> Result<Vec<MemberView>, String> {
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
async fn get_connection_status(state: State<'_, AppState>) -> Result<ConnectionStatus, String> {
    let client = current_session(&state).await?;
    let neighbors = client
        .session
        .neighbors()
        .await
        .into_iter()
        .map(|item| NeighborView {
            neighbor_id: neighbor(item.neighbor_id),
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

pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            create_group,
            discover_relays,
            join_group,
            send_group_text,
            send_direct_text,
            send_file,
            get_members,
            get_connection_status,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run LAN Mesh Tauri app");
}

async fn install_session(app: &AppHandle, state: &AppState, client: ClientSession) {
    if let Some(task) = state.event_task.lock().await.take() {
        task.abort();
    }
    if let Some(old_client) = state.client.lock().await.replace(client.clone()) {
        old_client.session.destroy().await;
    }
    let task = tokio::spawn(forward_events(app.clone(), client.session));
    *state.event_task.lock().await = Some(task);
}

async fn current_session(state: &AppState) -> Result<ClientSession, String> {
    state
        .client
        .lock()
        .await
        .clone()
        .ok_or_else(|| "no active mesh session".to_string())
}

async fn forward_events(app: AppHandle, session: Session) {
    let mut events = session.subscribe();
    while let Ok(event) = events.recv().await {
        emit_event(&app, event);
    }
}

fn emit_event(app: &AppHandle, event: SessionEvent) {
    match event {
        SessionEvent::NeighborOnline {
            neighbor_id,
            peer_addr,
        } => {
            let _ = app.emit(
                "mesh://neighbor-online",
                NeighborEvent {
                    neighbor_id: neighbor(neighbor_id),
                    peer_addr: peer_addr.to_string(),
                },
            );
        }
        SessionEvent::NeighborOffline {
            neighbor_id,
            peer_addr,
        } => {
            let _ = app.emit(
                "mesh://neighbor-offline",
                NeighborEvent {
                    neighbor_id: neighbor(neighbor_id),
                    peer_addr: peer_addr.to_string(),
                },
            );
        }
        SessionEvent::MessageReceived {
            neighbor_id,
            message,
        } => {
            emit_message_side_events(app, &message);
            let _ = app.emit(
                "mesh://message-received",
                MessageEvent {
                    neighbor_id: neighbor(neighbor_id),
                    message,
                },
            );
        }
    }
}

fn emit_message_side_events(app: &AppHandle, message: &Message) {
    match message {
        Message::MemberChanged { payload, .. } => {
            let _ = app.emit(
                "mesh://member-changed",
                MemberEvent {
                    device_id: id(payload.device_id.0),
                    change: payload.change,
                },
            );
        }
        Message::FileChunk { payload, .. } => {
            let _ = app.emit(
                "mesh://transfer-progress",
                TransferProgressEvent {
                    file_id: id(payload.file_id.0),
                    direction: "incoming",
                    chunk_index: payload.chunk_index,
                    chunk_count: payload.chunk_count,
                    done_chunks: payload.chunk_index + 1,
                    total_size: payload.total_size,
                },
            );
        }
        _ => {}
    }
}

fn relay_view(relay: RelayAnnouncement) -> RelayAnnouncementView {
    RelayAnnouncementView {
        device_id: id(relay.device_id.0),
        group_id: id(relay.group_id.0),
        group_name: relay.group_name,
        tcp_addr: relay.tcp_addr.to_string(),
        timestamp_ms: relay.timestamp_ms,
    }
}

fn route_view(route: RouteSnapshot) -> RouteView {
    RouteView {
        target_device_id: id(route.target_device_id.0),
        next_hop: neighbor(route.next_hop),
        path: route
            .path
            .into_iter()
            .map(|device_id| id(device_id.0))
            .collect(),
        last_updated_ms: duration_ms(route.last_updated_elapsed),
    }
}

fn parse_or_new_device_id(value: Option<String>) -> Result<DeviceId, String> {
    value
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(parse_device_id)
        .unwrap_or_else(|| Ok(DeviceId::new()))
}

fn parse_or_new_group_id(value: Option<String>) -> Result<GroupId, String> {
    value
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(parse_group_id)
        .unwrap_or_else(|| Ok(GroupId::new()))
}

fn parse_device_id(value: &str) -> Result<DeviceId, String> {
    Uuid::parse_str(value)
        .map(DeviceId)
        .map_err(|err| format!("invalid device_id: {err}"))
}

fn parse_group_id(value: &str) -> Result<GroupId, String> {
    Uuid::parse_str(value)
        .map(GroupId)
        .map_err(|err| format!("invalid group_id: {err}"))
}

fn parse_socket_addr(value: &str) -> Result<SocketAddr, String> {
    value
        .parse()
        .map_err(|err| format!("invalid socket address: {err}"))
}

fn parse_optional_ip(value: Option<String>) -> Result<Option<IpAddr>, String> {
    value
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse()
                .map_err(|err| format!("invalid local_ip: {err}"))
        })
        .transpose()
}

fn id(uuid: Uuid) -> String {
    uuid.to_string()
}

fn neighbor(neighbor_id: NeighborId) -> String {
    id(neighbor_id.0)
}

fn role_name(role: DeviceRole) -> &'static str {
    match role {
        DeviceRole::Relay => "relay",
        DeviceRole::Leaf => "leaf",
    }
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn err_string(err: impl ToString) -> String {
    err.to_string()
}

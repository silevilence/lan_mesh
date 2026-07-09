use crate::ids::{duration_ms, id, neighbor, role_name};
use lan_mesh_core::{
    DeviceRole, MemberChange, Message, NeighborId, RelayAnnouncement, RouteSnapshot,
};
use serde::Serialize;

#[derive(Serialize)]
pub(crate) struct SessionResponse {
    pub(crate) device_id: String,
    pub(crate) group_id: String,
    pub(crate) role: &'static str,
    pub(crate) bind_addr: Option<String>,
    pub(crate) neighbor_id: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct RelayAnnouncementView {
    device_id: String,
    group_id: String,
    group_name: String,
    tcp_addr: String,
    timestamp_ms: u64,
}

#[derive(Serialize)]
pub(crate) struct MemberView {
    pub(crate) device_id: String,
    pub(crate) online: bool,
    pub(crate) last_seen_ms: u64,
}

#[derive(Serialize)]
pub(crate) struct NeighborView {
    pub(crate) neighbor_id: String,
    pub(crate) peer_addr: String,
    pub(crate) last_active_ms: u64,
}

#[derive(Serialize)]
pub(crate) struct RouteView {
    target_device_id: String,
    next_hop: String,
    path: Vec<String>,
    last_updated_ms: u64,
}

#[derive(Serialize)]
pub(crate) struct ConnectionStatus {
    pub(crate) device_id: String,
    pub(crate) group_id: String,
    pub(crate) role: &'static str,
    pub(crate) neighbors: Vec<NeighborView>,
    pub(crate) routes: Vec<RouteView>,
}

#[derive(Serialize)]
pub(crate) struct NetworkInterfaceView {
    pub(crate) name: String,
    pub(crate) ip_addr: String,
    pub(crate) bind_addr: String,
    pub(crate) discovery_bind_addr: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct NeighborEvent {
    pub(crate) neighbor_id: String,
    pub(crate) peer_addr: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct MessageEvent {
    pub(crate) neighbor_id: String,
    pub(crate) message: Message,
}

#[derive(Clone, Serialize)]
pub(crate) struct MemberEvent {
    pub(crate) device_id: String,
    pub(crate) change: MemberChange,
}

#[derive(Clone, Serialize)]
pub(crate) struct TransferProgressEvent {
    pub(crate) file_id: String,
    pub(crate) file_name: Option<String>,
    pub(crate) direction: &'static str,
    pub(crate) chunk_index: u32,
    pub(crate) chunk_count: u32,
    pub(crate) done_chunks: u32,
    pub(crate) total_size: u64,
    pub(crate) status: &'static str,
    pub(crate) path: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) from: Option<String>,
    pub(crate) target_device_id: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct SendFileResponse {
    pub(crate) file_id: String,
    pub(crate) chunk_count: u32,
    pub(crate) total_size: u64,
}

#[derive(Serialize)]
pub(crate) struct ResumeFileResponse {
    pub(crate) file_id: String,
    pub(crate) resent_chunks: usize,
}

pub(crate) fn session_response(
    device_id: lan_mesh_core::DeviceId,
    group_id: lan_mesh_core::GroupId,
    role: DeviceRole,
    bind_addr: Option<String>,
    neighbor_id: Option<NeighborId>,
) -> SessionResponse {
    SessionResponse {
        device_id: id(device_id.0),
        group_id: id(group_id.0),
        role: role_name(role),
        bind_addr,
        neighbor_id: neighbor_id.map(neighbor),
    }
}

pub(crate) fn relay_view(relay: RelayAnnouncement) -> RelayAnnouncementView {
    RelayAnnouncementView {
        device_id: id(relay.device_id.0),
        group_id: id(relay.group_id.0),
        group_name: relay.group_name,
        tcp_addr: relay.tcp_addr.to_string(),
        timestamp_ms: relay.timestamp_ms,
    }
}

pub(crate) fn route_view(route: RouteSnapshot) -> RouteView {
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

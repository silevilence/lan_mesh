//! Core LAN Mesh communication library.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fmt, io,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpSocket, TcpStream, UdpSocket, tcp::OwnedReadHalf, tcp::OwnedWriteHalf},
    sync::{Mutex, broadcast, mpsc},
    task::JoinHandle,
    time::{self, Instant},
};
use uuid::Uuid;

pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;
const MESSAGE_ID_TTL: Duration = Duration::from_secs(300);
const MESSAGE_ID_CLEANUP_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_ROUTE_TTL: u8 = 8;
pub type TimestampMs = u64;

macro_rules! uuid_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }
    };
}

uuid_id!(DeviceId);
uuid_id!(GroupId);
uuid_id!(MessageId);
uuid_id!(FileId);
uuid_id!(NeighborId);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceRole {
    Relay,
    Leaf,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageTarget {
    Broadcast,
    Device { device_id: DeviceId },
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct MessageHeader {
    pub message_id: MessageId,
    pub group_id: GroupId,
    pub source_device_id: DeviceId,
    pub target: MessageTarget,
    pub ttl: u8,
    pub hop_count: u8,
    pub timestamp_ms: TimestampMs,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct TextPayload {
    pub content: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct FileChunkPayload {
    pub file_id: FileId,
    pub chunk_index: u32,
    pub chunk_count: u32,
    #[serde(with = "base64_data")]
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct HeartbeatPayload {
    pub device_id: DeviceId,
    pub timestamp_ms: TimestampMs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberChange {
    Online,
    Offline,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct MemberChangedPayload {
    pub device_id: DeviceId,
    pub change: MemberChange,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct RouteDiscoveryRequestPayload {
    pub target_device_id: DeviceId,
    pub path: Vec<DeviceId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct RouteDiscoveryResponsePayload {
    pub request_message_id: MessageId,
    pub path: Vec<DeviceId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Text {
        header: MessageHeader,
        payload: TextPayload,
    },
    FileChunk {
        header: MessageHeader,
        payload: FileChunkPayload,
    },
    Heartbeat {
        header: MessageHeader,
        payload: HeartbeatPayload,
    },
    MemberChanged {
        header: MessageHeader,
        payload: MemberChangedPayload,
    },
    RouteDiscoveryRequest {
        header: MessageHeader,
        payload: RouteDiscoveryRequestPayload,
    },
    RouteDiscoveryResponse {
        header: MessageHeader,
        payload: RouteDiscoveryResponsePayload,
    },
}

#[derive(Debug)]
pub enum FrameError {
    Io(io::Error),
    Json(serde_json::Error),
    FrameTooLarge { len: usize, max: usize },
}

#[derive(Debug)]
pub enum NetworkError {
    Io(io::Error),
    Frame(FrameError),
    WrongRole {
        required: DeviceRole,
        actual: DeviceRole,
    },
    LeafAlreadyConnected,
    NeighborMissing(NeighborId),
    ChannelClosed,
}

impl fmt::Display for NetworkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::Frame(err) => write!(f, "frame error: {err}"),
            Self::WrongRole { required, actual } => {
                write!(
                    f,
                    "wrong role: {actual:?} cannot perform {required:?} operation"
                )
            }
            Self::LeafAlreadyConnected => write!(f, "leaf session already has a neighbor"),
            Self::NeighborMissing(id) => write!(f, "neighbor not found: {}", id.0),
            Self::ChannelClosed => write!(f, "connection writer is closed"),
        }
    }
}

impl std::error::Error for NetworkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Frame(err) => Some(err),
            Self::WrongRole { .. }
            | Self::LeafAlreadyConnected
            | Self::NeighborMissing(_)
            | Self::ChannelClosed => None,
        }
    }
}

impl From<io::Error> for NetworkError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<FrameError> for NetworkError {
    fn from(err: FrameError) -> Self {
        Self::Frame(err)
    }
}

#[derive(Clone, Debug)]
pub struct ConnectionConfig {
    pub heartbeat_interval: Duration,
    pub heartbeat_timeout: Duration,
    pub timeout_check_interval: Duration,
    pub reconnect_initial_delay: Duration,
    pub reconnect_max_delay: Duration,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(5),
            heartbeat_timeout: Duration::from_secs(15),
            timeout_check_interval: Duration::from_secs(1),
            reconnect_initial_delay: Duration::from_secs(1),
            reconnect_max_delay: Duration::from_secs(30),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionEvent {
    NeighborOnline {
        neighbor_id: NeighborId,
        peer_addr: SocketAddr,
    },
    NeighborOffline {
        neighbor_id: NeighborId,
        peer_addr: SocketAddr,
    },
    MessageReceived {
        neighbor_id: NeighborId,
        message: Message,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NeighborSnapshot {
    pub neighbor_id: NeighborId,
    pub peer_addr: SocketAddr,
    pub last_active_elapsed: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemberSnapshot {
    pub device_id: DeviceId,
    pub online: bool,
    pub last_seen_elapsed: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteSnapshot {
    pub target_device_id: DeviceId,
    pub next_hop: NeighborId,
    pub path: Vec<DeviceId>,
    pub last_updated_elapsed: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct RelayAnnouncement {
    pub device_id: DeviceId,
    pub group_id: GroupId,
    pub group_name: String,
    pub tcp_addr: SocketAddr,
    pub timestamp_ms: TimestampMs,
}

#[derive(Clone)]
pub struct Session {
    inner: Arc<SessionInner>,
}

struct SessionInner {
    device_id: DeviceId,
    group_id: GroupId,
    role: DeviceRole,
    config: ConnectionConfig,
    neighbors: Mutex<HashMap<NeighborId, NeighborConnection>>,
    seen_messages: Mutex<HashMap<MessageId, Instant>>,
    routes: Mutex<HashMap<DeviceId, RouteEntry>>,
    reverse_routes: Mutex<HashMap<MessageId, ReverseRouteEntry>>,
    members: Mutex<HashMap<DeviceId, MemberEntry>>,
    events: broadcast::Sender<SessionEvent>,
    shutdown: broadcast::Sender<()>,
    leaf_reconnect_target: Mutex<Option<ReconnectTarget>>,
}

struct NeighborConnection {
    peer_addr: SocketAddr,
    device_id: Option<DeviceId>,
    sender: mpsc::UnboundedSender<Message>,
    last_active: Arc<Mutex<Instant>>,
    read_handle: JoinHandle<()>,
    write_handle: JoinHandle<()>,
    heartbeat_handle: JoinHandle<()>,
}

#[derive(Clone, Debug)]
struct ReconnectTarget {
    peer_addr: SocketAddr,
    local_ip: Option<IpAddr>,
}

struct MemberEntry {
    online: bool,
    last_seen: Instant,
}

struct RouteEntry {
    next_hop: NeighborId,
    path: Vec<DeviceId>,
    updated_at: Instant,
}

struct ReverseRouteEntry {
    neighbor_id: NeighborId,
    created_at: Instant,
}

impl Session {
    pub fn new(device_id: DeviceId, group_id: GroupId, role: DeviceRole) -> Self {
        Self::with_config(device_id, group_id, role, ConnectionConfig::default())
    }

    pub fn with_config(
        device_id: DeviceId,
        group_id: GroupId,
        role: DeviceRole,
        config: ConnectionConfig,
    ) -> Self {
        let (events, _) = broadcast::channel(256);
        let (shutdown, _) = broadcast::channel(1);
        let mut members = HashMap::new();
        members.insert(
            device_id,
            MemberEntry {
                online: true,
                last_seen: Instant::now(),
            },
        );
        let session = Self {
            inner: Arc::new(SessionInner {
                device_id,
                group_id,
                role,
                config,
                neighbors: Mutex::new(HashMap::new()),
                seen_messages: Mutex::new(HashMap::new()),
                routes: Mutex::new(HashMap::new()),
                reverse_routes: Mutex::new(HashMap::new()),
                members: Mutex::new(members),
                events,
                shutdown,
                leaf_reconnect_target: Mutex::new(None),
            }),
        };
        tokio::spawn(timeout_loop(session.inner.clone()));
        tokio::spawn(dedup_cleanup_loop(session.inner.clone()));
        if role == DeviceRole::Leaf {
            tokio::spawn(leaf_reconnect_loop(session.inner.clone()));
        }
        session
    }

    pub async fn create_group(
        device_id: DeviceId,
        group_id: GroupId,
        bind_addr: SocketAddr,
    ) -> Result<(Self, SocketAddr), NetworkError> {
        let session = Self::new(device_id, group_id, DeviceRole::Relay);
        let addr = session.listen(bind_addr).await?;
        Ok((session, addr))
    }

    pub async fn join_group(
        device_id: DeviceId,
        group_id: GroupId,
        relay_addr: SocketAddr,
        local_ip: Option<IpAddr>,
    ) -> Result<(Self, NeighborId), NetworkError> {
        let session = Self::new(device_id, group_id, DeviceRole::Leaf);
        let neighbor_id = session.connect(relay_addr, local_ip).await?;
        Ok((session, neighbor_id))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.inner.events.subscribe()
    }

    pub fn role(&self) -> DeviceRole {
        self.inner.role
    }

    pub fn device_id(&self) -> DeviceId {
        self.inner.device_id
    }

    pub async fn listen(&self, bind_addr: SocketAddr) -> Result<SocketAddr, NetworkError> {
        if self.inner.role != DeviceRole::Relay {
            return Err(NetworkError::WrongRole {
                required: DeviceRole::Relay,
                actual: self.inner.role,
            });
        }

        let listener = TcpListener::bind(bind_addr).await?;
        let local_addr = listener.local_addr()?;
        tokio::spawn(accept_loop(self.inner.clone(), listener));
        Ok(local_addr)
    }

    pub async fn connect(
        &self,
        peer_addr: SocketAddr,
        local_ip: Option<IpAddr>,
    ) -> Result<NeighborId, NetworkError> {
        if self.inner.role == DeviceRole::Leaf && !self.inner.neighbors.lock().await.is_empty() {
            return Err(NetworkError::LeafAlreadyConnected);
        }

        let stream = connect_stream(peer_addr, local_ip).await?;
        let neighbor_id = self.inner.register_neighbor(stream).await?;

        if self.inner.role == DeviceRole::Leaf {
            *self.inner.leaf_reconnect_target.lock().await = Some(ReconnectTarget {
                peer_addr,
                local_ip,
            });
        }

        Ok(neighbor_id)
    }

    pub async fn send_message(
        &self,
        neighbor_id: NeighborId,
        message: Message,
    ) -> Result<(), NetworkError> {
        self.inner.send_message(neighbor_id, message).await
    }

    pub async fn broadcast_message(&self, message: Message) -> Result<(), NetworkError> {
        self.inner.inject_message(message).await
    }

    pub async fn route_message(&self, message: Message) -> Result<(), NetworkError> {
        self.inner.inject_message(message).await
    }

    pub async fn discover_route(
        &self,
        target_device_id: DeviceId,
        ttl: u8,
    ) -> Result<MessageId, NetworkError> {
        self.inner
            .start_route_discovery(target_device_id, ttl)
            .await
    }

    pub async fn routes(&self) -> Vec<RouteSnapshot> {
        let routes = self.inner.routes.lock().await;
        routes
            .iter()
            .map(|(target_device_id, route)| RouteSnapshot {
                target_device_id: *target_device_id,
                next_hop: route.next_hop,
                path: route.path.clone(),
                last_updated_elapsed: route.updated_at.elapsed(),
            })
            .collect()
    }

    pub fn member_changed_message(&self, device_id: DeviceId, change: MemberChange) -> Message {
        member_changed_message(&self.inner, device_id, change)
    }

    pub async fn announce_member_change(
        &self,
        device_id: DeviceId,
        change: MemberChange,
    ) -> Result<(), NetworkError> {
        self.inner.apply_member_change(device_id, change).await;
        self.inner
            .broadcast_message(member_changed_message(&self.inner, device_id, change))
            .await
    }

    pub async fn members(&self) -> Vec<MemberSnapshot> {
        let members = self.inner.members.lock().await;
        members
            .iter()
            .map(|(device_id, member)| MemberSnapshot {
                device_id: *device_id,
                online: member.online,
                last_seen_elapsed: member.last_seen.elapsed(),
            })
            .collect()
    }

    pub async fn neighbors(&self) -> Vec<NeighborSnapshot> {
        let neighbors = self.inner.neighbors.lock().await;
        let mut snapshots = Vec::with_capacity(neighbors.len());
        for (neighbor_id, neighbor) in neighbors.iter() {
            snapshots.push(NeighborSnapshot {
                neighbor_id: *neighbor_id,
                peer_addr: neighbor.peer_addr,
                last_active_elapsed: neighbor.last_active.lock().await.elapsed(),
            });
        }
        snapshots
    }

    pub async fn destroy(&self) {
        let _ = self
            .announce_member_change(self.inner.device_id, MemberChange::Offline)
            .await;
        let _ = self.inner.shutdown.send(());
        self.inner.leaf_reconnect_target.lock().await.take();
        self.inner.seen_messages.lock().await.clear();
        self.inner.routes.lock().await.clear();
        self.inner.reverse_routes.lock().await.clear();
        self.inner.members.lock().await.clear();
        let neighbor_ids: Vec<_> = self.inner.neighbors.lock().await.keys().copied().collect();
        for neighbor_id in neighbor_ids {
            self.inner.remove_neighbor(neighbor_id).await;
        }
    }

    pub async fn start_relay_announcement(
        &self,
        bind_addr: SocketAddr,
        announce_addr: SocketAddr,
        tcp_addr: SocketAddr,
        group_name: impl Into<String>,
        interval: Duration,
    ) -> Result<SocketAddr, NetworkError> {
        if self.inner.role != DeviceRole::Relay {
            return Err(NetworkError::WrongRole {
                required: DeviceRole::Relay,
                actual: self.inner.role,
            });
        }

        let socket = UdpSocket::bind(bind_addr).await?;
        socket.set_broadcast(true)?;
        let local_addr = socket.local_addr()?;
        let payload = serde_json::to_vec(&RelayAnnouncement {
            device_id: self.inner.device_id,
            group_id: self.inner.group_id,
            group_name: group_name.into(),
            tcp_addr,
            timestamp_ms: now_timestamp_ms(),
        })
        .expect("relay announcement should serialize");
        let mut shutdown = self.inner.shutdown.subscribe();
        tokio::spawn(async move {
            loop {
                let _ = socket.send_to(&payload, announce_addr).await;
                tokio::select! {
                    _ = time::sleep(interval) => {}
                    _ = shutdown.recv() => return,
                }
            }
        });
        Ok(local_addr)
    }

    pub async fn discover_relays(
        &self,
        bind_addr: SocketAddr,
        duration: Duration,
    ) -> Result<Vec<RelayAnnouncement>, NetworkError> {
        if self.inner.role != DeviceRole::Leaf {
            return Err(NetworkError::WrongRole {
                required: DeviceRole::Leaf,
                actual: self.inner.role,
            });
        }

        let socket = UdpSocket::bind(bind_addr).await?;
        Ok(collect_relay_announcements(socket, duration).await)
    }
}

impl SessionInner {
    async fn register_neighbor(
        self: &Arc<Self>,
        stream: TcpStream,
    ) -> Result<NeighborId, NetworkError> {
        let peer_addr = stream.peer_addr()?;
        let neighbor_id = NeighborId::new();
        let last_active = Arc::new(Mutex::new(Instant::now()));
        let (reader, writer) = stream.into_split();
        let (sender, receiver) = mpsc::unbounded_channel();

        let read_handle = tokio::spawn(read_loop(
            self.clone(),
            neighbor_id,
            reader,
            last_active.clone(),
        ));
        let write_handle = tokio::spawn(write_loop(writer, receiver));
        let heartbeat_handle = tokio::spawn(heartbeat_loop(self.clone(), neighbor_id));

        self.neighbors.lock().await.insert(
            neighbor_id,
            NeighborConnection {
                peer_addr,
                device_id: None,
                sender,
                last_active,
                read_handle,
                write_handle,
                heartbeat_handle,
            },
        );
        let _ = self.events.send(SessionEvent::NeighborOnline {
            neighbor_id,
            peer_addr,
        });
        let _ = self
            .broadcast_message(member_changed_message(
                self,
                self.device_id,
                MemberChange::Online,
            ))
            .await;
        Ok(neighbor_id)
    }

    async fn send_message(
        &self,
        neighbor_id: NeighborId,
        message: Message,
    ) -> Result<(), NetworkError> {
        let sender = self
            .neighbors
            .lock()
            .await
            .get(&neighbor_id)
            .ok_or(NetworkError::NeighborMissing(neighbor_id))?
            .sender
            .clone();
        sender
            .send(message)
            .map_err(|_| NetworkError::ChannelClosed)
    }

    async fn broadcast_message(&self, message: Message) -> Result<(), NetworkError> {
        self.send_except(None, message).await
    }

    async fn send_except(
        &self,
        except: Option<NeighborId>,
        message: Message,
    ) -> Result<(), NetworkError> {
        let senders: Vec<_> = self
            .neighbors
            .lock()
            .await
            .iter()
            .filter(|(neighbor_id, _)| Some(**neighbor_id) != except)
            .map(|(_, neighbor)| neighbor.sender.clone())
            .collect();
        for sender in senders {
            sender
                .send(message.clone())
                .map_err(|_| NetworkError::ChannelClosed)?;
        }
        Ok(())
    }

    async fn mark_seen(&self, message_id: MessageId) -> bool {
        let mut seen = self.seen_messages.lock().await;
        let now = Instant::now();
        seen.retain(|_, seen_at| now.duration_since(*seen_at) < MESSAGE_ID_TTL);
        if seen.contains_key(&message_id) {
            return false;
        }
        seen.insert(message_id, now);
        true
    }

    async fn cleanup_seen_messages(&self) {
        let now = Instant::now();
        self.seen_messages
            .lock()
            .await
            .retain(|_, seen_at| now.duration_since(*seen_at) < MESSAGE_ID_TTL);
        self.reverse_routes
            .lock()
            .await
            .retain(|_, route| now.duration_since(route.created_at) < MESSAGE_ID_TTL);
    }

    async fn inject_message(&self, message: Message) -> Result<(), NetworkError> {
        let header = message_header(&message);
        if header.group_id != self.group_id || !self.mark_seen(header.message_id).await {
            return Ok(());
        }
        self.route_outbound_message(message).await
    }

    async fn route_outbound_message(&self, message: Message) -> Result<(), NetworkError> {
        match message_header(&message).target {
            MessageTarget::Broadcast => self.flood_message(None, &message).await,
            MessageTarget::Device { device_id } if device_id == self.device_id => Ok(()),
            MessageTarget::Device { device_id } => {
                if !self.forward_direct_message(device_id, &message).await? {
                    self.start_route_discovery(device_id, DEFAULT_ROUTE_TTL)
                        .await?;
                }
                Ok(())
            }
        }
    }

    async fn handle_received_message(
        self: &Arc<Self>,
        neighbor_id: NeighborId,
        message: &Message,
    ) -> Result<(), NetworkError> {
        let header = message_header(message);
        if header.hop_count == 0 {
            self.note_neighbor_device(neighbor_id, header.source_device_id)
                .await;
        }
        self.store_route(
            header.source_device_id,
            neighbor_id,
            vec![self.device_id, header.source_device_id],
        )
        .await;
        match message {
            Message::MemberChanged { payload, .. } => {
                self.apply_member_change(payload.device_id, payload.change)
                    .await;
            }
            Message::RouteDiscoveryRequest { header, payload } => {
                self.handle_route_discovery_request(neighbor_id, header, payload)
                    .await?;
            }
            Message::RouteDiscoveryResponse { header, payload } => {
                self.handle_route_discovery_response(header, payload)
                    .await?;
            }
            _ => match header.target {
                MessageTarget::Broadcast => self.flood_message(Some(neighbor_id), message).await?,
                MessageTarget::Device { device_id } if device_id == self.device_id => {}
                MessageTarget::Device { device_id } => {
                    let _ = self.forward_direct_message(device_id, message).await?;
                }
            },
        }
        Ok(())
    }

    async fn flood_message(
        &self,
        except: Option<NeighborId>,
        message: &Message,
    ) -> Result<(), NetworkError> {
        let Some(message) = next_hop_message(message) else {
            return Ok(());
        };
        self.send_except(except, message).await
    }

    async fn forward_direct_message(
        &self,
        target_device_id: DeviceId,
        message: &Message,
    ) -> Result<bool, NetworkError> {
        let next_hop = self
            .routes
            .lock()
            .await
            .get(&target_device_id)
            .map(|route| route.next_hop);
        let Some(next_hop) = next_hop else {
            return Ok(false);
        };
        let Some(message) = next_hop_message(message) else {
            return Ok(true);
        };
        if self.send_message(next_hop, message).await.is_err() {
            self.routes.lock().await.remove(&target_device_id);
            return Ok(false);
        }
        Ok(true)
    }

    async fn start_route_discovery(
        &self,
        target_device_id: DeviceId,
        ttl: u8,
    ) -> Result<MessageId, NetworkError> {
        let message_id = MessageId::new();
        let message = Message::RouteDiscoveryRequest {
            header: MessageHeader {
                message_id,
                group_id: self.group_id,
                source_device_id: self.device_id,
                target: MessageTarget::Device {
                    device_id: target_device_id,
                },
                ttl,
                hop_count: 0,
                timestamp_ms: now_timestamp_ms(),
            },
            payload: RouteDiscoveryRequestPayload {
                target_device_id,
                path: vec![self.device_id],
            },
        };
        self.mark_seen(message_id).await;
        self.flood_message(None, &message).await?;
        Ok(message_id)
    }

    async fn handle_route_discovery_request(
        &self,
        neighbor_id: NeighborId,
        header: &MessageHeader,
        payload: &RouteDiscoveryRequestPayload,
    ) -> Result<(), NetworkError> {
        let path = append_device(payload.path.clone(), self.device_id);
        if payload.target_device_id == self.device_id {
            let response = Message::RouteDiscoveryResponse {
                header: MessageHeader {
                    message_id: MessageId::new(),
                    group_id: self.group_id,
                    source_device_id: self.device_id,
                    target: MessageTarget::Device {
                        device_id: header.source_device_id,
                    },
                    ttl: DEFAULT_ROUTE_TTL.max(path.len() as u8),
                    hop_count: 0,
                    timestamp_ms: now_timestamp_ms(),
                },
                payload: RouteDiscoveryResponsePayload {
                    request_message_id: header.message_id,
                    path,
                },
            };
            self.send_message(neighbor_id, response).await?;
            return Ok(());
        }

        self.reverse_routes.lock().await.insert(
            header.message_id,
            ReverseRouteEntry {
                neighbor_id,
                created_at: Instant::now(),
            },
        );
        let mut forwarded = Message::RouteDiscoveryRequest {
            header: header.clone(),
            payload: RouteDiscoveryRequestPayload {
                target_device_id: payload.target_device_id,
                path,
            },
        };
        message_header_mut(&mut forwarded).hop_count = header.hop_count;
        self.flood_message(Some(neighbor_id), &forwarded).await
    }

    async fn handle_route_discovery_response(
        &self,
        header: &MessageHeader,
        payload: &RouteDiscoveryResponsePayload,
    ) -> Result<(), NetworkError> {
        self.store_route(
            header.source_device_id,
            self.next_hop_from_path(payload.path.as_slice()).await?,
            payload.path.clone(),
        )
        .await;
        if let MessageTarget::Device { device_id } = header.target {
            if device_id == self.device_id {
                return Ok(());
            }
        }
        if let Some(reverse) = self
            .reverse_routes
            .lock()
            .await
            .remove(&payload.request_message_id)
        {
            let message = Message::RouteDiscoveryResponse {
                header: header.clone(),
                payload: payload.clone(),
            };
            if let Some(message) = next_hop_message(&message) {
                self.send_message(reverse.neighbor_id, message).await?;
            }
        }
        Ok(())
    }

    async fn next_hop_from_path(&self, path: &[DeviceId]) -> Result<NeighborId, NetworkError> {
        let neighbors = self.neighbors.lock().await;
        for window in path.windows(2) {
            if window[0] == self.device_id {
                if let Some((neighbor_id, _)) = neighbors
                    .iter()
                    .find(|(_, neighbor)| neighbor.device_id == Some(window[1]))
                {
                    return Ok(*neighbor_id);
                }
            }
        }
        self.routes
            .lock()
            .await
            .get(path.last().unwrap_or(&self.device_id))
            .map(|route| route.next_hop)
            .ok_or(NetworkError::ChannelClosed)
    }

    async fn store_route(
        &self,
        target_device_id: DeviceId,
        next_hop: NeighborId,
        path: Vec<DeviceId>,
    ) {
        if target_device_id == self.device_id {
            return;
        }
        self.routes.lock().await.insert(
            target_device_id,
            RouteEntry {
                next_hop,
                path,
                updated_at: Instant::now(),
            },
        );
    }

    async fn remove_routes_for_device(&self, device_id: DeviceId) {
        self.routes
            .lock()
            .await
            .retain(|target, route| *target != device_id && !route.path.contains(&device_id));
    }

    async fn apply_member_change(&self, device_id: DeviceId, change: MemberChange) {
        self.members.lock().await.insert(
            device_id,
            MemberEntry {
                online: change == MemberChange::Online,
                last_seen: Instant::now(),
            },
        );
        if change == MemberChange::Offline {
            self.remove_routes_for_device(device_id).await;
        }
    }

    async fn note_neighbor_device(&self, neighbor_id: NeighborId, device_id: DeviceId) {
        if let Some(neighbor) = self.neighbors.lock().await.get_mut(&neighbor_id) {
            neighbor.device_id = Some(device_id);
        }
        self.apply_member_change(device_id, MemberChange::Online)
            .await;
    }

    async fn remove_neighbor(self: &Arc<Self>, neighbor_id: NeighborId) {
        let removed = self.neighbors.lock().await.remove(&neighbor_id);
        if let Some(neighbor) = removed {
            neighbor.read_handle.abort();
            neighbor.write_handle.abort();
            neighbor.heartbeat_handle.abort();
            let _ = self.events.send(SessionEvent::NeighborOffline {
                neighbor_id,
                peer_addr: neighbor.peer_addr,
            });
            if let Some(device_id) = neighbor.device_id {
                self.apply_member_change(device_id, MemberChange::Offline)
                    .await;
                self.remove_routes_for_device(device_id).await;
                let _ = self
                    .broadcast_message(member_changed_message(
                        self,
                        device_id,
                        MemberChange::Offline,
                    ))
                    .await;
            }
            self.routes
                .lock()
                .await
                .retain(|_, route| route.next_hop != neighbor_id);
            self.reverse_routes
                .lock()
                .await
                .retain(|_, route| route.neighbor_id != neighbor_id);
        }
    }
}

async fn accept_loop(session: Arc<SessionInner>, listener: TcpListener) {
    let mut shutdown = session.shutdown.subscribe();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else {
                    return;
                };
                let _ = session.register_neighbor(stream).await;
            }
            _ = shutdown.recv() => return,
        }
    }
}

async fn connect_stream(
    peer_addr: SocketAddr,
    local_ip: Option<IpAddr>,
) -> Result<TcpStream, io::Error> {
    let Some(local_ip) = local_ip else {
        return TcpStream::connect(peer_addr).await;
    };

    let socket = match local_ip {
        IpAddr::V4(_) => TcpSocket::new_v4()?,
        IpAddr::V6(_) => TcpSocket::new_v6()?,
    };
    socket.bind(SocketAddr::new(local_ip, 0))?;
    socket.connect(peer_addr).await
}

async fn read_loop(
    session: Arc<SessionInner>,
    neighbor_id: NeighborId,
    mut reader: OwnedReadHalf,
    last_active: Arc<Mutex<Instant>>,
) {
    loop {
        match read_message_frame(&mut reader).await {
            Ok(message) => {
                *last_active.lock().await = Instant::now();
                let header = message_header(&message);
                if header.group_id != session.group_id {
                    continue;
                }
                if !session.mark_seen(header.message_id).await {
                    continue;
                }
                let _ = session.handle_received_message(neighbor_id, &message).await;
                let _ = session.events.send(SessionEvent::MessageReceived {
                    neighbor_id,
                    message,
                });
            }
            Err(_) => {
                session.remove_neighbor(neighbor_id).await;
                return;
            }
        }
    }
}

async fn write_loop(mut writer: OwnedWriteHalf, mut receiver: mpsc::UnboundedReceiver<Message>) {
    while let Some(message) = receiver.recv().await {
        if write_message_frame(&mut writer, &message).await.is_err() {
            return;
        }
    }
}

async fn heartbeat_loop(session: Arc<SessionInner>, neighbor_id: NeighborId) {
    let mut interval = time::interval(session.config.heartbeat_interval);
    let mut shutdown = session.shutdown.subscribe();
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = shutdown.recv() => return,
        }
        if session
            .send_message(neighbor_id, heartbeat_message(&session))
            .await
            .is_err()
        {
            session.remove_neighbor(neighbor_id).await;
            return;
        }
    }
}

async fn timeout_loop(session: Arc<SessionInner>) {
    let mut interval = time::interval(session.config.timeout_check_interval);
    let mut shutdown = session.shutdown.subscribe();
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = shutdown.recv() => return,
        }
        let mut timed_out = Vec::new();
        {
            let neighbors = session.neighbors.lock().await;
            for (neighbor_id, neighbor) in neighbors.iter() {
                if neighbor.last_active.lock().await.elapsed() > session.config.heartbeat_timeout {
                    timed_out.push(*neighbor_id);
                }
            }
        }
        for neighbor_id in timed_out {
            session.remove_neighbor(neighbor_id).await;
        }
    }
}

async fn dedup_cleanup_loop(session: Arc<SessionInner>) {
    let mut interval = time::interval(MESSAGE_ID_CLEANUP_INTERVAL);
    let mut shutdown = session.shutdown.subscribe();
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = shutdown.recv() => return,
        }
        session.cleanup_seen_messages().await;
    }
}

async fn leaf_reconnect_loop(session: Arc<SessionInner>) {
    let mut delay = session.config.reconnect_initial_delay;
    let mut shutdown = session.shutdown.subscribe();
    loop {
        tokio::select! {
            _ = time::sleep(delay) => {}
            _ = shutdown.recv() => return,
        }
        if !session.neighbors.lock().await.is_empty() {
            delay = session.config.reconnect_initial_delay;
            continue;
        }

        let target = session.leaf_reconnect_target.lock().await.clone();
        let Some(target) = target else {
            delay = session.config.reconnect_initial_delay;
            continue;
        };

        if let Ok(stream) = connect_stream(target.peer_addr, target.local_ip).await {
            let _ = Box::pin(session.register_neighbor(stream)).await;
            delay = session.config.reconnect_initial_delay;
            continue;
        }

        delay = (delay * 2).min(session.config.reconnect_max_delay);
    }
}

fn message_header(message: &Message) -> &MessageHeader {
    match message {
        Message::Text { header, .. }
        | Message::FileChunk { header, .. }
        | Message::Heartbeat { header, .. }
        | Message::MemberChanged { header, .. }
        | Message::RouteDiscoveryRequest { header, .. }
        | Message::RouteDiscoveryResponse { header, .. } => header,
    }
}

fn message_header_mut(message: &mut Message) -> &mut MessageHeader {
    match message {
        Message::Text { header, .. }
        | Message::FileChunk { header, .. }
        | Message::Heartbeat { header, .. }
        | Message::MemberChanged { header, .. }
        | Message::RouteDiscoveryRequest { header, .. }
        | Message::RouteDiscoveryResponse { header, .. } => header,
    }
}

fn next_hop_message(message: &Message) -> Option<Message> {
    let header = message_header(message);
    if header.hop_count >= header.ttl {
        return None;
    }
    let mut message = message.clone();
    message_header_mut(&mut message).hop_count += 1;
    Some(message)
}

fn append_device(mut path: Vec<DeviceId>, device_id: DeviceId) -> Vec<DeviceId> {
    if path.last() != Some(&device_id) {
        path.push(device_id);
    }
    path
}

fn heartbeat_message(session: &SessionInner) -> Message {
    let timestamp_ms = now_timestamp_ms();
    Message::Heartbeat {
        header: MessageHeader {
            message_id: MessageId::new(),
            group_id: session.group_id,
            source_device_id: session.device_id,
            target: MessageTarget::Broadcast,
            ttl: 1,
            hop_count: 0,
            timestamp_ms,
        },
        payload: HeartbeatPayload {
            device_id: session.device_id,
            timestamp_ms,
        },
    }
}

fn member_changed_message(
    session: &SessionInner,
    device_id: DeviceId,
    change: MemberChange,
) -> Message {
    Message::MemberChanged {
        header: MessageHeader {
            message_id: MessageId::new(),
            group_id: session.group_id,
            source_device_id: session.device_id,
            target: MessageTarget::Broadcast,
            ttl: 8,
            hop_count: 0,
            timestamp_ms: now_timestamp_ms(),
        },
        payload: MemberChangedPayload { device_id, change },
    }
}

async fn collect_relay_announcements(
    socket: UdpSocket,
    duration: Duration,
) -> Vec<RelayAnnouncement> {
    let deadline = time::sleep(duration);
    tokio::pin!(deadline);
    let mut buf = [0; 2048];
    let mut relays = HashMap::new();

    loop {
        tokio::select! {
            _ = &mut deadline => return relays.into_values().collect(),
            received = socket.recv_from(&mut buf) => {
                let Ok((len, _)) = received else {
                    continue;
                };
                if let Ok(announcement) = serde_json::from_slice::<RelayAnnouncement>(&buf[..len]) {
                    relays.insert(announcement.device_id, announcement);
                }
            }
        }
    }
}

pub fn now_timestamp_ms() -> TimestampMs {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as TimestampMs
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::Json(err) => write!(f, "JSON error: {err}"),
            Self::FrameTooLarge { len, max } => {
                write!(f, "frame is too large: {len} bytes exceeds {max}")
            }
        }
    }
}

impl std::error::Error for FrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Json(err) => Some(err),
            Self::FrameTooLarge { .. } => None,
        }
    }
}

impl From<io::Error> for FrameError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<serde_json::Error> for FrameError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

pub fn message_to_json(message: &Message) -> serde_json::Result<String> {
    serde_json::to_string(message)
}

pub fn message_from_json(json: &str) -> serde_json::Result<Message> {
    serde_json::from_str(json)
}

pub fn encode_file_chunk_data(data: &[u8]) -> String {
    BASE64.encode(data)
}

pub fn decode_file_chunk_data(data: &str) -> Result<Vec<u8>, base64::DecodeError> {
    BASE64.decode(data)
}

pub async fn write_message_frame<W>(writer: &mut W, message: &Message) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
{
    let body = message_to_json(message)?.into_bytes();
    if body.len() > MAX_FRAME_LEN {
        return Err(FrameError::FrameTooLarge {
            len: body.len(),
            max: MAX_FRAME_LEN,
        });
    }

    let len = u32::try_from(body.len()).map_err(|_| FrameError::FrameTooLarge {
        len: body.len(),
        max: u32::MAX as usize,
    })?;

    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&body).await?;
    Ok(())
}

pub async fn read_message_frame<R>(reader: &mut R) -> Result<Message, FrameError>
where
    R: AsyncRead + Unpin,
{
    let mut prefix = [0; 4];
    reader.read_exact(&mut prefix).await?;
    let len = u32::from_be_bytes(prefix) as usize;
    if len > MAX_FRAME_LEN {
        return Err(FrameError::FrameTooLarge {
            len,
            max: MAX_FRAME_LEN,
        });
    }

    let mut body = vec![0; len];
    reader.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}

mod base64_data {
    use super::{decode_file_chunk_data, encode_file_chunk_data};
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    pub fn serialize<S>(data: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&encode_file_chunk_data(data))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        decode_file_chunk_data(&encoded).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::AsyncReadExt;
    use tokio::time::timeout;

    fn header() -> MessageHeader {
        MessageHeader {
            message_id: MessageId::new(),
            group_id: GroupId::new(),
            source_device_id: DeviceId::new(),
            target: MessageTarget::Broadcast,
            ttl: 8,
            hop_count: 0,
            timestamp_ms: 1_700_000_000_000,
        }
    }

    fn fast_config() -> ConnectionConfig {
        ConnectionConfig {
            heartbeat_interval: Duration::from_millis(10),
            heartbeat_timeout: Duration::from_millis(60),
            timeout_check_interval: Duration::from_millis(5),
            reconnect_initial_delay: Duration::from_millis(10),
            reconnect_max_delay: Duration::from_millis(20),
        }
    }

    async fn recv_matching(
        events: &mut broadcast::Receiver<SessionEvent>,
        mut matches: impl FnMut(&SessionEvent) -> bool,
    ) -> SessionEvent {
        timeout(Duration::from_secs(2), async {
            loop {
                let event = events.recv().await.unwrap();
                if matches(&event) {
                    return event;
                }
            }
        })
        .await
        .unwrap()
    }

    async fn wait_until(mut done: impl AsyncFnMut() -> bool) {
        timeout(Duration::from_secs(2), async {
            loop {
                if done().await {
                    return;
                }
                time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }

    fn text_message(
        message_id: MessageId,
        group_id: GroupId,
        source_device_id: DeviceId,
        target: MessageTarget,
        content: &str,
    ) -> Message {
        Message::Text {
            header: MessageHeader {
                message_id,
                group_id,
                source_device_id,
                target,
                ttl: 8,
                hop_count: 0,
                timestamp_ms: now_timestamp_ms(),
            },
            payload: TextPayload {
                content: content.to_string(),
            },
        }
    }

    #[test]
    fn text_message_uses_internal_type_tag() {
        let message = Message::Text {
            header: header(),
            payload: TextPayload {
                content: "hello".to_string(),
            },
        };

        let value = serde_json::to_value(&message).unwrap();

        assert_eq!(value["type"], json!("text"));
        assert_eq!(serde_json::from_value::<Message>(value).unwrap(), message);
    }

    #[test]
    fn target_can_address_one_device() {
        let device_id = DeviceId::new();
        let target = MessageTarget::Device { device_id };

        let value = serde_json::to_value(&target).unwrap();

        assert_eq!(value["kind"], json!("device"));
        assert_eq!(
            serde_json::from_value::<MessageTarget>(value).unwrap(),
            target
        );
    }

    #[test]
    fn file_chunk_data_uses_base64_in_json() {
        let message = Message::FileChunk {
            header: header(),
            payload: FileChunkPayload {
                file_id: FileId::new(),
                chunk_index: 0,
                chunk_count: 1,
                data: vec![0, 1, 2, 3, 255],
            },
        };

        let json = message_to_json(&message).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(value["payload"]["data"], json!("AAECA/8="));
        assert_eq!(message_from_json(&json).unwrap(), message);
        assert!(message_from_json(r#"{"type":"text"}"#).is_err());
    }

    #[test]
    fn file_chunk_encoding_helpers_round_trip() {
        let data = [0, 1, 2, 3, 255];

        let encoded = encode_file_chunk_data(&data);

        assert_eq!(encoded, "AAECA/8=");
        assert_eq!(decode_file_chunk_data(&encoded).unwrap(), data);
    }

    #[tokio::test]
    async fn message_frame_uses_big_endian_length_prefix() {
        let message = Message::Text {
            header: header(),
            payload: TextPayload {
                content: "hello".to_string(),
            },
        };
        let mut expected_body = message_to_json(&message).unwrap().into_bytes();
        let (mut writer, mut reader) = tokio::io::duplex(2048);

        write_message_frame(&mut writer, &message).await.unwrap();

        let mut prefix = [0; 4];
        reader.read_exact(&mut prefix).await.unwrap();
        assert_eq!(prefix, (expected_body.len() as u32).to_be_bytes());

        let mut body = vec![0; expected_body.len()];
        reader.read_exact(&mut body).await.unwrap();
        assert_eq!(body, expected_body);

        expected_body[0] = b'!';
        assert_ne!(body, expected_body);
    }

    #[tokio::test]
    async fn message_frame_round_trips_and_rejects_large_prefix() {
        let message = Message::Text {
            header: header(),
            payload: TextPayload {
                content: "hello".to_string(),
            },
        };
        let (mut writer, mut reader) = tokio::io::duplex(2048);

        write_message_frame(&mut writer, &message).await.unwrap();

        assert_eq!(read_message_frame(&mut reader).await.unwrap(), message);

        let len = ((MAX_FRAME_LEN + 1) as u32).to_be_bytes();
        let mut oversized = &len[..];
        assert!(matches!(
            read_message_frame(&mut oversized).await.unwrap_err(),
            FrameError::FrameTooLarge { .. }
        ));
    }

    #[tokio::test]
    async fn relay_listens_and_connector_registers_neighbors() {
        let group_id = GroupId::new();
        let relay =
            Session::with_config(DeviceId::new(), group_id, DeviceRole::Relay, fast_config());
        let leaf = Session::with_config(DeviceId::new(), group_id, DeviceRole::Leaf, fast_config());
        let mut relay_events = relay.subscribe();
        let bind_addr = "127.0.0.1:0".parse().unwrap();
        let local_ip = "127.0.0.1".parse().unwrap();

        let addr = relay.listen(bind_addr).await.unwrap();
        let leaf_neighbor_id = leaf.connect(addr, Some(local_ip)).await.unwrap();

        recv_matching(&mut relay_events, |event| {
            matches!(event, SessionEvent::NeighborOnline { .. })
        })
        .await;

        assert_eq!(leaf.neighbors().await[0].neighbor_id, leaf_neighbor_id);
        assert_eq!(relay.neighbors().await.len(), 1);
    }

    #[tokio::test]
    async fn heartbeat_timeout_removes_silent_neighbor() {
        let relay = Session::with_config(
            DeviceId::new(),
            GroupId::new(),
            DeviceRole::Relay,
            fast_config(),
        );
        let mut events = relay.subscribe();
        let addr = relay.listen("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let _silent_client = TcpStream::connect(addr).await.unwrap();

        let online = recv_matching(&mut events, |event| {
            matches!(event, SessionEvent::NeighborOnline { .. })
        })
        .await;
        let SessionEvent::NeighborOnline { neighbor_id, .. } = online else {
            unreachable!()
        };

        let offline = recv_matching(&mut events, |event| {
            matches!(
                event,
                SessionEvent::NeighborOffline {
                    neighbor_id: id,
                    ..
                } if *id == neighbor_id
            )
        })
        .await;

        assert!(matches!(offline, SessionEvent::NeighborOffline { .. }));
        assert!(relay.neighbors().await.is_empty());
    }

    #[tokio::test]
    async fn leaf_reconnects_after_disconnect() {
        let mut config = fast_config();
        config.heartbeat_interval = Duration::from_secs(1);
        config.heartbeat_timeout = Duration::from_secs(5);
        let leaf = Session::with_config(DeviceId::new(), GroupId::new(), DeviceRole::Leaf, config);
        let mut events = leaf.subscribe();
        let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = server.local_addr().unwrap();

        leaf.connect(addr, None).await.unwrap();
        let (first, _) = server.accept().await.unwrap();
        recv_matching(&mut events, |event| {
            matches!(event, SessionEvent::NeighborOnline { .. })
        })
        .await;

        drop(first);
        recv_matching(&mut events, |event| {
            matches!(event, SessionEvent::NeighborOffline { .. })
        })
        .await;

        let (_second, _) = timeout(Duration::from_secs(2), server.accept())
            .await
            .unwrap()
            .unwrap();
        recv_matching(&mut events, |event| {
            matches!(event, SessionEvent::NeighborOnline { .. })
        })
        .await;

        assert_eq!(leaf.neighbors().await.len(), 1);
    }

    #[tokio::test]
    async fn group_sessions_keep_members_and_messages_isolated() {
        let device_id = DeviceId::new();
        let group1 = GroupId::new();
        let group2 = GroupId::new();
        let relay1 = Session::with_config(device_id, group1, DeviceRole::Relay, fast_config());
        let relay2 =
            Session::with_config(DeviceId::new(), group2, DeviceRole::Relay, fast_config());
        let mut relay2_events = relay2.subscribe();
        let relay1_addr = relay1.listen("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let relay2_addr = relay2.listen("127.0.0.1:0".parse().unwrap()).await.unwrap();

        let leaf2 = Session::with_config(device_id, group2, DeviceRole::Leaf, fast_config());
        leaf2.connect(relay2_addr, None).await.unwrap();
        recv_matching(&mut relay2_events, |event| {
            matches!(
                event,
                SessionEvent::MessageReceived {
                    message: Message::MemberChanged { payload, .. },
                    ..
                } if payload.device_id == device_id && payload.change == MemberChange::Online
            )
        })
        .await;

        let wrong_group_leaf =
            Session::with_config(DeviceId::new(), group2, DeviceRole::Leaf, fast_config());
        let neighbor_id = wrong_group_leaf.connect(relay1_addr, None).await.unwrap();
        wrong_group_leaf
            .send_message(
                neighbor_id,
                wrong_group_leaf.member_changed_message(device_id, MemberChange::Offline),
            )
            .await
            .unwrap();
        time::sleep(Duration::from_millis(30)).await;

        assert!(
            relay1
                .members()
                .await
                .iter()
                .all(|m| m.device_id == device_id)
        );
        assert!(
            relay2
                .members()
                .await
                .iter()
                .any(|m| m.device_id == device_id && m.online)
        );

        leaf2.destroy().await;
        recv_matching(&mut relay2_events, |event| {
            matches!(event, SessionEvent::NeighborOffline { .. })
        })
        .await;
        assert!(
            relay2
                .members()
                .await
                .iter()
                .any(|m| m.device_id == device_id && !m.online)
        );
    }

    #[tokio::test]
    async fn dedup_cache_expires_seen_messages() {
        let session = Session::with_config(
            DeviceId::new(),
            GroupId::new(),
            DeviceRole::Relay,
            fast_config(),
        );
        let message_id = MessageId::new();

        assert!(session.inner.mark_seen(message_id).await);
        assert!(!session.inner.mark_seen(message_id).await);
        session.inner.seen_messages.lock().await.insert(
            message_id,
            Instant::now() - MESSAGE_ID_TTL - Duration::from_secs(1),
        );

        assert!(session.inner.mark_seen(message_id).await);
    }

    #[tokio::test]
    async fn broadcast_floods_once_and_skips_source_neighbor() {
        let group_id = GroupId::new();
        let leaf1_device = DeviceId::new();
        let leaf2_device = DeviceId::new();
        let relay =
            Session::with_config(DeviceId::new(), group_id, DeviceRole::Relay, fast_config());
        let leaf1 = Session::with_config(leaf1_device, group_id, DeviceRole::Leaf, fast_config());
        let leaf2 = Session::with_config(leaf2_device, group_id, DeviceRole::Leaf, fast_config());
        let relay_addr = relay.listen("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let leaf1_neighbor = leaf1.connect(relay_addr, None).await.unwrap();
        leaf2.connect(relay_addr, None).await.unwrap();
        wait_until(async || relay.neighbors().await.len() == 2).await;

        let message_id = MessageId::new();
        let message = text_message(
            message_id,
            group_id,
            leaf1_device,
            MessageTarget::Broadcast,
            "broadcast",
        );
        let mut leaf1_events = leaf1.subscribe();
        let mut leaf2_events = leaf2.subscribe();

        leaf1
            .send_message(leaf1_neighbor, message.clone())
            .await
            .unwrap();
        recv_matching(&mut leaf2_events, |event| {
            matches!(
                event,
                SessionEvent::MessageReceived {
                    message: Message::Text { header, payload },
                    ..
                } if header.message_id == message_id && payload.content == "broadcast"
            )
        })
        .await;
        leaf1.send_message(leaf1_neighbor, message).await.unwrap();

        assert!(
            timeout(Duration::from_millis(80), async {
                loop {
                    let event = leaf2_events.recv().await.unwrap();
                    if matches!(
                        event,
                        SessionEvent::MessageReceived {
                            message: Message::Text { header, .. },
                            ..
                        } if header.message_id == message_id
                    ) {
                        return;
                    }
                }
            })
            .await
            .is_err()
        );
        assert!(
            timeout(Duration::from_millis(80), async {
                loop {
                    let event = leaf1_events.recv().await.unwrap();
                    if matches!(
                        event,
                        SessionEvent::MessageReceived {
                            message: Message::Text { header, .. },
                            ..
                        } if header.message_id == message_id
                    ) {
                        return;
                    }
                }
            })
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn route_discovery_enables_direct_forward_and_offline_clears_route() {
        let group_id = GroupId::new();
        let leaf_a_device = DeviceId::new();
        let relay1_device = DeviceId::new();
        let relay2_device = DeviceId::new();
        let leaf_c_device = DeviceId::new();
        let relay1 =
            Session::with_config(relay1_device, group_id, DeviceRole::Relay, fast_config());
        let relay2 =
            Session::with_config(relay2_device, group_id, DeviceRole::Relay, fast_config());
        let leaf_a = Session::with_config(leaf_a_device, group_id, DeviceRole::Leaf, fast_config());
        let leaf_c = Session::with_config(leaf_c_device, group_id, DeviceRole::Leaf, fast_config());
        let relay1_addr = relay1.listen("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let relay2_addr = relay2.listen("127.0.0.1:0".parse().unwrap()).await.unwrap();
        leaf_a.connect(relay1_addr, None).await.unwrap();
        relay1.connect(relay2_addr, None).await.unwrap();
        leaf_c.connect(relay2_addr, None).await.unwrap();
        wait_until(async || relay1.neighbors().await.len() == 2).await;
        wait_until(async || relay2.neighbors().await.len() == 2).await;

        leaf_a.discover_route(leaf_c_device, 8).await.unwrap();
        wait_until(async || {
            leaf_a
                .routes()
                .await
                .iter()
                .any(|route| route.target_device_id == leaf_c_device)
        })
        .await;

        let message_id = MessageId::new();
        let message = text_message(
            message_id,
            group_id,
            leaf_a_device,
            MessageTarget::Device {
                device_id: leaf_c_device,
            },
            "direct",
        );
        let mut leaf_c_events = leaf_c.subscribe();
        leaf_a.route_message(message).await.unwrap();

        recv_matching(&mut leaf_c_events, |event| {
            matches!(
                event,
                SessionEvent::MessageReceived {
                    message: Message::Text { header, payload },
                    ..
                } if header.message_id == message_id && payload.content == "direct"
            )
        })
        .await;

        relay1
            .announce_member_change(leaf_c_device, MemberChange::Offline)
            .await
            .unwrap();
        wait_until(async || {
            !leaf_a
                .routes()
                .await
                .iter()
                .any(|route| route.target_device_id == leaf_c_device)
        })
        .await;
    }

    #[tokio::test]
    async fn relay_announcement_is_collected_over_udp() {
        let group_id = GroupId::new();
        let relay_device = DeviceId::new();
        let relay = Session::with_config(relay_device, group_id, DeviceRole::Relay, fast_config());
        let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let announce_addr = listener.local_addr().unwrap();
        let collecting = tokio::spawn(collect_relay_announcements(
            listener,
            Duration::from_millis(80),
        ));
        let tcp_addr = "127.0.0.1:12345".parse().unwrap();

        relay
            .start_relay_announcement(
                "127.0.0.1:0".parse().unwrap(),
                announce_addr,
                tcp_addr,
                "group",
                Duration::from_millis(10),
            )
            .await
            .unwrap();

        let relays = collecting.await.unwrap();
        assert!(relays.iter().any(|relay| {
            relay.device_id == relay_device
                && relay.group_id == group_id
                && relay.group_name == "group"
                && relay.tcp_addr == tcp_addr
        }));
    }

    #[tokio::test]
    async fn session_lifecycle_apis_create_join_destroy() {
        let group_id = GroupId::new();
        let (relay, relay_addr) =
            Session::create_group(DeviceId::new(), group_id, "127.0.0.1:0".parse().unwrap())
                .await
                .unwrap();
        let (leaf, _) = Session::join_group(DeviceId::new(), group_id, relay_addr, None)
            .await
            .unwrap();

        recv_matching(&mut relay.subscribe(), |event| {
            matches!(event, SessionEvent::NeighborOnline { .. })
        })
        .await;

        assert_eq!(relay.role(), DeviceRole::Relay);
        assert_eq!(leaf.role(), DeviceRole::Leaf);

        leaf.destroy().await;
        assert!(leaf.neighbors().await.is_empty());
        assert!(leaf.members().await.is_empty());
    }
}

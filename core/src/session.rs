mod leaf;
mod transport;
use transport::*;
#[cfg(all(feature = "relay", not(target_os = "android")))]
mod relay;
use crate::{
    DeviceId, DeviceRole, FrameError, GroupId, HeartbeatPayload, MemberChange,
    MemberChangedPayload, Message, MessageHeader, MessageId, MessageTarget, NeighborId,
    TextPayload, TimestampMs, now_timestamp_ms, read_message_frame, write_message_frame,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fmt, io,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::{TcpSocket, TcpStream, UdpSocket, tcp::OwnedReadHalf, tcp::OwnedWriteHalf},
    sync::{Mutex, broadcast, mpsc},
    task::JoinHandle,
    time::{self, Instant},
};

const MESSAGE_ID_TTL: Duration = Duration::from_secs(300);
const MESSAGE_ID_CLEANUP_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_ROUTE_TTL: u8 = 8;

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
    pub nickname: Option<String>,
    pub last_seen_elapsed: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(all(feature = "relay", not(target_os = "android")))]
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
    #[cfg(all(feature = "relay", not(target_os = "android")))]
    routes: Mutex<HashMap<DeviceId, RouteEntry>>,
    #[cfg(all(feature = "relay", not(target_os = "android")))]
    reverse_routes: Mutex<HashMap<MessageId, ReverseRouteEntry>>,
    members: Mutex<HashMap<DeviceId, MemberEntry>>,
    events: broadcast::Sender<SessionEvent>,
    inbox: Option<mpsc::Sender<SessionEvent>>,
    shutdown: tokio::sync::watch::Sender<bool>,
    leaf_reconnect_target: Mutex<Option<ReconnectTarget>>,
    reconnect_now: tokio::sync::Notify,
}

struct Outbound {
    message: Message,
    written: Option<tokio::sync::oneshot::Sender<Result<(), NetworkError>>>,
}

struct NeighborConnection {
    peer_addr: SocketAddr,
    device_id: Option<DeviceId>,
    sender: mpsc::UnboundedSender<Outbound>,
    last_active: Arc<Mutex<Instant>>,
    read_handle: JoinHandle<()>,
    write_handle: JoinHandle<()>,
    heartbeat_handle: JoinHandle<()>,
}

#[derive(Clone, Debug)]
struct ReconnectTarget {
    verify_group: bool,
    peer_addr: SocketAddr,
    local_ip: Option<IpAddr>,
}

struct MemberEntry {
    online: bool,
    nickname: Option<String>,
    last_seen: Instant,
}

#[cfg(all(feature = "relay", not(target_os = "android")))]
struct RouteEntry {
    next_hop: NeighborId,
    path: Vec<DeviceId>,
    updated_at: Instant,
}

#[cfg(all(feature = "relay", not(target_os = "android")))]
struct ReverseRouteEntry {
    neighbor_id: NeighborId,
    created_at: Instant,
}

impl Session {
    pub fn leaf(device_id: DeviceId, group_id: GroupId) -> Self {
        Self::build(
            device_id,
            group_id,
            DeviceRole::Leaf,
            ConnectionConfig::default(),
            None,
        )
    }

    #[cfg(all(feature = "relay", not(target_os = "android")))]
    pub fn new(device_id: DeviceId, group_id: GroupId, role: DeviceRole) -> Self {
        Self::with_config(device_id, group_id, role, ConnectionConfig::default())
    }

    #[cfg(all(feature = "relay", not(target_os = "android")))]
    pub fn with_config(
        device_id: DeviceId,
        group_id: GroupId,
        role: DeviceRole,
        config: ConnectionConfig,
    ) -> Self {
        Self::build(device_id, group_id, role, config, None)
    }

    fn build(
        device_id: DeviceId,
        group_id: GroupId,
        role: DeviceRole,
        config: ConnectionConfig,
        inbox: Option<mpsc::Sender<SessionEvent>>,
    ) -> Self {
        let (events, _) = broadcast::channel(256);
        let (shutdown, _) = tokio::sync::watch::channel(false);
        let mut members = HashMap::new();
        members.insert(
            device_id,
            MemberEntry {
                online: true,
                nickname: None,
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
                #[cfg(all(feature = "relay", not(target_os = "android")))]
                routes: Mutex::new(HashMap::new()),
                #[cfg(all(feature = "relay", not(target_os = "android")))]
                reverse_routes: Mutex::new(HashMap::new()),
                members: Mutex::new(members),
                events,
                inbox,
                shutdown,
                leaf_reconnect_target: Mutex::new(None),
                reconnect_now: tokio::sync::Notify::new(),
            }),
        };
        tokio::spawn(timeout_loop(session.inner.clone()));
        tokio::spawn(dedup_cleanup_loop(session.inner.clone()));
        if role == DeviceRole::Leaf {
            tokio::spawn(leaf_reconnect_loop(session.inner.clone()));
        }
        session
    }

    pub async fn join_group(
        device_id: DeviceId,
        group_id: GroupId,
        relay_addr: SocketAddr,
        local_ip: Option<IpAddr>,
    ) -> Result<(Self, NeighborId), NetworkError> {
        let session = Self::leaf(device_id, group_id);
        match session.connect(relay_addr, local_ip).await {
            Ok(neighbor_id) => Ok((session, neighbor_id)),
            Err(err) => {
                // A failed restoration/retry must not leave the session's
                // maintenance tasks alive after its handle is dropped.
                session.destroy().await;
                Err(err)
            }
        }
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
                verify_group: false,
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

    pub async fn send_group_message(
        &self,
        content: impl Into<String>,
    ) -> Result<MessageId, NetworkError> {
        self.send_group_message_with_nickname(content, None).await
    }

    pub async fn send_group_message_with_nickname(
        &self,
        content: impl Into<String>,
        sender_nickname: Option<String>,
    ) -> Result<MessageId, NetworkError> {
        let message_id = MessageId::new();
        self.inner
            .inject_message(text_message(
                &self.inner,
                message_id,
                MessageTarget::Broadcast,
                content.into(),
                sender_nickname,
            ))
            .await?;
        Ok(message_id)
    }

    pub async fn send_direct_message(
        &self,
        target_device_id: DeviceId,
        content: impl Into<String>,
    ) -> Result<MessageId, NetworkError> {
        self.send_direct_message_with_nickname(target_device_id, content, None)
            .await
    }

    pub async fn send_direct_message_with_nickname(
        &self,
        target_device_id: DeviceId,
        content: impl Into<String>,
        sender_nickname: Option<String>,
    ) -> Result<MessageId, NetworkError> {
        let message_id = MessageId::new();
        self.inner
            .inject_message(text_message(
                &self.inner,
                message_id,
                MessageTarget::Device {
                    device_id: target_device_id,
                },
                content.into(),
                sender_nickname,
            ))
            .await?;
        Ok(message_id)
    }

    pub fn member_changed_message(&self, device_id: DeviceId, change: MemberChange) -> Message {
        member_changed_message(&self.inner, device_id, change, None)
    }

    pub async fn announce_member_change(
        &self,
        device_id: DeviceId,
        change: MemberChange,
    ) -> Result<(), NetworkError> {
        self.inner
            .apply_member_change(device_id, change, None)
            .await;
        self.inner
            .broadcast_message(member_changed_message(&self.inner, device_id, change, None))
            .await
    }

    pub async fn announce_nickname(&self, nickname: Option<String>) -> Result<(), NetworkError> {
        self.inner
            .apply_member_change(
                self.inner.device_id,
                MemberChange::Online,
                Some(nickname.clone()),
            )
            .await;
        self.inner
            .broadcast_message(member_changed_message(
                &self.inner,
                self.inner.device_id,
                MemberChange::Online,
                Some(nickname),
            ))
            .await
    }

    pub async fn members(&self) -> Vec<MemberSnapshot> {
        let members = self.inner.members.lock().await;
        members
            .iter()
            .map(|(device_id, member)| MemberSnapshot {
                device_id: *device_id,
                online: member.online,
                nickname: member.nickname.clone(),
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
        self.inner.shutdown.send_replace(true);
        self.inner.leaf_reconnect_target.lock().await.take();
        self.inner.seen_messages.lock().await.clear();
        #[cfg(all(feature = "relay", not(target_os = "android")))]
        self.inner.routes.lock().await.clear();
        #[cfg(all(feature = "relay", not(target_os = "android")))]
        self.inner.reverse_routes.lock().await.clear();
        let neighbor_ids: Vec<_> = self.inner.neighbors.lock().await.keys().copied().collect();
        for neighbor_id in neighbor_ids {
            self.inner.remove_neighbor(neighbor_id).await;
        }
        self.inner.members.lock().await.clear();
    }
}

impl SessionInner {
    async fn emit(&self, event: SessionEvent) {
        if let Some(inbox) = &self.inbox {
            let mut shutdown = self.shutdown.subscribe();
            tokio::select! {
                _ = inbox.send(event) => {}
                _ = shutdown.wait_for(|closed| *closed) => {}
            }
        } else {
            let _ = self.events.send(event);
        }
    }
    async fn register_neighbor(
        self: &Arc<Self>,
        stream: TcpStream,
    ) -> Result<NeighborId, NetworkError> {
        let mut neighbors = self.neighbors.lock().await;
        if *self.shutdown.borrow() {
            return Err(NetworkError::ChannelClosed);
        }
        if self.role == DeviceRole::Leaf && !neighbors.is_empty() {
            return Err(NetworkError::LeafAlreadyConnected);
        }
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

        neighbors.insert(
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
        drop(neighbors);
        self.emit(SessionEvent::NeighborOnline {
            neighbor_id,
            peer_addr,
        })
        .await;
        let nickname = self
            .members
            .lock()
            .await
            .get(&self.device_id)
            .and_then(|member| member.nickname.clone());
        let _ = self
            .broadcast_message(member_changed_message(
                self,
                self.device_id,
                MemberChange::Online,
                Some(nickname),
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
            .send(Outbound {
                message,
                written: None,
            })
            .map_err(|_| NetworkError::ChannelClosed)
    }

    async fn broadcast_message(&self, message: Message) -> Result<(), NetworkError> {
        #[cfg(all(feature = "relay", not(target_os = "android")))]
        {
            self.send_except(None, message).await
        }
        #[cfg(not(all(feature = "relay", not(target_os = "android"))))]
        {
            self.inject_message(message).await
        }
    }

    #[cfg(all(feature = "relay", not(target_os = "android")))]
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
                .send(Outbound {
                    message: message.clone(),
                    written: None,
                })
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
        #[cfg(all(feature = "relay", not(target_os = "android")))]
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
        #[cfg(all(feature = "relay", not(target_os = "android")))]
        {
            self.route_outbound_message(message).await
        }
        #[cfg(not(all(feature = "relay", not(target_os = "android"))))]
        {
            let id = self
                .neighbors
                .lock()
                .await
                .keys()
                .next()
                .copied()
                .ok_or(NetworkError::ChannelClosed)?;
            self.send_message(id, message).await
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
        #[cfg(all(feature = "relay", not(target_os = "android")))]
        if self.role == DeviceRole::Relay
            || header.hop_count == 0
            || !matches!(message, Message::MemberChanged { .. })
        {
            self.store_route(
                header.source_device_id,
                neighbor_id,
                vec![self.device_id, header.source_device_id],
            )
            .await;
        }
        match message {
            Message::MemberChanged { payload, .. } => {
                // A relay's initial snapshot may describe our previous connection.
                if payload.device_id == self.device_id {
                    return Ok(());
                }
                self.apply_member_change(
                    payload.device_id,
                    payload.change,
                    payload.nickname_updated.then(|| payload.nickname.clone()),
                )
                .await;
                #[cfg(all(feature = "relay", not(target_os = "android")))]
                if self.role == DeviceRole::Relay {
                    if payload.change == MemberChange::Online {
                        self.learn_advertised_member(
                            neighbor_id,
                            header.source_device_id,
                            payload.device_id,
                        )
                        .await;
                    }
                    if header.source_device_id == payload.device_id
                        || payload.change == MemberChange::Offline
                    {
                        self.flood_message(Some(neighbor_id), message).await?;
                    }
                }
            }
            #[cfg(all(feature = "relay", not(target_os = "android")))]
            Message::RouteDiscoveryRequest { header, payload }
                if payload.target_device_id == self.device_id =>
            {
                self.answer_route_request(neighbor_id, header, payload)
                    .await?;
            }
            #[cfg(all(feature = "relay", not(target_os = "android")))]
            Message::RouteDiscoveryRequest { header, payload } => {
                self.handle_route_discovery_request(neighbor_id, header, payload)
                    .await?;
            }
            #[cfg(all(feature = "relay", not(target_os = "android")))]
            Message::RouteDiscoveryResponse { header, payload } => {
                self.handle_route_discovery_response(header, payload)
                    .await?;
            }
            #[cfg(all(feature = "relay", not(target_os = "android")))]
            _ => match header.target {
                MessageTarget::Broadcast => self.flood_message(Some(neighbor_id), message).await?,
                MessageTarget::Device { device_id } if device_id == self.device_id => {}
                MessageTarget::Device { device_id } => {
                    let _ = self.forward_direct_message(device_id, message).await?;
                }
            },
            #[cfg(not(all(feature = "relay", not(target_os = "android"))))]
            _ => {}
        }
        Ok(())
    }

    async fn apply_member_change(
        &self,
        device_id: DeviceId,
        change: MemberChange,
        nickname_update: Option<Option<String>>,
    ) {
        let mut members = self.members.lock().await;
        let member = members.entry(device_id).or_insert_with(|| MemberEntry {
            online: false,
            nickname: None,
            last_seen: Instant::now(),
        });
        member.online = change == MemberChange::Online;
        member.last_seen = Instant::now();
        if let Some(nickname) = nickname_update {
            member.nickname = nickname;
        }
        drop(members);
        #[cfg(all(feature = "relay", not(target_os = "android")))]
        if change == MemberChange::Offline {
            self.remove_routes_for_device(device_id).await;
        }
    }

    async fn note_neighbor_device(&self, neighbor_id: NeighborId, device_id: DeviceId) {
        let _newly_identified = self
            .neighbors
            .lock()
            .await
            .get_mut(&neighbor_id)
            .is_some_and(|neighbor| {
                if neighbor.device_id == Some(device_id) {
                    false
                } else {
                    neighbor.device_id = Some(device_id);
                    true
                }
            });
        self.apply_member_change(device_id, MemberChange::Online, None)
            .await;
        #[cfg(all(feature = "relay", not(target_os = "android")))]
        if _newly_identified && self.role == DeviceRole::Relay {
            self.sync_members_to_neighbor(neighbor_id).await;
        }
    }

    async fn remove_neighbor(self: &Arc<Self>, neighbor_id: NeighborId) {
        let removed = self.neighbors.lock().await.remove(&neighbor_id);
        if let Some(neighbor) = removed {
            neighbor.write_handle.abort();
            if self.role == DeviceRole::Leaf {
                for (id, member) in self.members.lock().await.iter_mut() {
                    if *id != self.device_id {
                        member.online = false;
                    }
                }
            }
            self.emit(SessionEvent::NeighborOffline {
                neighbor_id,
                peer_addr: neighbor.peer_addr,
            })
            .await;
            if let Some(device_id) = neighbor.device_id {
                self.apply_member_change(device_id, MemberChange::Offline, None)
                    .await;
                let _ = self
                    .broadcast_message(member_changed_message(
                        self,
                        device_id,
                        MemberChange::Offline,
                        None,
                    ))
                    .await;
            }
            #[cfg(all(feature = "relay", not(target_os = "android")))]
            self.routes
                .lock()
                .await
                .retain(|_, route| route.next_hop != neighbor_id);
            #[cfg(all(feature = "relay", not(target_os = "android")))]
            self.reverse_routes
                .lock()
                .await
                .retain(|_, route| route.neighbor_id != neighbor_id);
            neighbor.read_handle.abort();
            neighbor.heartbeat_handle.abort();
        }
    }
}

fn message_header(message: &Message) -> &MessageHeader {
    match message {
        Message::Text { header, .. }
        | Message::FileChunk { header, .. }
        | Message::FileResumeRequest { header, .. }
        | Message::UpdatePackage { header, .. }
        | Message::Heartbeat { header, .. }
        | Message::MemberChanged { header, .. }
        | Message::RouteDiscoveryRequest { header, .. }
        | Message::RouteDiscoveryResponse { header, .. } => header,
    }
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

fn text_message(
    session: &SessionInner,
    message_id: MessageId,
    target: MessageTarget,
    content: String,
    sender_nickname: Option<String>,
) -> Message {
    Message::Text {
        header: MessageHeader {
            message_id,
            group_id: session.group_id,
            source_device_id: session.device_id,
            target,
            ttl: DEFAULT_ROUTE_TTL,
            hop_count: 0,
            timestamp_ms: now_timestamp_ms(),
        },
        payload: TextPayload {
            content,
            sender_nickname,
        },
    }
}

fn member_changed_message(
    session: &SessionInner,
    device_id: DeviceId,
    change: MemberChange,
    nickname_update: Option<Option<String>>,
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
        payload: MemberChangedPayload {
            device_id,
            change,
            nickname: nickname_update.clone().flatten(),
            nickname_updated: nickname_update.is_some(),
        },
    }
}

#[cfg(all(test, feature = "relay", not(target_os = "android")))]
mod tests;

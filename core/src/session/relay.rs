use super::*;
use crate::{RouteDiscoveryRequestPayload, RouteDiscoveryResponsePayload};
use tokio::net::TcpListener;

impl Session {
    pub async fn create_group(
        device_id: DeviceId,
        group_id: GroupId,
        bind_addr: SocketAddr,
    ) -> Result<(Self, SocketAddr), NetworkError> {
        let session = Self::new(device_id, group_id, DeviceRole::Relay);
        let addr = session.listen(bind_addr).await?;
        Ok((session, addr))
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
                    _ = shutdown.wait_for(|closed| *closed) => return,
                }
            }
        });
        Ok(local_addr)
    }
}

impl SessionInner {
    pub(super) async fn answer_route_request(
        &self,
        neighbor_id: NeighborId,
        header: &MessageHeader,
        payload: &crate::RouteDiscoveryRequestPayload,
    ) -> Result<(), NetworkError> {
        let path = append_device(payload.path.clone(), self.device_id);
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
            payload: crate::RouteDiscoveryResponsePayload {
                request_message_id: header.message_id,
                path,
            },
        };
        self.send_message(neighbor_id, response).await?;
        Ok(())
    }

    pub(super) async fn route_outbound_message(
        &self,
        message: Message,
    ) -> Result<(), NetworkError> {
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

    pub(super) async fn flood_message(
        &self,
        except: Option<NeighborId>,
        message: &Message,
    ) -> Result<(), NetworkError> {
        let Some(message) = next_hop_message(message) else {
            return Ok(());
        };
        self.send_except(except, message).await
    }

    pub(super) async fn forward_direct_message(
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

    pub(super) async fn handle_route_discovery_request(
        &self,
        neighbor_id: NeighborId,
        header: &MessageHeader,
        payload: &RouteDiscoveryRequestPayload,
    ) -> Result<(), NetworkError> {
        let path = append_device(payload.path.clone(), self.device_id);
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

    pub(super) async fn handle_route_discovery_response(
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

    /// A newly connected relay learns older members from the neighbor's snapshot.
    /// Keep an established next hop when another neighbor describes the same member.
    pub(super) async fn learn_advertised_member(
        &self,
        neighbor: NeighborId,
        source: DeviceId,
        member: DeviceId,
    ) {
        if member == self.device_id || member == source {
            return;
        }
        self.routes
            .lock()
            .await
            .entry(member)
            .or_insert_with(|| RouteEntry {
                next_hop: neighbor,
                path: vec![self.device_id, source, member],
                updated_at: Instant::now(),
            });
    }

    pub(super) async fn store_route(
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

    pub(super) async fn remove_routes_for_device(&self, device_id: DeviceId) {
        self.routes
            .lock()
            .await
            .retain(|target, route| *target != device_id && !route.path.contains(&device_id));
    }

    pub(super) async fn sync_members_to_neighbor(&self, neighbor_id: NeighborId) {
        let members: Vec<_> = self
            .members
            .lock()
            .await
            .iter()
            .map(|(device_id, member)| (*device_id, member.online, member.nickname.clone()))
            .collect();
        for (device_id, online, nickname) in members {
            let change = if online {
                MemberChange::Online
            } else {
                MemberChange::Offline
            };
            if self
                .send_message(
                    neighbor_id,
                    member_changed_message(self, device_id, change, Some(nickname)),
                )
                .await
                .is_err()
            {
                return;
            }
        }
    }
}

async fn accept_loop(session: Arc<SessionInner>, listener: TcpListener) {
    let mut shutdown = session.shutdown.subscribe();
    loop {
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            _ = shutdown.wait_for(|closed| *closed) => return,
        };
        let Ok((stream, _)) = accepted else {
            return;
        };
        let _ = session.register_neighbor(stream).await;
    }
}

fn message_header_mut(message: &mut Message) -> &mut MessageHeader {
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

use super::*;

impl Session {
    /// A bounded inbox applies TCP backpressure while storing files.
    /// This session delivers events through the returned receiver.
    pub fn leaf_with_inbox(
        device: DeviceId,
        group: GroupId,
    ) -> (Self, mpsc::Receiver<SessionEvent>) {
        let (tx, rx) = mpsc::channel(32);
        (
            Self::build(
                device,
                group,
                DeviceRole::Leaf,
                ConnectionConfig::default(),
                Some(tx),
            ),
            rx,
        )
    }

    /// Identify the group advertised in the relay's first normal protocol frame.
    pub async fn probe_group(peer: SocketAddr) -> Result<GroupId, NetworkError> {
        let (_, group) = identified_stream(peer).await?;
        Ok(group)
    }

    /// Keep this Leaf connected even when the saved relay is initially offline.
    pub async fn follow_relay(&self, peer_addr: SocketAddr) -> Result<(), NetworkError> {
        if self.role() != DeviceRole::Leaf {
            return Err(NetworkError::WrongRole {
                required: DeviceRole::Leaf,
                actual: self.role(),
            });
        }
        *self.inner.leaf_reconnect_target.lock().await = Some(ReconnectTarget {
            peer_addr,
            local_ip: None,
            verify_group: true,
        });
        self.inner.reconnect_now.notify_one();
        Ok(())
    }

    /// Restart transport after Android reports a different network.
    pub async fn reconnect_leaf(&self) -> Result<(), NetworkError> {
        if self.role() != DeviceRole::Leaf {
            return Err(NetworkError::WrongRole {
                required: DeviceRole::Leaf,
                actual: self.role(),
            });
        }
        let previous = self.inner.neighbors.lock().await.keys().next().copied();
        if let Some(id) = previous {
            self.inner.remove_neighbor(id).await;
        }
        self.inner.reconnect_now.notify_one();
        Ok(())
    }

    pub async fn write_leaf_text(
        &self,
        peer: Option<DeviceId>,
        content: String,
        nickname: Option<String>,
    ) -> Result<MessageId, NetworkError> {
        let id = MessageId::new();
        let target = peer.map_or(MessageTarget::Broadcast, |device_id| {
            MessageTarget::Device { device_id }
        });
        self.write_leaf_message(text_message(&self.inner, id, target, content, nickname))
            .await?;
        Ok(id)
    }

    /// Wait until the frame has been written to the single relay socket.
    pub async fn write_leaf_message(&self, message: Message) -> Result<(), NetworkError> {
        if self.role() != DeviceRole::Leaf {
            return Err(NetworkError::WrongRole {
                required: DeviceRole::Leaf,
                actual: self.role(),
            });
        }
        let sender = self
            .inner
            .neighbors
            .lock()
            .await
            .values()
            .next()
            .ok_or(NetworkError::ChannelClosed)?
            .sender
            .clone();
        self.inner
            .mark_seen(message_header(&message).message_id)
            .await;
        let (written, result) = tokio::sync::oneshot::channel();
        sender
            .send(Outbound {
                message,
                written: Some(written),
            })
            .map_err(|_| NetworkError::ChannelClosed)?;
        result.await.map_err(|_| NetworkError::ChannelClosed)?
    }
}

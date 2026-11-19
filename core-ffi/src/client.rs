use crate::Result;
use lan_mesh::{
    DeviceId, FileChunkReader, FileId, GroupId, Message, MessageTarget, Session, SessionEvent,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::{sync::broadcast::error::RecvError, task::JoinHandle};

pub(crate) type EventSink = Arc<dyn Fn(Value) -> Result<()> + Send + Sync>;

#[derive(Deserialize)]
pub(crate) struct OpenGroup {
    group_id: GroupId,
    device_id: DeviceId,
    address: SocketAddr,
    nickname: String,
}

pub(crate) struct Client {
    pub session: Session,
    group_id: GroupId,
    nickname: String,
    emit: EventSink,
    events: JoinHandle<()>,
}

impl Client {
    pub async fn open(options: OpenGroup, emit: EventSink) -> Result<Self> {
        let session = Session::leaf(options.device_id, options.group_id);
        let mut receiver = session.subscribe();
        let sink = emit.clone();
        let leaf = session.clone();
        let events = tokio::spawn(async move {
            loop {
                let event = match receiver.recv().await {
                    Ok(SessionEvent::NeighborOnline { .. }) => {
                        json!({"type":"connection","online":true})
                    }
                    Ok(SessionEvent::NeighborOffline { .. }) => {
                        json!({"type":"connection","online":false})
                    }
                    Ok(SessionEvent::MessageReceived { message, .. }) => match message {
                        Message::Text { header, payload }
                            if addressed_to(&header.target, leaf.device_id())
                                && header.source_device_id != leaf.device_id() =>
                        {
                            json!({
                                "type":"text","id":header.message_id,"source":header.source_device_id,
                                "peer":direct_peer(&header.target, header.source_device_id),
                                "name":payload.sender_nickname,"content":payload.content,"time":header.timestamp_ms
                            })
                        }
                        Message::MemberChanged { .. } => json!({"type":"members"}),
                        _ => continue,
                    },
                    Err(RecvError::Closed) => break,
                    Err(RecvError::Lagged(_)) => {
                        json!({"type":"warning","text":"接收队列已满，部分消息可能未收到"})
                    }
                };
                if sink(event).is_err() {
                    break;
                }
            }
        });
        // Save the nickname before the first connection announces this identity.
        let _ = session
            .announce_nickname(Some(options.nickname.clone()))
            .await;
        session.follow_relay(options.address).await?;
        Ok(Self {
            session,
            group_id: options.group_id,
            nickname: options.nickname,
            emit,
            events,
        })
    }

    pub async fn members(&self) -> Result<Value> {
        let members = self
            .session
            .members()
            .await
            .into_iter()
            .map(|m| {
                json!({
                    "id":m.device_id, "name":m.nickname, "online":m.online
                })
            })
            .collect::<Vec<_>>();
        Ok(json!(members))
    }

    pub async fn send_text(&self, peer: Option<DeviceId>, content: String) -> Result<Value> {
        if content.trim().is_empty() {
            return Err("消息不能为空".into());
        }
        let id = self
            .session
            .write_leaf_text(peer, content, Some(self.nickname.clone()))
            .await?;
        Ok(json!(id))
    }

    pub async fn send_file(
        &self,
        peer: Option<DeviceId>,
        path: PathBuf,
        name: String,
        file_id: FileId,
    ) -> Result<Value> {
        let target = peer.map_or(MessageTarget::Broadcast, |device_id| {
            MessageTarget::Device { device_id }
        });
        let mut reader = FileChunkReader::open(
            &path,
            file_id,
            self.group_id,
            self.session.device_id(),
            target,
            8,
        )
        .await?
        .with_file_name(name)
        .with_sender_nickname(Some(self.nickname.clone()));
        let count = reader.chunk_count();
        let mut sent = 0u32;
        let mut last_percent = 0;
        while let Some(message) = reader.next_message().await? {
            self.session.write_leaf_message(message).await?;
            sent += 1;
            let percent = u64::from(sent) * 100 / u64::from(count);
            if percent != last_percent {
                (self.emit)(json!({"type":"sent","id":file_id,"percent":percent}))?;
                last_percent = percent;
            }
        }
        Ok(json!(file_id))
    }

    pub async fn close(&self) {
        self.events.abort();
        self.session.destroy().await;
    }
}

fn addressed_to(target: &MessageTarget, local: DeviceId) -> bool {
    matches!(target, MessageTarget::Broadcast)
        || matches!(target, MessageTarget::Device { device_id } if *device_id == local)
}

fn direct_peer(target: &MessageTarget, source: DeviceId) -> Option<DeviceId> {
    matches!(target, MessageTarget::Device { .. }).then_some(source)
}

#[cfg(test)]
mod tests;

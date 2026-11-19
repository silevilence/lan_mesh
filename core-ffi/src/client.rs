use crate::{
    Result,
    transfers::{Downloads, Uploads},
};
use lan_mesh::{
    DeviceId, FileId, GroupId, Message, MessageId, MessageTarget, Session, SessionEvent,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::{sync::Mutex, task::JoinHandle};

pub(crate) type EventSink = Arc<dyn Fn(Value) -> Result<()> + Send + Sync>;

#[derive(Deserialize)]
pub(crate) struct OpenGroup {
    group_id: GroupId,
    device_id: DeviceId,
    address: SocketAddr,
    cache: PathBuf,
    nickname: String,
}

pub(crate) struct Client {
    pub session: Session,
    group_id: GroupId,
    nickname: String,
    emit: EventSink,
    events: JoinHandle<()>,
    uploads: Arc<Uploads>,
    downloads: Arc<Mutex<Downloads>>,
}

impl Client {
    pub async fn open(options: OpenGroup, emit: EventSink) -> Result<Self> {
        let directory = Arc::new(
            tempfile::Builder::new()
                .prefix("mesh-")
                .tempdir_in(&options.cache)?,
        );
        let uploads = Arc::new(Uploads::new(
            directory.clone(),
            options.group_id,
            options.nickname.clone(),
        ));
        let downloads = Arc::new(Mutex::new(Downloads::new(directory)));
        let (session, mut receiver) = Session::leaf_with_inbox(options.device_id, options.group_id);
        let sink = emit.clone();
        let leaf = session.clone();
        let incoming = downloads.clone();
        let outgoing = uploads.clone();
        let group = options.group_id;
        let events = tokio::spawn(async move {
            while let Some(event) = receiver.recv().await {
                let event = match event {
                    SessionEvent::NeighborOnline { .. } => {
                        let requests = incoming.lock().await.resume(group, leaf.device_id(), None);
                        for request in requests {
                            let _ = leaf.write_leaf_message(request).await;
                        }
                        json!({"type":"connection","online":true})
                    }
                    SessionEvent::NeighborOffline { .. } => {
                        let _ = incoming.lock().await.offline(&sink);
                        json!({"type":"connection","online":false})
                    }
                    SessionEvent::MessageReceived { message, .. } => match message {
                        Message::FileChunk { header, payload }
                            if addressed_to(&header.target, leaf.device_id())
                                && header.source_device_id != leaf.device_id() =>
                        {
                            if let Err(e) = incoming
                                .lock()
                                .await
                                .chunk(group, &leaf, &sink, &header, &payload)
                                .await
                            {
                                let _ = sink(
                                    json!({"type":"warning","text":format!("接收文件失败：{e}")}),
                                );
                            }
                            continue;
                        }
                        Message::FileResumeRequest { header, payload } if matches!(header.target, MessageTarget::Device { device_id } if device_id == leaf.device_id()) =>
                        {
                            if let Err(e) = outgoing
                                .resend(&leaf, header.source_device_id, payload)
                                .await
                            {
                                let _ =
                                    sink(json!({"type":"warning","text":format!("续传失败：{e}")}));
                            }
                            continue;
                        }
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
            uploads,
            downloads,
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
        self.uploads
            .send(&self.session, &self.emit, peer, &path, name, file_id)
            .await?;
        Ok(json!(file_id))
    }

    pub async fn resume(&self, row: MessageId) -> Result<Value> {
        let requests =
            self.downloads
                .lock()
                .await
                .resume(self.group_id, self.session.device_id(), Some(row));
        for request in requests {
            self.session.write_leaf_message(request).await?;
        }
        Ok(Value::Null)
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

pub(super) fn direct_peer(target: &MessageTarget, source: DeviceId) -> Option<DeviceId> {
    matches!(target, MessageTarget::Device { .. }).then_some(source)
}

#[cfg(test)]
mod tests;

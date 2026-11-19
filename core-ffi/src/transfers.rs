use crate::{Result, client::EventSink};
use lan_mesh::{
    DeviceId, FileAssembler, FileChunkPayload, FileChunkReader, FileId, FileResumeRequestPayload,
    FileWriteStatus, GroupId, MessageHeader, MessageId, MessageTarget, Session,
    file_resume_request_message,
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tempfile::TempDir;

#[derive(Clone)]
struct Upload {
    name: String,
    peer: Option<DeviceId>,
    path: PathBuf,
}

pub(crate) struct Uploads {
    files: Mutex<HashMap<FileId, Upload>>,
    directory: Arc<TempDir>,
    group: GroupId,
    nickname: String,
}

impl Uploads {
    pub fn new(directory: Arc<TempDir>, group: GroupId, nickname: String) -> Self {
        Self {
            files: Mutex::default(),
            directory,
            group,
            nickname,
        }
    }

    async fn reader(
        &self,
        leaf: &Session,
        file: &Upload,
        id: FileId,
        peer: Option<DeviceId>,
    ) -> Result<FileChunkReader> {
        let target = peer.map_or(MessageTarget::Broadcast, |device_id| {
            MessageTarget::Device { device_id }
        });
        Ok(
            FileChunkReader::open(&file.path, id, self.group, leaf.device_id(), target, 8)
                .await?
                .with_file_name(file.name.clone())
                .with_sender_nickname(Some(self.nickname.clone())),
        )
    }

    pub async fn send(
        &self,
        leaf: &Session,
        emit: &EventSink,
        peer: Option<DeviceId>,
        staged: &Path,
        name: String,
        id: FileId,
    ) -> Result<()> {
        let file = Upload {
            name,
            peer,
            path: self.directory.path().join(format!("out-{}", id.0)),
        };
        // The Kotlin picker hands over its private staging file. Keep it for in-session resume.
        tokio::fs::rename(staged, &file.path).await?;
        let mut reader = self.reader(leaf, &file, id, peer).await?;
        self.files.lock().unwrap().insert(id, file);
        let mut sent = 0u64;
        let mut last = 0;
        while let Some(message) = reader.next_message().await? {
            leaf.write_leaf_message(message).await?;
            sent += 1;
            let percent = sent * 100 / u64::from(reader.chunk_count());
            if percent != last {
                emit(json!({"type":"sent","id":id,"percent":percent}))?;
                last = percent;
            }
        }
        Ok(())
    }

    pub async fn resend(
        &self,
        leaf: &Session,
        requester: DeviceId,
        request: FileResumeRequestPayload,
    ) -> Result<()> {
        let Some(file) = self.files.lock().unwrap().get(&request.file_id).cloned() else {
            return Ok(());
        };
        if file.peer.is_some_and(|peer| peer != requester) {
            return Ok(());
        }
        let mut reader = self
            .reader(leaf, &file, request.file_id, Some(requester))
            .await?;
        for index in request.missing_chunks {
            leaf.write_leaf_message(reader.message_at(index).await?)
                .await?;
        }
        Ok(())
    }
}

struct Download {
    row: MessageId,
    source: DeviceId,
    file_id: FileId,
    peer: Option<DeviceId>,
    sender: Option<String>,
    name: String,
    time: u64,
    percent: u64,
    assembler: Option<FileAssembler>,
    requested: HashSet<u32>,
    interrupted: bool,
}

impl Download {
    fn event(&self, state: &str, path: Option<&Path>) -> Value {
        json!({"type":"received","id":self.row,"source":self.source,"file_id":self.file_id,
            "peer":self.peer,"sender":self.sender,"name":self.name,"time":self.time,
            "percent":self.percent,"state":state,"path":path})
    }

    fn request(&mut self, group: GroupId, local: DeviceId) -> Option<lan_mesh::Message> {
        let assembler = self.assembler.as_ref()?;
        // Desktop senders may materialize the requested messages in memory.
        let indices: Vec<_> = assembler.missing_indices().take(128).collect();
        if indices.is_empty() {
            return None;
        }
        self.requested = indices.iter().copied().collect();
        self.interrupted = false;
        Some(file_resume_request_message(
            self.file_id,
            indices,
            group,
            local,
            MessageTarget::Device {
                device_id: self.source,
            },
            8,
        ))
    }
}

pub(crate) struct Downloads {
    files: HashMap<(DeviceId, FileId), Download>,
    directory: Arc<TempDir>,
}

impl Downloads {
    pub fn new(directory: Arc<TempDir>) -> Self {
        Self {
            files: HashMap::new(),
            directory,
        }
    }

    pub async fn chunk(
        &mut self,
        group: GroupId,
        leaf: &Session,
        emit: &EventSink,
        header: &MessageHeader,
        chunk: &FileChunkPayload,
    ) -> Result<()> {
        let key = (header.source_device_id, chunk.file_id);
        let fresh = !self.files.contains_key(&key);
        let row = self.files.entry(key).or_insert_with(|| Download {
            row: MessageId::new(),
            source: key.0,
            file_id: key.1,
            peer: super::client::direct_peer(&header.target, key.0),
            sender: chunk.sender_nickname.clone(),
            name: chunk.file_name.clone(),
            time: header.timestamp_ms,
            percent: 0,
            assembler: None,
            requested: HashSet::new(),
            interrupted: false,
        });
        let path = self.directory.path().join(row.row.0.to_string());
        if fresh {
            match FileAssembler::create(
                &path,
                chunk.file_id,
                chunk.chunk_count,
                chunk.total_size,
                &chunk.sha256,
            )
            .await
            {
                Ok(assembler) => row.assembler = Some(assembler),
                Err(e) => return emit(row.event(&format!("接收失败：{e}"), None)),
            }
        }
        let Some(assembler) = &mut row.assembler else {
            return Ok(());
        };
        let finished_batch = row.requested.remove(&chunk.chunk_index) && row.requested.is_empty();
        match assembler.write_chunk(chunk).await {
            Ok(FileWriteStatus::Incomplete { received }) => {
                let percent = u64::from(received) * 100 / u64::from(chunk.chunk_count);
                if fresh || row.percent != percent || row.interrupted {
                    row.percent = percent;
                    row.interrupted = false;
                    emit(row.event("接收中", None))?;
                }
                if (finished_batch || chunk.chunk_index == chunk.chunk_count - 1)
                    && row.requested.is_empty()
                    && let Some(request) = row.request(group, leaf.device_id())
                {
                    leaf.write_leaf_message(request).await?;
                }
            }
            Ok(FileWriteStatus::Complete { path }) => {
                row.assembler = None;
                row.percent = 100;
                emit(row.event("已校验", Some(&path)))?;
            }
            result => {
                let reason = match result {
                    Err(e) => e.to_string(),
                    _ => "SHA256 校验失败".to_owned(),
                };
                row.assembler = None;
                let _ = tokio::fs::remove_file(path).await;
                emit(row.event(&reason, None))?;
            }
        }
        Ok(())
    }

    pub fn offline(&mut self, emit: &EventSink) -> Result<()> {
        for row in self.files.values_mut().filter(|r| r.assembler.is_some()) {
            row.interrupted = true;
            row.requested.clear();
            emit(row.event("等待续传", None))?;
        }
        Ok(())
    }

    pub fn resume(
        &mut self,
        group: GroupId,
        local: DeviceId,
        row_id: Option<MessageId>,
    ) -> Vec<lan_mesh::Message> {
        self.files
            .values_mut()
            .filter(|r| row_id.map_or(r.interrupted, |id| id == r.row))
            .filter_map(|r| r.request(group, local))
            .collect()
    }
}

#[cfg(test)]
mod tests;

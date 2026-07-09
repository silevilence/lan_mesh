use crate::{
    DeviceId, FileChunkPayload, FileId, FileResumeRequestPayload, GroupId, Message, MessageHeader,
    MessageId, MessageTarget, now_timestamp_ms,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fmt, io,
    path::{Path, PathBuf},
};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom},
};

pub const FILE_CHUNK_SIZE: usize = 64 * 1024;

#[derive(Debug)]
pub enum FileTransferError {
    Io(io::Error),
    InvalidChunk(&'static str),
    ChunkOutOfRange { chunk_index: u32, chunk_count: u32 },
}

impl fmt::Display for FileTransferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::InvalidChunk(reason) => write!(f, "invalid file chunk: {reason}"),
            Self::ChunkOutOfRange {
                chunk_index,
                chunk_count,
            } => write!(
                f,
                "chunk index {chunk_index} is outside chunk count {chunk_count}"
            ),
        }
    }
}

impl std::error::Error for FileTransferError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::InvalidChunk(_) | Self::ChunkOutOfRange { .. } => None,
        }
    }
}

impl From<io::Error> for FileTransferError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

pub struct FileChunkReader {
    file: File,
    file_id: FileId,
    group_id: GroupId,
    source_device_id: DeviceId,
    target: MessageTarget,
    ttl: u8,
    file_name: String,
    total_size: u64,
    sha256: String,
    next_chunk_index: u32,
    chunk_count: u32,
}

impl FileChunkReader {
    pub async fn open(
        path: impl AsRef<Path>,
        file_id: FileId,
        group_id: GroupId,
        source_device_id: DeviceId,
        target: MessageTarget,
        ttl: u8,
    ) -> Result<Self, FileTransferError> {
        let path = path.as_ref();
        let total_size = tokio::fs::metadata(path).await?.len();
        let sha256 = sha256_file(path).await?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("received-file")
            .to_string();
        Ok(Self {
            file: File::open(path).await?,
            file_id,
            group_id,
            source_device_id,
            target,
            ttl,
            file_name,
            total_size,
            sha256,
            next_chunk_index: 0,
            chunk_count: chunk_count(total_size),
        })
    }

    pub fn chunk_count(&self) -> u32 {
        self.chunk_count
    }

    pub fn total_size(&self) -> u64 {
        self.total_size
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    pub async fn next_message(&mut self) -> Result<Option<Message>, FileTransferError> {
        if self.next_chunk_index >= self.chunk_count {
            return Ok(None);
        }

        let chunk_index = self.next_chunk_index;
        let expected_len = expected_chunk_len(chunk_index, self.total_size)?;
        let mut data = vec![0; expected_len];
        if expected_len > 0 {
            self.file.read_exact(&mut data).await?;
        }
        self.next_chunk_index += 1;
        Ok(Some(file_chunk_message(
            self.file_id,
            chunk_index,
            self.chunk_count,
            self.total_size,
            self.file_name.clone(),
            self.sha256.clone(),
            data,
            self.group_id,
            self.source_device_id,
            self.target.clone(),
            self.ttl,
        )))
    }
}

pub async fn resend_file_chunks(
    path: impl AsRef<Path>,
    request: &FileResumeRequestPayload,
    group_id: GroupId,
    source_device_id: DeviceId,
    target: MessageTarget,
    ttl: u8,
) -> Result<Vec<Message>, FileTransferError> {
    let path = path.as_ref();
    let total_size = tokio::fs::metadata(path).await?.len();
    let chunk_count = chunk_count(total_size);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("received-file")
        .to_string();
    let sha256 = sha256_file(path).await?;
    let mut file = File::open(path).await?;
    let mut messages = Vec::with_capacity(request.missing_chunks.len());

    for &chunk_index in &request.missing_chunks {
        if chunk_index >= chunk_count {
            return Err(FileTransferError::ChunkOutOfRange {
                chunk_index,
                chunk_count,
            });
        }
        let expected_len = expected_chunk_len(chunk_index, total_size)?;
        file.seek(SeekFrom::Start(chunk_offset(chunk_index)?))
            .await?;
        let mut data = vec![0; expected_len];
        if expected_len > 0 {
            file.read_exact(&mut data).await?;
        }
        messages.push(file_chunk_message(
            request.file_id,
            chunk_index,
            chunk_count,
            total_size,
            file_name.clone(),
            sha256.clone(),
            data,
            group_id,
            source_device_id,
            target.clone(),
            ttl,
        ));
    }

    Ok(messages)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileAssemblyStatus {
    Incomplete { missing_chunks: Vec<u32> },
    Complete { path: PathBuf },
    HashMismatch { expected: String, actual: String },
}

pub struct FileAssembler {
    file_id: FileId,
    path: PathBuf,
    file: File,
    chunk_count: u32,
    total_size: u64,
    sha256: String,
    received_chunks: BTreeSet<u32>,
}

impl FileAssembler {
    pub async fn create(
        path: impl Into<PathBuf>,
        file_id: FileId,
        chunk_count: u32,
        total_size: u64,
        sha256: impl Into<String>,
    ) -> Result<Self, FileTransferError> {
        if chunk_count == 0 || chunk_count != self::chunk_count(total_size) {
            return Err(FileTransferError::InvalidChunk("bad chunk count"));
        }
        let path = path.into();
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)
            .await?;
        file.set_len(total_size).await?;
        Ok(Self {
            file_id,
            path,
            file,
            chunk_count,
            total_size,
            sha256: sha256.into(),
            received_chunks: BTreeSet::new(),
        })
    }

    pub fn received_chunks(&self) -> Vec<u32> {
        self.received_chunks.iter().copied().collect()
    }

    pub fn missing_chunks(&self) -> Vec<u32> {
        (0..self.chunk_count)
            .filter(|index| !self.received_chunks.contains(index))
            .collect()
    }

    pub async fn push_chunk(
        &mut self,
        chunk: &FileChunkPayload,
    ) -> Result<FileAssemblyStatus, FileTransferError> {
        self.validate_chunk(chunk)?;
        self.file
            .seek(SeekFrom::Start(chunk_offset(chunk.chunk_index)?))
            .await?;
        self.file.write_all(&chunk.data).await?;
        self.received_chunks.insert(chunk.chunk_index);

        let missing_chunks = self.missing_chunks();
        if !missing_chunks.is_empty() {
            return Ok(FileAssemblyStatus::Incomplete { missing_chunks });
        }

        self.file.flush().await?;
        let actual = sha256_file(&self.path).await?;
        if actual == self.sha256 {
            Ok(FileAssemblyStatus::Complete {
                path: self.path.clone(),
            })
        } else {
            Ok(FileAssemblyStatus::HashMismatch {
                expected: self.sha256.clone(),
                actual,
            })
        }
    }

    pub fn resume_request_message(
        &self,
        group_id: GroupId,
        source_device_id: DeviceId,
        target: MessageTarget,
        ttl: u8,
    ) -> Message {
        file_resume_request_message(
            self.file_id,
            self.missing_chunks(),
            group_id,
            source_device_id,
            target,
            ttl,
        )
    }

    fn validate_chunk(&self, chunk: &FileChunkPayload) -> Result<(), FileTransferError> {
        if chunk.file_id != self.file_id {
            return Err(FileTransferError::InvalidChunk("wrong file id"));
        }
        if chunk.chunk_count != self.chunk_count || chunk.total_size != self.total_size {
            return Err(FileTransferError::InvalidChunk("metadata mismatch"));
        }
        if chunk.sha256 != self.sha256 {
            return Err(FileTransferError::InvalidChunk("hash mismatch"));
        }
        if chunk.chunk_index >= self.chunk_count {
            return Err(FileTransferError::ChunkOutOfRange {
                chunk_index: chunk.chunk_index,
                chunk_count: self.chunk_count,
            });
        }
        if chunk.data.len() != expected_chunk_len(chunk.chunk_index, self.total_size)? {
            return Err(FileTransferError::InvalidChunk("bad chunk length"));
        }
        Ok(())
    }
}

pub fn file_resume_request_message(
    file_id: FileId,
    missing_chunks: Vec<u32>,
    group_id: GroupId,
    source_device_id: DeviceId,
    target: MessageTarget,
    ttl: u8,
) -> Message {
    Message::FileResumeRequest {
        header: MessageHeader {
            message_id: MessageId::new(),
            group_id,
            source_device_id,
            target,
            ttl,
            hop_count: 0,
            timestamp_ms: now_timestamp_ms(),
        },
        payload: FileResumeRequestPayload {
            file_id,
            missing_chunks,
        },
    }
}

pub async fn sha256_file(path: impl AsRef<Path>) -> Result<String, FileTransferError> {
    let mut file = File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0; FILE_CHUNK_SIZE];
    loop {
        let len = file.read(&mut buf).await?;
        if len == 0 {
            break;
        }
        hasher.update(&buf[..len]);
    }
    Ok(hex(&hasher.finalize()))
}

fn file_chunk_message(
    file_id: FileId,
    chunk_index: u32,
    chunk_count: u32,
    total_size: u64,
    file_name: String,
    sha256: String,
    data: Vec<u8>,
    group_id: GroupId,
    source_device_id: DeviceId,
    target: MessageTarget,
    ttl: u8,
) -> Message {
    Message::FileChunk {
        header: MessageHeader {
            message_id: MessageId::new(),
            group_id,
            source_device_id,
            target,
            ttl,
            hop_count: 0,
            timestamp_ms: now_timestamp_ms(),
        },
        payload: FileChunkPayload {
            file_id,
            file_name,
            chunk_index,
            chunk_count,
            total_size,
            sha256,
            data,
        },
    }
}

fn chunk_count(total_size: u64) -> u32 {
    let count = total_size.div_ceil(FILE_CHUNK_SIZE as u64).max(1);
    u32::try_from(count).expect("file is too large for u32 chunk indexes")
}

fn expected_chunk_len(chunk_index: u32, total_size: u64) -> Result<usize, FileTransferError> {
    let offset = chunk_offset(chunk_index)?;
    if offset > total_size {
        return Err(FileTransferError::ChunkOutOfRange {
            chunk_index,
            chunk_count: chunk_count(total_size),
        });
    }
    Ok((total_size - offset).min(FILE_CHUNK_SIZE as u64) as usize)
}

fn chunk_offset(chunk_index: u32) -> Result<u64, FileTransferError> {
    u64::from(chunk_index)
        .checked_mul(FILE_CHUNK_SIZE as u64)
        .ok_or(FileTransferError::InvalidChunk("chunk offset overflow"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

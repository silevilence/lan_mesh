//! Core LAN Mesh communication library.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use std::{fmt, io};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;
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
}

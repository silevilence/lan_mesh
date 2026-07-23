use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_nickname: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct FileChunkPayload {
    pub file_id: FileId,
    pub file_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_nickname: Option<String>,
    pub chunk_index: u32,
    pub chunk_count: u32,
    pub total_size: u64,
    pub sha256: String,
    #[serde(with = "base64_data")]
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct FileResumeRequestPayload {
    pub file_id: FileId,
    pub missing_chunks: Vec<u32>,
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
    FileResumeRequest {
        header: MessageHeader,
        payload: FileResumeRequestPayload,
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

pub fn now_timestamp_ms() -> TimestampMs {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as TimestampMs
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

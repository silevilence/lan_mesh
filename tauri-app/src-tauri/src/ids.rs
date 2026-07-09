use lan_mesh_core::{DeviceId, DeviceRole, FileId, GroupId, NeighborId};
use std::{net::IpAddr, time::Duration};
use uuid::Uuid;

pub(crate) fn parse_or_new_device_id(value: Option<String>) -> Result<DeviceId, String> {
    value
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(parse_device_id)
        .unwrap_or_else(|| Ok(DeviceId::new()))
}

pub(crate) fn parse_or_new_group_id(value: Option<String>) -> Result<GroupId, String> {
    value
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(parse_group_id)
        .unwrap_or_else(|| Ok(GroupId::new()))
}

pub(crate) fn parse_device_id(value: &str) -> Result<DeviceId, String> {
    Uuid::parse_str(value)
        .map(DeviceId)
        .map_err(|err| format!("invalid device_id: {err}"))
}

pub(crate) fn parse_file_id(value: &str) -> Result<FileId, String> {
    Uuid::parse_str(value)
        .map(FileId)
        .map_err(|err| format!("invalid file_id: {err}"))
}

pub(crate) fn parse_group_id(value: &str) -> Result<GroupId, String> {
    Uuid::parse_str(value)
        .map(GroupId)
        .map_err(|err| format!("invalid group_id: {err}"))
}

pub(crate) fn parse_optional_ip(value: Option<String>) -> Result<Option<IpAddr>, String> {
    value
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse()
                .map_err(|err| format!("invalid local_ip: {err}"))
        })
        .transpose()
}

pub(crate) fn id(uuid: Uuid) -> String {
    uuid.to_string()
}

pub(crate) fn neighbor(neighbor_id: NeighborId) -> String {
    id(neighbor_id.0)
}

pub(crate) fn role_name(role: DeviceRole) -> &'static str {
    match role {
        DeviceRole::Relay => "relay",
        DeviceRole::Leaf => "leaf",
    }
}

pub(crate) fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

pub(crate) fn err_string(err: impl ToString) -> String {
    err.to_string()
}

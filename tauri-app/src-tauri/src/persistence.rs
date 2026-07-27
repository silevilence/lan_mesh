use lan_mesh_core::{DeviceId, DeviceRole, GroupId};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

/// A user-owned group entry.  Network objects are deliberately not persisted:
/// a fresh `Session` is created whenever the application restores this record.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct PersistedGroup {
    pub(crate) device_id: DeviceId,
    pub(crate) group_id: GroupId,
    pub(crate) group_name: String,
    pub(crate) role: DeviceRole,
    pub(crate) bind_addr: Option<String>,
    pub(crate) relay_addr: Option<String>,
    pub(crate) local_ip: Option<String>,
    pub(crate) status: GroupAvailability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GroupAvailability {
    Connected,
    Unreachable,
}

impl Default for GroupAvailability {
    fn default() -> Self {
        Self::Unreachable
    }
}

#[derive(Default, Deserialize, Serialize)]
struct PersistedGroupsFile {
    #[serde(default)]
    groups: Vec<PersistedGroup>,
}

#[derive(Clone)]
pub(crate) struct GroupStore {
    inner: Arc<GroupStoreInner>,
}

struct GroupStoreInner {
    path: PathBuf,
    groups: Mutex<Vec<PersistedGroup>>,
}

impl GroupStore {
    pub(crate) fn load(path: PathBuf) -> Self {
        let groups = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<PersistedGroupsFile>(&bytes).ok())
            .map(|file| file.groups)
            .unwrap_or_default();
        Self {
            inner: Arc::new(GroupStoreInner {
                path,
                groups: Mutex::new(groups),
            }),
        }
    }

    pub(crate) async fn list(&self) -> Vec<PersistedGroup> {
        self.inner.groups.lock().await.clone()
    }

    pub(crate) async fn find(&self, group_id: GroupId) -> Option<PersistedGroup> {
        self.inner
            .groups
            .lock()
            .await
            .iter()
            .find(|group| group.group_id == group_id)
            .cloned()
    }

    pub(crate) async fn upsert(&self, group: PersistedGroup) -> Result<(), String> {
        let mut groups = self.inner.groups.lock().await;
        if let Some(existing) = groups
            .iter_mut()
            .find(|item| item.group_id == group.group_id)
        {
            *existing = group;
        } else {
            groups.push(group);
        }
        self.write(&groups).await
    }

    pub(crate) async fn set_status(
        &self,
        group_id: GroupId,
        status: GroupAvailability,
        last_error: Option<String>,
    ) -> Result<(), String> {
        let mut groups = self.inner.groups.lock().await;
        let group = groups
            .iter_mut()
            .find(|group| group.group_id == group_id)
            .ok_or_else(|| "saved group not found".to_string())?;
        group.status = status;
        group.last_error = last_error;
        self.write(&groups).await
    }

    pub(crate) async fn remove(&self, group_id: GroupId) -> Result<(), String> {
        let mut groups = self.inner.groups.lock().await;
        groups.retain(|group| group.group_id != group_id);
        self.write(&groups).await
    }

    async fn write(&self, groups: &[PersistedGroup]) -> Result<(), String> {
        let parent = self
            .inner
            .path
            .parent()
            .ok_or_else(|| "groups.json has no parent directory".to_string())?;
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|err| format!("failed to create group storage directory: {err}"))?;
        let contents = serde_json::to_vec_pretty(&PersistedGroupsFile {
            groups: groups.to_vec(),
        })
        .map_err(|err| format!("failed to serialize groups: {err}"))?;
        tokio::fs::write(&self.inner.path, contents)
            .await
            .map_err(|err| format!("failed to save groups.json: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn group_store_round_trips_saved_group() {
        let directory =
            std::env::temp_dir().join(format!("lan-mesh-store-{}", uuid::Uuid::new_v4()));
        let path = directory.join("groups.json");
        let store = GroupStore::load(path.clone());
        let group = PersistedGroup {
            device_id: DeviceId::new(),
            group_id: GroupId::new(),
            group_name: "测试群组".into(),
            role: DeviceRole::Leaf,
            bind_addr: None,
            relay_addr: Some("127.0.0.1:9000".into()),
            local_ip: None,
            status: GroupAvailability::Unreachable,
            last_error: Some("unreachable".into()),
        };
        store.upsert(group.clone()).await.unwrap();

        let reloaded = GroupStore::load(path.clone());
        assert_eq!(reloaded.list().await.len(), 1);
        assert_eq!(
            reloaded.find(group.group_id).await.unwrap().group_name,
            "测试群组"
        );

        let _ = tokio::fs::remove_dir_all(directory).await;
    }
}

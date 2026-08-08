use crate::events::forward_events;
use crate::persistence::GroupStore;
use lan_mesh_core::{FileAssembler, FileId, GroupId, MessageTarget, Session, UpdatePackagePayload};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tauri::AppHandle;
use tokio::{sync::Mutex, task::JoinHandle};

pub(crate) type SentFiles = Arc<Mutex<HashMap<FileId, SentFile>>>;
pub(crate) type ReceivedFiles = Arc<Mutex<HashMap<FileId, FileAssembler>>>;
pub(crate) type ReceivedUpdatePackages = Arc<Mutex<HashMap<FileId, ReceivedUpdatePackage>>>;

#[derive(Clone)]
pub(crate) struct ReceivedUpdatePackage {
    pub(crate) metadata: UpdatePackagePayload,
    pub(crate) path: Option<PathBuf>,
}

pub(crate) struct AppState {
    pub(crate) clients: Mutex<HashMap<GroupId, ClientSession>>,
    pub(crate) active_group_id: Mutex<Option<GroupId>>,
    pub(crate) event_tasks: Mutex<HashMap<GroupId, JoinHandle<()>>>,
    pub(crate) sent_files: SentFiles,
    pub(crate) received_files: ReceivedFiles,
    pub(crate) received_update_packages: ReceivedUpdatePackages,
    pub(crate) groups: GroupStore,
}

impl AppState {
    pub(crate) fn new(groups: GroupStore) -> Self {
        Self {
            clients: Mutex::new(HashMap::new()),
            active_group_id: Mutex::new(None),
            event_tasks: Mutex::new(HashMap::new()),
            sent_files: Default::default(),
            received_files: Default::default(),
            received_update_packages: Default::default(),
            groups,
        }
    }
}

#[derive(Clone)]
pub(crate) struct ClientSession {
    pub(crate) session: Session,
    pub(crate) group_id: GroupId,
}

#[derive(Clone)]
pub(crate) struct SentFile {
    pub(crate) path: String,
    pub(crate) target: MessageTarget,
    pub(crate) sender_nickname: Option<String>,
}

pub(crate) async fn install_session(
    app: &AppHandle,
    state: &AppState,
    client: ClientSession,
    activate: bool,
) {
    let group_id = client.group_id;
    if let Some(task) = state.event_tasks.lock().await.remove(&group_id) {
        task.abort();
    }
    let old_client = state.clients.lock().await.insert(group_id, client.clone());
    if let Some(old_client) = old_client {
        old_client.session.destroy().await;
    }
    let task = tokio::spawn(forward_events(
        app.clone(),
        client.session,
        client.group_id,
        state.sent_files.clone(),
        state.received_files.clone(),
        state.received_update_packages.clone(),
        state.groups.clone(),
    ));
    state.event_tasks.lock().await.insert(group_id, task);
    let mut active_group_id = state.active_group_id.lock().await;
    if activate || active_group_id.is_none() {
        *active_group_id = Some(group_id);
        state.sent_files.lock().await.clear();
        state.received_files.lock().await.clear();
    }
}

pub(crate) async fn current_session(state: &AppState) -> Result<ClientSession, String> {
    let group_id = state
        .active_group_id
        .lock()
        .await
        .ok_or_else(|| "no active mesh session".to_string())?;
    state
        .clients
        .lock()
        .await
        .get(&group_id)
        .cloned()
        .ok_or_else(|| "active mesh session is unavailable".to_string())
}

pub(crate) async fn activate_session(
    state: &AppState,
    group_id: GroupId,
) -> Result<ClientSession, String> {
    let client = state
        .clients
        .lock()
        .await
        .get(&group_id)
        .cloned()
        .ok_or_else(|| "saved group is currently unreachable".to_string())?;
    *state.active_group_id.lock().await = Some(group_id);
    state.sent_files.lock().await.clear();
    state.received_files.lock().await.clear();
    Ok(client)
}

pub(crate) async fn remove_session(state: &AppState, group_id: GroupId) {
    if let Some(task) = state.event_tasks.lock().await.remove(&group_id) {
        task.abort();
    }
    if let Some(client) = state.clients.lock().await.remove(&group_id) {
        client.session.destroy().await;
    }
    let mut active_group_id = state.active_group_id.lock().await;
    if *active_group_id == Some(group_id) {
        *active_group_id = state.clients.lock().await.keys().next().copied();
        state.sent_files.lock().await.clear();
        state.received_files.lock().await.clear();
    }
}

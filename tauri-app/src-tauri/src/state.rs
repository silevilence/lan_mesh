use crate::events::forward_events;
use lan_mesh_core::{FileAssembler, FileId, GroupId, MessageTarget, Session};
use std::{collections::HashMap, sync::Arc};
use tauri::AppHandle;
use tokio::{sync::Mutex, task::JoinHandle};

pub(crate) type SentFiles = Arc<Mutex<HashMap<FileId, SentFile>>>;
pub(crate) type ReceivedFiles = Arc<Mutex<HashMap<FileId, FileAssembler>>>;

#[derive(Default)]
pub(crate) struct AppState {
    pub(crate) client: Mutex<Option<ClientSession>>,
    pub(crate) event_task: Mutex<Option<JoinHandle<()>>>,
    pub(crate) sent_files: SentFiles,
    pub(crate) received_files: ReceivedFiles,
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
}

pub(crate) async fn install_session(app: &AppHandle, state: &AppState, client: ClientSession) {
    if let Some(task) = state.event_task.lock().await.take() {
        task.abort();
    }
    let old_client = state.client.lock().await.replace(client.clone());
    if let Some(old_client) = old_client {
        old_client.session.destroy().await;
    }
    state.sent_files.lock().await.clear();
    state.received_files.lock().await.clear();
    let task = tokio::spawn(forward_events(
        app.clone(),
        client.session,
        client.group_id,
        state.sent_files.clone(),
        state.received_files.clone(),
    ));
    *state.event_task.lock().await = Some(task);
}

pub(crate) async fn current_session(state: &AppState) -> Result<ClientSession, String> {
    state
        .client
        .lock()
        .await
        .clone()
        .ok_or_else(|| "no active mesh session".to_string())
}

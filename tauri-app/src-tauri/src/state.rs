use crate::groups::Groups;
use lan_mesh_core::{FileAssembler, FileId, MessageTarget, UpdatePackagePayload};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

pub(crate) type SentFiles = Arc<Mutex<HashMap<FileId, SentFile>>>;
pub(crate) type ReceivedFiles = Arc<Mutex<HashMap<FileId, FileAssembler>>>;
pub(crate) type ReceivedUpdatePackages = Arc<Mutex<HashMap<FileId, ReceivedUpdatePackage>>>;

#[derive(Clone)]
pub(crate) struct ReceivedUpdatePackage {
    pub(crate) metadata: UpdatePackagePayload,
    pub(crate) path: Option<PathBuf>,
}

pub(crate) struct AppState {
    pub(crate) sent_files: SentFiles,
    pub(crate) received_files: ReceivedFiles,
    pub(crate) received_update_packages: ReceivedUpdatePackages,
    pub(crate) groups: Groups,
}

impl AppState {
    pub(crate) fn new(groups: Groups) -> Self {
        Self {
            sent_files: Default::default(),
            received_files: Default::default(),
            received_update_packages: Default::default(),
            groups,
        }
    }
}

#[derive(Clone)]
pub(crate) struct SentFile {
    pub(crate) path: String,
    pub(crate) target: MessageTarget,
    pub(crate) sender_nickname: Option<String>,
}

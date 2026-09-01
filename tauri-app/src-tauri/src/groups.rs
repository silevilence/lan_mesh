use crate::{
    DISCOVERY_PORT,
    ids::{err_string, parse_optional_ip},
    network::announcement_targets,
    persistence::{GroupStore, PersistedGroup},
};
use lan_mesh_core::{DeviceId, DeviceRole, GroupId, NeighborId, Session, SessionEvent};
use std::{
    collections::HashMap,
    future::Future,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Mutex, broadcast},
    task::JoinHandle,
};

pub(crate) use crate::persistence::GroupAvailability;

pub(crate) const RECONNECT_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub(crate) struct GroupSnapshot {
    pub(crate) device_id: DeviceId,
    pub(crate) group_id: GroupId,
    pub(crate) group_name: String,
    pub(crate) role: DeviceRole,
    pub(crate) bind_addr: Option<String>,
    pub(crate) relay_addr: Option<String>,
    pub(crate) local_ip: Option<String>,
    pub(crate) availability: GroupAvailability,
    pub(crate) last_error: Option<String>,
}

impl From<&PersistedGroup> for GroupSnapshot {
    fn from(group: &PersistedGroup) -> Self {
        Self {
            device_id: group.device_id,
            group_id: group.group_id,
            group_name: group.group_name.clone(),
            role: group.role,
            bind_addr: group.bind_addr.clone(),
            relay_addr: group.relay_addr.clone(),
            local_ip: group.local_ip.clone(),
            availability: group.status,
            last_error: group.last_error.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct GroupRuntime {
    group_id: GroupId,
    session: Session,
}

impl GroupRuntime {
    pub(crate) fn group_id(&self) -> GroupId {
        self.group_id
    }

    pub(crate) fn session(&self) -> &Session {
        &self.session
    }
}

#[derive(Clone)]
pub(crate) enum GroupEvent {
    Session {
        runtime: GroupRuntime,
        event: Arc<SessionEvent>,
    },
    GroupsChanged,
    ResyncRequired,
}

pub(crate) struct CreateRelayRequest {
    pub(crate) device_id: Option<DeviceId>,
    pub(crate) group_id: GroupId,
    pub(crate) group_name: String,
    pub(crate) bind_addr: SocketAddr,
}

pub(crate) struct JoinLeafRequest {
    pub(crate) device_id: Option<DeviceId>,
    pub(crate) group_id: GroupId,
    pub(crate) group_name: String,
    pub(crate) relay_addr: SocketAddr,
    pub(crate) local_ip: Option<IpAddr>,
}

pub(crate) struct StartedGroup {
    pub(crate) snapshot: GroupSnapshot,
    pub(crate) neighbor_id: Option<NeighborId>,
}

struct EstablishedSession {
    session: Session,
    neighbor_id: Option<NeighborId>,
}

type EstablishFuture<'a> =
    Pin<Box<dyn Future<Output = Result<EstablishedSession, String>> + Send + 'a>>;

trait SessionFactory: Send + Sync {
    fn establish<'a>(&'a self, group: &'a mut PersistedGroup) -> EstablishFuture<'a>;
}

struct NetworkSessionFactory;

impl SessionFactory for NetworkSessionFactory {
    fn establish<'a>(&'a self, group: &'a mut PersistedGroup) -> EstablishFuture<'a> {
        Box::pin(async move {
            match group.role {
                DeviceRole::Relay => establish_relay(group).await,
                DeviceRole::Leaf => establish_leaf(group).await,
            }
        })
    }
}

struct RuntimeEntry {
    runtime: GroupRuntime,
    generation: u64,
}

struct GroupsInner {
    store: GroupStore,
    session_factory: Arc<dyn SessionFactory>,
    runtimes: Mutex<HashMap<GroupId, RuntimeEntry>>,
    event_tasks: Mutex<HashMap<GroupId, JoinHandle<()>>>,
    operation_locks: Mutex<HashMap<GroupId, Arc<Mutex<()>>>>,
    events: broadcast::Sender<GroupEvent>,
    next_generation: AtomicU64,
    recovery_task: Mutex<Option<JoinHandle<()>>>,
    shutdown: broadcast::Sender<()>,
}

#[derive(Clone)]
pub(crate) struct Groups {
    inner: Arc<GroupsInner>,
}

impl Groups {
    pub(crate) fn load(path: PathBuf) -> Self {
        Self::with_factory(GroupStore::load(path), Arc::new(NetworkSessionFactory))
    }

    fn with_factory(store: GroupStore, session_factory: Arc<dyn SessionFactory>) -> Self {
        let (events, _) = broadcast::channel(256);
        let (shutdown, _) = broadcast::channel(1);
        Self {
            inner: Arc::new(GroupsInner {
                store,
                session_factory,
                runtimes: Mutex::new(HashMap::new()),
                event_tasks: Mutex::new(HashMap::new()),
                operation_locks: Mutex::new(HashMap::new()),
                events,
                next_generation: AtomicU64::new(1),
                recovery_task: Mutex::new(None),
                shutdown,
            }),
        }
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<GroupEvent> {
        self.inner.events.subscribe()
    }

    pub(crate) async fn create_relay(
        &self,
        request: CreateRelayRequest,
    ) -> Result<StartedGroup, String> {
        let operation = self.operation_lock(request.group_id).await;
        let _guard = operation.lock().await;
        let existing = self.inner.store.find(request.group_id).await;
        ensure_role(existing.as_ref(), DeviceRole::Relay)?;
        let device_id = existing
            .as_ref()
            .map(|group| group.device_id)
            .or(request.device_id)
            .unwrap_or_else(DeviceId::new);
        let group = PersistedGroup {
            device_id,
            group_id: request.group_id,
            group_name: request.group_name,
            role: DeviceRole::Relay,
            bind_addr: Some(request.bind_addr.to_string()),
            relay_addr: None,
            local_ip: None,
            status: GroupAvailability::Unreachable,
            last_error: None,
        };
        self.establish_and_commit(group, existing.is_some()).await
    }

    pub(crate) async fn join_leaf(&self, request: JoinLeafRequest) -> Result<StartedGroup, String> {
        let operation = self.operation_lock(request.group_id).await;
        let _guard = operation.lock().await;
        let existing = self.inner.store.find(request.group_id).await;
        ensure_role(existing.as_ref(), DeviceRole::Leaf)?;
        let device_id = existing
            .as_ref()
            .map(|group| group.device_id)
            .or(request.device_id)
            .unwrap_or_else(DeviceId::new);
        let group = PersistedGroup {
            device_id,
            group_id: request.group_id,
            group_name: request.group_name,
            role: DeviceRole::Leaf,
            bind_addr: None,
            relay_addr: Some(request.relay_addr.to_string()),
            local_ip: request.local_ip.map(|ip| ip.to_string()),
            status: GroupAvailability::Unreachable,
            last_error: None,
        };
        self.establish_and_commit(group, existing.is_some()).await
    }

    pub(crate) async fn retry(&self, group_id: GroupId) -> Result<StartedGroup, String> {
        let operation = self.operation_lock(group_id).await;
        let _guard = operation.lock().await;
        let group = self
            .inner
            .store
            .find(group_id)
            .await
            .ok_or_else(|| "saved group not found".to_string())?;
        if group.status == GroupAvailability::Connected
            && let Some(runtime) = self.runtime_if_present(group_id).await
        {
            return Ok(StartedGroup {
                snapshot: GroupSnapshot::from(&group),
                neighbor_id: direct_neighbor_id(&runtime).await,
            });
        }
        self.establish_and_commit(group, true).await
    }

    pub(crate) async fn restore_all(&self) {
        for group in self.inner.store.list().await {
            let _ = self.retry(group.group_id).await;
        }
    }

    pub(crate) async fn start_recovery(&self, interval: Duration) {
        let mut task = self.inner.recovery_task.lock().await;
        if task.is_some() {
            return;
        }
        let groups = self.clone();
        let mut shutdown = self.inner.shutdown.subscribe();
        *task = Some(tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {}
                    _ = shutdown.recv() => return,
                }
                for group in groups.inner.store.list().await {
                    if group.status == GroupAvailability::Unreachable
                        && groups.runtime_if_present(group.group_id).await.is_none()
                    {
                        let _ = groups.retry(group.group_id).await;
                    }
                }
            }
        }));
    }

    pub(crate) async fn list(&self) -> Vec<GroupSnapshot> {
        self.inner
            .store
            .list()
            .await
            .iter()
            .map(GroupSnapshot::from)
            .collect()
    }

    pub(crate) async fn snapshot(&self, group_id: GroupId) -> Result<GroupSnapshot, String> {
        self.inner
            .store
            .find(group_id)
            .await
            .as_ref()
            .map(GroupSnapshot::from)
            .ok_or_else(|| "saved group not found".to_string())
    }

    pub(crate) async fn runtime(&self, group_id: GroupId) -> Result<GroupRuntime, String> {
        if let Some(runtime) = self.runtime_if_present(group_id).await {
            return Ok(runtime);
        }
        if self.inner.store.find(group_id).await.is_none() {
            return Err("saved group not found".to_string());
        }
        let error = "saved group is currently unreachable".to_string();
        let _ = self
            .inner
            .store
            .set_status(
                group_id,
                GroupAvailability::Unreachable,
                Some(error.clone()),
            )
            .await;
        self.groups_changed();
        Err(error)
    }

    pub(crate) async fn remove(&self, group_id: GroupId) -> Result<(), String> {
        let operation = self.operation_lock(group_id).await;
        let _guard = operation.lock().await;
        self.inner.store.remove(group_id).await?;
        self.uninstall_runtime(group_id).await;
        self.groups_changed();
        Ok(())
    }

    #[cfg(test)]
    async fn shutdown(&self) {
        let _ = self.inner.shutdown.send(());
        if let Some(task) = self.inner.recovery_task.lock().await.take() {
            task.abort();
        }
        let tasks = self
            .inner
            .event_tasks
            .lock()
            .await
            .drain()
            .map(|(_, task)| task)
            .collect::<Vec<_>>();
        for task in tasks {
            task.abort();
        }
        let runtimes = self
            .inner
            .runtimes
            .lock()
            .await
            .drain()
            .map(|(_, entry)| entry.runtime)
            .collect::<Vec<_>>();
        for runtime in runtimes {
            runtime.session.destroy().await;
        }
    }

    async fn establish_and_commit(
        &self,
        mut group: PersistedGroup,
        existed: bool,
    ) -> Result<StartedGroup, String> {
        let established = match self.inner.session_factory.establish(&mut group).await {
            Ok(established) => established,
            Err(error) => {
                if existed {
                    self.inner
                        .store
                        .set_status(
                            group.group_id,
                            GroupAvailability::Unreachable,
                            Some(error.clone()),
                        )
                        .await?;
                    self.groups_changed();
                }
                return Err(error);
            }
        };
        group.status = GroupAvailability::Connected;
        group.last_error = None;
        if let Err(error) = self.inner.store.upsert(group.clone()).await {
            established.session.destroy().await;
            return Err(error);
        }
        let runtime = GroupRuntime {
            group_id: group.group_id,
            session: established.session,
        };
        self.install_runtime(runtime).await;
        self.groups_changed();
        Ok(StartedGroup {
            snapshot: GroupSnapshot::from(&group),
            neighbor_id: established.neighbor_id,
        })
    }

    async fn install_runtime(&self, runtime: GroupRuntime) {
        let group_id = runtime.group_id;
        if let Some(task) = self.inner.event_tasks.lock().await.remove(&group_id) {
            task.abort();
        }
        let generation = self.inner.next_generation.fetch_add(1, Ordering::Relaxed);
        let old = self.inner.runtimes.lock().await.insert(
            group_id,
            RuntimeEntry {
                runtime: runtime.clone(),
                generation,
            },
        );
        if let Some(old) = old {
            old.runtime.session.destroy().await;
        }
        let groups = self.clone();
        let task_runtime = runtime.clone();
        let task = tokio::spawn(async move {
            groups.pump_session_events(task_runtime, generation).await;
        });
        self.inner.event_tasks.lock().await.insert(group_id, task);
    }

    async fn uninstall_runtime(&self, group_id: GroupId) {
        if let Some(task) = self.inner.event_tasks.lock().await.remove(&group_id) {
            task.abort();
        }
        if let Some(entry) = self.inner.runtimes.lock().await.remove(&group_id) {
            entry.runtime.session.destroy().await;
        }
    }

    async fn pump_session_events(&self, runtime: GroupRuntime, generation: u64) {
        let mut events = runtime.session.subscribe();
        loop {
            match events.recv().await {
                Ok(event) => {
                    if !self.is_current(runtime.group_id, generation).await {
                        return;
                    }
                    self.sync_leaf_availability(&runtime, &event).await;
                    let _ = self.inner.events.send(GroupEvent::Session {
                        runtime: runtime.clone(),
                        event: Arc::new(event),
                    });
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let _ = self.inner.events.send(GroupEvent::ResyncRequired);
                }
                Err(broadcast::error::RecvError::Closed) => {
                    self.handle_event_stream_closed(runtime.group_id, generation)
                        .await;
                    return;
                }
            }
        }
    }

    async fn sync_leaf_availability(&self, runtime: &GroupRuntime, event: &SessionEvent) {
        if runtime.session.role() != DeviceRole::Leaf {
            return;
        }
        let (availability, error) = match event {
            SessionEvent::NeighborOnline { .. } => (GroupAvailability::Connected, None),
            SessionEvent::NeighborOffline { .. } => (
                GroupAvailability::Unreachable,
                Some("与 Relay 的连接已断开".to_string()),
            ),
            SessionEvent::MessageReceived { .. } => return,
        };
        if self
            .inner
            .store
            .set_status(runtime.group_id, availability, error)
            .await
            .is_ok()
        {
            self.groups_changed();
        }
    }

    async fn handle_event_stream_closed(&self, group_id: GroupId, generation: u64) {
        let operation = self.operation_lock(group_id).await;
        let _guard = operation.lock().await;
        if !self.is_current(group_id, generation).await {
            return;
        }
        self.inner.event_tasks.lock().await.remove(&group_id);
        if let Some(entry) = self.inner.runtimes.lock().await.remove(&group_id) {
            entry.runtime.session.destroy().await;
        }
        let _ = self
            .inner
            .store
            .set_status(
                group_id,
                GroupAvailability::Unreachable,
                Some("session event stream closed".to_string()),
            )
            .await;
        self.groups_changed();
    }

    async fn is_current(&self, group_id: GroupId, generation: u64) -> bool {
        self.inner
            .runtimes
            .lock()
            .await
            .get(&group_id)
            .is_some_and(|entry| entry.generation == generation)
    }

    async fn runtime_if_present(&self, group_id: GroupId) -> Option<GroupRuntime> {
        self.inner
            .runtimes
            .lock()
            .await
            .get(&group_id)
            .map(|entry| entry.runtime.clone())
    }

    async fn operation_lock(&self, group_id: GroupId) -> Arc<Mutex<()>> {
        self.inner
            .operation_locks
            .lock()
            .await
            .entry(group_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn groups_changed(&self) {
        let _ = self.inner.events.send(GroupEvent::GroupsChanged);
    }
}

fn ensure_role(group: Option<&PersistedGroup>, requested: DeviceRole) -> Result<(), String> {
    if let Some(group) = group
        && group.role != requested
    {
        return Err(format!(
            "saved group role is {:?}; remove it before using role {:?}",
            group.role, requested
        ));
    }
    Ok(())
}

async fn direct_neighbor_id(runtime: &GroupRuntime) -> Option<NeighborId> {
    if runtime.session.role() != DeviceRole::Leaf {
        return None;
    }
    runtime
        .session
        .neighbors()
        .await
        .first()
        .map(|item| item.neighbor_id)
}

async fn establish_relay(group: &mut PersistedGroup) -> Result<EstablishedSession, String> {
    let bind_addr = group
        .bind_addr
        .as_deref()
        .ok_or_else(|| "saved relay is missing its bind address".to_string())?
        .parse::<SocketAddr>()
        .map_err(|err| format!("invalid saved relay bind address: {err}"))?;
    let (session, local_addr) = Session::create_group(group.device_id, group.group_id, bind_addr)
        .await
        .map_err(err_string)?;
    let mut started = false;
    let mut last_error = None;
    for (announce_bind, tcp_addr) in announcement_targets(local_addr) {
        match session
            .start_relay_announcement(
                announce_bind,
                SocketAddr::from(([255, 255, 255, 255], DISCOVERY_PORT)),
                tcp_addr,
                group.group_name.clone(),
                Duration::from_secs(2),
            )
            .await
        {
            Ok(_) => started = true,
            Err(error) => last_error = Some(err_string(error)),
        }
    }
    if !started {
        session.destroy().await;
        return Err(last_error.unwrap_or_else(|| "failed to start relay announcement".to_string()));
    }
    group.bind_addr = Some(local_addr.to_string());
    Ok(EstablishedSession {
        session,
        neighbor_id: None,
    })
}

async fn establish_leaf(group: &PersistedGroup) -> Result<EstablishedSession, String> {
    let relay_addr = group
        .relay_addr
        .as_deref()
        .ok_or_else(|| "saved group is missing the relay address".to_string())?
        .parse::<SocketAddr>()
        .map_err(|err| format!("invalid saved relay address: {err}"))?;
    let local_ip = parse_optional_ip(group.local_ip.clone())?;
    let (session, neighbor_id) =
        Session::join_group(group.device_id, group.group_id, relay_addr, local_ip)
            .await
            .map_err(err_string)?;
    Ok(EstablishedSession {
        session,
        neighbor_id: Some(neighbor_id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    struct ScriptedSessionFactory {
        calls: AtomicUsize,
        fail: AtomicBool,
        delay: Duration,
    }

    impl ScriptedSessionFactory {
        fn succeeding() -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                fail: AtomicBool::new(false),
                delay: Duration::ZERO,
            })
        }
    }

    impl SessionFactory for ScriptedSessionFactory {
        fn establish<'a>(&'a self, group: &'a mut PersistedGroup) -> EstablishFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                if !self.delay.is_zero() {
                    tokio::time::sleep(self.delay).await;
                }
                if self.fail.load(Ordering::SeqCst) {
                    return Err("scripted connection failure".to_string());
                }
                if group.role == DeviceRole::Relay {
                    group.bind_addr = Some("127.0.0.1:37020".to_string());
                }
                Ok(EstablishedSession {
                    session: Session::new(group.device_id, group.group_id, group.role),
                    neighbor_id: (group.role == DeviceRole::Leaf).then(NeighborId::new),
                })
            })
        }
    }

    fn test_groups(factory: Arc<dyn SessionFactory>) -> (Groups, PathBuf) {
        let directory =
            std::env::temp_dir().join(format!("lan-mesh-groups-{}", uuid::Uuid::new_v4()));
        let groups = Groups::with_factory(GroupStore::load(directory.join("groups.json")), factory);
        (groups, directory)
    }

    fn leaf_request(group_id: GroupId, device_id: Option<DeviceId>) -> JoinLeafRequest {
        JoinLeafRequest {
            device_id,
            group_id,
            group_name: "测试群组".to_string(),
            relay_addr: "127.0.0.1:9000".parse().unwrap(),
            local_ip: None,
        }
    }

    #[tokio::test]
    async fn failed_first_join_does_not_create_a_saved_group() {
        let factory = ScriptedSessionFactory::succeeding();
        factory.fail.store(true, Ordering::SeqCst);
        let (groups, directory) = test_groups(factory);
        let group_id = GroupId::new();
        assert!(
            groups
                .join_leaf(leaf_request(group_id, None))
                .await
                .is_err()
        );
        assert!(groups.list().await.is_empty());
        groups.shutdown().await;
        let _ = tokio::fs::remove_dir_all(directory).await;
    }

    #[tokio::test]
    async fn rejoining_preserves_saved_identity_and_rejects_a_role_change() {
        let factory = ScriptedSessionFactory::succeeding();
        let (groups, directory) = test_groups(factory);
        let group_id = GroupId::new();
        let saved_device_id = DeviceId::new();
        groups
            .join_leaf(leaf_request(group_id, Some(saved_device_id)))
            .await
            .unwrap();
        let rejoined = groups
            .join_leaf(leaf_request(group_id, Some(DeviceId::new())))
            .await
            .unwrap();
        assert_eq!(rejoined.snapshot.device_id, saved_device_id);
        assert!(
            groups
                .create_relay(CreateRelayRequest {
                    device_id: None,
                    group_id,
                    group_name: "角色冲突".to_string(),
                    bind_addr: "127.0.0.1:0".parse().unwrap(),
                })
                .await
                .is_err()
        );
        groups.shutdown().await;
        let _ = tokio::fs::remove_dir_all(directory).await;
    }

    #[tokio::test]
    async fn failed_restore_keeps_the_group_and_marks_it_unreachable() {
        let factory = ScriptedSessionFactory::succeeding();
        factory.fail.store(true, Ordering::SeqCst);
        let (groups, directory) = test_groups(factory);
        let group = PersistedGroup {
            device_id: DeviceId::new(),
            group_id: GroupId::new(),
            group_name: "不可达群组".to_string(),
            role: DeviceRole::Leaf,
            bind_addr: None,
            relay_addr: Some("127.0.0.1:9000".to_string()),
            local_ip: None,
            status: GroupAvailability::Connected,
            last_error: None,
        };
        groups.inner.store.upsert(group.clone()).await.unwrap();
        assert!(groups.retry(group.group_id).await.is_err());
        let saved = groups.snapshot(group.group_id).await.unwrap();
        assert_eq!(saved.device_id, group.device_id);
        assert_eq!(saved.role, DeviceRole::Leaf);
        assert_eq!(saved.availability, GroupAvailability::Unreachable);
        groups.shutdown().await;
        let _ = tokio::fs::remove_dir_all(directory).await;
    }

    #[tokio::test]
    async fn concurrent_retries_establish_one_runtime() {
        let factory = Arc::new(ScriptedSessionFactory {
            calls: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
            delay: Duration::from_millis(25),
        });
        let (groups, directory) = test_groups(factory.clone());
        let group = PersistedGroup {
            device_id: DeviceId::new(),
            group_id: GroupId::new(),
            group_name: "并发恢复".to_string(),
            role: DeviceRole::Leaf,
            bind_addr: None,
            relay_addr: Some("127.0.0.1:9000".to_string()),
            local_ip: None,
            status: GroupAvailability::Unreachable,
            last_error: Some("offline".to_string()),
        };
        groups.inner.store.upsert(group.clone()).await.unwrap();
        let (first, second) =
            tokio::join!(groups.retry(group.group_id), groups.retry(group.group_id));
        assert!(first.is_ok());
        assert!(second.is_ok());
        assert_eq!(factory.calls.load(Ordering::SeqCst), 1);
        groups.shutdown().await;
        let _ = tokio::fs::remove_dir_all(directory).await;
    }

    #[tokio::test]
    async fn missing_runtime_marks_a_saved_group_unreachable() {
        let factory = ScriptedSessionFactory::succeeding();
        let (groups, directory) = test_groups(factory);
        let group = PersistedGroup {
            device_id: DeviceId::new(),
            group_id: GroupId::new(),
            group_name: "运行时缺失".to_string(),
            role: DeviceRole::Leaf,
            bind_addr: None,
            relay_addr: Some("127.0.0.1:9000".to_string()),
            local_ip: None,
            status: GroupAvailability::Connected,
            last_error: None,
        };
        groups.inner.store.upsert(group.clone()).await.unwrap();

        assert!(groups.runtime(group.group_id).await.is_err());
        assert_eq!(
            groups.snapshot(group.group_id).await.unwrap().availability,
            GroupAvailability::Unreachable
        );

        groups.shutdown().await;
        let _ = tokio::fs::remove_dir_all(directory).await;
    }

    #[tokio::test]
    async fn removing_one_group_leaves_other_runtimes_available() {
        let factory = ScriptedSessionFactory::succeeding();
        let (groups, directory) = test_groups(factory);
        let first = GroupId::new();
        let second = GroupId::new();
        groups.join_leaf(leaf_request(first, None)).await.unwrap();
        groups.join_leaf(leaf_request(second, None)).await.unwrap();

        groups.remove(first).await.unwrap();

        assert!(groups.runtime(first).await.is_err());
        assert!(groups.runtime(second).await.is_ok());
        assert_eq!(groups.list().await.len(), 1);

        groups.shutdown().await;
        let _ = tokio::fs::remove_dir_all(directory).await;
    }

    #[tokio::test]
    async fn persistence_failure_rolls_back_a_new_runtime_and_record() {
        let directory =
            std::env::temp_dir().join(format!("lan-mesh-groups-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let blocked_parent = directory.join("not-a-directory");
        tokio::fs::write(&blocked_parent, b"file").await.unwrap();
        let factory = ScriptedSessionFactory::succeeding();
        let groups = Groups::with_factory(
            GroupStore::load(blocked_parent.join("groups.json")),
            factory,
        );
        let group_id = GroupId::new();

        assert!(
            groups
                .join_leaf(leaf_request(group_id, None))
                .await
                .is_err()
        );
        assert!(groups.list().await.is_empty());
        assert!(groups.runtime_if_present(group_id).await.is_none());

        groups.shutdown().await;
        let _ = tokio::fs::remove_dir_all(directory).await;
    }

    #[tokio::test]
    async fn lifecycle_outcomes_cross_the_same_event_seam_as_the_tauri_adapter() {
        let factory = ScriptedSessionFactory::succeeding();
        let (groups, directory) = test_groups(factory);
        let mut events = groups.subscribe();

        groups
            .join_leaf(leaf_request(GroupId::new(), None))
            .await
            .unwrap();

        assert!(matches!(
            events.recv().await.unwrap(),
            GroupEvent::GroupsChanged
        ));

        groups.shutdown().await;
        let _ = tokio::fs::remove_dir_all(directory).await;
    }
}

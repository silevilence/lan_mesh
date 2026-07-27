use crate::{
    DISCOVERY_PORT,
    ids::{err_string, parse_optional_ip},
    network::announcement_targets,
    persistence::{GroupAvailability, PersistedGroup},
    state::{AppState, ClientSession, install_session},
};
use lan_mesh_core::{DeviceRole, Session};
use std::{net::SocketAddr, time::Duration};
use tauri::{AppHandle, Emitter, Manager};

pub(crate) const RECONNECT_INTERVAL: Duration = Duration::from_secs(30);

pub(crate) async fn restore_saved_groups(app: AppHandle) {
    let state = app.state::<AppState>();
    for group in state.groups.list().await {
        let _ = establish_saved_group(&app, &state, group, false).await;
    }

    tokio::spawn(reconnect_unreachable_groups(app));
}

pub(crate) async fn reconnect_unreachable_groups(app: AppHandle) {
    loop {
        tokio::time::sleep(RECONNECT_INTERVAL).await;
        let state = app.state::<AppState>();
        for group in state.groups.list().await {
            if group.status == GroupAvailability::Unreachable {
                // Leaf sessions keep their own reconnect loop.  Their
                // NeighborOnline event will restore the persisted status;
                // only records without a live session need a fresh attempt.
                if state.clients.lock().await.contains_key(&group.group_id) {
                    continue;
                }
                let _ = establish_saved_group(&app, &state, group, false).await;
            }
        }
    }
}

pub(crate) async fn establish_saved_group(
    app: &AppHandle,
    state: &AppState,
    mut group: PersistedGroup,
    activate: bool,
) -> Result<ClientSession, String> {
    let result = match group.role {
        DeviceRole::Relay => establish_relay(&mut group).await,
        DeviceRole::Leaf => establish_leaf(&group).await,
    };

    let client = match result {
        Ok(client) => client,
        Err(err) => {
            state
                .groups
                .set_status(
                    group.group_id,
                    GroupAvailability::Unreachable,
                    Some(err.clone()),
                )
                .await?;
            let _ = app.emit("mesh://groups-changed", ());
            return Err(err);
        }
    };

    group.status = GroupAvailability::Connected;
    group.last_error = None;
    state.groups.upsert(group).await?;
    install_session(app, state, client.clone(), activate).await;
    let _ = app.emit("mesh://groups-changed", ());
    Ok(client)
}

async fn establish_relay(group: &mut PersistedGroup) -> Result<ClientSession, String> {
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
            Err(err) => last_error = Some(err_string(err)),
        }
    }
    if !started {
        session.destroy().await;
        return Err(last_error.unwrap_or_else(|| "failed to start relay announcement".to_string()));
    }
    group.bind_addr = Some(local_addr.to_string());
    Ok(ClientSession {
        session,
        group_id: group.group_id,
    })
}

async fn establish_leaf(group: &PersistedGroup) -> Result<ClientSession, String> {
    let relay_addr = group
        .relay_addr
        .as_deref()
        .ok_or_else(|| "saved group is missing the relay address".to_string())?
        .parse::<SocketAddr>()
        .map_err(|err| format!("invalid saved relay address: {err}"))?;
    let local_ip = parse_optional_ip(group.local_ip.clone())?;
    let (session, _) = Session::join_group(group.device_id, group.group_id, relay_addr, local_ip)
        .await
        .map_err(err_string)?;
    Ok(ClientSession {
        session,
        group_id: group.group_id,
    })
}

use super::*;

pub(super) async fn connect_stream(
    peer_addr: SocketAddr,
    local_ip: Option<IpAddr>,
) -> Result<TcpStream, io::Error> {
    let Some(local_ip) = local_ip else {
        return TcpStream::connect(peer_addr).await;
    };

    let socket = match local_ip {
        IpAddr::V4(_) => TcpSocket::new_v4()?,
        IpAddr::V6(_) => TcpSocket::new_v6()?,
    };
    socket.bind(SocketAddr::new(local_ip, 0))?;
    socket.connect(peer_addr).await
}

pub(super) async fn read_loop(
    session: Arc<SessionInner>,
    neighbor_id: NeighborId,
    mut reader: OwnedReadHalf,
    last_active: Arc<Mutex<Instant>>,
) {
    loop {
        match read_message_frame(&mut reader).await {
            Ok(message) => {
                *last_active.lock().await = Instant::now();
                let header = message_header(&message);
                if header.group_id != session.group_id {
                    continue;
                }
                if !session.mark_seen(header.message_id).await {
                    continue;
                }
                let _ = session.handle_received_message(neighbor_id, &message).await;
                let _ = session.events.send(SessionEvent::MessageReceived {
                    neighbor_id,
                    message,
                });
            }
            Err(_) => {
                session.remove_neighbor(neighbor_id).await;
                return;
            }
        }
    }
}

pub(super) async fn write_loop(
    mut writer: OwnedWriteHalf,
    mut receiver: mpsc::UnboundedReceiver<Outbound>,
) {
    while let Some(outbound) = receiver.recv().await {
        let result = write_message_frame(&mut writer, &outbound.message)
            .await
            .map_err(NetworkError::from);
        let failed = result.is_err();
        if let Some(written) = outbound.written {
            let _ = written.send(result);
        }
        if failed {
            return;
        }
    }
}

pub(super) async fn heartbeat_loop(session: Arc<SessionInner>, neighbor_id: NeighborId) {
    let mut interval = time::interval(session.config.heartbeat_interval);
    let mut shutdown = session.shutdown.subscribe();
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = shutdown.wait_for(|closed| *closed) => return,
        }
        if session
            .send_message(neighbor_id, heartbeat_message(&session))
            .await
            .is_err()
        {
            session.remove_neighbor(neighbor_id).await;
            return;
        }
    }
}

pub(super) async fn timeout_loop(session: Arc<SessionInner>) {
    let mut interval = time::interval(session.config.timeout_check_interval);
    let mut shutdown = session.shutdown.subscribe();
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = shutdown.wait_for(|closed| *closed) => return,
        }
        let mut timed_out = Vec::new();
        {
            let neighbors = session.neighbors.lock().await;
            for (neighbor_id, neighbor) in neighbors.iter() {
                if neighbor.last_active.lock().await.elapsed() > session.config.heartbeat_timeout {
                    timed_out.push(*neighbor_id);
                }
            }
        }
        for neighbor_id in timed_out {
            session.remove_neighbor(neighbor_id).await;
        }
    }
}

pub(super) async fn dedup_cleanup_loop(session: Arc<SessionInner>) {
    let mut interval = time::interval(MESSAGE_ID_CLEANUP_INTERVAL);
    let mut shutdown = session.shutdown.subscribe();
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = shutdown.wait_for(|closed| *closed) => return,
        }
        session.cleanup_seen_messages().await;
    }
}

pub(super) async fn leaf_reconnect_loop(session: Arc<SessionInner>) {
    let mut delay = session.config.reconnect_initial_delay;
    let mut shutdown = session.shutdown.subscribe();
    loop {
        tokio::select! {
            _ = time::sleep(delay) => {}
            _ = session.reconnect_now.notified() => {delay = session.config.reconnect_initial_delay;}
            _ = shutdown.wait_for(|closed| *closed) => return,
        }
        if !session.neighbors.lock().await.is_empty() {
            delay = session.config.reconnect_initial_delay;
            continue;
        }

        let target = session.leaf_reconnect_target.lock().await.clone();
        let Some(target) = target else {
            delay = session.config.reconnect_initial_delay;
            continue;
        };

        let connect = async {
            if target.verify_group {
                let (stream, group) = identified_stream(target.peer_addr).await?;
                if group != session.group_id {
                    return Err(NetworkError::Io(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "relay group changed",
                    )));
                }
                Ok(stream)
            } else {
                Ok(connect_stream(target.peer_addr, target.local_ip).await?)
            }
        };
        let result = tokio::select! {
            result = connect => result,
            _ = shutdown.wait_for(|closed| *closed) => return,
            _ = session.reconnect_now.notified() => { session.reconnect_now.notify_one(); continue; }
        };
        if let Ok(stream) = result {
            let _ = Box::pin(session.register_neighbor(stream)).await;
            delay = session.config.reconnect_initial_delay;
            continue;
        }

        delay = (delay * 2).min(session.config.reconnect_max_delay);
    }
}

pub(super) async fn identified_stream(
    peer: SocketAddr,
) -> Result<(TcpStream, GroupId), NetworkError> {
    time::timeout(Duration::from_secs(5), async {
        let mut stream = TcpStream::connect(peer).await?;
        let first = read_message_frame(&mut stream).await?;
        Ok((stream, message_header(&first).group_id))
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "relay did not respond"))?
}

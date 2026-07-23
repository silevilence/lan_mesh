use super::*;
use crate::*;
use serde_json::json;
use tokio::io::AsyncReadExt;
use tokio::time::timeout;

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

fn fast_config() -> ConnectionConfig {
    ConnectionConfig {
        heartbeat_interval: Duration::from_millis(10),
        heartbeat_timeout: Duration::from_millis(60),
        timeout_check_interval: Duration::from_millis(5),
        reconnect_initial_delay: Duration::from_millis(10),
        reconnect_max_delay: Duration::from_millis(20),
    }
}

async fn recv_matching(
    events: &mut broadcast::Receiver<SessionEvent>,
    mut matches: impl FnMut(&SessionEvent) -> bool,
) -> SessionEvent {
    timeout(Duration::from_secs(2), async {
        loop {
            let event = events.recv().await.unwrap();
            if matches(&event) {
                return event;
            }
        }
    })
    .await
    .unwrap()
}

async fn wait_until(mut done: impl AsyncFnMut() -> bool) {
    timeout(Duration::from_secs(2), async {
        loop {
            if done().await {
                return;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

fn text_message(
    message_id: MessageId,
    group_id: GroupId,
    source_device_id: DeviceId,
    target: MessageTarget,
    content: &str,
) -> Message {
    Message::Text {
        header: MessageHeader {
            message_id,
            group_id,
            source_device_id,
            target,
            ttl: 8,
            hop_count: 0,
            timestamp_ms: now_timestamp_ms(),
        },
        payload: TextPayload {
            content: content.to_string(),
            sender_nickname: None,
        },
    }
}

#[test]
fn text_message_uses_internal_type_tag() {
    let message = Message::Text {
        header: header(),
        payload: TextPayload {
            content: "hello".to_string(),
            sender_nickname: None,
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
            file_name: "test.bin".to_string(),
            sender_nickname: None,
            chunk_index: 0,
            chunk_count: 1,
            total_size: 5,
            sha256: "unused".to_string(),
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
async fn file_chunks_assemble_out_of_order_and_verify_hash() {
    let dir = std::env::temp_dir().join(format!("lan-mesh-{}", FileId::new().0));
    tokio::fs::create_dir(&dir).await.unwrap();
    let source_path = dir.join("source.bin");
    let assembled_path = dir.join("assembled.bin");
    let data = vec![7; FILE_CHUNK_SIZE + 3];
    tokio::fs::write(&source_path, &data).await.unwrap();
    let file_id = FileId::new();
    let group_id = GroupId::new();
    let source_device_id = DeviceId::new();
    let mut reader = FileChunkReader::open(
        &source_path,
        file_id,
        group_id,
        source_device_id,
        MessageTarget::Broadcast,
        8,
    )
    .await
    .unwrap();
    let mut messages = Vec::new();
    while let Some(message) = reader.next_message().await.unwrap() {
        messages.push(message);
    }
    let first_payload = match &messages[0] {
        Message::FileChunk { payload, .. } => payload,
        _ => unreachable!(),
    };
    let mut assembler = FileAssembler::create(
        &assembled_path,
        file_id,
        first_payload.chunk_count,
        first_payload.total_size,
        first_payload.sha256.clone(),
    )
    .await
    .unwrap();

    let mut status = None;
    for message in messages.iter().rev() {
        let Message::FileChunk { payload, .. } = message else {
            unreachable!()
        };
        status = Some(assembler.push_chunk(payload).await.unwrap());
    }

    assert_eq!(
        status.unwrap(),
        FileAssemblyStatus::Complete {
            path: assembled_path.clone()
        }
    );
    assert_eq!(tokio::fs::read(&assembled_path).await.unwrap(), data);
    let _ = tokio::fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn resume_request_resends_only_missing_chunks() {
    let dir = std::env::temp_dir().join(format!("lan-mesh-{}", FileId::new().0));
    tokio::fs::create_dir(&dir).await.unwrap();
    let source_path = dir.join("source.bin");
    tokio::fs::write(&source_path, vec![9; FILE_CHUNK_SIZE * 2 + 1])
        .await
        .unwrap();
    let file_id = FileId::new();
    let group_id = GroupId::new();
    let source_device_id = DeviceId::new();
    let request = FileResumeRequestPayload {
        file_id,
        missing_chunks: vec![2, 0],
    };

    let resent = resend_file_chunks(
        &source_path,
        &request,
        group_id,
        source_device_id,
        MessageTarget::Broadcast,
        8,
    )
    .await
    .unwrap();

    let indexes: Vec<_> = resent
        .iter()
        .map(|message| match message {
            Message::FileChunk { payload, .. } => payload.chunk_index,
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(indexes, vec![2, 0]);
    let _ = tokio::fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn assembler_reports_hash_mismatch_after_all_chunks() {
    let dir = std::env::temp_dir().join(format!("lan-mesh-{}", FileId::new().0));
    tokio::fs::create_dir(&dir).await.unwrap();
    let assembled_path = dir.join("assembled.bin");
    let file_id = FileId::new();
    let mut assembler = FileAssembler::create(&assembled_path, file_id, 1, 3, "00")
        .await
        .unwrap();
    let chunk = FileChunkPayload {
        file_id,
        file_name: "assembled.bin".to_string(),
        sender_nickname: None,
        chunk_index: 0,
        chunk_count: 1,
        total_size: 3,
        sha256: "00".to_string(),
        data: vec![1, 2, 3],
    };

    let status = assembler.push_chunk(&chunk).await.unwrap();

    assert!(matches!(status, FileAssemblyStatus::HashMismatch { .. }));
    let _ = tokio::fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn message_frame_uses_big_endian_length_prefix() {
    let message = Message::Text {
        header: header(),
        payload: TextPayload {
            content: "hello".to_string(),
            sender_nickname: None,
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
            sender_nickname: None,
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

#[tokio::test]
async fn relay_listens_and_connector_registers_neighbors() {
    let group_id = GroupId::new();
    let relay = Session::with_config(DeviceId::new(), group_id, DeviceRole::Relay, fast_config());
    let leaf = Session::with_config(DeviceId::new(), group_id, DeviceRole::Leaf, fast_config());
    let mut relay_events = relay.subscribe();
    let bind_addr = "127.0.0.1:0".parse().unwrap();
    let local_ip = "127.0.0.1".parse().unwrap();

    let addr = relay.listen(bind_addr).await.unwrap();
    let leaf_neighbor_id = leaf.connect(addr, Some(local_ip)).await.unwrap();

    recv_matching(&mut relay_events, |event| {
        matches!(event, SessionEvent::NeighborOnline { .. })
    })
    .await;

    assert_eq!(leaf.neighbors().await[0].neighbor_id, leaf_neighbor_id);
    assert_eq!(relay.neighbors().await.len(), 1);
}

#[tokio::test]
async fn heartbeat_timeout_removes_silent_neighbor() {
    let relay = Session::with_config(
        DeviceId::new(),
        GroupId::new(),
        DeviceRole::Relay,
        fast_config(),
    );
    let mut events = relay.subscribe();
    let addr = relay.listen("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let _silent_client = TcpStream::connect(addr).await.unwrap();

    let online = recv_matching(&mut events, |event| {
        matches!(event, SessionEvent::NeighborOnline { .. })
    })
    .await;
    let SessionEvent::NeighborOnline { neighbor_id, .. } = online else {
        unreachable!()
    };

    let offline = recv_matching(&mut events, |event| {
        matches!(
            event,
            SessionEvent::NeighborOffline {
                neighbor_id: id,
                ..
            } if *id == neighbor_id
        )
    })
    .await;

    assert!(matches!(offline, SessionEvent::NeighborOffline { .. }));
    assert!(relay.neighbors().await.is_empty());
}

#[tokio::test]
async fn leaf_reconnects_after_disconnect() {
    let mut config = fast_config();
    config.heartbeat_interval = Duration::from_secs(1);
    config.heartbeat_timeout = Duration::from_secs(5);
    let leaf = Session::with_config(DeviceId::new(), GroupId::new(), DeviceRole::Leaf, config);
    let mut events = leaf.subscribe();
    let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = server.local_addr().unwrap();

    leaf.connect(addr, None).await.unwrap();
    let (first, _) = server.accept().await.unwrap();
    recv_matching(&mut events, |event| {
        matches!(event, SessionEvent::NeighborOnline { .. })
    })
    .await;

    drop(first);
    recv_matching(&mut events, |event| {
        matches!(event, SessionEvent::NeighborOffline { .. })
    })
    .await;

    let (_second, _) = timeout(Duration::from_secs(2), server.accept())
        .await
        .unwrap()
        .unwrap();
    recv_matching(&mut events, |event| {
        matches!(event, SessionEvent::NeighborOnline { .. })
    })
    .await;

    assert_eq!(leaf.neighbors().await.len(), 1);
}

#[tokio::test]
async fn group_sessions_keep_members_and_messages_isolated() {
    let device_id = DeviceId::new();
    let group1 = GroupId::new();
    let group2 = GroupId::new();
    let relay1 = Session::with_config(device_id, group1, DeviceRole::Relay, fast_config());
    let relay2 = Session::with_config(DeviceId::new(), group2, DeviceRole::Relay, fast_config());
    let mut relay2_events = relay2.subscribe();
    let relay1_addr = relay1.listen("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let relay2_addr = relay2.listen("127.0.0.1:0".parse().unwrap()).await.unwrap();

    let leaf2 = Session::with_config(device_id, group2, DeviceRole::Leaf, fast_config());
    leaf2.connect(relay2_addr, None).await.unwrap();
    recv_matching(&mut relay2_events, |event| {
        matches!(
            event,
            SessionEvent::MessageReceived {
                message: Message::MemberChanged { payload, .. },
                ..
            } if payload.device_id == device_id && payload.change == MemberChange::Online
        )
    })
    .await;

    let wrong_group_leaf =
        Session::with_config(DeviceId::new(), group2, DeviceRole::Leaf, fast_config());
    let neighbor_id = wrong_group_leaf.connect(relay1_addr, None).await.unwrap();
    wrong_group_leaf
        .send_message(
            neighbor_id,
            wrong_group_leaf.member_changed_message(device_id, MemberChange::Offline),
        )
        .await
        .unwrap();
    time::sleep(Duration::from_millis(30)).await;

    assert!(
        relay1
            .members()
            .await
            .iter()
            .all(|m| m.device_id == device_id)
    );
    assert!(
        relay2
            .members()
            .await
            .iter()
            .any(|m| m.device_id == device_id && m.online)
    );

    leaf2.destroy().await;
    recv_matching(&mut relay2_events, |event| {
        matches!(event, SessionEvent::NeighborOffline { .. })
    })
    .await;
    assert!(
        relay2
            .members()
            .await
            .iter()
            .any(|m| m.device_id == device_id && !m.online)
    );
}

#[tokio::test]
async fn dedup_cache_expires_seen_messages() {
    let session = Session::with_config(
        DeviceId::new(),
        GroupId::new(),
        DeviceRole::Relay,
        fast_config(),
    );
    let message_id = MessageId::new();

    assert!(session.inner.mark_seen(message_id).await);
    assert!(!session.inner.mark_seen(message_id).await);
    session.inner.seen_messages.lock().await.insert(
        message_id,
        Instant::now() - MESSAGE_ID_TTL - Duration::from_secs(1),
    );

    assert!(session.inner.mark_seen(message_id).await);
}

#[tokio::test]
async fn broadcast_floods_once_and_skips_source_neighbor() {
    let group_id = GroupId::new();
    let leaf1_device = DeviceId::new();
    let leaf2_device = DeviceId::new();
    let relay = Session::with_config(DeviceId::new(), group_id, DeviceRole::Relay, fast_config());
    let leaf1 = Session::with_config(leaf1_device, group_id, DeviceRole::Leaf, fast_config());
    let leaf2 = Session::with_config(leaf2_device, group_id, DeviceRole::Leaf, fast_config());
    let relay_addr = relay.listen("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let leaf1_neighbor = leaf1.connect(relay_addr, None).await.unwrap();
    leaf2.connect(relay_addr, None).await.unwrap();
    wait_until(async || relay.neighbors().await.len() == 2).await;

    let message_id = MessageId::new();
    let message = text_message(
        message_id,
        group_id,
        leaf1_device,
        MessageTarget::Broadcast,
        "broadcast",
    );
    let mut leaf1_events = leaf1.subscribe();
    let mut leaf2_events = leaf2.subscribe();

    leaf1
        .send_message(leaf1_neighbor, message.clone())
        .await
        .unwrap();
    recv_matching(&mut leaf2_events, |event| {
        matches!(
            event,
            SessionEvent::MessageReceived {
                message: Message::Text { header, payload },
                ..
            } if header.message_id == message_id && payload.content == "broadcast"
        )
    })
    .await;
    leaf1.send_message(leaf1_neighbor, message).await.unwrap();

    assert!(
        timeout(Duration::from_millis(80), async {
            loop {
                let event = leaf2_events.recv().await.unwrap();
                if matches!(
                    event,
                    SessionEvent::MessageReceived {
                        message: Message::Text { header, .. },
                        ..
                    } if header.message_id == message_id
                ) {
                    return;
                }
            }
        })
        .await
        .is_err()
    );
    assert!(
        timeout(Duration::from_millis(80), async {
            loop {
                let event = leaf1_events.recv().await.unwrap();
                if matches!(
                    event,
                    SessionEvent::MessageReceived {
                        message: Message::Text { header, .. },
                        ..
                    } if header.message_id == message_id
                ) {
                    return;
                }
            }
        })
        .await
        .is_err()
    );
}

#[tokio::test]
async fn send_group_message_fills_header_and_floods() {
    let group_id = GroupId::new();
    let leaf1_device = DeviceId::new();
    let relay = Session::with_config(DeviceId::new(), group_id, DeviceRole::Relay, fast_config());
    let leaf1 = Session::with_config(leaf1_device, group_id, DeviceRole::Leaf, fast_config());
    let leaf2 = Session::with_config(DeviceId::new(), group_id, DeviceRole::Leaf, fast_config());
    let relay_addr = relay.listen("127.0.0.1:0".parse().unwrap()).await.unwrap();
    leaf1.connect(relay_addr, None).await.unwrap();
    leaf2.connect(relay_addr, None).await.unwrap();
    wait_until(async || relay.neighbors().await.len() == 2).await;
    let mut leaf2_events = leaf2.subscribe();

    let message_id = leaf1.send_group_message("group").await.unwrap();

    recv_matching(&mut leaf2_events, |event| {
        matches!(
            event,
            SessionEvent::MessageReceived {
                message: Message::Text { header, payload },
                ..
            } if header.message_id == message_id
                && header.group_id == group_id
                && header.source_device_id == leaf1_device
                && header.target == MessageTarget::Broadcast
                && payload.content == "group"
        )
    })
    .await;
}

#[tokio::test]
async fn relay_forwards_file_chunks_without_assembly() {
    let group_id = GroupId::new();
    let leaf1_device = DeviceId::new();
    let leaf2_device = DeviceId::new();
    let relay = Session::with_config(DeviceId::new(), group_id, DeviceRole::Relay, fast_config());
    let leaf1 = Session::with_config(leaf1_device, group_id, DeviceRole::Leaf, fast_config());
    let leaf2 = Session::with_config(leaf2_device, group_id, DeviceRole::Leaf, fast_config());
    let relay_addr = relay.listen("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let leaf1_neighbor = leaf1.connect(relay_addr, None).await.unwrap();
    leaf2.connect(relay_addr, None).await.unwrap();
    wait_until(async || relay.neighbors().await.len() == 2).await;
    let message_id = MessageId::new();
    let file_id = FileId::new();
    let message = Message::FileChunk {
        header: MessageHeader {
            message_id,
            group_id,
            source_device_id: leaf1_device,
            target: MessageTarget::Broadcast,
            ttl: 8,
            hop_count: 0,
            timestamp_ms: now_timestamp_ms(),
        },
        payload: FileChunkPayload {
            file_id,
            file_name: "relay.bin".to_string(),
            sender_nickname: None,
            chunk_index: 0,
            chunk_count: 1,
            total_size: 3,
            sha256: "unused".to_string(),
            data: vec![1, 2, 3],
        },
    };
    let mut leaf2_events = leaf2.subscribe();

    leaf1.send_message(leaf1_neighbor, message).await.unwrap();

    recv_matching(&mut leaf2_events, |event| {
        matches!(
            event,
            SessionEvent::MessageReceived {
                message: Message::FileChunk { header, payload },
                ..
            } if header.message_id == message_id && payload.file_id == file_id
        )
    })
    .await;
    assert!(
        relay
            .inner
            .seen_messages
            .lock()
            .await
            .contains_key(&message_id)
    );
}

#[tokio::test]
async fn route_discovery_enables_direct_forward_and_offline_clears_route() {
    let group_id = GroupId::new();
    let leaf_a_device = DeviceId::new();
    let relay1_device = DeviceId::new();
    let relay2_device = DeviceId::new();
    let leaf_c_device = DeviceId::new();
    let relay1 = Session::with_config(relay1_device, group_id, DeviceRole::Relay, fast_config());
    let relay2 = Session::with_config(relay2_device, group_id, DeviceRole::Relay, fast_config());
    let leaf_a = Session::with_config(leaf_a_device, group_id, DeviceRole::Leaf, fast_config());
    let leaf_c = Session::with_config(leaf_c_device, group_id, DeviceRole::Leaf, fast_config());
    let relay1_addr = relay1.listen("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let relay2_addr = relay2.listen("127.0.0.1:0".parse().unwrap()).await.unwrap();
    leaf_a.connect(relay1_addr, None).await.unwrap();
    relay1.connect(relay2_addr, None).await.unwrap();
    leaf_c.connect(relay2_addr, None).await.unwrap();
    wait_until(async || relay1.neighbors().await.len() == 2).await;
    wait_until(async || relay2.neighbors().await.len() == 2).await;

    leaf_a.discover_route(leaf_c_device, 8).await.unwrap();
    wait_until(async || {
        leaf_a
            .routes()
            .await
            .iter()
            .any(|route| route.target_device_id == leaf_c_device)
    })
    .await;

    let mut leaf_c_events = leaf_c.subscribe();
    let message_id = leaf_a
        .send_direct_message(leaf_c_device, "direct")
        .await
        .unwrap();

    recv_matching(&mut leaf_c_events, |event| {
        matches!(
            event,
            SessionEvent::MessageReceived {
                message: Message::Text { header, payload },
                ..
            } if header.message_id == message_id && payload.content == "direct"
        )
    })
    .await;

    relay1
        .announce_member_change(leaf_c_device, MemberChange::Offline)
        .await
        .unwrap();
    wait_until(async || {
        !leaf_a
            .routes()
            .await
            .iter()
            .any(|route| route.target_device_id == leaf_c_device)
    })
    .await;
}

#[tokio::test]
async fn relay_announcement_is_collected_over_udp() {
    let group_id = GroupId::new();
    let relay_device = DeviceId::new();
    let relay = Session::with_config(relay_device, group_id, DeviceRole::Relay, fast_config());
    let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let announce_addr = listener.local_addr().unwrap();
    let collecting = tokio::spawn(collect_relay_announcements(
        listener,
        Duration::from_millis(80),
    ));
    let tcp_addr = "127.0.0.1:12345".parse().unwrap();

    relay
        .start_relay_announcement(
            "127.0.0.1:0".parse().unwrap(),
            announce_addr,
            tcp_addr,
            "group",
            Duration::from_millis(10),
        )
        .await
        .unwrap();

    let relays = collecting.await.unwrap();
    assert!(relays.iter().any(|relay| {
        relay.device_id == relay_device
            && relay.group_id == group_id
            && relay.group_name == "group"
            && relay.tcp_addr == tcp_addr
    }));
}

#[tokio::test]
async fn session_lifecycle_apis_create_join_destroy() {
    let group_id = GroupId::new();
    let (relay, relay_addr) =
        Session::create_group(DeviceId::new(), group_id, "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
    let (leaf, _) = Session::join_group(DeviceId::new(), group_id, relay_addr, None)
        .await
        .unwrap();

    recv_matching(&mut relay.subscribe(), |event| {
        matches!(event, SessionEvent::NeighborOnline { .. })
    })
    .await;

    assert_eq!(relay.role(), DeviceRole::Relay);
    assert_eq!(leaf.role(), DeviceRole::Leaf);

    leaf.destroy().await;
    assert!(leaf.neighbors().await.is_empty());
    assert!(leaf.members().await.is_empty());
}

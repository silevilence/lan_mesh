use super::*;
use lan_mesh::{
    HeartbeatPayload, MessageHeader, MessageId, now_timestamp_ms, read_message_frame,
    write_message_frame,
};
use tokio::{
    net::TcpListener,
    time::{Duration, timeout},
};

#[tokio::test]
async fn leaf_joins_announced_group_and_sends_to_its_only_relay() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let group = GroupId::new();
    let relay = DeviceId::new();
    let local = DeviceId::new();
    let peer = DeviceId::new();
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    let client = Client::open(
        OpenGroup {
            group_id: group,
            device_id: local,
            address,
            nickname: "Android".into(),
        },
        Arc::new(move |v| {
            tx.send(v)?;
            Ok(())
        }),
    )
    .await
    .unwrap();
    timeout(Duration::from_secs(5), async {
        let (mut socket, _) = listener.accept().await.unwrap();
        let timestamp_ms = now_timestamp_ms();
        let heartbeat = Message::Heartbeat {
            header: MessageHeader {
                message_id: MessageId::new(),
                group_id: group,
                source_device_id: relay,
                target: MessageTarget::Broadcast,
                ttl: 1,
                hop_count: 0,
                timestamp_ms,
            },
            payload: HeartbeatPayload {
                device_id: relay,
                timestamp_ms,
            },
        };
        write_message_frame(&mut socket, &heartbeat).await.unwrap();
        while events.recv().await.unwrap()["type"] != "connection" {}
        client.send_text(Some(peer), "hello".into()).await.unwrap();
        loop {
            if let Message::Text { header, payload } =
                read_message_frame(&mut socket).await.unwrap()
            {
                assert_eq!(header.target, MessageTarget::Device { device_id: peer });
                assert_eq!(header.group_id, group);
                assert_eq!(payload.content, "hello");
                break;
            }
        }
        client.close().await;
    })
    .await
    .unwrap();
}

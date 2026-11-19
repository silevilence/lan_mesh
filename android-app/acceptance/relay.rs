//! Windows-side interoperability fixture for the Android emulator.
use core::{
    DeviceId, DeviceRole, FileAssembler, FileAssemblyStatus, FileChunkReader, FileId, GroupId,
    Message, MessageTarget, Session, SessionEvent, resend_file_chunks,
};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    error::Error,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

fn main() -> Result<()> {
    tokio::runtime::Runtime::new()?.block_on(run())
}

async fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().ok_or("expected a fresh work directory")?);
    let udp: u16 = args
        .next()
        .ok_or("expected emulator UDP redirection port")?
        .parse()?;
    tokio::fs::create_dir_all(&dir).await?;
    let a = GroupId::new();
    let b = GroupId::new();
    let (ingress, address_a) = relay(a, "Relay-Alpha").await?;
    let (peer, _) = relay(a, "Peer-Alpha").await?;
    let (beta, address_b) = relay(b, "Peer-Beta").await?;
    peer.connect(address_a, None).await?;
    for (session, addr, name) in [(&ingress, address_a, "Alpha"), (&beta, address_b, "Beta")] {
        session
            .start_relay_announcement(
                "127.0.0.1:0".parse()?,
                ([127, 0, 0, 1], udp).into(),
                ([10, 0, 2, 2], addr.port()).into(),
                name,
                Duration::from_millis(300),
            )
            .await?;
    }
    let details = json!({"portA":address_a.port(),"portB":address_b.port(),
        "groupA":a,"groupB":b,"peerA":peer.device_id(),"peerB":beta.device_id()});
    tokio::fs::write(dir.join("relay.json"), details.to_string()).await?;
    println!("{details}");
    tokio::try_join!(
        serve(&dir, a, "Peer-Alpha", peer),
        serve(&dir, b, "Peer-Beta", beta)
    )?;
    Ok(())
}

async fn relay(group: GroupId, name: &str) -> Result<(Session, SocketAddr)> {
    let relay = Session::new(DeviceId::new(), group, DeviceRole::Relay);
    relay.announce_nickname(Some(name.to_owned())).await?;
    let address = relay.listen("127.0.0.1:0".parse()?).await?;
    Ok((relay, address))
}

async fn serve(dir: &Path, group: GroupId, name: &str, peer: Session) -> Result<()> {
    let mut events = peer.subscribe();
    let mut incoming = HashMap::<FileId, FileAssembler>::new();
    let mut skipped = HashSet::new();
    let mut outbound = HashMap::new();
    loop {
        let SessionEvent::MessageReceived { message, .. } = events.recv().await? else {
            continue;
        };
        match message {
            Message::Text { header, payload } if payload.content.starts_with("mesh-test-") => {
                if payload.content == "mesh-test-background" {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                let text = format!("{name}/{}", payload.content);
                match header.target {
                    MessageTarget::Broadcast => {
                        peer.send_group_message_with_nickname(text, Some(name.into()))
                            .await?;
                    }
                    MessageTarget::Device { device_id } if device_id == peer.device_id() => {
                        peer.send_direct_message_with_nickname(
                            header.source_device_id,
                            text,
                            Some(name.into()),
                        )
                        .await?;
                    }
                    _ => {}
                }
                println!("{name} text {}", payload.content);
            }
            Message::FileChunk { header, payload }
                if payload.file_name.starts_with("mesh-acceptance-") =>
            {
                let id = payload.file_id;
                if payload.chunk_index == 1 && skipped.insert(id) {
                    continue;
                }
                let file = dir.join(format!("received-{}", id.0));
                if let std::collections::hash_map::Entry::Vacant(entry) = incoming.entry(id) {
                    entry.insert(
                        FileAssembler::create(
                            &file,
                            id,
                            payload.chunk_count,
                            payload.total_size,
                            &payload.sha256,
                        )
                        .await?,
                    );
                }
                let assembler = incoming.get_mut(&id).unwrap();
                let target = MessageTarget::Device {
                    device_id: header.source_device_id,
                };
                match assembler.push_chunk(&payload).await? {
                    FileAssemblyStatus::Incomplete { .. }
                        if payload.chunk_index + 1 == payload.chunk_count =>
                    {
                        peer.route_message(assembler.resume_request_message(
                            group,
                            peer.device_id(),
                            target,
                            8,
                        ))
                        .await?;
                        println!("{name} requested missing upload chunk");
                    }
                    FileAssemblyStatus::Complete { .. } => {
                        incoming.remove(&id);
                        let reply = FileId::new();
                        outbound.insert(reply, (file.clone(), header.source_device_id));
                        let mut reader =
                            FileChunkReader::open(&file, reply, group, peer.device_id(), target, 8)
                                .await?
                                .with_file_name("mesh-roundtrip.bin".into())
                                .with_sender_nickname(Some(name.into()));
                        while let Some(msg) = reader.next_message().await? {
                            if matches!(&msg, Message::FileChunk {payload,..} if payload.chunk_index == 1)
                            {
                                continue;
                            }
                            peer.route_message(msg).await?;
                        }
                        println!("{name} verified upload {}", payload.sha256);
                    }
                    FileAssemblyStatus::HashMismatch { .. } => {
                        return Err("upload hash mismatch".into());
                    }
                    _ => {}
                }
            }
            Message::FileResumeRequest { header, payload } => {
                if let Some((file, owner)) = outbound.get(&payload.file_id) {
                    if *owner != header.source_device_id {
                        continue;
                    }
                    for message in resend_file_chunks(
                        file,
                        &payload,
                        group,
                        peer.device_id(),
                        MessageTarget::Device { device_id: *owner },
                        8,
                    )
                    .await?
                    {
                        peer.route_message(message).await?;
                    }
                    println!("{name} resent download chunks {:?}", payload.missing_chunks);
                }
            }
            _ => {}
        }
    }
}

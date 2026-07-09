use crate::{
    DEFAULT_TTL,
    ids::{err_string, id, neighbor},
    state::SentFiles,
    views::{MemberEvent, MessageEvent, NeighborEvent, TransferProgressEvent},
};
use lan_mesh_core::{
    FileResumeRequestPayload, GroupId, Message, Session, SessionEvent, resend_file_chunks,
};
use tauri::{AppHandle, Emitter};

pub(crate) async fn forward_events(
    app: AppHandle,
    session: Session,
    group_id: GroupId,
    sent_files: SentFiles,
) {
    let mut events = session.subscribe();
    while let Ok(event) = events.recv().await {
        emit_event(&app, &session, group_id, &sent_files, event).await;
    }
}

async fn emit_event(
    app: &AppHandle,
    session: &Session,
    group_id: GroupId,
    sent_files: &SentFiles,
    event: SessionEvent,
) {
    match event {
        SessionEvent::NeighborOnline {
            neighbor_id,
            peer_addr,
        } => {
            let _ = app.emit(
                "mesh://neighbor-online",
                NeighborEvent {
                    neighbor_id: neighbor(neighbor_id),
                    peer_addr: peer_addr.to_string(),
                },
            );
        }
        SessionEvent::NeighborOffline {
            neighbor_id,
            peer_addr,
        } => {
            let _ = app.emit(
                "mesh://neighbor-offline",
                NeighborEvent {
                    neighbor_id: neighbor(neighbor_id),
                    peer_addr: peer_addr.to_string(),
                },
            );
        }
        SessionEvent::MessageReceived {
            neighbor_id,
            message,
        } => {
            emit_message_side_events(app, &message);
            if let Message::FileResumeRequest { payload, .. } = &message {
                let _ = resend_saved_chunks(app, session, group_id, sent_files, payload).await;
            }
            let _ = app.emit(
                "mesh://message-received",
                MessageEvent {
                    neighbor_id: neighbor(neighbor_id),
                    message,
                },
            );
        }
    }
}

pub(crate) async fn resend_saved_chunks(
    app: &AppHandle,
    session: &Session,
    group_id: GroupId,
    sent_files: &SentFiles,
    request: &FileResumeRequestPayload,
) -> Result<usize, String> {
    let sent = sent_files
        .lock()
        .await
        .get(&request.file_id)
        .cloned()
        .ok_or_else(|| "file is not available for resume".to_string())?;
    let messages = resend_file_chunks(
        &sent.path,
        request,
        group_id,
        session.device_id(),
        sent.target,
        DEFAULT_TTL,
    )
    .await
    .map_err(err_string)?;
    let resent_chunks = messages.len();

    for message in messages {
        if let Message::FileChunk { payload, .. } = &message {
            let _ = app.emit(
                "mesh://transfer-progress",
                TransferProgressEvent {
                    file_id: id(payload.file_id.0),
                    direction: "outgoing",
                    chunk_index: payload.chunk_index,
                    chunk_count: payload.chunk_count,
                    done_chunks: payload.chunk_index + 1,
                    total_size: payload.total_size,
                },
            );
        }
        session.route_message(message).await.map_err(err_string)?;
    }

    Ok(resent_chunks)
}

fn emit_message_side_events(app: &AppHandle, message: &Message) {
    match message {
        Message::MemberChanged { payload, .. } => {
            let _ = app.emit(
                "mesh://member-changed",
                MemberEvent {
                    device_id: id(payload.device_id.0),
                    change: payload.change,
                },
            );
        }
        Message::FileChunk { payload, .. } => {
            let _ = app.emit(
                "mesh://transfer-progress",
                TransferProgressEvent {
                    file_id: id(payload.file_id.0),
                    direction: "incoming",
                    chunk_index: payload.chunk_index,
                    chunk_count: payload.chunk_count,
                    done_chunks: payload.chunk_index + 1,
                    total_size: payload.total_size,
                },
            );
        }
        _ => {}
    }
}

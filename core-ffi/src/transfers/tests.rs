use super::*;
use lan_mesh::{FILE_CHUNK_SIZE, Message};

#[tokio::test]
async fn only_verified_files_leave_private_staging() {
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let input = dir.path().join("input");
    let bytes = vec![0xA5; FILE_CHUNK_SIZE + 11];
    tokio::fs::write(&input, &bytes).await.unwrap();
    let group = GroupId::new();
    let leaf = Session::leaf(DeviceId::new(), group);
    let mut downloads = Downloads::new(dir.clone());
    let report = Arc::new(Mutex::new(Vec::new()));
    let capture = report.clone();
    let emit: EventSink = Arc::new(move |v| {
        capture.lock().unwrap().push(v);
        Ok(())
    });
    for intact in [true, false] {
        let mut reader = FileChunkReader::open(
            &input,
            FileId::new(),
            group,
            DeviceId::new(),
            MessageTarget::Broadcast,
            8,
        )
        .await
        .unwrap()
        .with_file_name("../picked.txt".into());
        while let Some(Message::FileChunk {
            header,
            mut payload,
        }) = reader.next_message().await.unwrap()
        {
            if !intact && payload.chunk_index == 0 {
                payload.data[0] ^= 1;
            }
            downloads
                .chunk(group, &leaf, &emit, &header, &payload)
                .await
                .unwrap();
        }
        let events = std::mem::take(&mut *report.lock().unwrap());
        assert!(events.first().unwrap()["path"].is_null());
        let result = events.last().unwrap();
        if intact {
            let path = Path::new(result["path"].as_str().unwrap());
            assert_eq!(path.parent(), Some(dir.path()));
            assert_eq!(tokio::fs::read(path).await.unwrap(), bytes);
        } else {
            assert!(result["state"].as_str().unwrap().contains("SHA256"));
            assert!(result["path"].is_null());
        }
    }
    leaf.destroy().await;
}

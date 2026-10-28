#[cfg(unix)]
use std::{fs, os::unix::fs::symlink};
#[cfg(unix)]
use vtorrent_torrent::{
    engine_disk::write_piece_to_disk,
    metainfo::{Metainfo, TorrentFile},
};

#[cfg(unix)]
#[tokio::main]
async fn main() {
    use sha1::Digest as _;
    tracker_response_limit().await;
    let root = std::env::temp_dir().join(format!("vtr-symlink-proof-{}", std::process::id()));
    let download = root.join("download");
    let outside = root.join("outside");
    fs::create_dir_all(download.join("payload")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("victim"), b"SAFE").unwrap();
    symlink(&outside, download.join("payload/escape")).unwrap();
    let metainfo = Metainfo {
        info_hash: [0; 20],
        name: "payload".into(),
        total_size: 4,
        piece_length: 4,
        piece_count: 1,
        pieces: vec![sha1::Sha1::digest(b"PWN!").into()],
        announce: None,
        announce_list: vec![],
        files: vec![TorrentFile {
            path: vec!["escape".into(), "victim".into()],
            length: 4,
            md5sum: None,
        }],
        created_at: None,
        comment: None,
        is_private: false,
    };
    assert!(!write_piece_to_disk(&metainfo, &download, 0, b"PWN!").await);
    assert_eq!(fs::read(outside.join("victim")).unwrap(), b"SAFE");
    println!("Pre-existing symlink was rejected; no write escaped the download directory");
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
async fn tracker_response_limit() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use vtorrent_torrent::tracker::{
        set_allow_private_tracker_targets, AnnounceEvent, AnnounceRequest, HttpTracker,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0u8; 4096];
        socket.read(&mut request).await.unwrap();
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let chunk = vec![b'x'; 65536];
        let mut sent = 0usize;
        for _ in 0..1024 {
            if socket.write_all(&chunk).await.is_err() {
                break;
            }
            sent += chunk.len();
        }
        sent
    });
    set_allow_private_tracker_targets(true);
    let tracker = HttpTracker::new().unwrap();
    let error = tracker
        .announce(&AnnounceRequest {
            tracker_url: format!("http://{addr}/announce"),
            info_hash: [0; 20],
            peer_id: [0; 20],
            port: 6881,
            uploaded: 0,
            downloaded: 0,
            left: 0,
            event: AnnounceEvent::Started,
            num_want: 1,
        })
        .await
        .unwrap_err();
    set_allow_private_tracker_targets(false);
    let sent = server.await.unwrap();
    assert!(
        sent < 8 * 1024 * 1024,
        "client read the entire oversized body"
    );
    assert!(error.to_string().contains("too large"));
    println!("Tracker write stopped after {sent} bytes; the client rejected the response at the 4 MiB cap");
}

#[cfg(not(unix))]
fn main() {
    eprintln!("This reproduction requires Unix symlinks.");
    std::process::exit(1);
}

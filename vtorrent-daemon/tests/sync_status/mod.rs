use super::*;

async fn wait_status(daemon: &Daemon, connections: u64, syncing: bool) {
    let mut last_info = Value::Null;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let info = daemon.get("/api/v1/info").await;
            let peer_count = info["connections"].as_u64().unwrap();
            if (if connections == 0 { peer_count == 0 } else { peer_count >= connections })
                && info["syncing"] == syncing {
                if !syncing {
                    assert_eq!(info["sync_percent"], 100.0);
                }
                return;
            }
            last_info = info;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("sync status did not converge (connections={connections}, syncing={syncing}): {last_info}; {}", daemon.logs()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_sync_status_recovers_after_last_peer_reconnects() {
    let _scenario = RECOVERY_SCENARIO.lock().await;
    let first_dir = tempfile::tempdir().unwrap();
    let second_dir = tempfile::tempdir().unwrap();
    let first = Daemon::start(first_dir.path(), "first.log", None).await;
    first.mint(91).await;
    first.persisted(1).await;
    let tip = first.get("/api/v1/info").await["best_block_hash"]
        .as_str()
        .unwrap()
        .to_owned();
    let second = Daemon::start(second_dir.path(), "second.log", Some(&first.p2p)).await;
    second.wait_tip(&tip).await;
    wait_status(&first, 1, false).await;
    wait_status(&second, 1, false).await;
    second.persisted(1).await;
    second.stop(false).await;
    wait_status(&first, 0, true).await;

    let second = Daemon::start(second_dir.path(), "reconnected.log", Some(&first.p2p)).await;
    wait_status(&first, 1, false).await;
    wait_status(&second, 1, false).await;
    first.mint(92).await;
    let tip = first.get("/api/v1/info").await["best_block_hash"]
        .as_str()
        .unwrap()
        .to_owned();
    second.wait_tip(&tip).await;
    wait_status(&first, 1, false).await;
    wait_status(&second, 1, false).await;
    second.stop(false).await;
    first.stop(false).await;
}

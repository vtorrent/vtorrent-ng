#![cfg(unix)]

use serde_json::{json, Value};
use std::{
    net::TcpListener,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::{Child, Command};
use vtorrent_store::store::BlockStore;

mod interrupted_reorg;
mod rpc_polling;
mod swap_recovery;

struct Daemon {
    child: Child,
    rpc: String,
    p2p: String,
    log: PathBuf,
    client: reqwest::Client,
}

impl Daemon {
    async fn start(directory: &Path, log_name: &str, seed: Option<&str>) -> Self {
        std::fs::create_dir_all(directory).unwrap();
        let rpc_port = TcpListener::bind("127.0.0.1:0").unwrap();
        let p2p_port = TcpListener::bind("127.0.0.1:0").unwrap();
        let rpc = rpc_port.local_addr().unwrap().to_string();
        let p2p = p2p_port.local_addr().unwrap().to_string();
        let log = directory.join(log_name);
        let output = std::fs::File::create(&log).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_vtorrent-daemon"));
        command
            .args([
                "--regtest",
                "--testnet",
                "--isolated",
                "--no-dht",
                "--rpc-addr",
                &rpc,
                "--listen",
                &p2p,
                "--public-addr",
                &p2p,
                "--log-level",
                "vtorrent_daemon=debug,vtorrent_node=warn,vtorrent_store=warn",
            ])
            .arg("--data-dir")
            .arg(directory)
            .env_remove("VTORRENT_BTC_SEED")
            .env_remove("VTORRENT_STAKING_WIF")
            .env_remove("VTORRENT_RPC_API_KEY")
            .env_remove("RUST_LOG")
            .stdout(Stdio::from(output.try_clone().unwrap()))
            .stderr(Stdio::from(output))
            .kill_on_drop(true);
        if let Some(seed) = seed {
            command.args(["--seed", seed]);
        }
        drop(rpc_port);
        drop(p2p_port);
        let mut daemon = Self {
            child: command.spawn().unwrap(),
            rpc: format!("http://{rpc}"),
            p2p,
            log,
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(3))
                .build()
                .unwrap(),
        };
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                assert!(
                    daemon.child.try_wait().unwrap().is_none(),
                    "daemon exited: {}",
                    daemon.logs()
                );
                if daemon
                    .client
                    .get(format!("{}/api/v1/info", daemon.rpc))
                    .send()
                    .await
                    .is_ok_and(|r| r.status().is_success())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("daemon startup timed out: {}", daemon.logs()));
        daemon
    }

    fn logs(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    async fn get(&self, path: &str) -> Value {
        rpc_polling::get_json(&self.client, &format!("{}{path}", self.rpc))
            .await
            .unwrap_or_else(|error| panic!("GET {path} failed: {error}; {}", self.logs()))
    }

    async fn mint(&self, tag: u8) -> Value {
        let address = vtorrent_core::address::Address::from_hash160(&[tag; 20], 70)
            .unwrap()
            .to_string();
        let response = self
            .client
            .post(format!("{}/api/v1/faucet", self.rpc))
            .json(&json!({"address": address, "amount_satoshis": u64::from(tag) * 100_000}))
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body: Value = response.json().await.unwrap();
        assert!(status.is_success(), "faucet failed: {body}");
        body
    }

    async fn persisted(&self, height: u64) {
        tokio::time::timeout(Duration::from_secs(15), async {
            while !self
                .logs()
                .contains(&format!("Persisted block at height {height}"))
            {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("block persistence timed out: {}", self.logs()));
    }

    async fn wait_tip(&self, hash: &str) {
        rpc_polling::wait_tip(
            &self.client,
            &format!("{}/api/v1/info", self.rpc),
            hash,
            Duration::from_secs(20),
        )
        .await
        .unwrap_or_else(|error| panic!("chain synchronization failed: {error}; {}", self.logs()));
    }

    async fn stop(mut self, crash: bool) {
        if crash {
            self.child.start_kill().unwrap();
        } else {
            let status = Command::new("kill")
                .args(["-TERM", &self.child.id().unwrap().to_string()])
                .status()
                .await
                .unwrap();
            assert!(status.success());
        }
        let status = tokio::time::timeout(Duration::from_secs(10), self.child.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            status.success(),
            !crash,
            "unexpected daemon exit: {}",
            self.logs()
        );
        let logs = self.logs();
        assert!(!logs.contains("BlockStore::"), "persistence failed: {logs}");
    }
}

async fn reorg_recovery(crash: bool) {
    let directory = tempfile::tempdir().unwrap();
    let a_dir = directory.path().join("a");
    let b_dir = directory.path().join("b");
    let a = Daemon::start(&a_dir, "initial.log", None).await;
    let b = Daemon::start(&b_dir, "initial.log", None).await;
    let orphaned = a.mint(1).await;
    a.persisted(1).await;
    let winning_first = b.mint(2).await;
    b.mint(3).await;
    b.persisted(2).await;
    let expected_fork = b.get("/api/v1/info").await["best_block_hash"]
        .as_str()
        .unwrap()
        .to_owned();
    b.stop(false).await;
    let b = Daemon::start(&b_dir, "connected.log", Some(&a.p2p)).await;
    a.wait_tip(&expected_fork).await;
    a.mint(4).await;
    a.persisted(3).await;
    let expected_tip = a.get("/api/v1/info").await["best_block_hash"]
        .as_str()
        .unwrap()
        .to_owned();
    b.stop(false).await;
    a.stop(crash).await;
    {
        let store = BlockStore::open(a_dir.join("chain.db")).unwrap();
        assert_eq!(store.best_height().unwrap(), 3);
        let losing_txid: [u8; 32] = hex::decode(orphaned["txid"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let winning_txid: [u8; 32] = hex::decode(winning_first["txid"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(
            hex::encode(store.best_hash().unwrap().unwrap()),
            expected_tip
        );
        assert!(!store.has_utxo(&losing_txid, 0).unwrap());
        assert!(store.has_utxo(&winning_txid, 0).unwrap());
        let chain = store.load_into_regtest_chain().unwrap();
        assert_eq!(hex::encode(chain.best_hash().unwrap()), expected_tip);
        assert!(chain.get_transaction(&losing_txid).is_none());
        assert!(chain.get_utxo(&losing_txid, 0).is_none());
        assert!(chain.get_utxo(&winning_txid, 0).is_some());
    }
    let restarted = Daemon::start(&a_dir, "restarted.log", None).await;
    let info = restarted.get("/api/v1/info").await;
    assert_eq!(info["block_height"], 3);
    assert_eq!(info["best_block_hash"], expected_tip);
    assert_eq!(restarted.mint(5).await["block_height"], 4);
    restarted.persisted(4).await;
    restarted.stop(false).await;
}

#[tokio::test]
async fn daemon_reorg_survives_sigterm_restart() {
    reorg_recovery(false).await;
}

#[tokio::test]
async fn daemon_reorg_survives_sigkill_restart() {
    reorg_recovery(true).await;
}

use super::refund_bump::{fixture, request};
use super::*;
use std::{net::SocketAddr, path::Path, time::Duration};
use vtorrent_node::{
    block::{Block, Transaction},
    chain::Chain,
    events::NodeEvent,
    node::{Node, NodeConfig},
};

struct RunningNode {
    state: AppState,
    address: SocketAddr,
    events: tokio::sync::broadcast::Receiver<Arc<NodeEvent>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for RunningNode {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

async fn blocks(state: &AppState) -> Vec<Block> {
    let chain = state.chain.lock().await;
    (1..=chain.best_height())
        .map(|h| chain.get_block_at_height(h).unwrap().clone())
        .collect()
}

fn replay(blocks: &[Block]) -> Chain {
    let mut chain = Chain::new_regtest().unwrap();
    for block in blocks {
        chain.add_block(block.clone()).unwrap();
    }
    chain
}

async fn start(
    base: &AppState,
    chain: Chain,
    directory: &Path,
    seeds: &[SocketAddr],
) -> RunningNode {
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    let config = NodeConfig {
        listen_addr: address.to_string(),
        data_dir: directory.to_path_buf(),
        extra_seeds: seeds.iter().map(ToString::to_string).collect(),
        isolated: true,
        use_dht: false,
        use_overlay: false,
        regtest: true,
        testnet: true,
        public_addr: Some(address),
        ..Default::default()
    };
    let mut node = Node::new_with_chain(config, chain).unwrap();
    let mut state = AppState::new_with_shared(node.chain_arc(), node.mempool_arc());
    state.swap_recovery_dir = base.swap_recovery_dir.clone();
    state.tx_submit = Some(node.tx_submit_sender());
    state.block_submit = Some(node.block_submit_sender());
    *state.mock_time.write().await = *base.mock_time.read().await;
    *state.wallet_wif.write().await = base.wallet_wif.read().await.clone();
    *state.wallet_change_address.write().await = base.wallet_change_address.read().await.clone();
    *state.wallet_unlock_expiry.write().await = *base.wallet_unlock_expiry.read().await;
    *state.swaps.write().await = base.swaps.read().await.clone();
    for order in base.order_book.read().await.list_orders() {
        state.order_book.write().await.add_order(order.clone());
    }
    node.set_order_book(state.order_book.clone());
    let (event_tx, events) = vtorrent_node::events::channel(256);
    node.set_event_sender(event_tx);
    let (stop, stopped) = tokio::sync::oneshot::channel();
    drop(reservation);
    let thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            tokio::select! {
                result = node.start() => panic!("node stopped unexpectedly: {result:?}"),
                _ = stopped => {},
            }
        });
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if tokio::net::TcpStream::connect(address).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("node did not listen");
    RunningNode {
        state,
        address,
        events,
        stop: Some(stop),
        thread: Some(thread),
    }
}

async fn connected(node: &mut RunningNode) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if matches!(
                &*node.events.recv().await.unwrap(),
                NodeEvent::PeerConnected { .. }
            ) {
                break;
            }
        }
    })
    .await
    .expect("TCP peer handshake timed out");
}

async fn reorganized(node: &mut RunningNode) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if matches!(&*node.events.recv().await.unwrap(), NodeEvent::Reorg { .. }) {
                break;
            }
        }
    })
    .await
    .expect("node did not finish processing the reorg");
}

async fn wait_mempool(node: &RunningNode, txid: [u8; 32]) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if node
                .state
                .mempool
                .lock()
                .await
                .get_transaction(&txid)
                .is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "{} did not receive transaction {}",
            node.address,
            hex::encode(txid)
        )
    });
}

async fn wait_tip(node: &RunningNode, hash: [u8; 32]) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if node.state.chain.lock().await.best_hash() == Some(hash) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{} did not adopt tip {}", node.address, hex::encode(hash)));
}

async fn mine(node: &RunningNode, transaction: Option<Transaction>, tag: u64) -> Block {
    let mut chain = node.state.chain.lock().await;
    let current: Vec<_> = (1..=chain.best_height())
        .map(|h| chain.get_block_at_height(h).unwrap().clone())
        .collect();
    let mut template = replay(&current);
    template.mint_to_address(&vtr_identity(1).1, tag).unwrap();
    let mut block = template
        .get_block_at_height(template.best_height())
        .unwrap()
        .clone();
    if let Some(tx) = transaction {
        block.transactions.push(tx);
    }
    block.header.merkle_root = block.compute_merkle_root();
    chain.add_block(block.clone()).unwrap();
    let confirmed: Vec<_> = block.transactions.iter().map(Transaction::txid).collect();
    let spent: Vec<_> = block
        .transactions
        .iter()
        .flat_map(|tx| tx.inputs.iter().map(|i| (i.prev_txid, i.prev_vout)))
        .collect();
    node.state
        .mempool
        .lock()
        .await
        .handle_confirmed_block(&confirmed, &spent);
    drop(chain);
    node.state
        .block_submit
        .as_ref()
        .unwrap()
        .send(block.clone())
        .await
        .unwrap();
    block
}

#[tokio::test]
async fn refund_network_replacement_crosses_tcp_after_peer_restart() {
    let directory = tempfile::tempdir().unwrap();
    let (base, id, original) = fixture(&directory.path().join("wallet")).await;
    let initial = blocks(&base).await;
    let mut a = start(&base, replay(&initial), &directory.path().join("a"), &[]).await;
    let mut b = start(
        &base,
        replay(&initial),
        &directory.path().join("b"),
        &[a.address],
    )
    .await;
    connected(&mut a).await;
    connected(&mut b).await;
    crate::refund_bump::submit_latest(&a.state, &a.state.swaps.read().await[&id])
        .await
        .unwrap();
    wait_mempool(&b, original).await;
    let mempool_path = directory.path().join("b-mempool.json");
    b.state.mempool.lock().await.save_to(&mempool_path).unwrap();
    drop(b);
    let bump = crate::refund_bump::bump(&a.state, request(&id, original, 20_000))
        .await
        .unwrap();
    let replacement = parse_hash32(&bump.txid, "id").unwrap();
    let chain_path = directory.path().join("a-chain.bin");
    std::fs::write(
        &chain_path,
        bincode::serialize(&blocks(&a.state).await).unwrap(),
    )
    .unwrap();
    drop(a);
    let saved: Vec<Block> = bincode::deserialize(&std::fs::read(&chain_path).unwrap()).unwrap();
    let mut a = start(
        &base,
        replay(&saved),
        &directory.path().join("a-restarted"),
        &[],
    )
    .await;
    assert!(a.state.swaps.read().await[&id]
        .vtr_refund_replacements
        .is_empty());
    crate::swap_recovery::restore_with_wif(&a.state, &vtr_identity(1).0)
        .await
        .unwrap();
    assert_eq!(
        a.state.swaps.read().await[&id]
            .latest_vtr_refund()
            .unwrap()
            .txid(),
        replacement
    );
    let mut b = start(
        &base,
        replay(&initial),
        &directory.path().join("b-restarted"),
        &[a.address],
    )
    .await;
    {
        let chain = b.state.chain.lock().await;
        for (tx, _) in vtorrent_node::mempool::Mempool::load_saved(&mempool_path) {
            b.state
                .mempool
                .lock()
                .await
                .admit_with_chain_fee(&chain, tx)
                .unwrap();
        }
    }
    connected(&mut b).await;
    connected(&mut a).await;
    assert!(b
        .state
        .mempool
        .lock()
        .await
        .get_transaction(&original)
        .is_some());
    crate::refund_bump::bump(&a.state, request(&id, original, 20_000))
        .await
        .unwrap();
    wait_mempool(&b, replacement).await;
    assert!(b
        .state
        .mempool
        .lock()
        .await
        .get_transaction(&original)
        .is_none());
    let mut c = start(
        &base,
        replay(&initial),
        &directory.path().join("c"),
        &[b.address],
    )
    .await;
    connected(&mut c).await;
    b.state
        .tx_submit
        .as_ref()
        .unwrap()
        .send(
            a.state.swaps.read().await[&id]
                .latest_vtr_refund()
                .unwrap()
                .clone(),
        )
        .await
        .unwrap();
    wait_mempool(&c, replacement).await;
    let latest = a.state.swaps.read().await[&id]
        .latest_vtr_refund()
        .unwrap()
        .clone();
    let block = mine(&a, Some(latest), 3).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if c.state.chain.lock().await.best_hash() == Some(block.hash()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("confirmation did not propagate over TCP");
    assert!(b
        .state
        .mempool
        .lock()
        .await
        .get_transaction(&replacement)
        .is_none());
    assert!(c
        .state
        .mempool
        .lock()
        .await
        .get_transaction(&replacement)
        .is_none());
}

async fn refund_network_reorg_evicts_losing_refund(replacement_wins: bool) {
    let directory = tempfile::tempdir().unwrap();
    let (base, id, original) = fixture(&directory.path().join("wallet")).await;
    let initial = blocks(&base).await;
    let mut a = start(&base, replay(&initial), &directory.path().join("a"), &[]).await;
    let b = start(
        &base,
        replay(&initial),
        &directory.path().join("b-offline"),
        &[],
    )
    .await;
    let bump_state = if replacement_wins { &b.state } else { &a.state };
    let bump = crate::refund_bump::bump(bump_state, request(&id, original, 20_000))
        .await
        .unwrap();
    let replacement = parse_hash32(&bump.txid, "id").unwrap();
    mine(&a, None, 10).await;
    let winning_tx = if replacement_wins {
        let swap = b.state.swaps.read().await[&id].clone();
        a.state.swaps.write().await.insert(id.clone(), swap.clone());
        let original_swap = base.swaps.read().await[&id].clone();
        crate::refund_bump::submit_latest(&a.state, &original_swap)
            .await
            .unwrap();
        swap.latest_vtr_refund().unwrap().clone()
    } else {
        base.swaps.read().await[&id].vtr_refund_tx.clone().unwrap()
    };
    let winning_txid = winning_tx.txid();
    let losing_txid = if replacement_wins {
        original
    } else {
        replacement
    };
    assert!(a
        .state
        .mempool
        .lock()
        .await
        .get_transaction(&losing_txid)
        .is_some());
    mine(&b, Some(winning_tx), 20).await;
    let winner = mine(&b, None, 21).await;
    let fork = blocks(&b.state).await;
    drop(b);
    let mut b = start(
        &base,
        replay(&fork),
        &directory.path().join("b-online"),
        &[a.address],
    )
    .await;
    connected(&mut a).await;
    connected(&mut b).await;
    wait_tip(&a, winner.hash()).await;
    reorganized(&mut a).await;
    let observation = crate::swap_reconciliation::status(&a.state, &id)
        .await
        .unwrap();
    assert_eq!(
        observation.spend.as_ref().unwrap().txid,
        hex::encode(winning_txid)
    );
    assert!(a.state.mempool.lock().await.get_transaction(&losing_txid).is_none(),
        "reorg left the losing refund in the mempool after its input was spent on the winning branch");
    assert!(a
        .state
        .mempool
        .lock()
        .await
        .get_transaction(&winning_txid)
        .is_none());
}

#[tokio::test]
async fn refund_network_reorg_evicts_replacement_when_original_wins() {
    refund_network_reorg_evicts_losing_refund(false).await;
}

#[tokio::test]
async fn refund_network_reorg_evicts_original_when_replacement_wins() {
    refund_network_reorg_evicts_losing_refund(true).await;
}

async fn claim_refund_fork(claim_wins: bool, loser_confirmed: bool) {
    use crate::swap_reconciliation::{reconcile, status};
    use vtorrent_node::atomic_swap::VtrSettlementState;

    let directory = tempfile::tempdir().unwrap();
    let (base, id, original) = fixture(&directory.path().join("maker-wallet")).await;
    let initial = blocks(&base).await;
    let refund_node = start(
        &base,
        replay(&initial),
        &directory.path().join("refund"),
        &[],
    )
    .await;
    let mut claim_node = start(
        &base,
        replay(&initial),
        &directory.path().join("claim"),
        &[],
    )
    .await;
    claim_node.state.swap_recovery_dir = Some(directory.path().join("taker-wallet"));
    *claim_node.state.wallet_wif.write().await = Some(vtr_identity(2).0.clone());
    *claim_node.state.wallet_change_address.write().await = Some(vtr_identity(2).1.clone());
    {
        let mut swaps = claim_node.state.swaps.write().await;
        let swap = swaps.get_mut(&id).unwrap();
        swap.vtr_refund_txid = None;
        swap.vtr_refund_tx = None;
        swap.refresh_status();
    }
    let preimage = base
        .order_book
        .read()
        .await
        .get_order(&id)
        .unwrap()
        .preimage
        .unwrap();
    vtr_claim_with_state(
        &claim_node.state,
        VtrClaimRequest {
            order_id: id.clone(),
            preimage: hex::encode(preimage),
            taker_wif: vtr_identity(2).0,
        },
    )
    .await
    .unwrap();
    crate::refund_bump::bump(&refund_node.state, request(&id, original, 20_000))
        .await
        .unwrap();
    let claim = claim_node.state.swaps.read().await[&id]
        .vtr_claim_tx
        .clone()
        .unwrap();
    let refund = refund_node.state.swaps.read().await[&id]
        .latest_vtr_refund()
        .unwrap()
        .clone();
    assert_eq!(claim.inputs[0].prev_txid, refund.inputs[0].prev_txid);
    assert_eq!(claim.inputs[0].prev_vout, refund.inputs[0].prev_vout);
    assert_eq!(
        status(&claim_node.state, &id).await.unwrap().state,
        VtrSettlementState::ClaimPending
    );
    assert_eq!(
        status(&refund_node.state, &id).await.unwrap().state,
        VtrSettlementState::RefundPending
    );
    let (mut loser, winner, losing_tx, winning_tx) = if claim_wins {
        (refund_node, claim_node, refund, claim)
    } else {
        (claim_node, refund_node, claim, refund)
    };
    mine(&loser, loser_confirmed.then(|| losing_tx.clone()), 30).await;
    let before = reconcile(&loser.state, &id).await.unwrap();
    assert_eq!(before.reorg_count, 0);
    assert_eq!(before.spend.is_some(), loser_confirmed);
    mine(&winner, Some(winning_tx.clone()), 40).await;
    let winning_tip = mine(&winner, None, 41).await;
    let fork = blocks(&winner.state).await;
    let winning_state = winner.state.clone();
    drop(winner);
    let mut winner = start(
        &winning_state,
        replay(&fork),
        &directory.path().join("winner-online"),
        &[loser.address],
    )
    .await;
    connected(&mut loser).await;
    connected(&mut winner).await;
    wait_tip(&loser, winning_tip.hash()).await;
    reorganized(&mut loser).await;
    for node in [&loser, &winner] {
        let chain = node.state.chain.lock().await;
        let mp = node.state.mempool.lock().await;
        assert!(chain.get_transaction(&losing_tx.txid()).is_none());
        assert!(chain.get_transaction(&winning_tx.txid()).is_some());
        assert!(mp.get_transaction(&losing_tx.txid()).is_none());
        assert!(mp.get_transaction(&winning_tx.txid()).is_none());
    }
    let observed = reconcile(&loser.state, &id).await.unwrap();
    assert_eq!(observed.state, VtrSettlementState::SpentElsewhere);
    assert_eq!(
        observed.spend.as_ref().unwrap().txid,
        hex::encode(winning_tx.txid())
    );
    assert_eq!(observed.reorg_count, u32::from(loser_confirmed));
    assert_eq!(
        reconcile(&loser.state, &id).await.unwrap().reorg_count,
        observed.reorg_count
    );
    let chain = loser.state.chain.lock().await;
    assert!(loser
        .state
        .mempool
        .lock()
        .await
        .admit_with_chain_fee(&chain, losing_tx)
        .is_err());
}

#[tokio::test]
async fn refund_network_pending_refund_loses_to_claim() {
    claim_refund_fork(true, false).await;
}

#[tokio::test]
async fn refund_network_confirmed_refund_loses_to_claim() {
    claim_refund_fork(true, true).await;
}

#[tokio::test]
async fn refund_network_pending_claim_loses_to_refund() {
    claim_refund_fork(false, false).await;
}

#[tokio::test]
async fn refund_network_confirmed_claim_loses_to_refund() {
    claim_refund_fork(false, true).await;
}

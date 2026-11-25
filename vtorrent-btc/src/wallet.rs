//! Top-level Bitcoin wallet facade.

use crate::error::{BtcError, Result};
use crate::filters::FilterHeaderStore;
use crate::headers::HeaderChain;
use crate::keys::derive_address;
use crate::utxo::{Utxo, UtxoSet, MAX_UNDO_ENTRIES};
use bitcoin::hashes::Hash;
use bitcoin::Network;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Whether `ancestor` is on the main chain ending at `tip`.
fn is_ancestor(chain: &HeaderChain, ancestor: [u8; 32], tip: [u8; 32]) -> bool {
    let ancestor_height = chain.get(&ancestor).map(|entry| entry.height);
    let mut cursor = tip;
    loop {
        if cursor == ancestor {
            return true;
        }
        let Some(entry) = chain.get(&cursor) else {
            return false;
        };
        if let Some(height) = ancestor_height {
            if entry.height <= height {
                return false;
            }
        }
        if entry.height == 0 {
            return false;
        }
        cursor = entry.header.prev_blockhash.to_byte_array();
    }
}

/// Highest height where the old chain and the current main chain agree.
///
/// Returns `None` when the old branch's headers are no longer retained: with
/// finite retention that is the common case for a reorg, and the prune floor
/// must not masquerade as a proven common ancestor.
fn find_fork(chain: &HeaderChain, old_best: [u8; 32]) -> Option<u32> {
    let mut cursor = old_best;
    loop {
        let entry = chain.get(&cursor)?;
        let height = entry.height;
        if chain.hashes_from(height).first().copied() == Some(cursor) {
            return Some(height);
        }
        if height == 0 {
            return Some(0);
        }
        cursor = entry.header.prev_blockhash.to_byte_array();
    }
}

/// A wallet-local send whose delivery has not been confirmed.
#[derive(Debug, Clone)]
pub struct PendingSend {
    pub txid: String,
    pub raw: Option<Vec<u8>>,
    pub inputs: Vec<Utxo>,
}

/// A Bitcoin SPV wallet.
pub struct BtcWallet {
    seed: [u8; 64],
    network: Network,
    headers: Arc<Mutex<HeaderChain>>,
    utxos: Arc<Mutex<UtxoSet>>,
    filter_headers: Arc<Mutex<FilterHeaderStore>>,
    /// Optional path for persisting the UTXO set to disk.
    utxo_path: Option<PathBuf>,
    /// Held for the lifetime of a persistent wallet so two writers cannot
    /// overwrite each other's state.
    state_lock: Option<crate::utxo::StateLock>,
    next_index: u32,
    synced: bool,
    /// Next block height to scan (incremental checkpoint).
    last_scanned_height: u32,
    rescan_on_sync: AtomicBool,
    reset_on_sync: bool,
    /// In-process only: the pending rescan replays from a journalled fork, so
    /// no history below the fork or the retained floor is re-derived. Never
    /// persisted, because a restart cannot continue a journal recovery.
    /// Atomic so the `&self` guard readers can abandon the recovery when the
    /// retained floor advances past the resumed cursor.
    journal_recovered: AtomicBool,
    /// Whether a rescan was already owed when the journal recovery began.
    /// A covered rollback proves nothing below its fork needs re-deriving, so
    /// it may not clear a requirement that predates it: doing so would drop a
    /// standing case-3 alert for a wallet whose birthday is below the floor.
    recovery_rescan_required: AtomicBool,
    completed_scan: Mutex<Option<(u32, [u8; 32])>>,
    /// Whether new transactions signal RBF (Replace-By-Fee).
    rbf_enabled: bool,
}

impl Drop for BtcWallet {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.seed.zeroize();
    }
}

impl BtcWallet {
    /// Create a wallet from a 64-byte BIP39 seed.
    pub fn new(seed: [u8; 64]) -> Self {
        Self::with_network(seed, Network::Bitcoin)
    }

    /// Create a wallet for a specific Bitcoin network (mainnet/regtest).
    pub fn with_network(seed: [u8; 64], network: Network) -> Self {
        Self {
            seed,
            network,
            headers: Arc::new(Mutex::new(HeaderChain::anchored(network))),
            utxos: Arc::new(Mutex::new(UtxoSet::new())),
            filter_headers: Arc::new(Mutex::new(FilterHeaderStore::default())),
            utxo_path: None,
            state_lock: None,
            next_index: 0,
            synced: false,
            last_scanned_height: 0,
            rescan_on_sync: AtomicBool::new(false),
            reset_on_sync: false,
            journal_recovered: AtomicBool::new(false),
            recovery_rescan_required: AtomicBool::new(false),
            completed_scan: Mutex::new(None),
            rbf_enabled: true,
        }
    }

    /// Bound the in-memory header chain to the most recent `retention` headers
    /// (0 = unbounded). Keeps SPV memory flat on mainnet (~870k headers).
    /// Must exceed the difficulty retarget period (~2016) and the UTXO-scan
    /// depth. See `docs/btc-spv-soak-plan.md`.
    pub fn set_header_retention(&self, retention: usize) {
        let mut chain = self.headers.lock();
        chain.set_retention(retention);
        self.filter_headers.lock().prune(&chain);
    }

    /// Open a wallet from an existing UTXO persistence file.
    /// Missing or pre-v3 files are rejected because seed history is unknown.
    ///
    /// The height journal is deliberately not restored. Its scope is one live
    /// process: a crash can leave a journal beside an older cursor, and
    /// replaying it in a new process would undo changes this process never
    /// made. Wallet-local pending sends are retained because their
    /// transactions may still be live; their inputs stay unselectable until a
    /// scan observes the spend or an explicit recovery cancels it. The recorded
    /// tip goes with the journal, since it describes the journal's lineage.
    pub fn with_persistence(
        seed: [u8; 64],
        network: Network,
        utxo_path: PathBuf,
    ) -> std::io::Result<Self> {
        Self::with_persistence_attested_seed(seed, network, utxo_path, false)
    }

    /// Only for a startup path where the operator independently established the
    /// seed has no prior BTC history or outstanding spends. It does not weaken
    /// the pre-v3 or future-version guards.
    pub fn with_persistence_attested_seed(
        seed: [u8; 64],
        network: Network,
        utxo_path: PathBuf,
        attested_new_seed: bool,
    ) -> std::io::Result<Self> {
        let state_lock = crate::utxo::lock_state_path(&utxo_path)?;
        Self::open_with_state_lock(seed, network, utxo_path, attested_new_seed, state_lock)
    }

    /// Open an existing wallet state after running `check` under its writer lock.
    ///
    /// `check` runs after the network writer lock is held and before any state
    /// is loaded or created, so a pending repair cannot appear between the two.
    pub fn with_persistence_checked(
        seed: [u8; 64],
        network: Network,
        utxo_path: PathBuf,
        check: impl FnOnce() -> std::io::Result<()>,
    ) -> std::io::Result<Self> {
        let state_lock = crate::utxo::lock_state_path(&utxo_path)?;
        check()?;
        Self::open_with_state_lock(seed, network, utxo_path, false, state_lock)
    }

    fn open_with_state_lock(
        seed: [u8; 64],
        network: Network,
        utxo_path: PathBuf,
        attested_new_seed: bool,
        state_lock: crate::utxo::StateLock,
    ) -> std::io::Result<Self> {
        let state_lock = Some(state_lock);
        let mut created = false;
        if attested_new_seed && !utxo_path.exists() {
            crate::utxo::record_wallet_initialization(&utxo_path, true)?;
            match UtxoSet::new().create_new(&utxo_path) {
                Ok(()) => created = true,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        let mut utxos = UtxoSet::load_for_wallet(&utxo_path)?;
        crate::utxo::record_wallet_initialization(&utxo_path, false)?;
        if created {
            tracing::warn!(
                path = %utxo_path.display(),
                ?network,
                "Created the first BTC wallet state under an operator new-seed assertion; this process did not verify that the seed has no prior history or outstanding sends"
            );
        }
        utxos.clear_journal();
        utxos.set_last_scanned_tip(None);
        let next_index = utxos.next_index();
        let last_scanned_height = utxos.last_scanned_height();
        let rescan_on_sync =
            utxos.rescan_required() || last_scanned_height > 0 || !utxos.list().is_empty();
        Ok(Self {
            seed,
            network,
            headers: Arc::new(Mutex::new(HeaderChain::anchored(network))),
            utxos: Arc::new(Mutex::new(utxos)),
            filter_headers: Arc::new(Mutex::new(FilterHeaderStore::default())),
            utxo_path: Some(utxo_path),
            state_lock,
            next_index,
            synced: false,
            last_scanned_height,
            rescan_on_sync: AtomicBool::new(rescan_on_sync),
            reset_on_sync: rescan_on_sync,
            journal_recovered: AtomicBool::new(false),
            recovery_rescan_required: AtomicBool::new(false),
            completed_scan: Mutex::new(None),
            rbf_enabled: true,
        })
    }

    /// Set or change the UTXO persistence path.  The current in-memory set
    /// is immediately saved to the new path.
    ///
    /// The new path is adopted only after that save succeeds. Otherwise the
    /// wallet would keep a path it never wrote, and every later background save
    /// would fail silently while the wallet appeared to persist.
    pub fn set_utxo_path(&mut self, path: PathBuf) -> std::io::Result<()> {
        let new_lock = if self.utxo_path.as_deref() == Some(path.as_path()) {
            None
        } else {
            Some(crate::utxo::lock_state_path(&path)?)
        };
        self.utxos.lock().save(&path)?;
        self.utxo_path = Some(path);
        if let Some(lock) = new_lock {
            self.state_lock = Some(lock);
        }
        Ok(())
    }

    /// Save the current UTXO set to disk (if a persistence path is set).
    ///
    /// Only for call sites where a failed write is survivable (background
    /// bookkeeping): the error is logged. A path whose in-memory change must be
    /// durable or undone calls [`BtcWallet::try_save_utxos`] instead.
    fn save_utxos(&self) {
        if let Err(error) = self.try_save_utxos() {
            tracing::warn!("Failed to save UTXO set: {}", error);
        }
    }

    /// Save the current UTXO set, reporting a failed write to the caller.
    fn try_save_utxos(&self) -> std::io::Result<()> {
        if let Some(ref path) = self.utxo_path {
            self.utxos.lock().save(path)?;
        }
        Ok(())
    }

    fn reset_chain_cache(&mut self) {
        self.reset_chain_cache_from(None);
    }

    fn reset_chain_cache_from(&mut self, _fork: Option<u32>) {
        self.rescan_on_sync.store(true, Ordering::Release);
        *self.completed_scan.lock() = None;
        self.filter_headers.lock().clear();
        {
            let mut utxos = self.utxos.lock();
            utxos.clear_chain_utxos();
            // A full reset abandons the whole journal: keeping entries from the
            // old branch would let a later journal claim coverage it lost.
            utxos.clear_journal();
            utxos.set_last_scanned_tip(None);
            utxos.set_last_scanned_height(0);
            utxos.set_rescan_required(true);
        }
        self.last_scanned_height = 0;
        self.synced = false;
        self.journal_recovered.store(false, Ordering::Release);
        self.recovery_rescan_required
            .store(false, Ordering::Release);
        self.save_utxos();
    }

    /// Roll the wallet back to `fork` by replaying the undo journal.
    ///
    /// Returns false without changing anything when the journal does not cover
    /// every height above `fork`; the caller then falls back to a full reset.
    /// A replayed reorg resumes scanning at `fork + 1`, so outputs at or below
    /// the fork survive and only a rescan of the shortened branch is required.
    fn reset_chain_cache_from_journal(&mut self, fork: u32) -> bool {
        let next_height = fork.saturating_add(1);
        let rescan_owed_before = {
            let mut utxos = self.utxos.lock();
            if !utxos.undo_above(fork) {
                return false;
            }
            let owed = utxos.rescan_required();
            utxos.set_last_scanned_height(next_height);
            owed
        };
        *self.completed_scan.lock() = None;
        self.filter_headers.lock().clear();
        self.last_scanned_height = next_height;
        // The journal already rewound every height above the fork and the scan
        // resumes at fork + 1, so nothing below the fork — or below the retained
        // floor, which coverage guarantees is at most fork + 1 — is re-derived.
        // Mark the rescan journal-recovered so the missing-history guard and
        // alert stay quiet, then hide the balance until the scan completes.
        // A rescan the wallet already owed survives: the rollback only proves
        // nothing below the fork needs re-deriving.
        self.recovery_rescan_required
            .store(rescan_owed_before, Ordering::Release);
        self.journal_recovered.store(true, Ordering::Release);
        self.rescan_on_sync.store(true, Ordering::Release);
        self.save_utxos();
        true
    }

    /// The Bitcoin network this wallet operates on.
    pub fn network(&self) -> Network {
        self.network
    }

    /// Derive the next unused receiving address.
    pub fn next_address(&mut self) -> Result<String> {
        let addr = derive_address(&self.seed, self.next_index, self.network)?;
        let next_index = self
            .next_index
            .checked_add(1)
            .ok_or_else(|| crate::error::BtcError::Bitcoin("BTC address index overflow".into()))?;
        {
            let mut set = self.utxos.lock();
            let previous = set.clone();
            set.set_next_index(next_index);
            if let Some(path) = &self.utxo_path {
                if let Err(error) = set.save(path) {
                    if crate::utxo::save_may_have_committed(&error) {
                        self.next_index = next_index;
                    } else {
                        *set = previous;
                    }
                    return Err(crate::error::BtcError::Bitcoin(format!(
                        "Could not persist BTC address index: {error}"
                    )));
                }
            }
        }
        self.next_index = next_index;
        Ok(addr)
    }

    /// The current receiving address (without advancing).
    pub fn current_address(&self) -> Result<String> {
        derive_address(&self.seed, self.next_index, self.network)
    }

    /// Derive the WIF private key for the given index.
    pub fn derive_wif(&self, index: u32) -> Result<String> {
        crate::keys::derive_wif(&self.seed, index, self.network)
    }

    /// Find the signing key for an address issued by this wallet.
    pub fn derive_wif_for_address(&self, address: &str) -> Result<String> {
        for index in 0..=self.next_index {
            if derive_address(&self.seed, index, self.network)? == address {
                return self.derive_wif(index);
            }
        }
        Err(crate::error::BtcError::InvalidAddress(
            "Address does not belong to this BTC wallet".into(),
        ))
    }

    /// Derive a gap of addresses (indices 0..=gap) for scanning.
    ///
    /// The wallet must watch a range of derived addresses, not just the
    /// current one, so funds sent to any previously-issued address are still
    /// discovered.
    pub fn watch_addresses(&self, gap: u32) -> Result<Vec<String>> {
        let mut addrs = Vec::with_capacity(gap as usize + 1);
        for i in 0..=gap {
            addrs.push(derive_address(&self.seed, i, self.network)?);
        }
        Ok(addrs)
    }

    /// Total confirmed balance in satoshis.
    ///
    /// Restored chain outputs are hidden until the first successful sync has
    /// rebuilt and rescanned the header lineage.
    pub fn balance(&self) -> u64 {
        if self.rescan_on_sync.load(Ordering::Acquire) {
            return 0;
        }
        self.utxos.lock().total()
    }

    /// List spendable UTXOs.
    ///
    /// Restored chain outputs are hidden until the first successful sync has
    /// rebuilt and rescanned the header lineage.
    pub fn list_utxos(&self) -> Vec<Utxo> {
        if self.rescan_on_sync.load(Ordering::Acquire) {
            return Vec::new();
        }
        self.utxos.lock().list().to_vec()
    }

    /// Exclude a swap funding input from selection and future rescans before broadcast.
    pub fn reserve_swap_input(
        &self,
        input: &Utxo,
        raw: &[u8],
        order_id: String,
        htlc: &crate::htlc::BtcHtlc,
    ) -> Result<()> {
        if self.rescan_on_sync.load(Ordering::Acquire) {
            return Err(crate::error::BtcError::NotSynced);
        }
        let transaction: bitcoin::Transaction = bitcoin::consensus::deserialize(raw)
            .map_err(|error| crate::error::BtcError::Bitcoin(error.to_string()))?;
        let mut set = self.utxos.lock();
        if !set.list().iter().any(|utxo| {
            utxo.txid == input.txid
                && utxo.vout == input.vout
                && utxo.value == input.value
                && utxo.address == input.address
        }) {
            return Err(crate::error::BtcError::Bitcoin(
                "BTC funding input is no longer available".into(),
            ));
        }
        let previous = set.clone();
        set.reserve_utxo(input.clone());
        set.record_swap_transaction(transaction.compute_txid().to_string(), raw.to_vec());
        set.record_swap_contract(crate::utxo::SwapContract {
            order_id,
            funding_txid: transaction.compute_txid().to_string(),
            hash_lock: htlc.hash_lock,
            recipient: htlc.recipient.clone(),
            refund_address: htlc.refund_address.clone(),
            expiry: htlc.expiry,
            amount: htlc.amount,
            refund_raw: None,
            input_txid: Some(input.txid.clone()),
            input_vout: Some(input.vout),
            input_released: false,
        });
        if let Some(path) = &self.utxo_path {
            if let Err(error) = set.save(path) {
                if !crate::utxo::save_may_have_committed(&error) {
                    *set = previous;
                }
                return Err(crate::error::BtcError::Bitcoin(format!(
                    "Could not persist BTC input reservation: {error}"
                )));
            }
        }
        Ok(())
    }

    /// Signed swap transactions retained for reconciliation after restart.
    pub fn pending_swap_transactions(&self) -> std::collections::BTreeMap<String, Vec<u8>> {
        self.utxos.lock().pending_swap_transactions().clone()
    }

    /// Sent transactions whose confirming spend has not been observed.
    ///
    /// A retained raw transaction can be rebroadcast after a restart instead
    /// of releasing inputs whose delivery is uncertain.
    pub fn pending_sends(&self) -> Vec<PendingSend> {
        self.utxos.lock().pending_sends()
    }

    /// The signed bytes retained for a pending send, if recorded.
    pub fn pending_send_raw(&self, txid: &str) -> Option<Vec<u8>> {
        self.pending_send(txid).and_then(|send| send.raw)
    }

    /// The pending record for `txid`, including records without signed bytes.
    pub fn pending_send(&self, txid: &str) -> Option<PendingSend> {
        self.pending_sends()
            .into_iter()
            .find(|send| send.txid == txid)
    }

    pub fn swap_contract(&self, order_id: &str) -> Option<crate::utxo::SwapContract> {
        self.utxos.lock().swap_contract(order_id).cloned()
    }

    /// All persisted swap contracts, for startup reconciliation.
    pub fn swap_contracts(&self) -> Vec<crate::utxo::SwapContract> {
        self.utxos.lock().swap_contracts()
    }

    pub fn reserved_utxo(&self, txid: &str, vout: u32) -> Option<Utxo> {
        self.utxos.lock().reserved_utxo(txid, vout).cloned()
    }

    pub fn reserved_input_spent(&self, txid: &str, vout: u32) -> bool {
        self.utxos.lock().was_reserved_spent(txid, vout)
    }

    /// Scan from the reserved input's confirmation height and report whether a
    /// confirmed transaction spent it. A spent reserved input proves the signed
    /// funding transaction can no longer confirm.
    ///
    /// The scan runs against a scratch set seeded with the reservation, never
    /// the shared set: a reconciliation can start far below the scan cursor,
    /// and rewriting shared chain state from an old height would change it with
    /// no journal entry covering that height. Only the spend outcome is applied.
    /// If the spend predates the retained floor the scan cannot see it and
    /// reports not spent, leaving the reservation in place.
    pub async fn scan_reserved_input_spent(
        &self,
        txid: &str,
        vout: u32,
        peers: &[std::net::SocketAddr],
    ) -> Result<bool> {
        let Some(utxo) = self.reserved_utxo(txid, vout) else {
            return Ok(false);
        };
        let addresses = self.watch_addresses(self.next_index)?;
        let mut spent = false;
        for addr in peers {
            let Ok(mut peer) = crate::p2p::BtcPeer::connect_with_network(*addr, self.network).await
            else {
                continue;
            };
            let mut scratch = UtxoSet::new();
            scratch.reserve_utxo(utxo.clone());
            let scratch = Arc::new(Mutex::new(scratch));
            let sync = crate::sync::BtcSync::new_with_filter_headers(
                self.headers.clone(),
                scratch.clone(),
                self.filter_headers.clone(),
                addresses.clone(),
                self.network,
            );
            if sync.scan_utxos_bip158(&mut peer, utxo.height).await.is_ok()
                && scratch.lock().was_reserved_spent(txid, vout)
            {
                spent = true;
                break;
            }
        }
        if spent {
            // The observation is only as old as the chain the scan saw, so
            // record the tip it was made against: a rollback above that tip
            // puts the reservation back, one below it leaves it spent.
            let observed_at = self.headers.lock().best_height();
            let mut set = self.utxos.lock();
            if set.note_spend(txid, vout) {
                // No height carries this observation, so remember it as
                // live-process state: a covered rollback puts the reservation
                // back instead of leaving the outpoint spent forever.
                set.record_reservation_spend(utxo, observed_at);
                drop(set);
                self.save_utxos();
            }
        }
        Ok(spent || self.reserved_input_spent(txid, vout))
    }

    /// Release the input reservation for `order_id` and drop its contract.
    ///
    /// The caller must have established that the funding transaction is not
    /// live (not in the chain and not observed in the mempool) and that the
    /// HTLC has expired. Releasing an input whose signed funding tx could still
    /// confirm would let a second swap spend the same outpoint.
    ///
    /// Returns true if a reservation was released. Persists atomically; a
    /// failed save rolls the in-memory set back.
    pub fn release_swap_reservation(&self, order_id: &str) -> Result<bool> {
        let mut set = self.utxos.lock();
        let Some(contract) = set.swap_contract(order_id).cloned() else {
            return Ok(false);
        };
        let (Some(txid), Some(vout)) = (contract.input_txid.clone(), contract.input_vout) else {
            // Contract predates input tracking; nothing to release safely.
            return Ok(false);
        };
        let previous = set.clone();
        let reserved = set.reserved_utxo(&txid, vout).cloned();
        let spent = set.was_reserved_spent(&txid, vout);
        let released = set.release(&txid, vout);
        set.remove_swap_contract(order_id);
        set.clear_spent_reserved(&txid, vout);
        // The observation exists to put the reservation back after a covered
        // rollback. With the contract gone nothing could release it again, so
        // keeping it would hide the outpoint on the next covered reorg.
        set.forget_reservation_spend(&txid, vout);
        if released && !spent {
            if let Some(utxo) = reserved {
                set.add(utxo);
            }
        }
        if let Some(path) = &self.utxo_path {
            if let Err(error) = set.save(path) {
                if !crate::utxo::save_may_have_committed(&error) {
                    *set = previous;
                }
                return Err(crate::error::BtcError::Bitcoin(format!(
                    "Could not persist BTC reservation release: {error}"
                )));
            }
        }
        Ok(released || spent)
    }

    /// Release the reserved input but keep the swap contract.
    ///
    /// Used when a partial scan cannot prove the funding transaction absent:
    /// the outpoint is freed, while `refund_raw` and the HTLC terms stay for a
    /// later covered scan or manual recovery.
    pub fn release_swap_input_keep_contract(&self, order_id: &str) -> Result<bool> {
        let mut set = self.utxos.lock();
        let previous = set.clone();
        let released = set.release_swap_input_keep_contract(order_id);
        if let Some(path) = &self.utxo_path {
            if let Err(error) = set.save(path) {
                if !crate::utxo::save_may_have_committed(&error) {
                    *set = previous;
                }
                return Err(crate::error::BtcError::Bitcoin(format!(
                    "Could not persist BTC reservation release: {error}"
                )));
            }
        }
        Ok(released)
    }

    pub fn record_swap_refund(&self, order_id: &str, raw: &[u8]) -> Result<()> {
        let mut set = self.utxos.lock();
        let mut contract = set.swap_contract(order_id).cloned().ok_or_else(|| {
            crate::error::BtcError::Bitcoin("Persisted BTC swap contract missing".into())
        })?;
        let previous = set.clone();
        contract.refund_raw = Some(raw.to_vec());
        set.record_swap_contract(contract);
        if let Some(path) = &self.utxo_path {
            if let Err(error) = set.save(path) {
                if !crate::utxo::save_may_have_committed(&error) {
                    *set = previous;
                }
                return Err(crate::error::BtcError::Bitcoin(format!(
                    "Could not persist BTC refund: {error}"
                )));
            }
        }
        Ok(())
    }

    /// Best known header height.
    pub fn best_height(&self) -> u32 {
        self.headers.lock().best_height()
    }

    /// Best known header hash.
    pub fn best_hash(&self) -> Option<[u8; 32]> {
        self.headers.lock().best_hash()
    }

    /// Oldest BTC height still available for scanning.
    pub fn retained_floor(&self) -> u32 {
        self.headers.lock().retained_floor()
    }

    /// Snapshot of the chain-derived undo journal, oldest entry first.
    #[cfg(test)]
    pub(crate) fn undo_entries_for_tests(&self) -> Vec<crate::utxo::UndoEntry> {
        self.utxos.lock().undo_entries().to_vec()
    }

    /// Wallet-local spends the scan has not observed yet.
    #[cfg(test)]
    pub(crate) fn pending_removal_count_for_tests(&self) -> usize {
        self.utxos.lock().pending_removals().len()
    }

    /// Append a journal entry as a scan would.
    #[cfg(test)]
    pub(crate) fn push_undo_for_tests(&self, entry: crate::utxo::UndoEntry) {
        self.utxos.lock().push_undo(entry);
    }

    /// Recover from `previous_best` having been orphaned.
    ///
    /// Rolls chain state back to the highest proven common ancestor, replaying
    /// the journal when it covers the fork and falling back to a full rescan
    /// otherwise. Returns true when the journal covered the fork.
    fn reset_for_reorg(&mut self, previous_best: [u8; 32]) -> bool {
        let fork = {
            let chain = self.headers.lock();
            find_fork(&chain, previous_best)
        };
        self.reset_for_reorg_at(fork)
    }

    /// Apply the reorg decision for a known fork.
    ///
    /// `None` means no common ancestor is retained, which forces the full
    /// reset: the journal path may only claim a proven ancestor.
    fn reset_for_reorg_at(&mut self, fork: Option<u32>) -> bool {
        if let Some(fork) = fork {
            if self.reset_chain_cache_from_journal(fork) {
                tracing::warn!(
                    "BTC header reorg detected near height {fork}; replayed the undo journal back to the fork"
                );
                return true;
            }
        }
        self.reset_chain_cache_from(fork);
        match fork {
            Some(fork) => tracing::warn!(
                "BTC header reorg detected near height {fork}; journal does not cover the fork, cleared chain state for a full rescan"
            ),
            None => tracing::warn!(
                "BTC header reorg abandoned a pruned branch; no common ancestor is retained, cleared chain state for a full rescan"
            ),
        }
        false
    }

    /// Take the reorg decision for `fork` as a header sync would.
    #[cfg(test)]
    pub(crate) fn reset_for_reorg_at_for_tests(&mut self, fork: Option<u32>) -> bool {
        self.reset_for_reorg_at(fork)
    }

    /// Take the reorg path a header sync would take for `previous_best`.
    #[cfg(test)]
    pub(crate) fn reset_for_reorg_for_tests(&mut self, previous_best: [u8; 32]) -> bool {
        self.reset_for_reorg(previous_best)
    }

    /// The wallet's record of a spendable outpoint.
    #[cfg(test)]
    pub(crate) fn utxo_for_tests(&self, txid: &str, vout: u32) -> Option<Utxo> {
        self.utxos
            .lock()
            .list()
            .iter()
            .find(|utxo| utxo.txid == txid && utxo.vout == vout)
            .cloned()
    }

    /// Drop a spendable outpoint as a confirmed spend would.
    #[cfg(test)]
    pub(crate) fn remove_utxo_for_tests(&self, txid: &str, vout: u32) {
        self.utxos.lock().remove(txid, vout);
    }

    /// Live-process reservation observations the set is holding.
    #[cfg(test)]
    pub(crate) fn observed_reservation_spend_count_for_tests(&self) -> usize {
        self.utxos.lock().observed_reservation_spend_count()
    }

    /// Whether the persisted UTXO set wants a full rescan.
    #[cfg(test)]
    pub(crate) fn rescan_required_for_tests(&self) -> bool {
        self.utxos.lock().rescan_required()
    }

    /// Whether the balance and spendable list are hidden pending a rescan.
    #[cfg(test)]
    pub(crate) fn rescan_on_sync_for_tests(&self) -> bool {
        self.rescan_on_sync.load(Ordering::Acquire)
    }

    /// Whether the pending rescan replays from a journalled fork.
    #[cfg(test)]
    pub(crate) fn journal_recovered_for_tests(&self) -> bool {
        self.journal_recovered.load(Ordering::Acquire)
    }

    /// Freshly scan a swap contract, without trusting the persisted wallet UTXO cache.
    pub async fn verify_swap_funding(
        &self,
        htlc: &crate::htlc::BtcHtlc,
        funding_txid: [u8; 32],
        peers: &[std::net::SocketAddr],
        now: u64,
    ) -> Result<()> {
        if htlc.network != self.network {
            return Err(crate::error::BtcError::Sync(
                "BTC swap network mismatch".into(),
            ));
        }
        let sync = crate::sync::BtcSync::new(
            Arc::new(Mutex::new(self.headers.lock().clone())),
            Arc::new(Mutex::new(UtxoSet::new())),
            vec![htlc.address()?],
            self.network,
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(120),
            sync.verify_swap_funding(htlc, funding_txid, peers, now),
        )
        .await
        .map_err(|_| crate::error::BtcError::Sync("BTC contract verification timed out".into()))?
    }

    /// Observe confirmed funding and spends without disclosing a preimage or trusting cached UTXOs.
    pub async fn observe_swap(
        &self,
        htlc: &crate::htlc::BtcHtlc,
        funding_txid: [u8; 32],
        peers: &[std::net::SocketAddr],
        now: u64,
        previous: &[crate::sync::SwapAnchor],
    ) -> Result<crate::sync::SwapScan> {
        use crate::error::BtcError;
        if htlc.network != self.network {
            return Err(BtcError::Sync("BTC swap network mismatch".into()));
        }
        let headers = Arc::new(Mutex::new(self.headers.lock().clone()));
        let sync = crate::sync::BtcSync::new(
            headers.clone(),
            Arc::new(Mutex::new(UtxoSet::new())),
            vec![htlc.address()?],
            self.network,
        );
        let mut scan = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            sync.scan_swap_contract(htlc, funding_txid, peers, now, false),
        )
        .await
        .map_err(|_| BtcError::Sync("BTC settlement scan timed out".into()))??;
        let chain = headers.lock();
        if chain.best_hash() != Some(scan.tip_hash) {
            return Err(BtcError::Sync(
                "BTC tip changed after settlement scan".into(),
            ));
        }
        scan.invalidated_anchor = previous
            .iter()
            .any(|anchor| !chain.is_active_anchor(&anchor.block_hash, anchor.height));
        Ok(scan)
    }

    /// Add a header to the chain.
    pub fn add_header(&self, raw: &[u8], height: u32) -> Result<()> {
        self.headers.lock().add_header(raw, height)
    }

    /// Add a UTXO.
    pub fn add_utxo(&self, utxo: Utxo) {
        if self.rescan_on_sync.load(Ordering::Acquire) {
            return;
        }
        self.utxos.lock().add(utxo);
        self.save_utxos();
    }

    /// Whether the header chain has synced at least once.
    pub fn synced(&self) -> bool {
        self.synced
    }

    /// Mark the wallet as synced.
    pub fn mark_synced(&mut self) {
        self.synced = true;
    }

    /// Run a sync pass against a peer, updating headers and the synced flag.
    pub async fn sync(&mut self, peer: &mut crate::p2p::BtcPeer) -> Result<usize> {
        let previous_best = self.headers.lock().best_hash();
        let staged_headers = Arc::new(Mutex::new(self.headers.lock().clone()));
        let sync = crate::sync::BtcSync::new_with_filter_headers(
            staged_headers.clone(),
            self.utxos.clone(),
            self.filter_headers.clone(),
            self.watch_addresses(self.next_index)?,
            self.network,
        );
        let added = sync.sync_once(peer).await?;
        drop(sync);
        let staged_headers = Arc::try_unwrap(staged_headers)
            .map_err(|_| BtcError::Sync("BTC header staging is still shared".into()))?
            .into_inner();
        *self.headers.lock() = staged_headers;
        if self.reset_on_sync {
            self.reset_chain_cache();
            self.reset_on_sync = false;
            tracing::warn!(
                "BTC headers were rebuilt without persisted lineage; cleared restored chain cache for a full wallet rescan"
            );
        }
        if let Some(previous_best) = previous_best {
            let reorged = {
                let chain = self.headers.lock();
                match chain.best_hash() {
                    Some(new_best) if new_best != previous_best => {
                        !is_ancestor(&chain, previous_best, new_best)
                    }
                    _ => false,
                }
            };
            if reorged {
                self.reset_for_reorg(previous_best);
            }
        }
        if added > 0 {
            self.synced = true;
        }
        {
            let chain = self.headers.lock();
            self.filter_headers.lock().prune(&chain);
        }
        Ok(added)
    }

    /// Sync headers with a responsive peer, removing connections that fail.
    pub async fn sync_with_peers(&mut self, peers: &mut Vec<crate::p2p::BtcPeer>) -> Result<usize> {
        let mut last_error = BtcError::Sync("no BTC peers available".into());
        while !peers.is_empty() {
            match self.sync(&mut peers[0]).await {
                Ok(added) => return Ok(added),
                Err(error) => {
                    let peer = peers.remove(0);
                    tracing::warn!("BTC header peer {} failed: {}", peer.addr(), error);
                    last_error = error;
                }
            }
        }
        Err(last_error)
    }

    /// Scan blocks using BIP-158 compact block filters (the modern,
    /// privacy-preserving alternative to BIP-37, which most mainnet nodes
    /// disable). Returns the number of blocks scanned.
    pub async fn scan_utxos_bip158(
        &self,
        peer: &mut crate::p2p::BtcPeer,
        start_height: u32,
    ) -> Result<usize> {
        self.require_retained_wallet_history()?;
        let sync = crate::sync::BtcSync::new_with_filter_headers(
            self.headers.clone(),
            self.utxos.clone(),
            self.filter_headers.clone(),
            self.watch_addresses(self.next_index)?,
            self.network,
        );
        let tip = self.headers.lock().best_hash();
        let scanned = sync.scan_utxos_bip158(peer, start_height).await?;
        self.finish_scan_batch(start_height, scanned, tip)?;
        Ok(scanned)
    }

    pub async fn scan_utxos_bip158_multi(
        &self,
        peers: &mut [crate::p2p::BtcPeer],
        start_height: u32,
    ) -> Result<usize> {
        self.require_retained_wallet_history()?;
        let sync = crate::sync::BtcSync::new_with_filter_headers(
            self.headers.clone(),
            self.utxos.clone(),
            self.filter_headers.clone(),
            self.watch_addresses(self.next_index)?,
            self.network,
        );
        let tip = self.headers.lock().best_hash();
        let scanned = sync.scan_utxos_bip158_multi(peers, start_height).await?;
        self.finish_scan_batch(start_height, scanned, tip)?;
        Ok(scanned)
    }

    fn finish_scan_batch(&self, start: u32, scanned: usize, tip: Option<[u8; 32]>) -> Result<()> {
        if scanned > 0 {
            self.try_save_utxos().map_err(|error| {
                BtcError::Sync(format!("BTC scan state could not be saved: {error}"))
            })?;
        }
        self.note_completed_scan(start, scanned, tip);
        Ok(())
    }

    fn require_retained_wallet_history(&self) -> Result<()> {
        if self.journal_recovered.load(Ordering::Acquire) {
            // The covered rollback kept everything at or below the fork and the
            // scan starts at fork + 1, so no earlier history is needed — but
            // only while the retained floor has not advanced past that cursor.
            // Once it has, the skipped range can no longer be re-derived and
            // the ordinary case 2/3 logic (and alert) must decide.
            if self.recovery_exemption_active() {
                return Ok(());
            }
            self.abandon_journal_recovery();
        }
        let floor = self.headers.lock().retained_floor();
        enum Decision {
            Ready,
            Adopted,
            Blocked,
        }
        let decision = {
            let mut utxos = self.utxos.lock();
            match utxos.birthday() {
                None => {
                    // A wallet that never scanned has no earlier history to
                    // lose: the retained floor is the earliest height it can
                    // ever have received funds at.
                    utxos.set_birthday(Some(floor));
                    Decision::Adopted
                }
                Some(birthday) if birthday >= floor => Decision::Ready,
                Some(_) if !utxos.has_seen_funds() => {
                    // The floor advanced past the birthday, but this wallet has
                    // never held an output, so nothing below it can be lost.
                    utxos.set_birthday(Some(floor));
                    Decision::Adopted
                }
                Some(_) => Decision::Blocked,
            }
        };
        match decision {
            Decision::Ready => Ok(()),
            Decision::Adopted => {
                self.save_utxos();
                Ok(())
            }
            Decision::Blocked => {
                self.rescan_on_sync.store(true, Ordering::Release);
                self.utxos.lock().set_rescan_required(true);
                self.save_utxos();
                Err(BtcError::Sync(format!(
                    "wallet history before retained header floor {floor} is unavailable; restart with sufficient header retention"
                )))
            }
        }
    }

    /// Earliest height this wallet intends to scan.
    pub fn birthday(&self) -> Option<u32> {
        self.utxos.lock().birthday()
    }

    /// Record the earliest height this wallet intends to cover.
    pub fn set_birthday(&mut self, birthday: u32) -> bool {
        let mut utxos = self.utxos.lock();
        let previous = utxos.birthday();
        utxos.set_birthday(Some(birthday));
        if let Some(path) = &self.utxo_path {
            if let Err(error) = utxos.save(path) {
                utxos.set_birthday(previous);
                tracing::warn!("Failed to save BTC birthday: {}", error);
                return false;
            }
        }
        true
    }

    pub fn set_initial_birthday(&mut self, birthday: u32) -> bool {
        let mut utxos = self.utxos.lock();
        let previous = utxos.clone();
        utxos.set_birthday(Some(birthday));
        utxos.set_last_scanned_height(birthday);
        utxos.set_last_scanned_tip(None);
        if let Some(path) = &self.utxo_path {
            if let Err(error) = utxos.save(path) {
                *utxos = previous;
                tracing::warn!("Failed to save BTC initial birthday: {}", error);
                return false;
            }
        }
        self.last_scanned_height = birthday;
        true
    }

    /// True when a required rescan would start below the retained header floor.
    ///
    /// The floor advances as headers arrive, so a wallet that keeps scanning
    /// incrementally can end up with a birthday below the current floor while
    /// still holding complete history. Only a wallet that needs to rescan is
    /// affected.
    ///
    /// A journal-recovered rescan is exempt only while the resumed cursor is
    /// still at or above the retained floor: it only re-reads heights above the
    /// fork, so nothing below the floor can be lost.
    pub fn history_unavailable(&self) -> bool {
        if !self.rescan_on_sync.load(Ordering::Acquire) {
            return false;
        }
        if self.journal_recovered.load(Ordering::Acquire) {
            if self.recovery_exemption_active() {
                return false;
            }
            self.abandon_journal_recovery();
        }
        let floor = self.retained_floor();
        let utxos = self.utxos.lock();
        utxos.has_seen_funds() && utxos.birthday().is_some_and(|birthday| birthday < floor)
    }

    /// Whether the balance and spendable list are currently withheld.
    ///
    /// The status APIs report `balance()` beside `synced`, so without this a
    /// wallet mid-recovery or blocked by the history guard would render as
    /// "0 BTC / Synced" with no indication that the number is hidden.
    ///
    /// Both terms are needed: `history_unavailable` is the case-3 alert, and it
    /// short-circuits to false while the journal-recovery exemption is active —
    /// exactly the window a covered rollback hides the balance in.
    pub fn balance_hidden(&self) -> bool {
        self.rescan_on_sync.load(Ordering::Acquire) || self.history_unavailable()
    }

    /// Whether the journal-recovery exemption is still sound.
    ///
    /// The covered rollback resumes at `fork + 1`; if the retained floor has
    /// since advanced past that cursor, the scan would start above the range the
    /// journal undid and could not re-derive it. The exemption then has to be
    /// withdrawn so the regular history guard decides.
    fn recovery_exemption_active(&self) -> bool {
        self.journal_recovered.load(Ordering::Acquire)
            && self.last_scanned_height >= self.retained_floor()
    }

    /// Withdraw a journal recovery whose resumed cursor fell below the retained
    /// floor.
    ///
    /// Clears the journal and tip and sets `rescan_required` so the ordinary
    /// case 2/3 path (including the critical alert) decides what happens next.
    /// Returns true if a recovery was actually abandoned.
    pub fn abandon_journal_recovery(&self) -> bool {
        if !self.journal_recovered.swap(false, Ordering::AcqRel) {
            return false;
        }
        {
            let mut utxos = self.utxos.lock();
            utxos.clear_journal();
            utxos.set_last_scanned_tip(None);
            utxos.set_rescan_required(true);
        }
        // The requirement now outlives the recovery, so a later completion
        // must not clear it.
        self.recovery_rescan_required.store(true, Ordering::Release);
        tracing::warn!(
            "BTC journal recovery abandoned: the retained header floor advanced past the resumed scan cursor; cleared the journal so the history guard decides"
        );
        self.save_utxos();
        true
    }

    /// The next block height to scan.
    pub fn last_scanned_height(&self) -> u32 {
        self.last_scanned_height
    }

    fn note_completed_scan(&self, start: u32, scanned: usize, tip: Option<[u8; 32]>) {
        if !self.rescan_on_sync.load(Ordering::Acquire) || self.reset_on_sync || scanned == 0 {
            return;
        }
        let chain = self.headers.lock();
        let Some(tip) = tip else { return };
        let end = start.saturating_add(scanned as u32);
        if chain.best_hash() == Some(tip)
            && start <= self.last_scanned_height.max(chain.retained_floor())
            && end > chain.best_height()
        {
            *self.completed_scan.lock() = Some((end, tip));
        }
    }

    /// The header tip recorded alongside the cursor, when known.
    pub fn last_scanned_tip(&self) -> Option<[u8; 32]> {
        self.utxos.lock().last_scanned_tip()
    }

    /// Set the next block height to scan and persist it with the UTXO set.
    ///
    /// Clears the recorded header tip: the tip belongs to the journal pair, and
    /// a cursor written without one must not claim a lineage it cannot prove.
    pub fn set_last_scanned_height(&mut self, height: u32) -> bool {
        self.persist_scan_progress(height, None)
    }

    /// Set the next block height and the header tip it was scanned against,
    /// persisted together with the UTXO set in a single save.
    pub fn set_scan_progress(&mut self, next_height: u32, tip: Option<[u8; 32]>) -> bool {
        self.persist_scan_progress(next_height, tip)
    }

    fn persist_scan_progress(&mut self, height: u32, tip: Option<[u8; 32]>) -> bool {
        let chain = self.headers.lock();
        let completed_scan = self.completed_scan.lock().take();
        let completed = completed_scan.is_some_and(|(end, tip_hash)| {
            end == height && chain.best_hash() == Some(tip_hash) && height > chain.best_height()
        });
        let mut utxos = self.utxos.lock();
        let previous = utxos.clone();
        utxos.set_last_scanned_height(height);
        utxos.set_last_scanned_tip(tip);
        utxos.prune_undo(chain.retained_floor(), MAX_UNDO_ENTRIES);
        let rescan_still_owed = completed && self.recovery_rescan_required.load(Ordering::Acquire);
        if completed && !rescan_still_owed {
            utxos.set_rescan_required(false);
        }
        let persisted = match &self.utxo_path {
            Some(path) => match utxos.save(path) {
                Ok(()) => true,
                Err(error) => {
                    tracing::warn!("Failed to save UTXO set: {}", error);
                    *utxos = previous;
                    *self.completed_scan.lock() = completed_scan;
                    false
                }
            },
            None => true,
        };
        drop(utxos);
        if !persisted {
            return false;
        }
        self.last_scanned_height = height;
        if completed && persisted {
            // A rescan the wallet owed before the rollback is not satisfied by
            // re-deriving the abandoned branch: keep the balance hidden and the
            // guard armed so the critical alert stays up.
            if !rescan_still_owed {
                self.rescan_on_sync.store(false, Ordering::Release);
            }
            self.journal_recovered.store(false, Ordering::Release);
        }
        true
    }

    /// Complete a journal-recovered rescan that has nothing left to scan.
    ///
    /// A covered rollback resumes at `fork + 1`; when the tip never moved past
    /// the fork (or the scan finds no blocks) no `set_scan_progress` call runs,
    /// so without this the recovery would leave the balance hidden and the
    /// history guard short-circuited indefinitely. The journal already restored
    /// everything at or below the fork, so recovery ends here: the exemption is
    /// dropped, the balance becomes visible, and the rescan requirement clears.
    /// A no-op for any wallet that is not journal-recovered.
    pub fn finish_recovered_scan(&mut self) -> bool {
        if !self.journal_recovered.load(Ordering::Acquire) {
            return true;
        }
        let rescan_owed = self.recovery_rescan_required.load(Ordering::Acquire);
        let mut utxos = self.utxos.lock();
        let previous = utxos.rescan_required();
        if !rescan_owed {
            utxos.set_rescan_required(false);
        }
        if let Some(path) = &self.utxo_path {
            if let Err(error) = utxos.save(path) {
                utxos.set_rescan_required(previous);
                tracing::warn!("Failed to save BTC recovery completion: {}", error);
                return false;
            }
        }
        drop(utxos);
        self.journal_recovered.store(false, Ordering::Release);
        if !rescan_owed {
            self.rescan_on_sync.store(false, Ordering::Release);
        }
        true
    }

    /// Whether new transactions signal RBF.
    pub fn rbf_enabled(&self) -> bool {
        self.rbf_enabled
    }

    /// Enable or disable RBF signaling for new transactions.
    pub fn set_rbf_enabled(&mut self, enabled: bool) {
        self.rbf_enabled = enabled;
    }

    /// Send BTC to `to_address`, selecting UTXOs, signing, and removing
    /// spent UTXOs from the in-memory set.
    ///
    /// `fee_sats` is a flat fee in satoshis (default 1 000).
    /// Returns `(txid_hex, raw_tx_bytes)` — the caller should broadcast
    /// `raw_tx_bytes` to the network.
    ///
    /// Fails, leaving the in-memory set untouched, when the removal cannot be
    /// persisted: an unpersisted removal is lost on restart, and the rescan
    /// would offer the same outpoints for a second spend.
    ///
    /// Only works when all selected UTXOs belong to the same derived address.
    pub fn send_to(
        &mut self,
        to_address: &str,
        amount_sats: u64,
        fee_sats: u64,
    ) -> Result<(String, Vec<u8>, Vec<Utxo>)> {
        if self.rescan_on_sync.load(Ordering::Acquire) {
            return Err(crate::error::BtcError::NotSynced);
        }
        use crate::keys::derive_wif;
        use crate::tx::{build_and_sign, txid_of};

        let selected = {
            let u = self.utxos.lock();
            match u.select(amount_sats, fee_sats) {
                Some(s) => s,
                None => {
                    let available = u.total();
                    return Err(crate::error::BtcError::InsufficientFunds {
                        available,
                        required: amount_sats.saturating_add(fee_sats),
                    });
                }
            }
        };

        let change_address = self.current_address()?;

        // Find the WIF whose derived address matches the selected UTXOs.
        let mut wif: Option<zeroize::Zeroizing<String>> = None;
        for idx in 0..=self.next_index {
            let addr = derive_address(&self.seed, idx, self.network)?;
            if selected.iter().any(|u| u.address == addr) {
                if wif.is_some() {
                    return Err(crate::error::BtcError::InvalidAddress(
                        "send_to requires all UTXOs to belong to the same address index".into(),
                    ));
                }
                wif = Some(zeroize::Zeroizing::new(derive_wif(
                    &self.seed,
                    idx,
                    self.network,
                )?));
            }
        }
        let wif = wif.ok_or_else(|| {
            crate::error::BtcError::InvalidAddress("no matching address for selected UTXOs".into())
        })?;

        let raw = build_and_sign(
            &selected,
            to_address,
            amount_sats,
            fee_sats,
            &change_address,
            wif.as_str(),
            self.network,
            self.rbf_enabled,
        )?;

        let txid_hex = txid_of(&raw)?.to_string();

        // Hold the selected inputs and persist the signed send before any
        // broadcast. A broadcast error does not prove the transaction failed
        // to propagate, so it must not release the inputs.
        //
        // The removal is tracked as a pending removal, not as a journal entry at
        // the scan cursor: the spend can only confirm at or after that cursor,
        // so a reorg with the fork at or above it would keep the entry and never
        // replay it. The pending record is moved into the confirming height's
        // journal entry when the scan sees the spend, and restored by any
        // covered rollback until then.
        //
        // One guard covers the mutation, the persist, and the rollback: the
        // persisted set is what protects the inputs after a restart, so a
        // failure must undo the removal before another writer can observe or
        // extend the half-applied state.
        {
            let mut set = self.utxos.lock();
            let previous = set.clone();
            let cursor = self.last_scanned_height;
            let removed: Vec<Utxo> = selected
                .iter()
                .filter(|utxo| set.outpoint(&utxo.txid, utxo.vout).is_some())
                .cloned()
                .collect();
            if set.pending_removals().len().saturating_add(removed.len()) > MAX_UNDO_ENTRIES {
                return Err(crate::error::BtcError::Bitcoin(
                    "BTC pending-send capacity reached; reconcile existing sends before sending again".into(),
                ));
            }
            for u in &selected {
                set.remove(&u.txid, u.vout);
            }
            set.record_pending_removals(&removed, &txid_hex, cursor);
            set.record_pending_send_raw(&txid_hex, &raw);
            if let Some(path) = &self.utxo_path {
                if let Err(error) = set.save(path) {
                    // The removal only protects the inputs once it is on disk:
                    // after a restart the scan would find them spendable again.
                    // Building a send on top of an unpersisted removal would
                    // license spending the same outpoints twice, so undo it and
                    // report the failure instead.
                    if !crate::utxo::save_may_have_committed(&error) {
                        *set = previous;
                    }
                    return Err(crate::error::BtcError::Bitcoin(format!(
                        "Could not persist BTC send: {error}"
                    )));
                }
            }
        }

        tracing::info!(
            "BTC built: {} sats to {} (txid: {}, fee: {} sats)",
            amount_sats,
            to_address,
            txid_hex,
            fee_sats,
        );

        Ok((txid_hex, raw, selected))
    }

    /// Re-add previously spent UTXOs (e.g. when a broadcast failed) and
    /// persist the restored set.
    ///
    /// This cancels the pending removal `send_to` recorded, because the send
    /// never happened: undoing a later reorg must not restore an outpoint twice
    /// or drop one that was never actually spent. A removal a scan already
    /// journalled is cancelled too.
    ///
    /// The restore is only useful if it reaches disk — otherwise a restart
    /// rescan would still see the send's removal and hide the funds — so a
    /// failed write rolls the in-memory set back and reports the error.
    ///
    /// If the rename completed but the directory sync failed, the disk may hold
    /// either version. The restored inputs stay in memory and the error reports
    /// the uncertainty instead of claiming the rollback took effect.
    pub fn restore_utxos(&self, utxos: &[Utxo]) -> Result<()> {
        let mut set = self.utxos.lock();
        let previous = set.clone();
        let mut added = Vec::new();
        for u in utxos {
            if set.add(u.clone()) {
                added.push(u.clone());
            }
        }
        set.cancel_local_removals(&added);
        if let Some(path) = &self.utxo_path {
            if let Err(error) = set.save(path) {
                if !crate::utxo::save_may_have_committed(&error) {
                    *set = previous;
                }
                return Err(crate::error::BtcError::Bitcoin(format!(
                    "Could not persist restored BTC inputs: {error}"
                )));
            }
        }
        Ok(())
    }

    /// Broadcast a raw transaction to the Bitcoin network via DNS seeds.
    pub async fn broadcast_raw(raw: &[u8]) -> Result<[u8; 32]> {
        crate::sync::broadcast_tx(raw).await
    }

    /// Estimate the fee in satoshis for a transaction with the given number
    /// of inputs and outputs, targeting `target_blocks` confirmation.
    ///
    /// Uses a simple heuristic: the peer's feefilter as a floor, then
    /// applies a multiplier based on how urgently the user wants confirmation.
    ///
    /// - `target_blocks = 1` → high priority (2× feefilter)
    /// - `target_blocks = 3` → medium priority (1× feefilter)
    /// - `target_blocks = 6` → low priority (0.5× feefilter, min 1 sat/vB)
    pub fn estimate_fee(
        input_count: usize,
        output_count: usize,
        feefilter_sats_per_kb: i64,
        target_blocks: u32,
    ) -> u64 {
        // Peer-supplied feefilter is untrusted: clamp to a sane range so a
        // hostile value (e.g. i64::MAX) cannot overflow the fee computation.
        let base_rate = if (1..=10_000_000).contains(&feefilter_sats_per_kb) {
            feefilter_sats_per_kb as u64
        } else {
            1_000 // 1 sat/byte default
        };

        let multiplier = match target_blocks {
            0..=1 => 2000, // urgent: 2×
            2..=3 => 1000, // standard: 1×
            _ => 500,      // economy: 0.5×
        };

        let vsize = crate::tx::estimate_vsize(input_count, output_count);
        // fee = rate × multiplier/1000 × vsize / 1000
        let fee = (base_rate * multiplier * vsize).div_ceil(1_000_000);
        fee.max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_fixture(
        seed: [u8; 64],
        network: bitcoin::Network,
        path: PathBuf,
    ) -> std::io::Result<BtcWallet> {
        if !path.exists() {
            UtxoSet::new().save(&path)?;
        }
        BtcWallet::with_persistence(seed, network, path)
    }

    #[test]
    fn reorg_fork_point_is_common_ancestor() {
        use bitcoin::blockdata::block::{Header, Version};
        use bitcoin::{BlockHash, CompactTarget, TxMerkleNode};

        fn make_header(prev: [u8; 32], nonce: u32) -> Header {
            let bits = CompactTarget::from_consensus(0x207fffff);
            let mut nonce = nonce;
            loop {
                let header = Header {
                    version: Version::ONE,
                    prev_blockhash: BlockHash::from_byte_array(prev),
                    merkle_root: TxMerkleNode::all_zeros(),
                    time: 1_700_000_000 + nonce,
                    bits,
                    nonce,
                };
                if header.validate_pow(bits.into()).is_ok() {
                    return header;
                }
                nonce += 1;
            }
        }

        let mut chain = HeaderChain::unanchored_for_tests();
        let h0 = make_header([0u8; 32], 0);
        let h0_hash = h0.block_hash().to_byte_array();
        chain
            .add_header(&bitcoin::consensus::encode::serialize(&h0), 0)
            .unwrap();
        let h1 = make_header(h0_hash, 1);
        let h1_hash = h1.block_hash().to_byte_array();
        chain
            .add_header(&bitcoin::consensus::encode::serialize(&h1), 1)
            .unwrap();
        let h2 = make_header(h1_hash, 2);
        let h2_hash = h2.block_hash().to_byte_array();
        chain
            .add_header(&bitcoin::consensus::encode::serialize(&h2), 2)
            .unwrap();

        let fork2 = make_header(h1_hash, 20);
        let fork2_hash = fork2.block_hash().to_byte_array();
        chain
            .add_header(&bitcoin::consensus::encode::serialize(&fork2), 2)
            .unwrap();
        let fork3 = make_header(fork2_hash, 30);
        let fork3_hash = fork3.block_hash().to_byte_array();
        chain
            .add_header(&bitcoin::consensus::encode::serialize(&fork3), 3)
            .unwrap();

        assert!(!is_ancestor(&chain, h2_hash, fork3_hash));
        assert!(is_ancestor(&chain, h1_hash, fork3_hash));
        assert_eq!(find_fork(&chain, h2_hash), Some(1));

        // With finite retention the abandoned branch can be pruned entirely;
        // the prune floor is then not a proven common ancestor.
        chain.set_retention(1);
        assert_eq!(chain.retained_floor(), 2);
        assert_eq!(
            find_fork(&chain, h2_hash),
            None,
            "a pruned branch must not report the retained floor as its fork"
        );
        assert_eq!(
            find_fork(&chain, fork3_hash),
            Some(3),
            "the tip is trivially its own fork"
        );
    }

    #[test]
    fn test_next_address_advances() {
        let mut w = BtcWallet::new([7u8; 64]);
        let a = w.next_address().unwrap();
        let b = w.next_address().unwrap();
        assert_ne!(a, b);
        assert!(a.starts_with("bc1q"));
    }

    #[test]
    fn fresh_wallet_adopts_the_retained_floor_and_is_not_blocked() {
        let w = BtcWallet::new([7u8; 64]);
        *w.headers.lock() = HeaderChain::unanchored_for_tests();
        let mut prev = [0u8; 32];
        for height in 0..7u32 {
            let header = test_header(prev, height);
            prev = header.block_hash().to_byte_array();
            w.add_header(&bitcoin::consensus::encode::serialize(&header), height)
                .unwrap();
        }
        w.set_header_retention(2);
        assert_eq!(
            w.retained_floor(),
            4,
            "retention must prune below the floor"
        );

        assert!(
            w.require_retained_wallet_history().is_ok(),
            "a wallet that never scanned has no earlier history to lose"
        );
        assert_eq!(w.birthday(), Some(4), "birthday is recorded at the floor");
        assert!(!w.history_unavailable());
    }

    #[test]
    fn wallet_birthday_below_the_retained_floor_stays_blocked() {
        let mut w = BtcWallet::new([7u8; 64]);
        *w.headers.lock() = HeaderChain::unanchored_for_tests();
        let mut prev = [0u8; 32];
        for height in 0..7u32 {
            let header = test_header(prev, height);
            prev = header.block_hash().to_byte_array();
            w.add_header(&bitcoin::consensus::encode::serialize(&header), height)
                .unwrap();
        }
        w.set_header_retention(2);
        w.set_birthday(1);
        w.add_utxo(Utxo {
            txid: "ee".repeat(32),
            vout: 0,
            value: 1000,
            address: "bc1qtest".to_string(),
            height: 2,
        });

        assert!(
            w.require_retained_wallet_history().is_err(),
            "history below the floor cannot be reconstructed"
        );
        assert!(w.history_unavailable());
    }

    #[test]
    fn wallet_without_funds_adopts_the_advanced_floor() {
        let mut w = BtcWallet::new([7u8; 64]);
        *w.headers.lock() = HeaderChain::unanchored_for_tests();
        let mut prev = [0u8; 32];
        for height in 0..7u32 {
            let header = test_header(prev, height);
            prev = header.block_hash().to_byte_array();
            w.add_header(&bitcoin::consensus::encode::serialize(&header), height)
                .unwrap();
        }
        w.set_header_retention(2);
        w.set_birthday(1);

        assert!(
            w.require_retained_wallet_history().is_ok(),
            "a wallet that never held funds loses nothing when the floor advances"
        );
        assert_eq!(w.birthday(), Some(4));
        assert!(!w.history_unavailable());
    }

    /// A wallet whose birthday predates the retained floor, with a journal
    /// entry at `floor + 1` so the fork at the floor is covered.
    fn wallet_awaiting_journal_recovery() -> BtcWallet {
        let mut w = BtcWallet::new([7u8; 64]);
        *w.headers.lock() = HeaderChain::unanchored_for_tests();
        let mut prev = [0u8; 32];
        for height in 0..7u32 {
            let header = test_header(prev, height);
            prev = header.block_hash().to_byte_array();
            w.add_header(&bitcoin::consensus::encode::serialize(&header), height)
                .unwrap();
        }
        w.set_header_retention(2);
        let floor = w.retained_floor();
        assert_eq!(floor, 4, "the fixture must prune below a non-zero floor");
        w.set_birthday(floor - 1);
        w.add_utxo(Utxo {
            txid: "ef".repeat(32),
            vout: 0,
            value: 1_000,
            address: "bc1qtest".to_string(),
            height: floor - 1,
        });
        w.push_undo_for_tests(crate::utxo::UndoEntry {
            height: floor + 1,
            created: vec![],
            removed: vec![],
        });
        w
    }

    #[test]
    fn a_covered_reorg_keeps_history_available_below_the_floor() {
        let mut w = wallet_awaiting_journal_recovery();
        let floor = w.retained_floor();

        assert!(
            w.require_retained_wallet_history().is_err(),
            "precondition: without the rollback the birthday predates the floor"
        );
        assert!(w.reset_for_reorg_at(Some(floor)), "the journal covers it");

        assert!(
            !w.history_unavailable(),
            "a covered rollback loses nothing below the floor, so no alert"
        );
        assert!(
            w.require_retained_wallet_history().is_ok(),
            "the post-fork scan starts at fork + 1 and needs no earlier history"
        );
        assert_eq!(
            w.balance(),
            0,
            "the balance stays hidden until the post-fork scan completes"
        );
    }

    #[test]
    fn a_completed_post_fork_rescan_clears_the_journal_recovered_marker() {
        let mut w = wallet_awaiting_journal_recovery();
        let floor = w.retained_floor();
        assert!(w.reset_for_reorg_at(Some(floor)));
        assert!(w.journal_recovered_for_tests());

        let tip = w.best_hash().unwrap();
        w.note_completed_scan(floor + 1, 2, Some(tip));
        w.set_scan_progress(floor + 3, Some(tip));

        assert!(
            !w.journal_recovered_for_tests(),
            "a completed scan ends the in-process recovery exemption"
        );
    }

    #[test]
    fn finishing_a_stalled_journal_recovery_restores_the_balance_and_guard() {
        let mut w = wallet_awaiting_journal_recovery();
        let floor = w.retained_floor();
        assert!(w.reset_for_reorg_at(Some(floor)));
        assert!(w.journal_recovered_for_tests());
        assert_eq!(w.balance(), 0, "still hidden before the recovery completes");

        w.finish_recovered_scan();

        assert!(
            !w.journal_recovered_for_tests(),
            "nothing above the fork ends the recovery and its exemption"
        );
        assert_eq!(
            w.balance(),
            1_000,
            "the journal already restored everything at or below the fork"
        );
        assert!(!w.rescan_required_for_tests());
    }

    #[test]
    fn a_fully_pruned_old_branch_forces_a_full_reset_not_the_journal() {
        let mut w = BtcWallet::new([7u8; 64]);
        *w.headers.lock() = HeaderChain::unanchored_for_tests();
        let mut prev = [0u8; 32];
        let mut hashes = Vec::new();
        for height in 0..7u32 {
            let header = test_header(prev, height);
            prev = header.block_hash().to_byte_array();
            hashes.push(prev);
            w.add_header(&bitcoin::consensus::encode::serialize(&header), height)
                .unwrap();
        }
        w.set_header_retention(2);
        let floor = w.retained_floor();
        assert_eq!(floor, 4, "the fixture must prune below a non-zero floor");
        // A journal entry at floor + 1 would cover a fork *at* the floor, so
        // only the fallback itself decides the path.
        w.push_undo_for_tests(crate::utxo::UndoEntry {
            height: floor + 1,
            created: vec![],
            removed: vec![],
        });
        w.set_scan_progress(floor + 2, Some([3u8; 32]));

        // The abandoned branch's header at height 2 is below the floor.
        let pruned = hashes[2];
        assert!(
            !w.reset_for_reorg_for_tests(pruned),
            "a pruned old branch has no proven common ancestor, so the journal may not claim one"
        );
        assert_eq!(
            w.last_scanned_height(),
            0,
            "the full reset rescans the retained window from its floor"
        );
        assert!(
            w.undo_entries_for_tests().is_empty(),
            "the journal built on the abandoned branch must be dropped"
        );
        assert_eq!(w.last_scanned_tip(), None);
        assert!(
            w.rescan_required_for_tests(),
            "a full reset owes a rescan of the new branch"
        );
        assert!(!w.journal_recovered_for_tests());
    }

    #[test]
    fn reopening_a_wallet_drops_a_previous_processs_journal() {
        let path = std::env::temp_dir().join(format!(
            "btc-journal-reload-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut w = open_fixture([7u8; 64], bitcoin::Network::Regtest, path.clone()).unwrap();
        w.push_undo_for_tests(crate::utxo::UndoEntry {
            height: 4,
            created: vec![],
            removed: vec![],
        });
        w.set_scan_progress(5, Some([7u8; 32]));
        assert_eq!(
            UtxoSet::load(&path).unwrap().undo_entries().len(),
            1,
            "precondition: the journal reached disk"
        );
        drop(w);

        let loaded = open_fixture([7u8; 64], bitcoin::Network::Regtest, path.clone()).unwrap();

        assert!(
            loaded.undo_entries_for_tests().is_empty(),
            "journal scope is one process; a crash must not hand it to the next"
        );
        assert_eq!(
            loaded.last_scanned_tip(),
            None,
            "the recorded tip belongs to the abandoned journal"
        );
        assert_eq!(
            loaded.last_scanned_height(),
            5,
            "the scan cursor itself still resumes"
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn clearing_the_chain_cache_clears_the_journal() {
        let mut w = BtcWallet::new([7u8; 64]);
        w.push_undo_for_tests(crate::utxo::UndoEntry {
            height: 3,
            created: vec![],
            removed: vec![],
        });
        w.set_scan_progress(4, Some([3u8; 32]));
        assert_eq!(w.last_scanned_height(), 4);
        assert_eq!(w.last_scanned_tip(), Some([3u8; 32]));

        w.reset_chain_cache();

        assert!(
            w.undo_entries_for_tests().is_empty(),
            "restart path must not replay stale entries"
        );
        assert_eq!(
            w.last_scanned_tip(),
            None,
            "a full reset must not keep the abandoned tip"
        );
    }

    #[test]
    fn a_covered_reorg_does_not_raise_the_history_alert() {
        let mut w = wallet_awaiting_journal_recovery();
        let floor = w.retained_floor();
        assert!(
            w.require_retained_wallet_history().is_err(),
            "precondition: the alert is armed before the rollback"
        );
        assert!(w.history_unavailable(), "precondition: the alert is up");

        assert!(w.reset_for_reorg_at(Some(floor)), "the journal covers it");

        assert!(
            !w.history_unavailable(),
            "a covered reorg is recoverable, not an operator incident"
        );
    }

    #[test]
    fn a_covered_rollback_keeps_a_rescan_the_wallet_already_owed() {
        let mut w = wallet_awaiting_journal_recovery();
        let floor = w.retained_floor();
        assert!(
            w.require_retained_wallet_history().is_err(),
            "precondition: the wallet is blocked with a standing case-3 alert"
        );
        assert!(w.rescan_required_for_tests());
        assert!(w.history_unavailable(), "precondition: the alert is up");

        assert!(w.reset_for_reorg_at(Some(floor)), "the journal covers it");

        let tip = w.best_hash().unwrap();
        w.note_completed_scan(floor + 1, 2, Some(tip));
        w.set_scan_progress(floor + 3, Some(tip));

        assert!(
            w.rescan_required_for_tests(),
            "a covered rollback cannot clear a rescan the wallet already owed"
        );
        assert!(
            w.history_unavailable(),
            "the critical alert must survive the rollback and its rescan"
        );
        assert!(w.balance_hidden(), "the balance must stay hidden");
        assert!(!w.journal_recovered_for_tests());
    }

    #[test]
    fn balance_hidden_tracks_a_covered_rollback_until_it_completes() {
        let mut w = wallet_awaiting_journal_recovery();
        let floor = w.retained_floor();
        assert!(!w.balance_hidden(), "precondition: nothing is hidden yet");

        assert!(w.reset_for_reorg_at(Some(floor)));
        assert!(
            w.balance_hidden(),
            "a covered rollback hides the balance while it rescans"
        );

        w.finish_recovered_scan();

        assert!(
            !w.balance_hidden(),
            "completing the recovery makes the balance visible again"
        );
        assert_eq!(w.balance(), 1_000);
    }

    #[test]
    fn a_full_reset_discards_reservation_observations() {
        let mut w = BtcWallet::new([7u8; 64]);
        let record = Utxo {
            txid: "ad".repeat(32),
            vout: 0,
            value: 900,
            address: "bc1qtest".to_string(),
            height: 2,
        };
        {
            let mut set = w.utxos.lock();
            set.reserve_utxo(record.clone());
            assert!(set.note_spend(&record.txid, record.vout));
            set.record_reservation_spend(record.clone(), 10);
        }

        w.reset_chain_cache();
        w.push_undo_for_tests(crate::utxo::UndoEntry {
            height: 6,
            created: vec![],
            removed: vec![],
        });

        assert!(w.reset_for_reorg_at_for_tests(Some(5)));
        assert!(
            w.reserved_utxo(&record.txid, record.vout).is_none(),
            "an observation from the abandoned branch must not re-reserve the input"
        );
        assert!(
            w.reserved_input_spent(&record.txid, record.vout),
            "the reset dropped the observation, so the spend itself is untouched"
        );
    }

    #[test]
    fn a_covered_rollback_does_not_require_a_rescan_until_it_finishes() {
        let mut w = wallet_awaiting_journal_recovery();
        let floor = w.retained_floor();
        assert!(
            !w.rescan_required_for_tests(),
            "precondition: the wallet owes no rescan before the reorg"
        );

        assert!(w.reset_for_reorg_at(Some(floor)));
        assert!(
            !w.rescan_required_for_tests(),
            "a covered rollback resumes from the fork, so it must not demand a full rescan"
        );
        assert_eq!(w.balance(), 0, "the balance stays hidden during the rescan");
        assert!(w.rescan_on_sync_for_tests());

        w.finish_recovered_scan();

        assert!(!w.rescan_required_for_tests());
        assert!(
            !w.rescan_on_sync_for_tests(),
            "finishing the recovery must stop hiding the balance"
        );
        assert_eq!(
            w.balance(),
            1_000,
            "finishing the recovery must stop hiding the balance"
        );
    }

    #[test]
    fn a_full_reset_keeps_a_pending_send_hidden_during_rescan() {
        let mut w = BtcWallet::with_network([7u8; 64], bitcoin::Network::Regtest);
        let owned = Utxo {
            txid: "ab".repeat(32),
            vout: 0,
            value: 100_000,
            address: w.current_address().unwrap(),
            height: 100,
        };
        w.add_utxo(owned.clone());
        let recipient =
            crate::keys::derive_address(&[9u8; 64], 0, bitcoin::Network::Regtest).unwrap();
        w.send_to(&recipient, 50_000, 1_000).unwrap();
        w.reset_chain_cache();
        assert!(!w.utxos.lock().add(owned.clone()));
        assert!(w.utxo_for_tests(&owned.txid, owned.vout).is_none());
        assert_eq!(w.pending_removal_count_for_tests(), 1);
    }

    #[test]
    fn a_full_reset_preserves_pending_send_across_restart() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-reset-pending-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let mut w = open_fixture([7u8; 64], bitcoin::Network::Regtest, path.clone()).unwrap();
        let owned = Utxo {
            txid: "ac".repeat(32),
            vout: 0,
            value: 100_000,
            address: w.current_address().unwrap(),
            height: 100,
        };
        w.add_utxo(owned.clone());
        let recipient =
            crate::keys::derive_address(&[9u8; 64], 0, bitcoin::Network::Regtest).unwrap();
        w.send_to(&recipient, 50_000, 1_000).unwrap();
        w.reset_chain_cache();
        drop(w);
        let reopened = open_fixture([7u8; 64], bitcoin::Network::Regtest, path).unwrap();
        assert_eq!(reopened.pending_removal_count_for_tests(), 1);
        assert!(!reopened.utxos.lock().add(owned));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn send_at_pending_capacity_keeps_the_input_spendable() {
        let mut w = BtcWallet::with_network([7u8; 64], bitcoin::Network::Regtest);
        let owned = Utxo {
            txid: "ab".repeat(32),
            vout: 0,
            value: 100_000,
            address: w.current_address().unwrap(),
            height: 100,
        };
        w.add_utxo(owned.clone());
        let other = Utxo {
            txid: "cd".repeat(32),
            ..owned.clone()
        };
        w.utxos
            .lock()
            .fill_pending_for_test(MAX_UNDO_ENTRIES, other);
        let recipient =
            crate::keys::derive_address(&[9u8; 64], 0, bitcoin::Network::Regtest).unwrap();
        let error = w.send_to(&recipient, 50_000, 1_000).unwrap_err();
        assert!(error.to_string().contains("pending-send capacity"));
        assert!(w.utxo_for_tests(&owned.txid, owned.vout).is_some());
        assert_eq!(w.pending_removal_count_for_tests(), MAX_UNDO_ENTRIES);
    }

    #[test]
    fn a_pending_send_keeps_its_raw_transaction_across_restart() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-pending-raw-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let mut w = open_fixture([7u8; 64], bitcoin::Network::Regtest, path.clone()).unwrap();
        let owned = Utxo {
            txid: "ae".repeat(32),
            vout: 0,
            value: 100_000,
            address: w.current_address().unwrap(),
            height: 100,
        };
        w.add_utxo(owned.clone());
        let recipient =
            crate::keys::derive_address(&[9u8; 64], 0, bitcoin::Network::Regtest).unwrap();
        let (txid, raw, _) = w.send_to(&recipient, 50_000, 1_000).unwrap();
        drop(w);

        let reopened = open_fixture([7u8; 64], bitcoin::Network::Regtest, path).unwrap();
        let recovered = reopened
            .pending_send_raw(&txid)
            .expect("the signed transaction must survive restart");
        assert_eq!(recovered, raw);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn checked_open_runs_its_check_after_acquiring_and_retains_the_writer_lock() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-checked-lock-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        UtxoSet::new().save(&path).unwrap();

        let wallet = BtcWallet::with_persistence_checked(
            [7; 64],
            bitcoin::Network::Regtest,
            path.clone(),
            || {
                let error = crate::utxo::lock_state_path(&path)
                    .expect_err("the check must run while the wallet holds its writer lock");
                assert!(crate::repair::is_writer_contention(&error), "{error}");
                Ok(())
            },
        )
        .unwrap();

        let error = crate::utxo::lock_state_path(&path)
            .expect_err("the opened wallet must retain its writer lock");
        assert!(crate::repair::is_writer_contention(&error), "{error}");

        drop(wallet);
        crate::utxo::lock_state_path(&path).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_second_writer_cannot_open_the_same_state() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-single-writer-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        UtxoSet::new().save(&path).unwrap();
        let first =
            BtcWallet::with_persistence([7; 64], bitcoin::Network::Regtest, path.clone()).unwrap();
        let error = BtcWallet::with_persistence([7; 64], bitcoin::Network::Regtest, path.clone())
            .err()
            .expect("a second writer must be refused");
        assert!(error.to_string().contains("in use"), "{error}");
        drop(first);
        BtcWallet::with_persistence([7; 64], bitcoin::Network::Regtest, path).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_state_path_cannot_bypass_the_writer_lock() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-symlink-lock-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        let alias = dir.join("alias.json");
        UtxoSet::new().save(&path).unwrap();
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        let first = BtcWallet::with_persistence([7; 64], bitcoin::Network::Regtest, path).unwrap();
        assert!(BtcWallet::with_persistence([7; 64], bitcoin::Network::Regtest, alias).is_err());
        drop(first);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn hardlinked_state_path_cannot_bypass_the_writer_lock() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-hardlink-lock-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        let alias = dir.join("alias.json");
        UtxoSet::new().save(&path).unwrap();
        let first =
            BtcWallet::with_persistence([7; 64], bitcoin::Network::Regtest, path.clone()).unwrap();
        std::fs::hard_link(&path, &alias).unwrap();
        assert!(BtcWallet::with_persistence([7; 64], bitcoin::Network::Regtest, alias).is_err());
        drop(first);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_scan_observed_spend_retires_the_pending_record() {
        let mut w = BtcWallet::with_network([7u8; 64], bitcoin::Network::Regtest);
        let owned = Utxo {
            txid: "af".repeat(32),
            vout: 0,
            value: 100_000,
            address: w.current_address().unwrap(),
            height: 100,
        };
        w.add_utxo(owned);
        let recipient =
            crate::keys::derive_address(&[9u8; 64], 0, bitcoin::Network::Regtest).unwrap();
        let (txid, _, selected) = w.send_to(&recipient, 50_000, 1_000).unwrap();
        assert!(w.pending_send_raw(&txid).is_some());

        {
            let mut set = w.utxos.lock();
            for input in &selected {
                set.take_pending_removal(&txid, &input.txid, input.vout);
            }
        }

        assert!(
            w.pending_send(&txid).is_none(),
            "a confirmed spend must leave no retryable record"
        );
        assert!(w.pending_send_raw(&txid).is_none());
        assert_eq!(w.pending_removal_count_for_tests(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn scan_save_failure_preserves_pending_send_on_restart() {
        use crate::utxo::UndoEntry;

        for after_rename in [false, true] {
            let dir = std::env::temp_dir().join(format!(
                "vtr-btc-scan-failed-save-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                after_rename
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("utxos.json");
            let mut wallet =
                open_fixture([7; 64], bitcoin::Network::Regtest, path.clone()).unwrap();
            let owned = Utxo {
                txid: "ab".repeat(32),
                vout: 0,
                value: 100_000,
                address: wallet.current_address().unwrap(),
                height: 100,
            };
            wallet.add_utxo(owned.clone());
            let recipient =
                crate::keys::derive_address(&[9; 64], 0, bitcoin::Network::Regtest).unwrap();
            let (txid, raw, _) = wallet.send_to(&recipient, 50_000, 1_000).unwrap();
            {
                let mut set = wallet.utxos.lock();
                let removed = set
                    .take_pending_removal(&txid, &owned.txid, owned.vout)
                    .unwrap();
                set.push_undo(UndoEntry {
                    height: 101,
                    created: vec![],
                    removed: vec![removed],
                });
            }
            if after_rename {
                crate::utxo::fail_next_parent_sync_for_test();
            } else {
                crate::utxo::fail_next_rename_for_test();
            }
            assert!(wallet.try_save_utxos().is_err());
            drop(wallet);
            let reopened = open_fixture([7; 64], bitcoin::Network::Regtest, path).unwrap();
            assert_eq!(reopened.pending_send_raw(&txid), Some(raw));
            assert!(!reopened.utxos.lock().add(owned));
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn failed_scan_batch_save_is_reported_to_the_caller() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-scan-report-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let wallet = open_fixture([7; 64], bitcoin::Network::Regtest, path).unwrap();
        crate::utxo::fail_next_rename_for_test();
        let error = wallet.finish_scan_batch(1, 1, None).unwrap_err();
        assert!(error.to_string().contains("injected pre-rename failure"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_scan_progress_save_does_not_advance_cursor() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-progress-failure-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let mut wallet = open_fixture([7; 64], bitcoin::Network::Regtest, path).unwrap();
        crate::utxo::fail_next_rename_for_test();
        assert!(!wallet.set_scan_progress(100, None));
        assert_eq!(wallet.last_scanned_height(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_birthday_save_does_not_change_birthday() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-birthday-save-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let mut wallet = open_fixture([7; 64], bitcoin::Network::Regtest, path).unwrap();
        crate::utxo::fail_next_rename_for_test();
        wallet.set_birthday(100);
        assert_eq!(wallet.birthday(), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn initial_birthday_and_cursor_are_saved_together() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-initial-birthday-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let mut wallet = open_fixture([7; 64], bitcoin::Network::Regtest, path.clone()).unwrap();
        let original = std::fs::read(&path).unwrap();
        crate::utxo::fail_next_rename_for_test();
        assert!(!wallet.set_initial_birthday(100));
        assert_eq!(wallet.birthday(), None);
        assert_eq!(wallet.last_scanned_height(), 0);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(wallet.set_initial_birthday(100));
        drop(wallet);
        let reopened = open_fixture([7; 64], bitcoin::Network::Regtest, path).unwrap();
        assert_eq!(reopened.birthday(), Some(100));
        assert_eq!(reopened.last_scanned_height(), 100);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn uncertain_scan_progress_save_keeps_cursor_retryable() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-progress-uncertain-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let mut wallet = open_fixture([7; 64], bitcoin::Network::Regtest, path.clone()).unwrap();
        crate::utxo::fail_next_parent_sync_for_test();
        assert!(!wallet.set_scan_progress(100, None));
        assert_eq!(wallet.last_scanned_height(), 0);
        assert!(wallet.set_scan_progress(100, None));
        assert_eq!(wallet.last_scanned_height(), 100);
        drop(wallet);
        let reopened = open_fixture([7; 64], bitcoin::Network::Regtest, path).unwrap();
        assert_eq!(reopened.last_scanned_height(), 100);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_recovery_completion_keeps_the_wallet_hidden() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-recovery-save-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let mut wallet = open_fixture([7; 64], bitcoin::Network::Regtest, path).unwrap();
        wallet.rescan_on_sync.store(true, Ordering::Release);
        wallet.journal_recovered.store(true, Ordering::Release);
        wallet.utxos.lock().set_rescan_required(true);
        crate::utxo::fail_next_rename_for_test();
        assert!(!wallet.finish_recovered_scan());
        assert!(wallet.rescan_on_sync.load(Ordering::Acquire));
        assert!(wallet.journal_recovered.load(Ordering::Acquire));
        assert!(wallet.utxos.lock().rescan_required());
        assert!(wallet.finish_recovered_scan());
        assert!(!wallet.rescan_on_sync.load(Ordering::Acquire));
        assert!(!wallet.journal_recovered.load(Ordering::Acquire));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_covered_reorg_keeps_a_pending_send_hidden() {
        let mut w = BtcWallet::with_network([7u8; 64], bitcoin::Network::Regtest);
        let address = w.current_address().unwrap();
        let owned = Utxo {
            txid: "ab".repeat(32),
            vout: 0,
            value: 100_000,
            address,
            height: 100,
        };
        w.add_utxo(owned.clone());
        // Coverage starts at 61 so a fork at 60 is covered; the output itself
        // predates the send and must survive the rollback.
        w.push_undo_for_tests(crate::utxo::UndoEntry {
            height: 61,
            created: vec![],
            removed: vec![],
        });
        w.set_last_scanned_height(101);
        let recipient =
            crate::keys::derive_address(&[9u8; 64], 0, bitcoin::Network::Regtest).unwrap();

        let (_, _, selected) = w.send_to(&recipient, 50_000, 1_000).unwrap();
        assert_eq!(selected.len(), 1);
        assert!(
            w.utxo_for_tests(&owned.txid, owned.vout).is_none(),
            "precondition: the send removed the output from the set"
        );

        assert!(
            w.reset_for_reorg_at_for_tests(Some(60)),
            "the journal covers fork 60"
        );
        assert!(
            w.utxo_for_tests(&owned.txid, owned.vout).is_none(),
            "a reorg does not prove the send left the mempool; keep its input hidden"
        );
        assert_eq!(w.pending_removal_count_for_tests(), 1);
    }

    #[test]
    fn a_send_whose_removal_cannot_be_persisted_fails_and_keeps_its_inputs() {
        let dir = std::env::temp_dir().join(format!(
            "vtorrent_btc_send_save_failure_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut w =
            open_fixture([7u8; 64], bitcoin::Network::Regtest, dir.join("utxos.json")).unwrap();
        let owned = Utxo {
            txid: "ba".repeat(32),
            vout: 0,
            value: 100_000,
            address: w.current_address().unwrap(),
            height: 100,
        };
        w.add_utxo(owned.clone());
        // A path whose parent directory does not exist fails the write for
        // every user, root included.
        let original = dir.join("utxos.json");
        let before = std::fs::read(&original).unwrap();
        w.utxo_path = Some(dir.join("missing").join("utxos.json"));
        let recipient =
            crate::keys::derive_address(&[9u8; 64], 0, bitcoin::Network::Regtest).unwrap();

        let error = w.send_to(&recipient, 50_000, 1_000).unwrap_err();
        assert!(
            error.to_string().contains("Could not persist BTC send"),
            "the error must name the persist failure, got: {error}"
        );
        assert_eq!(
            std::fs::read(&original).unwrap(),
            before,
            "a send that cannot persist its removal must not rewrite the original file"
        );
        assert_eq!(
            w.utxo_for_tests(&owned.txid, owned.vout)
                .map(|utxo| utxo.value),
            Some(100_000),
            "the rolled-back set must still hold the selected outpoint"
        );
        assert_eq!(w.utxos.lock().total(), 100_000);
        assert_eq!(
            w.pending_removal_count_for_tests(),
            0,
            "a failed persist must roll the recorded removal back too"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_send_write_keeps_committed_inputs_after_restart() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-send-write-restart-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let mut wallet = open_fixture([7; 64], bitcoin::Network::Regtest, path.clone()).unwrap();
        let owned = Utxo {
            txid: "ab".repeat(32),
            vout: 0,
            value: 100_000,
            address: wallet.current_address().unwrap(),
            height: 100,
        };
        wallet.add_utxo(owned.clone());
        let before = std::fs::read(&path).unwrap();
        crate::utxo::fail_next_rename_for_test();
        let recipient =
            crate::keys::derive_address(&[9; 64], 0, bitcoin::Network::Regtest).unwrap();
        assert!(wallet.send_to(&recipient, 50_000, 1_000).is_err());
        drop(wallet);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let reopened = open_fixture([7; 64], bitcoin::Network::Regtest, path).unwrap();
        assert_eq!(
            reopened
                .utxo_for_tests(&owned.txid, owned.vout)
                .map(|u| u.value),
            Some(owned.value)
        );
        assert!(reopened.pending_sends().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn orphaned_partial_temp_file_does_not_release_committed_send_after_restart() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-partial-send-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let mut wallet = open_fixture([7; 64], bitcoin::Network::Regtest, path.clone()).unwrap();
        let owned = Utxo {
            txid: "cd".repeat(32),
            vout: 0,
            value: 100_000,
            address: wallet.current_address().unwrap(),
            height: 100,
        };
        wallet.add_utxo(owned.clone());
        let recipient =
            crate::keys::derive_address(&[9; 64], 0, bitcoin::Network::Regtest).unwrap();
        let (txid, raw, _) = wallet.send_to(&recipient, 50_000, 1_000).unwrap();
        drop(wallet);
        std::fs::write(path.with_extension("tmp"), b"{\"pending_removals\": [").unwrap();
        let reopened = open_fixture([7; 64], bitcoin::Network::Regtest, path).unwrap();
        assert_eq!(reopened.pending_send_raw(&txid), Some(raw));
        assert!(!reopened.utxos.lock().add(owned));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn send_with_uncertain_directory_sync_keeps_inputs_held_after_restart() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-uncertain-sync-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let mut wallet = open_fixture([7; 64], bitcoin::Network::Regtest, path.clone()).unwrap();
        let owned = Utxo {
            txid: "da".repeat(32),
            vout: 0,
            value: 100_000,
            address: wallet.current_address().unwrap(),
            height: 100,
        };
        wallet.add_utxo(owned.clone());
        let recipient =
            crate::keys::derive_address(&[9; 64], 0, bitcoin::Network::Regtest).unwrap();
        crate::utxo::fail_next_parent_sync_for_test();
        assert!(
            wallet.send_to(&recipient, 50_000, 1_000).is_err(),
            "send must not return signed bytes when durability is uncertain"
        );
        assert!(
            !wallet.utxos.lock().add(owned.clone()),
            "uncertain save must keep input held in memory"
        );
        drop(wallet);
        let reopened = open_fixture([7; 64], bitcoin::Network::Regtest, path).unwrap();
        assert!(
            !reopened.utxos.lock().add(owned),
            "visible renamed record must keep input held after restart"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn uncertain_address_index_save_does_not_reuse_index_in_process() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-address-sync-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let mut wallet = open_fixture([7; 64], bitcoin::Network::Regtest, path.clone()).unwrap();
        let initial = wallet.current_address().unwrap();
        crate::utxo::fail_next_parent_sync_for_test();
        assert!(wallet.next_address().is_err());
        assert_ne!(wallet.current_address().unwrap(), initial);
        drop(wallet);
        let reopened = open_fixture([7; 64], bitcoin::Network::Regtest, path).unwrap();
        assert_ne!(reopened.current_address().unwrap(), initial);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn uncertain_reservation_release_keeps_the_input_reserved() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-release-sync-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let wallet = open_fixture([7; 64], bitcoin::Network::Regtest, path.clone()).unwrap();
        let owned = Utxo {
            txid: "ea".repeat(32),
            vout: 0,
            value: 100_000,
            address: wallet.current_address().unwrap(),
            height: 100,
        };
        wallet.add_utxo(owned.clone());
        let htlc = crate::htlc::BtcHtlc {
            hash_lock: [3; 32],
            recipient: owned.address.clone(),
            refund_address: owned.address.clone(),
            expiry: 1_900_000_000,
            amount: 90_000,
            network: bitcoin::Network::Regtest,
        };
        let funding = htlc
            .build_funding_tx([1; 32], 0, 100_000, 1_000, &owned.address)
            .unwrap();
        wallet
            .reserve_swap_input(
                &owned,
                &bitcoin::consensus::serialize(&funding),
                "order".into(),
                &htlc,
            )
            .unwrap();
        crate::utxo::fail_next_parent_sync_for_test();
        assert!(wallet.release_swap_reservation("order").is_err());
        assert!(
            wallet.utxos.lock().swap_contract("order").is_none(),
            "the released version must remain in memory"
        );
        drop(wallet);
        let reopened = open_fixture([7; 64], bitcoin::Network::Regtest, path).unwrap();
        let released_on_disk = reopened.utxos.lock().swap_contract("order").is_none()
            && reopened.utxo_for_tests(&owned.txid, owned.vout).is_some();
        assert!(
            released_on_disk,
            "the visible renamed record must be the released version"
        );
        assert!(
            reopened.utxo_for_tests(&owned.txid, owned.vout).is_some(),
            "released input remains spendable after restart"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn partial_scan_release_frees_the_input_and_keeps_the_contract() {
        let wallet = BtcWallet::new([7u8; 64]);
        let owned = Utxo {
            txid: "fb".repeat(32),
            vout: 0,
            value: 100_000,
            address: wallet.current_address().unwrap(),
            height: 100,
        };
        wallet.add_utxo(owned.clone());
        let htlc = crate::htlc::BtcHtlc {
            hash_lock: [3; 32],
            recipient: owned.address.clone(),
            refund_address: owned.address.clone(),
            expiry: 1_900_000_000,
            amount: 90_000,
            network: bitcoin::Network::Bitcoin,
        };
        let funding = htlc
            .build_funding_tx([1; 32], 0, 100_000, 1_000, &owned.address)
            .unwrap();
        wallet
            .reserve_swap_input(
                &owned,
                &bitcoin::consensus::serialize(&funding),
                "order".into(),
                &htlc,
            )
            .unwrap();
        assert!(wallet.release_swap_input_keep_contract("order").unwrap());
        // The contract is retained for a later refund or manual recovery.
        assert!(wallet.swap_contract("order").is_some());
        // The outpoint is spendable again, not silently re-reserved.
        assert!(
            wallet
                .list_utxos()
                .iter()
                .any(|utxo| utxo.txid == owned.txid && utxo.vout == owned.vout),
            "released input must be visible to coin selection"
        );
    }

    #[test]
    fn failed_persistence_path_change_keeps_the_working_path() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-path-change-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("utxos.json");
        let mut wallet = open_fixture([7; 64], bitcoin::Network::Regtest, good.clone()).unwrap();
        let owned = Utxo {
            txid: "fa".repeat(32),
            vout: 0,
            value: 100_000,
            address: wallet.current_address().unwrap(),
            height: 100,
        };
        wallet.add_utxo(owned.clone());
        let good_before = std::fs::read(&good).unwrap();

        let bad = dir.join("missing").join("utxos.json");
        assert!(wallet.set_utxo_path(bad.clone()).is_err());
        assert_eq!(
            wallet.utxo_path,
            Some(good.clone()),
            "a failed path change must not keep the unwritable path"
        );
        assert_eq!(
            std::fs::read(&good).unwrap(),
            good_before,
            "the good file must be unchanged"
        );

        wallet.next_address().unwrap();
        let persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&good).unwrap()).unwrap();
        assert_ne!(
            persisted["next_index"].as_u64(),
            Some(0),
            "later saves must still reach the working path"
        );
        assert!(!bad.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_wallet_with_pruned_history_is_not_opened_or_modified() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-pruned-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let original = br#"{"utxos":[],"last_scanned_height":100,"undo":[]}"#;
        std::fs::write(&path, original).unwrap();
        let error = open_fixture([7; 64], bitcoin::Network::Regtest, path.clone())
            .err()
            .expect("legacy history must not be silently upgraded");
        assert!(error.to_string().contains("legacy"));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(BtcWallet::with_persistence_attested_seed(
            [7; 64],
            bitcoin::Network::Regtest,
            path.clone(),
            true
        )
        .is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_wallet_state_is_not_treated_as_new() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let error = BtcWallet::with_persistence([7; 64], bitcoin::Network::Regtest, path.clone())
            .err()
            .expect("missing state cannot prove a seed is new");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(!path.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn explicit_new_seed_attestation_creates_one_v3_file() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-new-seed-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let wallet = BtcWallet::with_persistence_attested_seed(
            [7; 64],
            bitcoin::Network::Regtest,
            path.clone(),
            true,
        )
        .unwrap();
        assert!(path.exists());
        drop(wallet);
        let reopened =
            BtcWallet::with_persistence([7; 64], bitcoin::Network::Regtest, path).unwrap();
        assert_eq!(reopened.birthday(), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn repeated_attestation_cannot_recreate_lost_wallet_state() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-repeat-attestation-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let wallet = BtcWallet::with_persistence_attested_seed(
            [7; 64],
            bitcoin::Network::Regtest,
            path.clone(),
            true,
        )
        .unwrap();
        drop(wallet);
        std::fs::remove_file(&path).unwrap();
        assert!(
            BtcWallet::with_persistence_attested_seed(
                [7; 64],
                bitcoin::Network::Regtest,
                path.clone(),
                true,
            )
            .is_err(),
            "repeated attestation must not recreate lost state"
        );
        assert!(!path.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn attested_creation_ignores_a_stale_temporary_symlink() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-new-seed-symlink-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, b"must not be overwritten").unwrap();
        std::os::unix::fs::symlink(&victim, path.with_extension("tmp")).unwrap();

        BtcWallet::with_persistence_attested_seed(
            [7; 64],
            bitcoin::Network::Regtest,
            path.clone(),
            true,
        )
        .unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"must not be overwritten");
        assert!(path.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn version_two_wallet_with_unknown_provenance_is_not_opened() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-v2-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let original = br#"{"utxos":[],"journal_version":2,"undo":[]}"#;
        std::fs::write(&path, original).unwrap();
        let error = open_fixture([7; 64], bitcoin::Network::Regtest, path.clone())
            .err()
            .expect("version 2 does not prove that the wallet was never upgraded");
        assert!(error.to_string().contains("legacy"));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        UtxoSet::load(&path).unwrap().save(&path).unwrap();
        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(after["journal_version"], 2);
        assert!(open_fixture([7; 64], bitcoin::Network::Regtest, path).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unknown_future_wallet_version_is_not_opened() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-future-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let original = br#"{"utxos":[],"journal_version":255}"#;
        std::fs::write(&path, original).unwrap();
        assert!(open_fixture([7; 64], bitcoin::Network::Regtest, path.clone()).is_err());
        assert_eq!(std::fs::read(path).unwrap(), original);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_wallet_with_retained_history_is_also_refused() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-legacy-upgrade-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let spent = Utxo {
            txid: "ab".repeat(32),
            vout: 0,
            value: 100_000,
            address: crate::keys::derive_address(&[7; 64], 0, bitcoin::Network::Regtest).unwrap(),
            height: 100,
        };
        // A file written by a build that did not record the sending transaction.
        let legacy = serde_json::json!({
            "utxos": [],
            "next_index": 0,
            "last_scanned_height": 107,
            "undo": [{
                "height": 106,
                "created": [],
                "removed": [{"utxo": spent, "prior": "Free"}],
            }],
        });
        std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();

        let before = std::fs::read(&path).unwrap();
        let error = open_fixture([7; 64], bitcoin::Network::Regtest, path.clone())
            .err()
            .expect("retained entries do not prove complete history");
        assert!(error.to_string().contains("legacy"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A send must hold the one `utxos` guard from mutation through persist, so
    /// a concurrent writer cannot slip an insertion under the rollback that
    /// would then discard it.
    #[cfg(unix)]
    #[test]
    fn a_send_keeps_the_utxo_set_locked_across_its_persist() {
        let dir = std::env::temp_dir().join(format!(
            "vtorrent_btc_send_lock_window_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let mut w = open_fixture([7u8; 64], bitcoin::Network::Regtest, path.clone()).unwrap();
        let owned = Utxo {
            txid: "bc".repeat(32),
            vout: 0,
            value: 100_000,
            address: w.current_address().unwrap(),
            height: 100,
        };
        w.add_utxo(owned.clone());
        let (release_send, release_receive) = std::sync::mpsc::channel();
        let shared = w.utxos.clone();
        let marker = Utxo {
            txid: "0d".repeat(32),
            vout: 7,
            value: 777,
            address: "bc1qmarker".to_string(),
            height: 0,
        };
        let observer_marker = marker.clone();
        let observer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let mut saw_lock_held = false;
            loop {
                match shared.try_lock() {
                    Some(mut set) => {
                        set.add(observer_marker.clone());
                        return saw_lock_held;
                    }
                    None if std::time::Instant::now() < deadline => {
                        if !saw_lock_held {
                            saw_lock_held = true;
                            let _ = release_send.send(());
                        }
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    None => return false,
                }
            }
        });
        let recipient =
            crate::keys::derive_address(&[9u8; 64], 0, bitcoin::Network::Regtest).unwrap();
        let (w, result) = std::thread::spawn(move || {
            crate::utxo::pause_next_save_before_rename_for_test(release_receive);
            crate::utxo::fail_next_rename_for_test();
            let result = w.send_to(&recipient, 50_000, 1_000);
            (w, result)
        })
        .join()
        .unwrap();
        let observed = observer.join().unwrap();

        assert!(
            observed,
            "the observer must see the set guard held across the paused save"
        );
        let error = result.unwrap_err();
        assert!(
            error.to_string().contains("Could not persist BTC send"),
            "the send must fail on the persist, got: {error}"
        );
        assert_eq!(
            w.utxo_for_tests(&marker.txid, marker.vout)
                .map(|utxo| utxo.value),
            Some(777),
            "a concurrent insertion must not be discarded by the send's rollback"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A restore is the compensating write for a failed broadcast: when it
    /// cannot persist, memory must not claim an input the file does not hold.
    #[test]
    fn a_restore_whose_write_fails_rolls_back_and_reports() {
        let dir = std::env::temp_dir().join(format!(
            "vtorrent_btc_restore_save_failure_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let mut w = open_fixture([7u8; 64], bitcoin::Network::Regtest, path.clone()).unwrap();
        let owned = Utxo {
            txid: "bd".repeat(32),
            vout: 0,
            value: 100_000,
            address: w.current_address().unwrap(),
            height: 100,
        };
        w.add_utxo(owned.clone());
        let recipient =
            crate::keys::derive_address(&[9u8; 64], 0, bitcoin::Network::Regtest).unwrap();
        let (_, _, selected) = w.send_to(&recipient, 50_000, 1_000).unwrap();
        assert!(w.utxo_for_tests(&owned.txid, owned.vout).is_none());

        w.utxo_path = Some(dir.join("missing").join("utxos.json"));
        let restored = w.restore_utxos(&selected);

        assert!(
            restored.as_ref().is_err_and(|error| error
                .to_string()
                .contains("Could not persist restored BTC inputs")),
            "the restore must report the persist failure, got: {restored:?}"
        );
        assert!(
            w.utxo_for_tests(&owned.txid, owned.vout).is_none(),
            "a restore whose write failed must not claim an input the file does not hold"
        );
        drop(w);
        let reloaded = open_fixture([7u8; 64], bitcoin::Network::Regtest, path).unwrap();
        assert!(
            reloaded.utxo_for_tests(&owned.txid, owned.vout).is_none(),
            "the failed restore must leave the persisted set unchanged"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Feed a confirmed transaction into the wallet's shared set exactly as the
    /// BIP-158 scan does.
    fn scan_confirmed_tx(w: &BtcWallet, tx: &bitcoin::Transaction, height: u32) {
        let sync = crate::sync::BtcSync::new(
            w.headers.clone(),
            w.utxos.clone(),
            w.watch_addresses(w.next_index).unwrap(),
            w.network,
        );
        sync.record_matching_outputs(tx, height);
    }

    /// A transaction paying `address`, with a foreign input the wallet does not
    /// track.
    fn incoming_tx(address: &str, value: u64, seed: u8) -> bitcoin::Transaction {
        use std::str::FromStr;

        let script = bitcoin::Address::from_str(address)
            .unwrap()
            .require_network(bitcoin::Network::Regtest)
            .unwrap()
            .script_pubkey();
        bitcoin::Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![bitcoin::TxIn {
                previous_output: bitcoin::OutPoint::new(
                    bitcoin::Txid::from_byte_array([seed; 32]),
                    0,
                ),
                script_sig: bitcoin::ScriptBuf::new(),
                sequence: bitcoin::Sequence::MAX,
                witness: bitcoin::Witness::new(),
            }],
            output: vec![bitcoin::TxOut {
                value: bitcoin::Amount::from_sat(value),
                script_pubkey: script,
            }],
        }
    }

    #[test]
    fn a_reorg_at_the_send_cursor_restores_the_sent_input() {
        let mut w = BtcWallet::with_network([7u8; 64], bitcoin::Network::Regtest);
        let owned = Utxo {
            txid: "ab".repeat(32),
            vout: 0,
            value: 100_000,
            address: w.current_address().unwrap(),
            height: 100,
        };
        w.add_utxo(owned.clone());
        w.set_last_scanned_height(101);
        let recipient =
            crate::keys::derive_address(&[9u8; 64], 0, bitcoin::Network::Regtest).unwrap();

        let (txid, raw, _) = w.send_to(&recipient, 50_000, 1_000).unwrap();
        assert!(
            w.undo_entries_for_tests().is_empty(),
            "a send cannot claim a scanned height: it may confirm at or above the cursor"
        );
        assert_eq!(w.pending_removal_count_for_tests(), 1);

        // The send confirms at 102. The scan adds the change and takes the
        // removal from the pending record, so height 102 owns it.
        let tx: bitcoin::Transaction = bitcoin::consensus::deserialize(&raw).unwrap();
        scan_confirmed_tx(&w, &tx, 102);
        assert_eq!(
            w.pending_removal_count_for_tests(),
            0,
            "the confirming height retires the pending record"
        );
        let entry = w
            .undo_entries_for_tests()
            .into_iter()
            .find(|entry| entry.height == 102)
            .expect("the confirming block is journalled");
        assert!(
            entry
                .removed
                .iter()
                .any(|removed| removed.utxo.txid == owned.txid),
            "the confirming entry must carry the removal so the fold stays complete"
        );
        assert!(
            entry.created.iter().any(|created| created.txid == txid),
            "precondition: the change is journalled at 102"
        );

        // One-block reorg to the cursor the send was built at: below the confirm
        // height, so the spend is reorged out.
        assert!(w.reset_for_reorg_at_for_tests(Some(101)));
        assert!(
            w.utxo_for_tests(&owned.txid, owned.vout).is_none(),
            "an orphaned local send may still be in mempools"
        );
        assert_eq!(w.pending_send_raw(&txid), Some(raw));
        assert!(
            w.utxo_for_tests(&txid, 1).is_none(),
            "the change created above the fork is dropped"
        );
        assert_eq!(
            w.last_scanned_height(),
            102,
            "the post-fork rescan starts at the fork + 1"
        );
    }

    #[test]
    fn a_reorg_below_the_confirm_height_keeps_a_confirmed_send_reserved() {
        let mut w = BtcWallet::with_network([7u8; 64], bitcoin::Network::Regtest);
        let owned = Utxo {
            txid: "cd".repeat(32),
            vout: 0,
            value: 80_000,
            address: w.current_address().unwrap(),
            height: 100,
        };
        w.add_utxo(owned.clone());
        // A block at 104 touched the wallet, so a fork at 103 is covered.
        w.push_undo_for_tests(crate::utxo::UndoEntry {
            height: 104,
            created: vec![],
            removed: vec![],
        });
        w.set_last_scanned_height(105);
        let recipient =
            crate::keys::derive_address(&[9u8; 64], 0, bitcoin::Network::Regtest).unwrap();

        let (txid, raw, _) = w.send_to(&recipient, 40_000, 1_000).unwrap();
        let tx: bitcoin::Transaction = bitcoin::consensus::deserialize(&raw).unwrap();
        scan_confirmed_tx(&w, &tx, 106);

        let entry = w
            .undo_entries_for_tests()
            .into_iter()
            .find(|entry| entry.height == 106)
            .expect("the confirming block is journalled");
        assert!(
            entry
                .removed
                .iter()
                .any(|removed| removed.utxo.txid == owned.txid),
            "a confirmed send is journalled at its confirm height"
        );

        assert!(w.reset_for_reorg_at_for_tests(Some(103)));
        assert!(
            w.utxo_for_tests(&owned.txid, owned.vout).is_none(),
            "a reorg does not prove the original transaction left mempools"
        );
        assert_eq!(w.pending_send_raw(&txid), Some(raw));
        assert!(
            w.utxo_for_tests(&txid, 1).is_none(),
            "the change from the abandoned block is dropped"
        );
    }

    #[test]
    fn confirmed_local_send_is_retained_after_restart() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-confirmed-restart-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let mut w = open_fixture([7u8; 64], bitcoin::Network::Regtest, path.clone()).unwrap();
        let owned = Utxo {
            txid: "cc".repeat(32),
            vout: 0,
            value: 80_000,
            address: w.current_address().unwrap(),
            height: 100,
        };
        w.add_utxo(owned.clone());
        let recipient =
            crate::keys::derive_address(&[9u8; 64], 0, bitcoin::Network::Regtest).unwrap();
        let (txid, raw, _) = w.send_to(&recipient, 40_000, 1_000).unwrap();
        let tx: bitcoin::Transaction = bitcoin::consensus::deserialize(&raw).unwrap();
        scan_confirmed_tx(&w, &tx, 106);
        w.try_save_utxos().unwrap();
        drop(w);

        let reopened = open_fixture([7u8; 64], bitcoin::Network::Regtest, path).unwrap();
        assert_eq!(reopened.pending_send_raw(&txid), Some(raw));
        assert!(!reopened.utxos.lock().add(owned));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn competing_confirmation_and_reorg_keep_multi_input_send_held_after_restart() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-competing-send-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let mut wallet = open_fixture([7; 64], bitcoin::Network::Regtest, path.clone()).unwrap();
        let address = wallet.current_address().unwrap();
        let inputs: Vec<_> = ["aa", "bb"]
            .into_iter()
            .map(|id| Utxo {
                txid: id.repeat(32),
                vout: 0,
                value: 50_000,
                address: address.clone(),
                height: 100,
            })
            .collect();
        for input in &inputs {
            wallet.add_utxo(input.clone());
        }
        wallet.set_last_scanned_height(101);
        let recipient =
            crate::keys::derive_address(&[9; 64], 0, bitcoin::Network::Regtest).unwrap();
        let (txid, raw, _) = wallet.send_to(&recipient, 90_000, 1_000).unwrap();
        let original: bitcoin::Transaction = bitcoin::consensus::deserialize(&raw).unwrap();
        assert_eq!(original.input.len(), 2);
        let mut competing = original.clone();
        competing.input.truncate(1);
        competing.output.truncate(1);
        competing.output[0].value = bitcoin::Amount::from_sat(49_000);
        assert_ne!(competing.compute_txid(), original.compute_txid());
        scan_confirmed_tx(&wallet, &competing, 102);
        assert_eq!(wallet.pending_send_raw(&txid), Some(raw.clone()));
        assert!(wallet.reset_for_reorg_at_for_tests(Some(101)));
        wallet.try_save_utxos().unwrap();
        drop(wallet);
        let reopened = open_fixture([7; 64], bitcoin::Network::Regtest, path).unwrap();
        assert_eq!(reopened.pending_send_raw(&txid), Some(raw));
        for input in inputs {
            assert!(
                !reopened.utxos.lock().add(input),
                "a potentially live send must hold every input"
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_covered_reorg_keeps_a_pending_send_hidden_until_observed() {
        let mut w = BtcWallet::with_network([7u8; 64], bitcoin::Network::Regtest);
        let owned = Utxo {
            txid: "ef".repeat(32),
            vout: 0,
            value: 100_000,
            address: w.current_address().unwrap(),
            height: 100,
        };
        w.add_utxo(owned.clone());
        w.set_last_scanned_height(101);
        let recipient =
            crate::keys::derive_address(&[9u8; 64], 0, bitcoin::Network::Regtest).unwrap();

        let (_, _, _) = w.send_to(&recipient, 50_000, 1_000).unwrap();
        // Block 102 touched the wallet (an incoming payment), so a fork at 101
        // is covered by its entry.
        let incoming = incoming_tx(&w.current_address().unwrap(), 5_000, 7);
        scan_confirmed_tx(&w, &incoming, 102);

        assert!(w.reset_for_reorg_at_for_tests(Some(101)));
        assert!(
            w.utxo_for_tests(&owned.txid, owned.vout).is_none(),
            "a reorg alone must not make the sent input selectable"
        );
        assert_eq!(
            w.pending_removal_count_for_tests(),
            1,
            "the send remains pending until the scan observes it or a recovery cancels it"
        );

        let unrelated = incoming_tx(
            &crate::keys::derive_address(&[9u8; 64], 9, bitcoin::Network::Regtest).unwrap(),
            1_000,
            8,
        );
        scan_confirmed_tx(&w, &unrelated, 102);
        assert!(
            w.utxo_for_tests(&owned.txid, owned.vout).is_none(),
            "an unrelated rescan must not resurrect the pending send"
        );
    }

    #[test]
    fn advancing_the_floor_past_the_resumed_cursor_abandons_the_recovery() {
        let mut w = wallet_awaiting_journal_recovery();
        let floor = w.retained_floor();
        assert!(w.reset_for_reorg_at(Some(floor)));
        assert!(w.journal_recovered_for_tests());
        let resumed = w.last_scanned_height();
        assert!(resumed >= floor, "precondition: the exemption is active");

        let mut prev = w.best_hash().unwrap();
        for height in 7..9u32 {
            let header = test_header(prev, height);
            prev = header.block_hash().to_byte_array();
            w.add_header(&bitcoin::consensus::encode::serialize(&header), height)
                .unwrap();
        }
        let advanced = w.retained_floor();
        assert!(
            advanced > resumed,
            "the fixture must advance the floor past the resumed cursor"
        );

        assert!(
            w.history_unavailable(),
            "an abandoned recovery must let the critical alert fire"
        );
        assert!(!w.journal_recovered_for_tests());
        assert!(
            w.undo_entries_for_tests().is_empty(),
            "the abandoned journal must not cover a later reorg"
        );
        assert_eq!(w.last_scanned_tip(), None);
        assert!(
            w.rescan_required_for_tests(),
            "the regular case 2/3 path must decide after an abandoned recovery"
        );
    }

    #[test]
    fn the_recovery_exemption_holds_while_the_floor_stays_below_the_cursor() {
        let mut w = wallet_awaiting_journal_recovery();
        let floor = w.retained_floor();
        assert!(w.reset_for_reorg_at(Some(floor)));
        assert!(w.last_scanned_height() >= w.retained_floor());

        assert!(w.require_retained_wallet_history().is_ok());
        assert!(!w.history_unavailable());
        assert!(
            w.journal_recovered_for_tests(),
            "an active exemption must not be abandoned"
        );
    }

    #[test]
    fn a_chain_cache_reset_clears_the_journal_recovered_marker() {
        let mut w = wallet_awaiting_journal_recovery();
        let floor = w.retained_floor();
        assert!(w.reset_for_reorg_at(Some(floor)));
        assert!(w.journal_recovered_for_tests());

        w.reset_chain_cache();

        assert!(
            !w.journal_recovered_for_tests(),
            "a stale exemption must not suppress a later alert"
        );
        assert!(
            w.history_unavailable(),
            "a full reset needs history below the floor again, so the alert returns"
        );
    }

    #[test]
    fn synced_wallet_is_not_blocked_when_the_floor_advances_past_its_birthday() {
        let mut w = BtcWallet::new([7u8; 64]);
        *w.headers.lock() = HeaderChain::unanchored_for_tests();
        let mut prev = [0u8; 32];
        for height in 0..7u32 {
            let header = test_header(prev, height);
            prev = header.block_hash().to_byte_array();
            w.add_header(&bitcoin::consensus::encode::serialize(&header), height)
                .unwrap();
        }
        w.set_header_retention(2);
        w.set_birthday(1);

        assert!(
            !w.history_unavailable(),
            "a wallet with complete history is not blocked by a stale birthday"
        );
    }

    fn test_header(prev: [u8; 32], nonce: u32) -> bitcoin::blockdata::block::Header {
        use bitcoin::blockdata::block::{Header, Version};
        use bitcoin::{BlockHash, CompactTarget, TxMerkleNode};

        let bits = CompactTarget::from_consensus(0x207fffff);
        let mut nonce = nonce;
        loop {
            let header = Header {
                version: Version::ONE,
                prev_blockhash: BlockHash::from_byte_array(prev),
                merkle_root: TxMerkleNode::all_zeros(),
                time: 1_700_000_000 + nonce,
                bits,
                nonce,
            };
            if header.validate_pow(bits.into()).is_ok() {
                return header;
            }
            nonce += 1;
        }
    }

    #[test]
    fn test_balance_tracks_utxos() {
        let w = BtcWallet::new([7u8; 64]);
        w.add_utxo(Utxo {
            txid: "11".repeat(32),
            vout: 0,
            value: 5000,
            address: "bc1qtest".to_string(),
            height: 1,
        });
        assert_eq!(w.balance(), 5000);
    }

    #[test]
    fn reorg_rebuilds_chain_outputs_from_the_start() {
        let mut w = BtcWallet::new([7u8; 64]);
        for (txid, height) in [("aa", 100u32), ("bb", 120), ("cc", 121)] {
            w.add_utxo(Utxo {
                txid: txid.repeat(32),
                vout: 0,
                value: 1000,
                address: "bc1qtest".to_string(),
                height,
            });
        }
        w.mark_synced();
        w.set_last_scanned_height(200);

        w.reset_chain_cache_from(Some(120));

        assert_eq!(
            w.last_scanned_height(),
            0,
            "reorg must rebuild outputs spent on the abandoned branch"
        );
        assert!(!w.synced(), "a reorg invalidates the synced flag");
        assert_eq!(
            w.balance(),
            0,
            "outputs stay hidden until the shortened rescan completes"
        );
        let kept: Vec<_> = w
            .utxos
            .lock()
            .list()
            .iter()
            .map(|u| u.txid[..2].to_string())
            .collect();
        assert!(kept.is_empty(), "chain outputs must be rebuilt");
    }

    #[test]
    fn rebuilt_lineage_still_forces_a_full_rescan() {
        let mut w = BtcWallet::new([7u8; 64]);
        w.add_utxo(Utxo {
            txid: "dd".repeat(32),
            vout: 0,
            value: 1000,
            address: "bc1qtest".to_string(),
            height: 120,
        });
        w.push_undo_for_tests(crate::utxo::UndoEntry {
            height: 120,
            created: vec![],
            removed: vec![],
        });
        w.set_scan_progress(200, Some([3u8; 32]));

        w.reset_chain_cache();

        assert_eq!(
            w.last_scanned_height(),
            0,
            "an unknown fork means start over"
        );
        assert!(w.utxos.lock().list().is_empty());
        assert!(
            w.undo_entries_for_tests().is_empty(),
            "a full reset must drop the abandoned journal"
        );
        assert_eq!(
            w.last_scanned_tip(),
            None,
            "a full reset must not keep the abandoned tip"
        );
    }

    #[test]
    fn the_height_only_setter_clears_a_stored_tip() {
        let mut w = BtcWallet::new([7u8; 64]);
        w.set_scan_progress(10, Some([5u8; 32]));
        assert_eq!(
            w.last_scanned_tip(),
            Some([5u8; 32]),
            "precondition: a tip is recorded"
        );

        w.set_last_scanned_height(11);

        assert_eq!(w.last_scanned_height(), 11);
        assert_eq!(
            w.last_scanned_tip(),
            None,
            "a cursor written without a tip must not keep the previous lineage"
        );
    }

    #[test]
    fn scan_progress_persists_the_cursor_without_a_previous_processs_tip() {
        let path = std::env::temp_dir().join(format!("btc-progress-{}.json", std::process::id()));
        let mut w = open_fixture([7u8; 64], bitcoin::Network::Regtest, path.clone()).unwrap();
        w.set_scan_progress(42, Some([9u8; 32]));
        drop(w);

        let loaded = open_fixture([7u8; 64], bitcoin::Network::Regtest, path.clone()).unwrap();
        assert_eq!(loaded.last_scanned_height(), 42);
        assert_eq!(
            loaded.last_scanned_tip(),
            None,
            "the tip describes a journal that does not survive the process"
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn test_best_height_default_zero() {
        let w = BtcWallet::new([7u8; 64]);
        assert_eq!(w.best_height(), 0);
    }

    #[test]
    fn test_synced_default_false() {
        let w = BtcWallet::new([7u8; 64]);
        assert!(!w.synced());
    }

    fn contract(order_id: &str, input: Option<(&str, u32)>) -> crate::utxo::SwapContract {
        crate::utxo::SwapContract {
            order_id: order_id.into(),
            funding_txid: "ab".repeat(32),
            hash_lock: [0u8; 32],
            recipient: "bc1qrecipient".into(),
            refund_address: "bc1qrefund".into(),
            expiry: 100,
            amount: 10_000,
            refund_raw: None,
            input_txid: input.map(|(t, _)| t.to_string()),
            input_vout: input.map(|(_, v)| v),
            input_released: false,
        }
    }

    #[test]
    fn test_release_swap_reservation_makes_an_unspent_input_spendable() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-release-spendable-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let w = open_fixture([7u8; 64], bitcoin::Network::Regtest, path.clone()).unwrap();
        w.add_utxo(Utxo {
            txid: "ce".repeat(32),
            vout: 0,
            value: 50_000,
            address: "bc1qtest".into(),
            height: 1,
        });
        let mut set = w.utxos.lock();
        set.reserve_utxo(Utxo {
            txid: "ce".repeat(32),
            vout: 0,
            value: 50_000,
            address: "bc1qtest".into(),
            height: 1,
        });
        set.record_swap_contract(contract("order2", Some((&"ce".repeat(32), 0))));
        set.save(&path).unwrap();
        drop(set);

        assert!(w.release_swap_reservation("order2").unwrap());
        assert_eq!(w.list_utxos().len(), 1, "released input must be spendable");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_release_swap_reservation_restores_input_and_persists() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-btc-release-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");

        let w = open_fixture([7u8; 64], bitcoin::Network::Regtest, path.clone()).unwrap();
        w.add_utxo(Utxo {
            txid: "cd".repeat(32),
            vout: 0,
            value: 50_000,
            address: "bc1qtest".into(),
            height: 1,
        });
        // Reserve the input and record a contract pointing at it.
        let mut set = w.utxos.lock();
        set.reserve(&"cd".repeat(32), 0);
        set.record_swap_contract(contract("order1", Some((&"cd".repeat(32), 0))));
        set.save(&path).unwrap();
        drop(set);
        assert!(w.list_utxos().is_empty());

        assert!(w.release_swap_reservation("order1").unwrap());
        // The contract is gone and the input can be re-added by a rescan.
        assert!(w.swap_contract("order1").is_none());
        w.add_utxo(Utxo {
            txid: "cd".repeat(32),
            vout: 0,
            value: 50_000,
            address: "bc1qtest".into(),
            height: 1,
        });
        assert_eq!(w.list_utxos().len(), 1);

        // The release persisted: a fresh wallet sees no reservation.
        drop(w);
        let reloaded = open_fixture([7u8; 64], bitcoin::Network::Regtest, path.clone()).unwrap();
        reloaded.add_utxo(Utxo {
            txid: "cd".repeat(32),
            vout: 0,
            value: 50_000,
            address: "bc1qtest".into(),
            height: 1,
        });
        assert!(reloaded.list_utxos().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_released_swap_cannot_be_re_reserved_by_a_covered_rollback() {
        let mut w = BtcWallet::new([7u8; 64]);
        let record = Utxo {
            txid: "ce".repeat(32),
            vout: 0,
            value: 40_000,
            address: "bc1qtest".into(),
            height: 5,
        };
        {
            let mut set = w.utxos.lock();
            set.reserve_utxo(record.clone());
            set.record_swap_contract(contract("order1", Some((&record.txid, record.vout))));
            assert!(set.note_spend(&record.txid, record.vout));
            set.record_reservation_spend(record.clone(), 10);
        }

        assert!(w.release_swap_reservation("order1").unwrap());
        assert!(w.swap_contract("order1").is_none());
        assert!(!w.reserved_input_spent(&record.txid, record.vout));
        assert_eq!(
            w.observed_reservation_spend_count_for_tests(),
            0,
            "releasing the contract must drop the observation that would re-reserve it"
        );

        w.push_undo_for_tests(crate::utxo::UndoEntry {
            height: 11,
            created: vec![],
            removed: vec![],
        });
        assert!(w.reset_for_reorg_at_for_tests(Some(10)));

        assert!(
            w.reserved_utxo(&record.txid, record.vout).is_none(),
            "the contract is gone, so a covered rollback must not re-reserve the outpoint"
        );
        assert!(
            !w.reserved_input_spent(&record.txid, record.vout),
            "an observation of a released swap must not survive the rollback"
        );
    }

    #[test]
    fn test_release_skips_contract_without_tracked_input() {
        // Contracts persisted before input tracking existed must never be
        // auto-released, since we cannot identify the reserved outpoint.
        let w = BtcWallet::new([7u8; 64]);
        {
            let mut set = w.utxos.lock();
            set.record_swap_contract(contract("legacy", None));
        }
        assert!(!w.release_swap_reservation("legacy").unwrap());
        assert!(w.swap_contract("legacy").is_some(), "contract must remain");
    }
}

//! UTXO set tracking for the wallet's addresses.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug)]
struct UncertainSave(std::io::Error);

impl std::fmt::Display for UncertainSave {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rename completed but directory sync failed: {}", self.0)
    }
}

impl std::error::Error for UncertainSave {}

pub(crate) fn save_may_have_committed(error: &std::io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|source| source.is::<UncertainSave>())
}

/// A per-process unique temporary beside `path`.
///
/// Unlike a fixed `<path>.tmp`, `create_new` on this name cannot follow a
/// planted symlink or overwrite another writer's file.
fn unique_temp_path(path: &Path) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "utxos.json".to_owned());
    path.with_file_name(format!(".{name}.tmp.{}.{sequence}", std::process::id()))
}

fn open_unique_temp(path: &Path) -> std::io::Result<(std::path::PathBuf, std::fs::File)> {
    let tmp = unique_temp_path(path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&tmp)?;
    Ok((tmp, file))
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::File::open(parent)?.sync_all()
}

/// Acquire the single-writer lock for a wallet-state path.
///
/// The OS releases the lock when the file is dropped or the process exits, so a
/// crash cannot leave a stale lock.
pub fn lock_state_path(path: &Path) -> std::io::Result<std::fs::File> {
    use fs2::FileExt;

    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "BTC state path must be a regular file, not a symlink",
            ));
        }
        Ok(metadata) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.nlink() != 1 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "BTC state file must not have hard-link aliases",
                    ));
                }
            }
            #[cfg(not(unix))]
            let _ = metadata;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let lock_path = path.with_extension("lock");
    match std::fs::symlink_metadata(&lock_path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "BTC lock path must be a regular file, not a symlink",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&lock_path)?;
    file.try_lock_exclusive().map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!(
                "BTC wallet state {} is in use by another process: {error}",
                path.display()
            ),
        )
    })?;
    Ok(file)
}

pub(crate) fn record_wallet_initialization(path: &Path, must_be_new: bool) -> std::io::Result<()> {
    let marker = path.with_extension("initialized");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = match options.open(&marker) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && !must_be_new => {
            if !std::fs::symlink_metadata(&marker)?.file_type().is_file()
                || std::fs::read(&marker)? != b"vtorrent-btc-initialized-v1\n"
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "BTC initialization record is invalid",
                ));
            }
            std::fs::File::open(&marker)?.sync_all()?;
            #[cfg(unix)]
            sync_parent(&marker)?;
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "BTC state is missing but an initialization record exists; preserve it for recovery"));
        }
        Err(error) => return Err(error),
    };
    std::io::Write::write_all(&mut file, b"vtorrent-btc-initialized-v1\n")?;
    file.sync_all()?;
    #[cfg(unix)]
    sync_parent(&marker)?;
    Ok(())
}

/// Hard cap on journalled heights. Independent of `--btc-header-retention`, so
/// an unbounded header chain cannot produce an unbounded journal.
pub(crate) const MAX_UNDO_ENTRIES: usize = 200_000;

/// Schema version of the persisted set. Version 2 records a confirmed local
/// send's transaction inside its journal entry, so a reorg can restore the hold.
/// Anything older cannot say whether a removal was a local send.
pub(crate) const JOURNAL_VERSION: u8 = 2;
const WALLET_FORMAT_VERSION: u8 = 3;

/// Stand-in transaction id for a legacy removal whose spending transaction was
/// never recorded. The scan retires a hold by outpoint, so the value only has to
/// stay stable and impossible to confuse with a real transaction id.
const UNKNOWN_SEND_TXID: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// A spendable output owned by the wallet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Utxo {
    /// Transaction id (hex).
    pub txid: String,
    /// Output index.
    pub vout: u32,
    /// Value in satoshis.
    pub value: u64,
    /// The address this output pays to.
    pub address: String,
    /// Block height where this output was confirmed (0 = mempool).
    pub height: u32,
}

/// Reservation state an outpoint had before a block removed it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PriorState {
    /// The outpoint was spendable.
    Free,
    /// The outpoint was held for a swap; the record restores it as reserved.
    Reserved(Utxo),
    /// The outpoint had already been spent while reserved.
    SpentReserved,
    /// A local send may return to a mempool if its confirmation is reorged out.
    PendingSend {
        txid: String,
        height: u32,
        raw: Option<Vec<u8>>,
    },
}

/// An output a block removed from the set, with the state to restore.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemovedOutput {
    pub utxo: Utxo,
    pub prior: PriorState,
}

/// Per-height record of what one block did to the set, so a reorg can undo it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoEntry {
    pub height: u32,
    pub created: Vec<Utxo>,
    pub removed: Vec<RemovedOutput>,
}

/// A wallet-local spend the scan has not observed on chain yet.
///
/// A send removes its inputs immediately, before any block can confirm them, so
/// its removal belongs to no scanned height. Keeping it out of the height-indexed
/// journal is what makes the boundary case correct: a spend built at cursor `C`
/// can confirm at any `H >= C`, so a fork `F` with `C <= F < H` would reorg the
/// spend out while the journalled height `C` survives [`UtxoSet::undo_above`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingRemoval {
    /// The outpoint the send removed, with the state to restore.
    pub removed: RemovedOutput,
    /// Transaction id the send broadcast. The scan retires the record when it
    /// sees this transaction spend the outpoint.
    pub txid: String,
    /// Scan cursor when the send was built.
    pub height: u32,
    /// Signed transaction to rebroadcast when delivery is uncertain.
    #[serde(default)]
    pub raw: Option<Vec<u8>>,
}

/// Public contract terms needed to recover the BTC refund after restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapContract {
    pub order_id: String,
    pub funding_txid: String,
    pub hash_lock: [u8; 32],
    pub recipient: String,
    pub refund_address: String,
    pub expiry: u32,
    pub amount: u64,
    pub refund_raw: Option<Vec<u8>>,
    /// The wallet outpoint reserved to fund this swap, so the reservation can
    /// be released after the funding tx is confirmed dead. `None` for
    /// contracts persisted before this field existed (never auto-released).
    #[serde(default)]
    pub input_txid: Option<String>,
    #[serde(default)]
    pub input_vout: Option<u32>,
}

/// In-memory UTXO set with optional disk persistence.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UtxoSet {
    utxos: Vec<Utxo>,
    #[serde(default)]
    reserved: std::collections::BTreeSet<(String, u32)>,
    #[serde(default)]
    reserved_utxos: Vec<Utxo>,
    #[serde(default)]
    spent_reserved: std::collections::BTreeSet<(String, u32)>,
    #[serde(default)]
    pending_swap_transactions: std::collections::BTreeMap<String, Vec<u8>>,
    #[serde(default)]
    swap_contracts: std::collections::BTreeMap<String, SwapContract>,
    #[serde(default)]
    next_index: u32,
    #[serde(default)]
    last_scanned_height: u32,
    #[serde(default)]
    rescan_required: bool,
    /// Earliest height this wallet intends to cover, recorded when it starts
    /// scanning. A birthday below the retained floor means history is missing.
    #[serde(default)]
    birthday: Option<u32>,
    /// Hash of the tip the scan cursor reached. Persisted, always cleared on
    /// load, and read by no decision today: it is the seam for a future
    /// lineage-persistence design, not a coverage check.
    #[serde(default)]
    last_scanned_tip: Option<[u8; 32]>,
    /// Sparse ascending journal of chain-derived changes, for rollback.
    #[serde(default)]
    undo: Vec<UndoEntry>,
    /// Wallet-local spends the scan has not observed yet, appended by a send.
    /// Independent of the journal's height ordering: a spend built at the scan
    /// cursor can confirm at or above it.
    #[serde(default)]
    pending_removals: Vec<PendingRemoval>,
    /// Schema version of the file this set was read from.
    #[serde(default)]
    journal_version: u8,
    /// Reservation spends observed by this process's reconciliation scan,
    /// mapped to the reserved record they replaced and the chain height the
    /// observation was made at. Live-process only: a covered rollback restores
    /// the observations made above its fork.
    #[serde(default, skip)]
    observed_reservation_spends: std::collections::BTreeMap<(String, u32), (Utxo, u32)>,
    /// Legacy swap contracts already reported as unable to prove what they
    /// hold, so the warning fires once per contract rather than once per
    /// rollback. Live-process only.
    #[serde(default, skip)]
    legacy_contract_warnings: std::collections::BTreeSet<String>,
    /// Set once any confirmed output has been tracked, so a floor change can
    /// never silently adopt a height this wallet may have held funds below.
    #[serde(default)]
    seen_funds: bool,
}

impl UtxoSet {
    pub fn new() -> Self {
        Self {
            journal_version: WALLET_FORMAT_VERSION,
            ..Self::default()
        }
    }

    pub fn next_index(&self) -> u32 {
        self.next_index
    }

    pub fn set_next_index(&mut self, index: u32) {
        self.next_index = index;
    }

    pub fn last_scanned_height(&self) -> u32 {
        self.last_scanned_height
    }

    pub fn birthday(&self) -> Option<u32> {
        self.birthday
    }

    pub fn set_birthday(&mut self, birthday: Option<u32>) {
        self.birthday = birthday;
    }

    pub fn set_last_scanned_height(&mut self, height: u32) {
        self.last_scanned_height = height;
    }

    pub fn last_scanned_tip(&self) -> Option<[u8; 32]> {
        self.last_scanned_tip
    }

    pub fn set_last_scanned_tip(&mut self, tip: Option<[u8; 32]>) {
        self.last_scanned_tip = tip;
    }

    pub(crate) fn rescan_required(&self) -> bool {
        self.rescan_required
    }

    pub(crate) fn set_rescan_required(&mut self, required: bool) {
        self.rescan_required = required;
    }

    /// Insert a spendable output.
    ///
    /// Returns true only when the output was newly recorded. An outpoint the
    /// set already holds — or one currently reserved — is left untouched.
    pub fn add(&mut self, utxo: Utxo) -> bool {
        let key = (utxo.txid.clone(), utxo.vout);
        if self.spent_reserved.contains(&key)
            || self.reserved.contains(&key)
            || self.pending_removals.iter().any(|pending| {
                pending.removed.utxo.txid == utxo.txid && pending.removed.utxo.vout == utxo.vout
            })
        {
            return false;
        }
        if self.swap_holds_outpoint(&utxo.txid, utxo.vout) {
            self.reserve_utxo(utxo);
            return false;
        }
        if !self
            .utxos
            .iter()
            .any(|u| u.txid == utxo.txid && u.vout == utxo.vout)
        {
            self.seen_funds = true;
            self.utxos.push(utxo);
            return true;
        }
        false
    }

    /// Whether this wallet has ever tracked a confirmed output.
    pub fn has_seen_funds(&self) -> bool {
        self.seen_funds
    }

    pub fn remove(&mut self, txid: &str, vout: u32) {
        self.utxos.retain(|u| !(u.txid == txid && u.vout == vout));
    }

    /// Clear chain-derived UTXOs while preserving recovery metadata.
    ///
    /// A reorg invalidates confirmed outputs, but reservations, signed swap
    /// transactions, contracts, and the address index are wallet state, not
    /// chain state. Keeping them prevents a rescan from making a reserved
    /// input selectable again and preserves recovery after restart.
    pub fn clear_chain_utxos(&mut self) {
        self.utxos.clear();
    }

    /// Drop chain-derived UTXOs confirmed above `height`.
    ///
    /// A reorg only invalidates blocks above the last common ancestor, so the
    /// rescan can resume from there instead of re-deriving the whole retained
    /// window. Mempool entries (height 0) and wallet metadata are untouched.
    pub fn rollback_above(&mut self, height: u32) {
        self.utxos.retain(|utxo| utxo.height <= height);
    }

    /// The wallet's record of an outpoint, whether it is spendable in `utxos`
    /// or held in `reserved_utxos`. Reserved inputs have already left the
    /// spendable set, so a scan that sees one spent still needs its full record
    /// to journal the removal.
    pub fn outpoint(&self, txid: &str, vout: u32) -> Option<Utxo> {
        self.utxos
            .iter()
            .chain(self.reserved_utxos.iter())
            .find(|utxo| utxo.txid == txid && utxo.vout == vout)
            .cloned()
    }

    /// The undo journal, oldest entry first.
    pub fn undo_entries(&self) -> &[UndoEntry] {
        &self.undo
    }

    /// Replace the journal verbatim, bypassing [`Self::push_undo`]'s ordering
    /// guard, so tests can construct an unordered or gapped journal.
    #[cfg(test)]
    pub(crate) fn set_undo_raw(&mut self, entries: Vec<UndoEntry>) {
        self.undo = entries;
    }

    /// Record one height's changes, keeping the journal ascending and bounded.
    ///
    /// An entry for the same height as the last one is merged, because a block
    /// can touch the wallet through several transactions. An entry for a height
    /// *below* the tail is ignored: it can only come from a reconciliation scan
    /// re-reading old history, and appending it would make [`Self::undo_above`]
    /// reject the whole journal as non-ascending. The oldest entries are then
    /// trimmed so the journal never exceeds [`MAX_UNDO_ENTRIES`], whatever the
    /// header retention and whichever scan path appends.
    pub fn push_undo(&mut self, entry: UndoEntry) {
        if let Some(last) = self.undo.last_mut() {
            if last.height == entry.height {
                last.created.extend(entry.created);
                last.removed.extend(entry.removed);
                return;
            }
            if last.height > entry.height {
                self.preserve_local_sends(vec![entry]);
                return;
            }
        }
        self.undo.push(entry);
        if self.undo.len() > MAX_UNDO_ENTRIES {
            let removed: Vec<_> = self
                .undo
                .drain(..self.undo.len() - MAX_UNDO_ENTRIES)
                .collect();
            self.preserve_local_sends(removed);
        }
    }

    /// Drop every recovery record built on the chain state being reset.
    ///
    /// Reservation observations belong here too: they describe a spend on the
    /// abandoned branch, and replaying one after a full reset would re-reserve
    /// an outpoint the new branch may already have spent.
    pub fn clear_undo(&mut self) {
        self.undo.clear();
        self.pending_removals.clear();
        self.observed_reservation_spends.clear();
    }

    /// Clear the height journal while retaining wallet-local sends whose
    /// transactions may still be live. A restart cannot know whether an
    /// unconfirmed send reached a peer, so its inputs must stay unselectable
    /// until a scan observes the spend or an explicit recovery cancels it.
    pub fn clear_journal(&mut self) {
        let entries = std::mem::take(&mut self.undo);
        self.preserve_local_sends(entries);
        self.observed_reservation_spends.clear();
    }

    fn preserve_local_sends(&mut self, entries: Vec<UndoEntry>) {
        for entry in entries {
            for removed in entry.removed {
                if let PriorState::PendingSend { txid, height, raw } = removed.prior {
                    if !self.pending_removals.iter().any(|pending| {
                        pending.removed.utxo.txid == removed.utxo.txid
                            && pending.removed.utxo.vout == removed.utxo.vout
                    }) {
                        self.pending_removals.push(PendingRemoval {
                            removed: RemovedOutput {
                                utxo: removed.utxo,
                                prior: PriorState::Free,
                            },
                            txid,
                            height,
                            raw,
                        });
                    }
                }
            }
        }
    }

    /// The wallet-local spends the scan has not observed yet.
    pub fn pending_removals(&self) -> &[PendingRemoval] {
        &self.pending_removals
    }

    /// Turn a legacy journal's plain removals into fail-closed holds.
    ///
    /// Before [`JOURNAL_VERSION`] 2 the journal did not record whether a
    /// removal came from a local send, so a reorg could restore an input whose
    /// transaction was still live in a mempool. Each such removal now replays
    /// as a hold with no signed bytes. The scan retires it when it observes any
    /// spend of the outpoint, so a genuine external spend is not hidden for
    /// longer than the next scan.
    fn adopt_legacy_journal_removals(&mut self) {
        let mut adopted = Vec::new();
        for entry in &mut self.undo {
            let height = entry.height;
            for removed in &mut entry.removed {
                if matches!(removed.prior, PriorState::Free) {
                    removed.prior = PriorState::PendingSend {
                        txid: UNKNOWN_SEND_TXID.into(),
                        height,
                        raw: None,
                    };
                    adopted.push((removed.utxo.clone(), height));
                }
            }
        }
        for (utxo, height) in adopted {
            if !self.pending_removals.iter().any(|pending| {
                pending.removed.utxo.txid == utxo.txid && pending.removed.utxo.vout == utxo.vout
            }) {
                self.pending_removals.push(PendingRemoval {
                    removed: RemovedOutput {
                        utxo,
                        prior: PriorState::Free,
                    },
                    txid: UNKNOWN_SEND_TXID.into(),
                    height,
                    raw: None,
                });
            }
        }
    }

    pub(crate) fn pending_sends(&self) -> Vec<crate::wallet::PendingSend> {
        let mut sends: Vec<crate::wallet::PendingSend> = Vec::new();
        for pending in &self.pending_removals {
            match sends.iter_mut().find(|send| send.txid == pending.txid) {
                Some(send) => send.inputs.push(pending.removed.utxo.clone()),
                None => sends.push(crate::wallet::PendingSend {
                    txid: pending.txid.clone(),
                    raw: pending.raw.clone(),
                    inputs: vec![pending.removed.utxo.clone()],
                }),
            }
        }
        sends
    }

    #[cfg(test)]
    pub(crate) fn fill_pending_for_test(&mut self, count: usize, utxo: Utxo) {
        self.record_pending_removals(&[utxo], "test", 0);
        self.pending_removals
            .resize(count, self.pending_removals[0].clone());
    }

    /// Record the outpoints a wallet-local send removed.
    ///
    /// Each record carries the spending transaction and the cursor the send was
    /// built at, so the scan can retire it when the spend confirms and a covered
    /// rollback can restore it without consulting the journal's heights.
    pub fn record_pending_removals(&mut self, utxos: &[Utxo], txid: &str, height: u32) {
        for utxo in utxos {
            if self.pending_removals.iter().any(|pending| {
                pending.removed.utxo.txid == utxo.txid && pending.removed.utxo.vout == utxo.vout
            }) {
                continue;
            }
            let prior = if let Some(reserved) = self.reserved_utxo(&utxo.txid, utxo.vout) {
                PriorState::Reserved(reserved.clone())
            } else if self.was_reserved_spent(&utxo.txid, utxo.vout) {
                PriorState::SpentReserved
            } else {
                PriorState::Free
            };
            self.pending_removals.push(PendingRemoval {
                removed: RemovedOutput {
                    utxo: utxo.clone(),
                    prior,
                },
                txid: txid.to_owned(),
                height,
                raw: None,
            });
        }
    }

    /// Attach the signed transaction to the records of one send.
    pub fn record_pending_send_raw(&mut self, txid: &str, raw: &[u8]) {
        for pending in &mut self.pending_removals {
            if pending.txid == txid {
                pending.raw = Some(raw.to_vec());
            }
        }
    }

    /// Retire the pending record for an outpoint the scan just saw spent.
    ///
    /// The spending transaction is matched first so the record is retired by the
    /// send it belongs to. A record for the same outpoint is still taken when
    /// the txid differs, because an outpoint can only be spent once: a
    /// replacement transaction then keeps the fold complete instead of losing
    /// the removal.
    pub fn take_pending_removal(
        &mut self,
        spending_txid: &str,
        txid: &str,
        vout: u32,
    ) -> Option<RemovedOutput> {
        let matches = |pending: &PendingRemoval| {
            pending.removed.utxo.txid == txid && pending.removed.utxo.vout == vout
        };
        let index = self
            .pending_removals
            .iter()
            .position(|pending| matches(pending) && pending.txid == spending_txid)
            .or_else(|| self.pending_removals.iter().position(matches))?;
        let pending = self.pending_removals.remove(index);
        Some(RemovedOutput {
            utxo: pending.removed.utxo,
            prior: PriorState::PendingSend {
                txid: pending.txid,
                height: pending.height,
                raw: pending.raw,
            },
        })
    }

    /// Cancel the removal state a failed broadcast must not leave behind.
    ///
    /// A send no longer journals at the cursor, so the matching record is a
    /// pending removal. A removal a scan already journalled is cancelled too,
    /// so the send never happened as far as a later rollback is concerned.
    pub fn cancel_local_removals(&mut self, utxos: &[Utxo]) {
        for entry in &mut self.undo {
            entry.removed.retain(|removed| {
                !utxos
                    .iter()
                    .any(|utxo| removed.utxo.txid == utxo.txid && removed.utxo.vout == utxo.vout)
            });
        }
        self.pending_removals.retain(|pending| {
            !utxos.iter().any(|utxo| {
                pending.removed.utxo.txid == utxo.txid && pending.removed.utxo.vout == utxo.vout
            })
        });
    }

    /// Remember that this process's reconciliation scan observed `utxo` spent
    /// while it was reserved, so a covered rollback can put the reservation
    /// back instead of leaving `spent_reserved` behind forever.
    ///
    /// `observed_at` is the chain height the observation was made at. A
    /// rollback only restores observations above its fork: a spend observed
    /// below the fork happened on the surviving branch and is still true.
    pub(crate) fn record_reservation_spend(&mut self, utxo: Utxo, observed_at: u32) {
        self.observed_reservation_spends
            .insert((utxo.txid.clone(), utxo.vout), (utxo, observed_at));
    }

    /// Drop the live-process observation recorded for an outpoint.
    ///
    /// Once the contract that held the input is gone, the observation can only
    /// re-reserve an outpoint nothing needs: `release_expired_reservations`
    /// iterates contracts, so a contract-less reservation is never released.
    pub(crate) fn forget_reservation_spend(&mut self, txid: &str, vout: u32) {
        self.observed_reservation_spends
            .remove(&(txid.to_owned(), vout));
    }

    /// How many live-process reservation observations are held.
    #[cfg(test)]
    pub(crate) fn observed_reservation_spend_count(&self) -> usize {
        self.observed_reservation_spends.len()
    }

    /// Drop journal entries below `floor`, then keep at most `max_entries`
    /// of the newest ones.
    ///
    /// Pending removals are retained independently of journal pruning.
    pub fn prune_undo(&mut self, floor: u32, max_entries: usize) {
        let (keep, removed): (Vec<_>, Vec<_>) = std::mem::take(&mut self.undo)
            .into_iter()
            .partition(|entry| entry.height >= floor);
        self.undo = keep;
        self.preserve_local_sends(removed);
        if self.undo.len() > max_entries {
            let removed: Vec<_> = self.undo.drain(..self.undo.len() - max_entries).collect();
            self.preserve_local_sends(removed);
        }
    }

    /// Roll the set back to `fork` by replaying the journal.
    ///
    /// Returns true only when the journal covers every height above `fork`:
    /// entries must be strictly ascending and the lowest one above the fork
    /// must be `fork + 1`. Otherwise nothing is mutated and the caller falls
    /// back to a full reset.
    ///
    /// A `removed` outpoint the same entry also created is not restored: the
    /// block created and spent it, so undoing the creation must win. Restoring
    /// it would leave a phantom UTXO the post-fork rescan can never remove.
    ///
    /// Pending wallet-local removals and reservation observations are put back
    /// before the replay, not after: an entry that undoes the creation of the
    /// very outpoint they belong to must win, or the rollback would leave a
    /// phantom record behind. A pending spend the new branch still confirms is
    /// removed again by the post-fork scan, so restoring it is self-correcting.
    /// An observation below the fork survives: it recorded a spend on the
    /// branch the rollback keeps.
    pub fn undo_above(&mut self, fork: u32) -> bool {
        if !self.undo_covers(fork) {
            return false;
        }
        for (key, (utxo, observed_at)) in std::mem::take(&mut self.observed_reservation_spends) {
            if observed_at <= fork {
                self.observed_reservation_spends
                    .insert(key, (utxo, observed_at));
                continue;
            }
            self.spent_reserved.remove(&key);
            // Same guard as `restore_removed`: an observation can outlive the
            // contract that made it, and nothing would ever release it again.
            if self.swap_holds_outpoint(&key.0, key.1) {
                self.reserve_utxo(utxo);
            } else {
                // The observation was the last copy of the record: `note_spend`
                // dropped it from `reserved_utxos`. The spend happened on the
                // branch this rollback abandons, so the outpoint is unspent
                // again, and the post-fork scan starts at `fork + 1`, so only
                // restoring it here puts it back in `utxos`.
                self.insert_direct(utxo);
            }
        }
        // Keep wallet-local sends in place. A reorg does not prove that the
        // transaction left the mempool, so restoring its inputs here would let
        // the wallet build a conflicting spend while the first one is live.
        while let Some(entry) = self.undo.last() {
            if entry.height <= fork {
                break;
            }
            let entry = self.undo.pop().expect("entry just inspected");
            let created_in_block: std::collections::HashSet<(&str, u32)> = entry
                .created
                .iter()
                .map(|utxo| (utxo.txid.as_str(), utxo.vout))
                .collect();
            for removed in entry.removed {
                if created_in_block.contains(&(removed.utxo.txid.as_str(), removed.utxo.vout)) {
                    continue;
                }
                self.restore_removed(removed);
            }
            for created in entry.created {
                self.forget_outpoint(&created.txid, created.vout);
            }
        }
        self.last_scanned_tip = None;
        true
    }

    /// Drop every record of an outpoint: spendable, reserved, and
    /// spent-reserved. Undoing a creation must not leave a phantom reservation
    /// whose funding transaction no longer exists.
    fn forget_outpoint(&mut self, txid: &str, vout: u32) {
        let key = (txid.to_owned(), vout);
        self.utxos.retain(|u| !(u.txid == txid && u.vout == vout));
        if self.swap_holds_outpoint(txid, vout) {
            return;
        }
        self.reserved.remove(&key);
        self.reserved_utxos
            .retain(|u| !(u.txid == txid && u.vout == vout));
        self.spent_reserved.remove(&key);
    }

    fn undo_covers(&self, fork: u32) -> bool {
        let ascending = self
            .undo
            .windows(2)
            .all(|pair| pair[0].height < pair[1].height);
        if !ascending {
            return false;
        }
        self.undo
            .iter()
            .find(|entry| entry.height > fork)
            .is_some_and(|entry| entry.height == fork.saturating_add(1))
    }

    fn restore_removed(&mut self, removed: RemovedOutput) {
        let RemovedOutput { utxo, prior } = removed;
        if let PriorState::PendingSend { txid, height, raw } = prior {
            // Replay can meet a hold that `clear_journal` already preserved, so
            // a second copy for the same outpoint would never be retired and
            // would hide the input for good.
            if !self.pending_removals.iter().any(|pending| {
                pending.removed.utxo.txid == utxo.txid && pending.removed.utxo.vout == utxo.vout
            }) {
                self.pending_removals.push(PendingRemoval {
                    removed: RemovedOutput {
                        utxo,
                        prior: PriorState::Free,
                    },
                    txid,
                    height,
                    raw,
                });
            }
            return;
        }
        let key = (utxo.txid.clone(), utxo.vout);
        let restored = utxo.clone();
        self.insert_direct(utxo);
        match prior {
            PriorState::Free => {}
            PriorState::Reserved(reserved) => {
                self.spent_reserved.remove(&key);
                // Only re-reserve while something still needs the input held.
                // A released swap contract cannot release it again, and no
                // confirmed spend exists to expire the reservation, so an
                // orphaned reservation would hide the outpoint forever.
                if self.swap_holds_outpoint(&key.0, key.1) {
                    self.reserve_utxo(reserved);
                }
            }
            PriorState::SpentReserved => {
                if self.swap_holds_outpoint(&key.0, key.1) {
                    self.spent_reserved.remove(&key);
                    self.reserve_utxo(restored);
                } else {
                    self.reserved.remove(&key);
                    self.reserved_utxos
                        .retain(|u| !(u.txid == key.0 && u.vout == key.1));
                    self.utxos.retain(|u| !(u.txid == key.0 && u.vout == key.1));
                    self.spent_reserved.insert(key);
                }
            }
            PriorState::PendingSend { .. } => unreachable!(),
        }
    }

    /// Whether a stored outpoint id references `txid:vout`.
    ///
    /// Both ids are compared as parsed [`bitcoin::Txid`] bytes, so a
    /// non-canonical stored spelling still matches the outpoint it names. An id
    /// that cannot be parsed cannot prove it is a different outpoint, so it
    /// counts as a match (fail closed).
    fn outpoint_matches(stored_txid: &str, stored_vout: u32, txid: &str, vout: u32) -> bool {
        use std::str::FromStr;

        if stored_vout != vout {
            return false;
        }
        match (
            bitcoin::Txid::from_str(stored_txid),
            bitcoin::Txid::from_str(txid),
        ) {
            (Ok(stored), Ok(queried)) => stored == queried,
            _ => true,
        }
    }

    /// Whether a persisted swap contract or a retained signed funding
    /// transaction may still spend `txid:vout`.
    ///
    /// Fails closed: a retained record that cannot prove it does not reference
    /// the outpoint counts as referencing it, so no rollback frees an input a
    /// live contract still holds. Only a definite non-reference frees one.
    fn swap_holds_outpoint(&mut self, txid: &str, vout: u32) -> bool {
        let mut unreported: Vec<SwapContract> = Vec::new();
        let mut contract_holds = false;
        for contract in self.swap_contracts.values() {
            match (contract.input_txid.as_deref(), contract.input_vout) {
                // A contract persisted before input tracking has no recorded
                // input; only its retained funding raw can rule the outpoint
                // out.
                (None, _) => {
                    let held = self.legacy_contract_holds(contract, txid, vout);
                    if held
                        && !self.retained_funding_is_usable(contract)
                        && !self.legacy_contract_warnings.contains(&contract.order_id)
                    {
                        unreported.push(contract.clone());
                    }
                    contract_holds |= held;
                }
                (Some(_), None) => contract_holds = true,
                (Some(held_txid), Some(held_vout)) => {
                    contract_holds |= Self::outpoint_matches(held_txid, held_vout, txid, vout);
                }
            }
        }
        for contract in unreported {
            self.warn_unresolvable_legacy_contract(&contract);
        }
        contract_holds
            || self.pending_swap_transactions.values().any(|raw| {
                match bitcoin::consensus::deserialize::<bitcoin::Transaction>(raw) {
                    // An unparseable retained transaction could still be the
                    // funding spend of any outpoint.
                    Err(_) => true,
                    Ok(tx) => tx.input.iter().any(|input| {
                        Self::outpoint_matches(
                            &input.previous_output.txid.to_string(),
                            input.previous_output.vout,
                            txid,
                            vout,
                        )
                    }),
                }
            })
    }

    /// Whether a legacy contract retains a funding raw that can be decoded and
    /// compared against a restored outpoint.
    fn retained_funding_is_usable(&self, contract: &SwapContract) -> bool {
        self.pending_swap_transactions
            .get(&contract.funding_txid)
            .is_some_and(|raw| bitcoin::consensus::deserialize::<bitcoin::Transaction>(raw).is_ok())
    }

    /// Whether a contract persisted before input tracking may still hold
    /// `txid:vout`.
    ///
    /// With no recorded input, the retained funding raw is the only proof: a
    /// transaction that parses and spends another outpoint rules the reference
    /// out, and one that spends this outpoint keeps it held. Without a raw — or
    /// with one that does not parse — nothing can rule it out, so it stays held.
    fn legacy_contract_holds(&self, contract: &SwapContract, txid: &str, vout: u32) -> bool {
        let Some(raw) = self.pending_swap_transactions.get(&contract.funding_txid) else {
            return true;
        };
        match bitcoin::consensus::deserialize::<bitcoin::Transaction>(raw) {
            Err(_) => true,
            Ok(funding) => funding.input.iter().any(|input| {
                Self::outpoint_matches(
                    &input.previous_output.txid.to_string(),
                    input.previous_output.vout,
                    txid,
                    vout,
                )
            }),
        }
    }

    /// Report a legacy contract that cannot be narrowed, once per contract.
    ///
    /// Without a retained funding raw — or with one that does not parse — there
    /// is nothing to compare inputs against, so every restored reservation stays
    /// held for as long as the contract exists. That is deliberate, but silent
    /// stickiness is not: an operator needs the reason in the log.
    fn warn_unresolvable_legacy_contract(&mut self, contract: &SwapContract) {
        if !self
            .legacy_contract_warnings
            .insert(contract.order_id.clone())
        {
            return;
        }
        tracing::warn!(
            order_id = %contract.order_id,
            funding_txid = %contract.funding_txid,
            "BTC swap contract predates input tracking and retains no usable funding transaction; \
             every restored reservation is kept held while it exists"
        );
    }

    fn insert_direct(&mut self, utxo: Utxo) {
        if self
            .utxos
            .iter()
            .any(|existing| existing.txid == utxo.txid && existing.vout == utxo.vout)
        {
            return;
        }
        self.seen_funds = true;
        self.utxos.push(utxo);
    }

    pub fn reserve(&mut self, txid: &str, vout: u32) {
        self.remove(txid, vout);
        self.reserved.insert((txid.to_owned(), vout));
    }

    /// Reserve a known UTXO and retain its details so a later scan can detect
    /// whether the outpoint was spent.
    pub fn reserve_utxo(&mut self, utxo: Utxo) {
        self.reserve(&utxo.txid, utxo.vout);
        self.spent_reserved.remove(&(utxo.txid.clone(), utxo.vout));
        self.reserved_utxos
            .retain(|existing| !(existing.txid == utxo.txid && existing.vout == utxo.vout));
        self.reserved_utxos.push(utxo);
    }

    pub fn reserved_utxo(&self, txid: &str, vout: u32) -> Option<&Utxo> {
        self.reserved_utxos
            .iter()
            .find(|utxo| utxo.txid == txid && utxo.vout == vout)
    }

    /// Mark a reserved outpoint as spent by a confirmed transaction.
    pub fn note_spend(&mut self, txid: &str, vout: u32) -> bool {
        let key = (txid.to_owned(), vout);
        if self.reserved.remove(&key) {
            self.reserved_utxos
                .retain(|utxo| !(utxo.txid == txid && utxo.vout == vout));
            self.spent_reserved.insert(key);
            true
        } else {
            false
        }
    }

    pub fn was_reserved_spent(&self, txid: &str, vout: u32) -> bool {
        self.spent_reserved.contains(&(txid.to_owned(), vout))
    }

    pub fn clear_spent_reserved(&mut self, txid: &str, vout: u32) {
        self.spent_reserved.remove(&(txid.to_owned(), vout));
    }

    /// Release a reservation so the input can be selected again.
    ///
    /// Only safe once the funding transaction is known not to be live (see
    /// `BtcWallet::release_swap_reservation`): releasing an input whose signed
    /// funding tx could still confirm would let a second swap spend it.
    /// Returns true if a reservation was actually removed.
    pub fn release(&mut self, txid: &str, vout: u32) -> bool {
        let key = (txid.to_owned(), vout);
        self.reserved_utxos
            .retain(|utxo| !(utxo.txid == txid && utxo.vout == vout));
        self.reserved.remove(&key)
    }

    /// All persisted swap contracts, for reconciliation.
    pub fn swap_contracts(&self) -> Vec<SwapContract> {
        self.swap_contracts.values().cloned().collect()
    }

    /// Drop a swap contract and its retained signed transaction.
    pub fn remove_swap_contract(&mut self, order_id: &str) -> Option<SwapContract> {
        let contract = self.swap_contracts.remove(order_id)?;
        self.pending_swap_transactions
            .remove(&contract.funding_txid);
        Some(contract)
    }

    pub fn record_swap_transaction(&mut self, txid: String, raw: Vec<u8>) {
        self.pending_swap_transactions.insert(txid, raw);
    }

    pub fn record_swap_contract(&mut self, contract: SwapContract) {
        self.swap_contracts
            .insert(contract.order_id.clone(), contract);
    }

    pub fn swap_contract(&self, order_id: &str) -> Option<&SwapContract> {
        self.swap_contracts.get(order_id)
    }

    pub fn pending_swap_transactions(&self) -> &std::collections::BTreeMap<String, Vec<u8>> {
        &self.pending_swap_transactions
    }

    pub fn total(&self) -> u64 {
        self.utxos.iter().map(|u| u.value).sum()
    }

    pub fn list(&self) -> &[Utxo] {
        &self.utxos
    }

    /// Select UTXOs to cover `amount` (plus `fee`), largest-first.
    pub fn select(&self, amount: u64, fee: u64) -> Option<Vec<Utxo>> {
        if amount > crate::MAX_MONEY_SATS || fee > crate::MAX_MONEY_SATS {
            return None;
        }
        let required = amount.checked_add(fee)?;
        let mut sorted: Vec<Utxo> = self.utxos.clone();
        sorted.sort_by_key(|utxo| std::cmp::Reverse(utxo.value));
        let mut selected = Vec::new();
        let mut sum = 0u64;
        for u in sorted {
            sum = sum.saturating_add(u.value);
            selected.push(u);
            if sum >= required {
                return Some(selected);
            }
        }
        None
    }

    fn serialized(&self) -> std::io::Result<Vec<u8>> {
        let mut value = serde_json::to_value(self).map_err(std::io::Error::other)?;
        value["journal_version"] = serde_json::json!(self.journal_version);
        Ok(serde_json::to_string_pretty(&value)
            .map_err(std::io::Error::other)?
            .into_bytes())
    }

    /// Persist the UTXO set to a JSON file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let json = self.serialized()?;
        let (tmp, mut file) = open_unique_temp(path)?;
        if let Err(error) = std::io::Write::write_all(&mut file, &json) {
            let _ = std::fs::remove_file(&tmp);
            return Err(error);
        }
        if let Err(error) = file.sync_all() {
            let _ = std::fs::remove_file(&tmp);
            return Err(error);
        }
        drop(file);
        #[cfg(test)]
        pause_before_rename();
        #[cfg(test)]
        if FAIL_BEFORE_RENAME.with(|flag| flag.replace(false)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(std::io::Error::other("injected pre-rename failure"));
        }
        if let Err(error) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(error);
        }
        #[cfg(unix)]
        {
            #[cfg(test)]
            let injected = FAIL_PARENT_SYNC.with(|flag| flag.replace(false));
            #[cfg(not(test))]
            let injected = false;
            let result = if injected {
                Err(std::io::Error::other("injected parent sync failure"))
            } else {
                sync_parent(path)
            };
            result.map_err(|error| std::io::Error::other(UncertainSave(error)))?;
        }
        tracing::debug!(
            "UTXO set saved: {} entries → {}",
            self.utxos.len(),
            path.display()
        );
        Ok(())
    }

    /// Create a new state file without replacing an existing one or following a
    /// stale temporary path.
    ///
    /// Used only for first-time attested initialization: an existing target is
    /// a hard error, and the no-clobber link makes the final publication atomic.
    pub(crate) fn create_new(&self, path: &Path) -> std::io::Result<()> {
        let json = self.serialized()?;
        let (tmp, mut file) = open_unique_temp(path)?;
        let result = std::io::Write::write_all(&mut file, &json).and_then(|()| file.sync_all());
        drop(file);
        if let Err(error) = result {
            let _ = std::fs::remove_file(&tmp);
            return Err(error);
        }
        if let Err(error) = std::fs::hard_link(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(error);
        }
        std::fs::remove_file(&tmp)?;
        #[cfg(unix)]
        sync_parent(path).map_err(|error| std::io::Error::other(UncertainSave(error)))?;
        Ok(())
    }

    /// Publish this set over an existing state file after an explicit,
    /// separately confirmed legacy promotion.
    ///
    /// Only `btc-state-stage promote` calls this. The temporary name is
    /// exclusive and unpredictable, so a planted file or symlink cannot redirect
    /// the write.
    pub fn replace_for_confirmed_promotion(&self, path: &Path) -> std::io::Result<()> {
        let json = self.serialized()?;
        let (tmp, mut file) = open_unique_temp(path)?;
        let result = std::io::Write::write_all(&mut file, &json).and_then(|()| file.sync_all());
        drop(file);
        if let Err(error) = result {
            let _ = std::fs::remove_file(&tmp);
            return Err(error);
        }
        if let Err(error) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(error);
        }
        #[cfg(unix)]
        sync_parent(path).map_err(|error| std::io::Error::other(UncertainSave(error)))?;
        Ok(())
    }

    /// Convert the exact bytes of an operator-verified legacy copy.
    ///
    /// The caller passes bytes it has already compared against the retained
    /// archive, so promotion cannot be redirected by replacing the staged file
    /// between verification and conversion. The caller must have independently
    /// established that the seed has no outstanding BTC sends or swaps.
    pub fn from_verified_legacy_bytes(bytes: &[u8]) -> std::io::Result<Self> {
        let mut value: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        sanitize_journal_fields(&mut value);
        let mut set: Self = serde_json::from_value(value)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        if set.journal_version >= WALLET_FORMAT_VERSION {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "copy is already in wallet format; nothing to promote",
            ));
        }
        set.adopt_legacy_journal_removals();
        set.journal_version = WALLET_FORMAT_VERSION;
        Ok(set)
    }

    /// Load the UTXO set from a JSON file.  Returns an empty set if the
    /// file does not exist.
    ///
    /// A corrupt tip is discarded. Corrupt undo or pending records fail the
    /// load because they can contain local sends that may still be live.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        Self::load_inner(path, false)
    }

    pub(crate) fn load_for_wallet(path: &Path) -> std::io::Result<Self> {
        Self::load_inner(path, true)
    }

    /// Validate a state file against the wallet-openability guard without
    /// taking the writer lock. Callers that hold [`lock_state_path`] use this.
    pub fn validate_wallet_state(path: &Path) -> std::io::Result<()> {
        Self::load_inner(path, true).map(|_| ())
    }

    fn load_inner(path: &Path, reject_legacy: bool) -> std::io::Result<Self> {
        let json = match std::fs::read_to_string(path) {
            Ok(json) => json,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && reject_legacy => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "BTC wallet state is missing; seed history is unknown and initialization is disabled",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Self::new()),
            Err(error) => return Err(error),
        };
        let mut value: serde_json::Value = serde_json::from_str(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        sanitize_journal_fields(&mut value);
        let set: UtxoSet = serde_json::from_value(value)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let mut set = set;
        if reject_legacy && set.journal_version < WALLET_FORMAT_VERSION {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "legacy BTC wallet history is incomplete or unverifiable; preserve the file for recovery",
            ));
        }
        if reject_legacy && set.journal_version > WALLET_FORMAT_VERSION {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unsupported BTC wallet format version; preserve the file and use compatible software",
            ));
        }
        if set.journal_version < JOURNAL_VERSION {
            set.adopt_legacy_journal_removals();
            set.journal_version = JOURNAL_VERSION;
        }
        tracing::debug!(
            "UTXO set loaded: {} entries from {}",
            set.utxos.len(),
            path.display()
        );
        Ok(set)
    }
}

/// Discard a journalled field that does not parse, leaving the rest of the set
/// intact. Only the stored tip is tolerated this way.
fn sanitize_journal_fields(value: &mut serde_json::Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    if object.contains_key("last_scanned_tip")
        && serde_json::from_value::<Option<[u8; 32]>>(object["last_scanned_tip"].clone()).is_err()
    {
        object.remove("last_scanned_tip");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_pending_removals_fail_closed() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-corrupt-pending-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let mut json = serde_json::to_value(UtxoSet::new()).unwrap();
        json["pending_removals"] = serde_json::json!([{"txid": "broken"}]);
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        assert!(UtxoSet::load(&path).is_err());
        assert!(crate::wallet::BtcWallet::with_persistence(
            [7; 64],
            bitcoin::Network::Regtest,
            path
        )
        .is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn corrupt_undo_cannot_silently_discard_confirmed_send_history() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-corrupt-confirmed-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let mut json = serde_json::to_value(UtxoSet::new()).unwrap();
        json["undo"] = serde_json::json!([{"height": 1, "removed": "corrupt"}]);
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        assert!(UtxoSet::load(&path).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_journal_removals_become_fail_closed_holds() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-legacy-journal-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let spent = utxo("ba", 0, 100_000);
        let json = serde_json::json!({
            "utxos": [utxo("bb", 0, 5_000)],
            "undo": [{
                "height": 106,
                "created": [],
                "removed": [{"utxo": spent, "prior": "Free"}],
            }],
        });
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();

        let loaded = UtxoSet::load(&path).unwrap();
        assert_eq!(
            loaded.pending_removals().len(),
            1,
            "a legacy confirmed removal must become a hold"
        );
        assert_eq!(loaded.pending_removals()[0].removed.utxo.txid, spent.txid);
        assert!(
            loaded.pending_removals()[0].raw.is_none(),
            "a legacy hold has no signed bytes to retry"
        );
        let mut loaded = loaded;
        assert!(
            !loaded.add(spent),
            "a legacy hold must keep the input unselectable"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn current_format_journal_never_creates_holds() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-current-journal-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let mut set = UtxoSet::new();
        set.push_undo(UndoEntry {
            height: 106,
            created: vec![],
            removed: vec![RemovedOutput {
                utxo: utxo("bc", 0, 100_000),
                prior: PriorState::Free,
            }],
        });
        set.save(&path).unwrap();

        let loaded = UtxoSet::load(&path).unwrap();
        assert!(
            loaded.pending_removals().is_empty(),
            "a spend recorded by current code is not an unknown local send"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_out_of_order_journal_entry_cannot_discard_a_local_send() {
        let mut set = UtxoSet::new();
        let input = utxo("aa", 0, 500);
        set.record_pending_removals(std::slice::from_ref(&input), "ff", 5);
        set.record_pending_send_raw("ff", &[1, 2, 3]);
        let removed = set.take_pending_removal("ff", "aa", 0).unwrap();
        set.push_undo(UndoEntry {
            height: 10,
            created: vec![],
            removed: vec![],
        });
        set.push_undo(UndoEntry {
            height: 9,
            created: vec![],
            removed: vec![removed],
        });
        assert_eq!(set.pending_removals().len(), 1);
        assert!(!set.add(input));
        assert_eq!(
            set.pending_removals()[0].raw.as_deref(),
            Some(&[1, 2, 3][..])
        );
    }

    #[test]
    fn pruning_a_confirmed_local_send_retains_its_hold() {
        let mut set = UtxoSet::new();
        let input = utxo("aa", 0, 500);
        set.record_pending_removals(std::slice::from_ref(&input), "ff", 5);
        set.record_pending_send_raw("ff", &[1, 2, 3]);
        let removed = set.take_pending_removal("ff", "aa", 0).unwrap();
        set.push_undo(UndoEntry {
            height: 10,
            created: vec![],
            removed: vec![removed],
        });
        set.prune_undo(11, MAX_UNDO_ENTRIES);
        assert_eq!(set.pending_removals().len(), 1);
        assert!(!set.add(input));
        assert_eq!(
            set.pending_removals()[0].raw.as_deref(),
            Some(&[1, 2, 3][..])
        );
    }

    fn utxo(txid: &str, vout: u32, value: u64) -> Utxo {
        Utxo {
            txid: txid.to_string(),
            vout,
            value,
            address: "bc1qtest".to_string(),
            height: 100,
        }
    }

    fn swap_contract(order_id: &str, input: Option<(&str, u32)>) -> SwapContract {
        SwapContract {
            order_id: order_id.to_string(),
            funding_txid: format!("funding{order_id}"),
            hash_lock: [0u8; 32],
            recipient: "bc1q".to_string(),
            refund_address: "bc1q".to_string(),
            expiry: 100,
            amount: 1000,
            refund_raw: None,
            input_txid: input.map(|(txid, _)| txid.to_string()),
            input_vout: input.map(|(_, vout)| vout),
        }
    }

    #[test]
    fn test_add_and_total() {
        let mut set = UtxoSet::new();
        set.add(utxo("a", 0, 1000));
        set.add(utxo("b", 0, 2000));
        assert_eq!(set.total(), 3000);
    }

    #[test]
    fn test_dedup() {
        let mut set = UtxoSet::new();
        set.add(utxo("a", 0, 1000));
        set.add(utxo("a", 0, 1000));
        assert_eq!(set.list().len(), 1);
    }

    #[test]
    fn test_select_covers_amount() {
        let mut set = UtxoSet::new();
        set.add(utxo("a", 0, 500));
        set.add(utxo("b", 0, 1000));
        set.add(utxo("c", 0, 2000));
        let selected = set.select(1500, 100).unwrap();
        assert!(selected.iter().map(|u| u.value).sum::<u64>() >= 1600);
    }

    #[test]
    fn test_select_rejects_overflowing_amount_or_fee() {
        let mut set = UtxoSet::new();
        set.add(utxo("a", 0, 1000));
        assert!(set.select(u64::MAX, 1).is_none());
        assert!(set.select(1, u64::MAX).is_none());
        assert!(set.select(crate::MAX_MONEY_SATS + 1, 0).is_none());
    }

    #[test]
    fn test_select_insufficient() {
        let mut set = UtxoSet::new();
        set.add(utxo("a", 0, 100));
        assert!(set.select(1000, 0).is_none());
    }

    #[test]
    fn test_save_and_load() {
        let dir = std::env::temp_dir().join("vtorrent_utxo_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("utxos.json");

        let mut set = UtxoSet::new();
        set.add(utxo("aa", 0, 5000));
        set.add(utxo("bb", 1, 3000));
        set.save(&path).unwrap();

        let loaded = UtxoSet::load(&path).unwrap();
        assert_eq!(loaded.total(), 8000);
        assert_eq!(loaded.list().len(), 2);

        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn save_ignores_a_stale_fixed_temporary_symlink() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-utxo-symlink-{}-{}",
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
        UtxoSet::new().save(&path).unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"must not be overwritten");
        assert!(path.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn create_new_never_replaces_an_existing_file() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-utxo-create-new-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utxos.json");
        let existing = b"{\"journal_version\":3,\"utxos\":[]}";
        std::fs::write(&path, existing).unwrap();
        let error = UtxoSet::new().create_new(&path).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&path).unwrap(), existing);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn test_load_nonexistent_returns_empty() {
        let path = std::env::temp_dir().join("nonexistent_utxo_file.json");
        let set = UtxoSet::load(&path).unwrap();
        assert_eq!(set.total(), 0);
    }

    #[test]
    fn test_release_restores_selectability() {
        let mut set = UtxoSet::new();
        set.add(utxo("a", 0, 5000));
        set.reserve("a", 0);
        assert!(set.list().is_empty(), "reserved input must leave selection");
        assert!(set.release("a", 0), "release must report the reservation");
        assert!(!set.release("a", 0), "second release is a no-op");
        // A rescan can now re-add it.
        set.add(utxo("a", 0, 5000));
        assert_eq!(set.list().len(), 1);
    }

    #[test]
    fn test_scan_cursor_round_trips() {
        let dir = std::env::temp_dir().join(format!("vtorrent_utxo_cursor_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("utxos.json");
        let mut set = UtxoSet::new();
        set.set_last_scanned_height(123);
        set.save(&path).unwrap();
        let loaded = UtxoSet::load(&path).unwrap();
        assert_eq!(loaded.last_scanned_height(), 123);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_note_spend_marks_a_reserved_outpoint() {
        let mut set = UtxoSet::new();
        set.add(utxo("a", 0, 5000));
        set.reserve_utxo(utxo("a", 0, 5000));
        assert!(set.reserved_utxo("a", 0).is_some());
        assert!(set.note_spend("a", 0));
        assert!(set.reserved_utxo("a", 0).is_none());
        assert!(set.was_reserved_spent("a", 0));
        assert!(!set.note_spend("a", 0));
        set.clear_spent_reserved("a", 0);
        assert!(!set.was_reserved_spent("a", 0));
    }

    #[test]
    fn add_does_not_revive_a_spent_reservation() {
        let mut set = UtxoSet::new();
        let output = utxo("a", 0, 5000);
        set.reserve_utxo(output.clone());
        assert!(set.note_spend("a", 0));
        assert!(!set.add(output));
        assert!(set.list().is_empty());
        assert!(set.was_reserved_spent("a", 0));
    }

    #[test]
    fn reserve_utxo_clears_a_stale_spent_marker() {
        let mut set = UtxoSet::new();
        set.reserve_utxo(utxo("a", 0, 5000));
        assert!(set.note_spend("a", 0));
        set.reserve_utxo(utxo("a", 0, 5000));
        assert!(!set.was_reserved_spent("a", 0));
        assert!(set.reserved_utxo("a", 0).is_some());
    }

    #[test]
    fn rollback_of_a_spent_reservation_keeps_a_live_contract_reserved() {
        let mut set = UtxoSet::new();
        let output = utxo("a", 0, 5000);
        set.reserve_utxo(output.clone());
        set.record_swap_contract(swap_contract("order", Some(("a", 0))));
        assert!(set.note_spend("a", 0));
        set.restore_removed(RemovedOutput {
            utxo: output,
            prior: PriorState::SpentReserved,
        });
        assert!(set.list().is_empty());
        assert!(set.reserved_utxo("a", 0).is_some());
        assert!(!set.was_reserved_spent("a", 0));
    }

    #[test]
    fn forgetting_a_creation_keeps_a_live_contract_reserved() {
        let mut set = UtxoSet::new();
        set.reserve_utxo(utxo("a", 0, 5000));
        set.record_swap_contract(swap_contract("order", Some(("a", 0))));
        set.forget_outpoint("a", 0);
        assert!(set.reserved_utxo("a", 0).is_some());
    }

    #[test]
    fn test_rollback_above_keeps_blocks_at_or_below_the_fork() {
        let mut set = UtxoSet::new();
        for (txid, height) in [
            ("old", 100u32),
            ("fork", 120),
            ("above", 121),
            ("newer", 130),
        ] {
            set.add(Utxo {
                txid: txid.to_string(),
                vout: 0,
                value: 1000,
                address: "bc1qtest".to_string(),
                height,
            });
        }
        set.reserve("reserved", 0);
        set.set_next_index(9);

        set.rollback_above(120);

        let mut kept: Vec<_> = set.list().iter().map(|u| u.txid.clone()).collect();
        kept.sort();
        assert_eq!(kept, vec!["fork".to_string(), "old".to_string()]);
        assert_eq!(set.next_index(), 9, "address index is not chain state");
        assert!(
            set.release("reserved", 0),
            "reservation must survive a reorg"
        );
    }

    #[test]
    fn test_rollback_above_zero_keeps_only_mempool_entries() {
        let mut set = UtxoSet::new();
        set.add(Utxo {
            txid: "mempool".to_string(),
            vout: 0,
            value: 500,
            address: "bc1qtest".to_string(),
            height: 0,
        });
        set.add(utxo("confirmed", 0, 1000));

        set.rollback_above(0);

        assert_eq!(set.list().len(), 1);
        assert_eq!(set.list()[0].txid, "mempool");
    }

    #[test]
    fn test_clear_chain_utxos_preserves_recovery_metadata() {
        let mut set = UtxoSet::new();
        set.add(utxo("a", 0, 5000));
        set.reserve("b", 0);
        set.set_next_index(7);
        set.record_swap_transaction("tx1".into(), vec![1, 2, 3]);
        set.record_swap_contract(SwapContract {
            order_id: "order1".into(),
            funding_txid: "tx1".into(),
            hash_lock: [0u8; 32],
            recipient: "bc1q".into(),
            refund_address: "bc1q".into(),
            expiry: 100,
            amount: 1000,
            refund_raw: None,
            input_txid: Some("b".into()),
            input_vout: Some(0),
        });

        set.clear_chain_utxos();

        assert!(set.list().is_empty());
        assert_eq!(set.next_index(), 7);
        assert!(set.release("b", 0), "reservation must survive a reorg");
        assert!(set.pending_swap_transactions().contains_key("tx1"));
        assert!(set.swap_contract("order1").is_some());
    }

    #[test]
    fn undo_entry_restores_outputs_removed_above_the_fork() {
        let mut set = UtxoSet::new();
        set.add(Utxo {
            txid: "aa".into(),
            vout: 0,
            value: 500,
            address: "bc1q".into(),
            height: 5,
        });
        set.push_undo(UndoEntry {
            height: 6,
            created: vec![Utxo {
                txid: "bb".into(),
                vout: 0,
                value: 700,
                address: "bc1q".into(),
                height: 6,
            }],
            removed: vec![RemovedOutput {
                utxo: Utxo {
                    txid: "aa".into(),
                    vout: 0,
                    value: 500,
                    address: "bc1q".into(),
                    height: 5,
                },
                prior: PriorState::Free,
            }],
        });
        set.add(Utxo {
            txid: "bb".into(),
            vout: 0,
            value: 700,
            address: "bc1q".into(),
            height: 6,
        });
        set.remove("aa", 0);

        assert!(set.undo_above(5));
        let ids: Vec<_> = set.list().iter().map(|u| u.txid.clone()).collect();
        assert_eq!(
            ids,
            vec!["aa".to_string()],
            "created above fork dropped, removed restored"
        );
        assert_eq!(set.list()[0].value, 500);
        assert_eq!(set.list()[0].height, 5);
    }

    #[test]
    fn undo_restores_reserved_state_instead_of_freeing_the_input() {
        let mut set = UtxoSet::new();
        let reserved = utxo("cc", 0, 900);
        set.reserve_utxo(reserved.clone());
        set.record_swap_contract(SwapContract {
            order_id: "order-cc".into(),
            funding_txid: "ff".into(),
            hash_lock: [0u8; 32],
            recipient: "bc1q".into(),
            refund_address: "bc1q".into(),
            expiry: 100,
            amount: 900,
            refund_raw: None,
            input_txid: Some("cc".into()),
            input_vout: Some(0),
        });
        set.note_spend("cc", 0);
        set.push_undo(UndoEntry {
            height: 7,
            created: vec![],
            removed: vec![RemovedOutput {
                utxo: reserved.clone(),
                prior: PriorState::Reserved(reserved.clone()),
            }],
        });

        assert!(set.undo_above(6));
        assert!(
            set.reserved_utxo("cc", 0).is_some(),
            "input returns reserved, not selectable"
        );
        assert!(
            !set.was_reserved_spent("cc", 0),
            "a restored reservation must not stay marked spent"
        );
        assert!(!set.list().iter().any(|u| u.txid == "cc"));
    }

    #[test]
    fn undo_does_not_resurrect_a_reservation_a_released_swap_no_longer_needs() {
        let mut set = UtxoSet::new();
        let reserved = utxo("cd", 0, 900);
        set.reserve_utxo(reserved.clone());
        set.record_swap_contract(SwapContract {
            order_id: "order-cd".into(),
            funding_txid: "fe".into(),
            hash_lock: [0u8; 32],
            recipient: "bc1q".into(),
            refund_address: "bc1q".into(),
            expiry: 100,
            amount: 900,
            refund_raw: None,
            input_txid: Some("cd".into()),
            input_vout: Some(0),
        });
        set.push_undo(UndoEntry {
            height: 7,
            created: vec![],
            removed: vec![RemovedOutput {
                utxo: reserved.clone(),
                prior: PriorState::Reserved(reserved.clone()),
            }],
        });
        // The swap is released before the reorg: nothing can release the
        // reservation again, so restoring it would hide the input forever.
        set.note_spend("cd", 0);
        set.remove_swap_contract("order-cd");

        assert!(set.undo_above(6));
        assert!(
            set.reserved_utxo("cd", 0).is_none(),
            "no live swap holds the outpoint, so it must not come back reserved"
        );
        assert!(
            set.list().iter().any(|u| u.txid == "cd" && u.vout == 0),
            "the outpoint must return spendable"
        );
        assert!(!set.was_reserved_spent("cd", 0));
    }

    #[test]
    fn undo_above_restores_a_spent_reserved_outpoint_as_already_spent() {
        let mut set = UtxoSet::new();
        let record = utxo("dd", 0, 400);
        set.reserve_utxo(record.clone());
        set.push_undo(UndoEntry {
            height: 8,
            created: vec![],
            removed: vec![RemovedOutput {
                utxo: record,
                prior: PriorState::SpentReserved,
            }],
        });

        assert!(set.undo_above(7));
        assert!(set.was_reserved_spent("dd", 0));
        assert!(set.reserved_utxo("dd", 0).is_none());
        assert!(
            !set.release("dd", 0),
            "a spent reservation must not reappear as reserved"
        );
        assert!(!set.list().iter().any(|u| u.txid == "dd"));
    }

    #[test]
    fn outpoint_finds_spendable_and_reserved_records() {
        let mut set = UtxoSet::new();
        set.add(utxo("aa", 0, 500));
        set.reserve_utxo(utxo("bb", 1, 700));

        let spendable = set.outpoint("aa", 0).expect("spendable outpoint");
        assert_eq!(spendable.value, 500);
        let reserved = set.outpoint("bb", 1).expect("reserved outpoint");
        assert_eq!(reserved.value, 700);
        assert_eq!(reserved.vout, 1);
        assert_eq!(reserved.height, 100);
        assert!(set.outpoint("cc", 0).is_none());
    }

    #[test]
    fn undo_above_drops_an_outpoint_a_block_created_and_spent() {
        let mut set = UtxoSet::new();
        // A block can create an output and spend it in the same block. Undoing
        // the block must leave the outpoint absent: the restore of the spend
        // must not resurrect the creation that is also being undone.
        let created = Utxo {
            txid: "ab".repeat(32),
            vout: 0,
            value: 700,
            address: "bc1q".into(),
            height: 9,
        };
        set.push_undo(UndoEntry {
            height: 9,
            created: vec![created.clone()],
            removed: vec![RemovedOutput {
                utxo: created,
                prior: PriorState::Free,
            }],
        });

        assert!(set.undo_above(8));
        assert!(
            set.outpoint(&"ab".repeat(32), 0).is_none(),
            "an output created and spent in the undone block must not be restored"
        );
        assert!(set.list().is_empty());
    }

    #[test]
    fn undo_above_reports_no_coverage_when_the_fork_is_below_the_journal() {
        let mut set = UtxoSet::new();
        set.push_undo(UndoEntry {
            height: 10,
            created: vec![],
            removed: vec![],
        });
        assert!(!set.undo_above(5));
        assert_eq!(set.undo_entries().len(), 1, "no partial application");
    }

    #[test]
    fn undo_above_reports_no_coverage_for_an_unordered_or_gapped_journal() {
        // `push_undo` drops an out-of-order entry, so build the bad journals
        // directly: the coverage check must reject them without mutating.
        for entries in [
            vec![
                UndoEntry {
                    height: 12,
                    created: vec![utxo("aa", 0, 500)],
                    removed: vec![],
                },
                UndoEntry {
                    height: 10,
                    created: vec![],
                    removed: vec![],
                },
            ],
            vec![UndoEntry {
                height: 12,
                created: vec![],
                removed: vec![],
            }],
        ] {
            let mut set = UtxoSet::new();
            set.add(utxo("aa", 0, 500));
            set.set_last_scanned_tip(Some([9u8; 32]));
            set.set_undo_raw(entries);
            let before = serde_json::to_string(&set).unwrap();

            assert!(!set.undo_above(9), "a bad journal must not claim coverage");
            assert_eq!(
                serde_json::to_string(&set).unwrap(),
                before,
                "an uncovered rollback must leave the set byte-identical"
            );
        }
    }

    #[test]
    fn undo_above_clears_reservation_state_of_a_dropped_created_outpoint() {
        let record = utxo("aa", 0, 500);
        let mut set = UtxoSet::new();
        set.reserve_utxo(record.clone());
        // The outpoint is created at 101, reserved, then spent while reserved
        // at 102. Its creation is above the fork, so the rollback drops it.
        set.push_undo(UndoEntry {
            height: 101,
            created: vec![record.clone()],
            removed: vec![],
        });
        set.push_undo(UndoEntry {
            height: 102,
            created: vec![],
            removed: vec![RemovedOutput {
                utxo: record.clone(),
                prior: PriorState::Reserved(record.clone()),
            }],
        });
        set.note_spend(&record.txid, record.vout);

        assert!(set.undo_above(100));
        assert!(
            set.outpoint(&record.txid, record.vout).is_none(),
            "a dropped creation must not leave a phantom reservation"
        );
        assert!(!set.was_reserved_spent(&record.txid, record.vout));
        assert!(!set.release(&record.txid, record.vout));
        assert!(!set.reserved.contains(&(record.txid.clone(), record.vout)));
    }

    #[test]
    fn undo_above_does_not_resurrect_a_reservation_for_a_dropped_created_outpoint() {
        let record = utxo("ab", 0, 700);
        let mut set = UtxoSet::new();
        set.reserve_utxo(record.clone());
        assert!(set.note_spend(&record.txid, record.vout));
        // The reconciliation scan observed the spend, but the outpoint's own
        // creation is above the fork, so the replay drops it entirely. Restoring
        // the observation afterwards would reserve an outpoint that no longer
        // exists anywhere else in the set.
        set.record_reservation_spend(record.clone(), 101);
        set.push_undo(UndoEntry {
            height: 101,
            created: vec![record.clone()],
            removed: vec![],
        });
        set.push_undo(UndoEntry {
            height: 102,
            created: vec![],
            removed: vec![],
        });

        assert!(set.undo_above(100));
        assert!(
            set.outpoint(&record.txid, record.vout).is_none(),
            "a dropped creation must not return as a phantom reservation"
        );
        assert!(!set.was_reserved_spent(&record.txid, record.vout));
        assert!(!set.release(&record.txid, record.vout));
        assert!(!set.reserved.contains(&(record.txid.clone(), record.vout)));
    }

    #[test]
    fn undo_above_restores_a_reservation_observed_spent_by_reconciliation() {
        let record = utxo("aa", 0, 500);
        let mut set = UtxoSet::new();
        set.reserve_utxo(record.clone());
        set.record_swap_contract(swap_contract("live", Some((&record.txid, record.vout))));
        assert!(set.note_spend(&record.txid, record.vout));
        set.record_reservation_spend(record.clone(), 10);
        assert!(set.was_reserved_spent(&record.txid, record.vout));
        set.push_undo(UndoEntry {
            height: 10,
            created: vec![],
            removed: vec![],
        });

        assert!(set.undo_above(9));
        assert!(
            !set.was_reserved_spent(&record.txid, record.vout),
            "a covered rollback must not leave the observed spend behind"
        );
        assert!(
            set.reserved_utxo(&record.txid, record.vout).is_some(),
            "the outpoint returns to its pre-observation reserved state"
        );
    }

    #[test]
    fn undo_above_does_not_re_reserve_an_observation_no_contract_holds() {
        let record = utxo("af", 0, 500);
        let mut set = UtxoSet::new();
        set.reserve_utxo(record.clone());
        assert!(set.note_spend(&record.txid, record.vout));
        set.record_reservation_spend(record.clone(), 10);
        set.push_undo(UndoEntry {
            height: 10,
            created: vec![],
            removed: vec![],
        });

        assert!(set.undo_above(9));

        assert!(
            !set.was_reserved_spent(&record.txid, record.vout),
            "the rollback still undoes the observation"
        );
        assert!(
            set.reserved_utxo(&record.txid, record.vout).is_none(),
            "no contract holds the outpoint, so restoring the reservation would hide it forever"
        );
        assert!(
            set.observed_reservation_spends.is_empty(),
            "the restored observation must not be kept for a later rollback"
        );

        assert!(
            set.outpoint(&record.txid, record.vout).is_some(),
            "the observed spend was on the abandoned branch, so the outpoint is unspent again"
        );
        assert_eq!(
            set.total(),
            record.value,
            "a restored outpoint must count toward the balance, not just stop being reserved"
        );
    }

    #[test]
    fn undo_above_restores_an_outpoint_whose_contract_was_released() {
        let record = utxo("aj", 0, 700);
        let mut set = UtxoSet::new();
        set.reserve_utxo(record.clone());
        set.record_swap_contract(swap_contract("order1", Some((&record.txid, record.vout))));
        assert!(set.note_spend(&record.txid, record.vout));
        set.record_reservation_spend(record.clone(), 10);

        set.release(&record.txid, record.vout);
        set.remove_swap_contract("order1");
        assert!(set.reserved_utxo(&record.txid, record.vout).is_none());
        assert_eq!(set.total(), 0, "a spent reservation is not spendable");

        set.push_undo(UndoEntry {
            height: 10,
            created: vec![],
            removed: vec![],
        });
        assert!(set.undo_above(9));

        assert!(
            set.reserved_utxo(&record.txid, record.vout).is_none(),
            "the released contract cannot hold the outpoint again"
        );
        let restored = set
            .outpoint(&record.txid, record.vout)
            .expect("the last copy of the record must return to the spendable set");
        assert_eq!(
            (restored.txid, restored.vout, restored.value),
            (record.txid.clone(), record.vout, record.value)
        );
        assert_eq!(
            set.total(),
            record.value,
            "the restored outpoint must count toward the balance"
        );
    }

    #[test]
    fn a_legacy_contract_keeps_its_outpoint_reserved() {
        let record = utxo("ag", 0, 500);
        let mut set = UtxoSet::new();
        set.record_swap_contract(swap_contract("legacy", None));
        set.push_undo(UndoEntry {
            height: 11,
            created: vec![],
            removed: vec![RemovedOutput {
                utxo: record.clone(),
                prior: PriorState::Reserved(record.clone()),
            }],
        });

        assert!(set.undo_above(10));
        assert!(
            set.reserved_utxo(&record.txid, record.vout).is_some(),
            "a contract without input tracking cannot prove it dropped the input"
        );
    }

    /// Serialize a funding transaction whose single input spends `txid:vout`.
    fn funding_spending_raw(txid: &str, vout: u32) -> Vec<u8> {
        use std::str::FromStr;

        let transaction = bitcoin::Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![bitcoin::TxIn {
                previous_output: bitcoin::OutPoint::new(
                    bitcoin::Txid::from_str(txid).expect("test txid is valid hex"),
                    vout,
                ),
                script_sig: bitcoin::ScriptBuf::new(),
                sequence: bitcoin::Sequence::MAX,
                witness: bitcoin::Witness::new(),
            }],
            output: vec![],
        };
        bitcoin::consensus::serialize(&transaction)
    }

    #[test]
    fn a_legacy_contract_whose_retained_funding_spends_the_outpoint_keeps_it_reserved() {
        let record = utxo(&"ac".repeat(32), 0, 500);
        let mut set = UtxoSet::new();
        let contract = swap_contract("legacy-funding", None);
        set.record_swap_transaction(
            contract.funding_txid.clone(),
            funding_spending_raw(&record.txid, 0),
        );
        set.record_swap_contract(contract);
        set.push_undo(UndoEntry {
            height: 11,
            created: vec![],
            removed: vec![RemovedOutput {
                utxo: record.clone(),
                prior: PriorState::Reserved(record.clone()),
            }],
        });

        assert!(set.undo_above(10));
        assert!(
            set.reserved_utxo(&record.txid, record.vout).is_some(),
            "the retained funding transaction demonstrably spends the outpoint"
        );
    }

    #[test]
    fn a_legacy_contract_whose_retained_funding_spends_another_outpoint_frees_it() {
        let record = utxo(&"ad".repeat(32), 0, 500);
        let other = "ae".repeat(32);
        let mut set = UtxoSet::new();
        let contract = swap_contract("legacy-elsewhere", None);
        set.record_swap_transaction(
            contract.funding_txid.clone(),
            funding_spending_raw(&other, 1),
        );
        set.record_swap_contract(contract);
        set.push_undo(UndoEntry {
            height: 11,
            created: vec![],
            removed: vec![RemovedOutput {
                utxo: record.clone(),
                prior: PriorState::Reserved(record.clone()),
            }],
        });

        assert!(set.undo_above(10));
        assert!(
            set.reserved_utxo(&record.txid, record.vout).is_none(),
            "a retained funding transaction spending another outpoint frees this one"
        );
        assert_eq!(set.list().len(), 1, "the output itself is restored");
        assert_eq!(
            set.total(),
            record.value,
            "the freed outpoint must count toward the balance"
        );
    }

    #[test]
    fn a_legacy_contract_without_a_retained_funding_still_fails_closed() {
        let record = utxo(&"af".repeat(32), 0, 500);
        let mut set = UtxoSet::new();
        set.record_swap_contract(swap_contract("legacy-no-raw", None));
        set.push_undo(UndoEntry {
            height: 11,
            created: vec![],
            removed: vec![RemovedOutput {
                utxo: record.clone(),
                prior: PriorState::Reserved(record.clone()),
            }],
        });

        assert!(set.undo_above(10));
        assert!(
            set.reserved_utxo(&record.txid, record.vout).is_some(),
            "with no retained raw nothing can prove the outpoint is not held"
        );
        assert!(
            set.legacy_contract_warnings.contains("legacy-no-raw"),
            "an operator must be able to see why the reservation stays stuck"
        );
    }

    #[test]
    fn a_legacy_contract_without_a_retained_funding_warns_once() {
        let record = utxo(&"ag".repeat(32), 0, 500);
        let mut set = UtxoSet::new();
        set.record_swap_contract(swap_contract("legacy-once", None));
        set.push_undo(UndoEntry {
            height: 11,
            created: vec![],
            removed: vec![RemovedOutput {
                utxo: record.clone(),
                prior: PriorState::Reserved(record.clone()),
            }],
        });
        assert!(set.undo_above(10));
        let warned: std::collections::BTreeSet<String> =
            set.legacy_contract_warnings.iter().cloned().collect();

        set.push_undo(UndoEntry {
            height: 21,
            created: vec![],
            removed: vec![RemovedOutput {
                utxo: record.clone(),
                prior: PriorState::Reserved(record.clone()),
            }],
        });
        assert!(set.undo_above(20));
        assert_eq!(
            set.legacy_contract_warnings, warned,
            "a second rollback must not repeat the warning"
        );
    }

    #[test]
    fn an_unparseable_retained_funding_transaction_keeps_its_outpoint_reserved() {
        let record = utxo("ah", 0, 500);
        let mut set = UtxoSet::new();
        set.record_swap_transaction("funding".into(), vec![1, 2, 3]);
        set.push_undo(UndoEntry {
            height: 11,
            created: vec![],
            removed: vec![RemovedOutput {
                utxo: record.clone(),
                prior: PriorState::Reserved(record.clone()),
            }],
        });

        assert!(set.undo_above(10));
        assert!(
            set.reserved_utxo(&record.txid, record.vout).is_some(),
            "an undecodable retained transaction might still spend the input"
        );
    }

    #[test]
    fn an_unparseable_retained_funding_still_warns() {
        let record = utxo("aj", 0, 500);
        let mut set = UtxoSet::new();
        let contract = swap_contract("legacy-corrupt-raw", None);
        set.record_swap_transaction(contract.funding_txid.clone(), vec![1, 2, 3]);
        set.record_swap_contract(contract);
        set.push_undo(UndoEntry {
            height: 11,
            created: vec![],
            removed: vec![RemovedOutput {
                utxo: record.clone(),
                prior: PriorState::Reserved(record.clone()),
            }],
        });

        assert!(set.undo_above(10));
        assert!(
            set.reserved_utxo(&record.txid, record.vout).is_some(),
            "a raw that does not parse cannot rule the reference out"
        );
        assert!(
            set.legacy_contract_warnings.contains("legacy-corrupt-raw"),
            "a present-but-unparseable raw is just as unresolvable as a missing one"
        );
    }

    #[test]
    fn a_non_canonical_stored_txid_still_matches_the_raw_that_spends_it() {
        let canonical = "ab".repeat(32);
        let mut set = UtxoSet::new();
        set.record_swap_transaction("funding".into(), funding_spending_raw(&canonical, 4));

        assert!(
            set.swap_holds_outpoint(&canonical.to_uppercase(), 4),
            "the raw really spends this outpoint, so a differently-cased id must not free it"
        );
    }

    #[test]
    fn an_unparseable_stored_txid_fails_closed() {
        let canonical = "ac".repeat(32);
        let mut set = UtxoSet::new();
        set.record_swap_transaction("funding".into(), funding_spending_raw(&canonical, 2));

        assert!(
            set.swap_holds_outpoint("not-a-txid", 2),
            "an id that cannot be parsed cannot prove it is a different outpoint"
        );
    }

    #[test]
    fn a_contract_for_another_outpoint_frees_the_restored_one() {
        let record = utxo("ai", 0, 500);
        let mut set = UtxoSet::new();
        set.record_swap_contract(swap_contract("live", Some(("b", 1))));
        set.push_undo(UndoEntry {
            height: 11,
            created: vec![],
            removed: vec![RemovedOutput {
                utxo: record.clone(),
                prior: PriorState::Reserved(record.clone()),
            }],
        });

        assert!(set.undo_above(10));
        assert!(
            set.reserved_utxo(&record.txid, record.vout).is_none(),
            "a definite non-reference frees the outpoint"
        );
        assert_eq!(set.list().len(), 1, "the output itself is restored");
    }

    #[test]
    fn undo_above_leaves_an_observation_below_the_fork_alone() {
        let record = utxo("ae", 0, 500);
        let mut set = UtxoSet::new();
        set.reserve_utxo(record.clone());
        assert!(set.note_spend(&record.txid, record.vout));
        set.record_reservation_spend(record.clone(), 10);
        // An entry above the fork makes a fork at 300 covered.
        set.push_undo(UndoEntry {
            height: 301,
            created: vec![],
            removed: vec![],
        });

        assert!(set.undo_above(300));

        assert!(
            set.reserved_utxo(&record.txid, record.vout).is_none(),
            "a spend observed below the fork happened on the surviving branch"
        );
        assert!(
            set.was_reserved_spent(&record.txid, record.vout),
            "the observation must stay spent"
        );
        assert_eq!(
            set.observed_reservation_spends
                .get(&(record.txid.clone(), record.vout))
                .map(|(_, observed_at)| *observed_at),
            Some(10),
            "an observation below the fork is untouched, not discarded"
        );
    }

    fn folded(entries: &[(&str, u32, u64)]) -> Vec<(String, u32, u64, u32)> {
        let mut folded: Vec<(String, u32, u64, u32)> = entries
            .iter()
            .map(|(txid, vout, value)| (txid.to_string(), *vout, *value, 100))
            .collect();
        folded.sort();
        folded
    }

    #[test]
    fn undo_above_n_blocks_equals_a_forward_fold_to_the_fork() {
        let base = utxo("aa", 0, 500);
        let six = utxo("bb", 0, 600);
        let eight = utxo("cc", 0, 800);
        let mut set = UtxoSet::new();
        // Fold the chain forward: aa is created below the fork, bb at 6 spends
        // aa, cc at 8 spends bb.
        set.add(base.clone());
        // Height 5 is a sentinel so fork 4 is covered (the first journalled
        // height above a fork must be fork + 1).
        set.push_undo(UndoEntry {
            height: 5,
            created: vec![],
            removed: vec![],
        });
        set.push_undo(UndoEntry {
            height: 6,
            created: vec![six.clone()],
            removed: vec![RemovedOutput {
                utxo: base.clone(),
                prior: PriorState::Free,
            }],
        });
        set.push_undo(UndoEntry {
            height: 8,
            created: vec![eight.clone()],
            removed: vec![RemovedOutput {
                utxo: six.clone(),
                prior: PriorState::Free,
            }],
        });
        set.remove(&base.txid, base.vout);
        set.add(six.clone());
        set.remove(&six.txid, six.vout);
        set.add(eight.clone());

        assert!(set.undo_above(4));
        let rolled: Vec<_> = set
            .list()
            .iter()
            .map(|u| (u.txid.clone(), u.vout, u.value, u.height))
            .collect();
        let expected = folded(&[("aa", 0, 500)]);
        assert_eq!(rolled, expected, "N-entry replay must equal the fork state");
    }

    #[test]
    fn a_corrupt_journal_fails_closed() {
        let dir =
            std::env::temp_dir().join(format!("vtorrent_utxo_corrupt_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("utxos.json");
        let mut set = UtxoSet::new();
        set.add(utxo("aa", 0, 5000));
        set.set_last_scanned_tip(Some([7u8; 32]));
        let mut value = serde_json::to_value(&set).unwrap();
        value["undo"] = serde_json::json!("not-a-journal");
        value["last_scanned_tip"] = serde_json::json!("not-a-tip");
        std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

        assert!(UtxoSet::load(&path).is_err());

        // The settled balance is not recovery data: corrupting it still fails.
        value["utxos"] = serde_json::json!("not-a-balance");
        std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();
        assert!(UtxoSet::load(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn push_undo_ignores_an_older_height_and_keeps_coverage() {
        let mut set = UtxoSet::new();
        set.push_undo(UndoEntry {
            height: 9,
            created: vec![],
            removed: vec![],
        });
        set.push_undo(UndoEntry {
            height: 5,
            created: vec![],
            removed: vec![],
        });

        assert_eq!(
            set.undo_entries().len(),
            1,
            "older entries are not appended"
        );
        assert_eq!(set.undo_entries()[0].height, 9);
        assert!(
            set.undo_above(8),
            "a reconciliation scan must not disable journal coverage"
        );
        assert!(set.undo_entries().is_empty());
    }

    #[test]
    fn push_undo_trims_the_oldest_entries_at_the_cap() {
        let mut set = UtxoSet::new();
        for height in 0..=MAX_UNDO_ENTRIES as u32 {
            set.push_undo(UndoEntry {
                height,
                created: vec![],
                removed: vec![],
            });
        }

        assert_eq!(set.undo_entries().len(), MAX_UNDO_ENTRIES);
        assert_eq!(
            set.undo_entries()[0].height,
            1,
            "the oldest entry is dropped"
        );
        assert_eq!(
            set.undo_entries().last().unwrap().height,
            MAX_UNDO_ENTRIES as u32
        );
    }

    #[test]
    fn prune_undo_drops_below_floor_and_enforces_cap() {
        let mut set = UtxoSet::new();
        for height in 1..=5u32 {
            set.push_undo(UndoEntry {
                height,
                created: vec![],
                removed: vec![],
            });
        }
        set.prune_undo(3, 10);
        assert_eq!(set.undo_entries().first().unwrap().height, 3);
        set.prune_undo(0, 2);
        assert_eq!(set.undo_entries().len(), 2);
        assert_eq!(set.undo_entries().last().unwrap().height, 5);
    }

    #[test]
    fn undo_above_keeps_a_pending_send_hidden() {
        let record = utxo("aa", 0, 500);
        let mut set = UtxoSet::new();
        set.add(record.clone());
        set.remove(&record.txid, record.vout);
        set.record_pending_removals(std::slice::from_ref(&record), "ff", 101);
        set.push_undo(UndoEntry {
            height: 102,
            created: vec![],
            removed: vec![],
        });

        assert!(set.undo_above(101));
        assert!(
            set.outpoint(&record.txid, record.vout).is_none(),
            "a possibly live send must keep its input unselectable"
        );
        assert_eq!(
            set.pending_removals().len(),
            1,
            "the send stays pending until the scan observes it"
        );
        assert!(!set.add(record));
    }

    #[test]
    fn a_reloaded_pending_send_keeps_its_input_hidden() {
        let dir =
            std::env::temp_dir().join(format!("vtorrent_utxo_pending_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("utxos.json");
        let record = utxo("ab", 0, 500);
        let mut set = UtxoSet::new();
        set.add(record.clone());
        set.remove(&record.txid, record.vout);
        set.record_pending_removals(std::slice::from_ref(&record), "ff", 101);
        set.save(&path).unwrap();

        let mut loaded = UtxoSet::load(&path).unwrap();
        loaded.clear_journal();
        assert!(!loaded.add(record));
        assert!(loaded.list().is_empty());
        assert_eq!(loaded.pending_removals().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_broadcast_cancels_its_pending_removal() {
        let record = utxo("aa", 0, 500);
        let mut set = UtxoSet::new();
        set.add(record.clone());
        set.remove(&record.txid, record.vout);
        set.record_pending_removals(std::slice::from_ref(&record), "ff", 101);

        set.cancel_local_removals(std::slice::from_ref(&record));
        assert!(set.add(record.clone()));

        assert!(
            set.pending_removals().is_empty(),
            "the send never happened, so its pending removal must be cancelled"
        );
        assert_eq!(set.list().len(), 1);
    }

    #[test]
    fn clear_undo_clears_pending_removals() {
        let mut set = UtxoSet::new();
        set.record_pending_removals(&[utxo("aa", 0, 500)], "ff", 101);
        set.clear_undo();
        assert!(set.pending_removals().is_empty());
    }

    #[test]
    fn clear_undo_clears_reservation_observations() {
        let mut set = UtxoSet::new();
        let record = utxo("aa", 0, 500);
        set.reserve_utxo(record.clone());
        set.note_spend(&record.txid, record.vout);
        set.record_reservation_spend(record.clone(), 10);

        set.clear_undo();

        assert!(
            set.observed_reservation_spends.is_empty(),
            "a full reset abandons observations made on the old chain state"
        );
    }

    #[test]
    fn prune_undo_keeps_pending_removals_below_the_floor() {
        let mut set = UtxoSet::new();
        set.record_pending_removals(&[utxo("aa", 0, 500)], "ff", 5);
        set.record_pending_removals(&[utxo("bb", 0, 700)], "ee", 6);
        set.push_undo(UndoEntry {
            height: 5,
            created: vec![],
            removed: vec![],
        });

        set.prune_undo(6, MAX_UNDO_ENTRIES);

        assert_eq!(
            set.pending_removals().len(),
            2,
            "a pending removal is self-contained and may be the only copy of an unseen send"
        );
        assert_eq!(set.pending_removals()[0].removed.utxo.txid, "aa");
        assert!(
            set.undo_entries().is_empty(),
            "an undo entry below the floor is pruned"
        );
    }

    #[test]
    fn prune_undo_retains_pending_removals_over_journal_cap() {
        let mut set = UtxoSet::new();
        for vout in 0..3u32 {
            set.record_pending_removals(&[utxo("aa", vout, 500)], "ff", 5);
        }

        set.prune_undo(0, 2);

        assert_eq!(set.pending_removals().len(), 3);
        assert_eq!(
            set.pending_removals()[2].removed.utxo.vout,
            2,
            "all pending records are kept"
        );
    }

    #[test]
    fn pending_input_remains_unselectable_after_undo_pruning() {
        let mut set = UtxoSet::new();
        let input = utxo("aa", 0, 500);
        set.record_pending_removals(std::slice::from_ref(&input), "ff", 5);
        set.prune_undo(0, 0);
        assert!(!set.add(input), "a pending input must not become spendable");
    }

    #[test]
    fn recording_at_capacity_does_not_evict_an_existing_pending_input() {
        let mut set = UtxoSet::new();
        let first = utxo("aa", 0, 500);
        set.record_pending_removals(std::slice::from_ref(&first), "ff", 5);
        let pending = set.pending_removals[0].clone();
        set.pending_removals.resize(MAX_UNDO_ENTRIES, pending);
        set.record_pending_removals(&[utxo("bb", 0, 500)], "ee", 6);
        assert_eq!(
            set.pending_removals
                .iter()
                .filter(|entry| entry.removed.utxo.txid == first.txid)
                .count(),
            MAX_UNDO_ENTRIES,
            "existing pending records must not be silently evicted"
        );
    }

    #[test]
    fn journal_and_tip_round_trip_through_persistence() {
        let path = std::env::temp_dir().join(format!("btc-undo-rt-{}.json", std::process::id()));
        let mut set = UtxoSet::new();
        set.set_last_scanned_tip(Some([7u8; 32]));
        set.push_undo(UndoEntry {
            height: 4,
            created: vec![],
            removed: vec![],
        });
        set.save(&path).unwrap();

        let loaded = UtxoSet::load(&path).unwrap();
        assert_eq!(loaded.last_scanned_tip(), Some([7u8; 32]));
        assert_eq!(loaded.undo_entries().len(), 1);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn test_remove_swap_contract_drops_pending_tx() {
        let mut set = UtxoSet::new();
        set.record_swap_transaction("tx1".into(), vec![1, 2, 3]);
        set.record_swap_contract(SwapContract {
            order_id: "order1".into(),
            funding_txid: "tx1".into(),
            hash_lock: [0u8; 32],
            recipient: "bc1q".into(),
            refund_address: "bc1q".into(),
            expiry: 100,
            amount: 1000,
            refund_raw: None,
            input_txid: Some("a".into()),
            input_vout: Some(0),
        });
        let removed = set.remove_swap_contract("order1").unwrap();
        assert_eq!(removed.funding_txid, "tx1");
        assert!(set.swap_contract("order1").is_none());
        assert!(
            set.pending_swap_transactions().is_empty(),
            "pending tx must be dropped with the contract"
        );
    }
}
#[cfg(test)]
thread_local! {
    static FAIL_BEFORE_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static PAUSE_BEFORE_RENAME: std::cell::RefCell<Option<std::sync::mpsc::Receiver<()>>> =
        const { std::cell::RefCell::new(None) };
    #[cfg(unix)]
    static FAIL_PARENT_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn fail_next_rename_for_test() {
    FAIL_BEFORE_RENAME.with(|flag| flag.set(true));
}

#[cfg(test)]
pub(crate) fn pause_next_save_before_rename_for_test(receiver: std::sync::mpsc::Receiver<()>) {
    PAUSE_BEFORE_RENAME.with(|slot| *slot.borrow_mut() = Some(receiver));
}

#[cfg(test)]
fn pause_before_rename() {
    let receiver = PAUSE_BEFORE_RENAME.with(|slot| slot.borrow_mut().take());
    if let Some(receiver) = receiver {
        let _ = receiver.recv();
    }
}

#[cfg(all(test, unix))]
pub(crate) fn fail_next_parent_sync_for_test() {
    FAIL_PARENT_SYNC.with(|flag| flag.set(true));
}

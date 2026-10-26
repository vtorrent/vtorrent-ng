use crate::error::{BtcError, Result};
use crate::headers::HeaderChain;
use crate::sync::{network_group, NetworkGroup};
use bitcoin::bip158::{FilterHash, FilterHeader};
use bitcoin::hashes::Hash;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::net::SocketAddr;

pub(crate) const CHECKPOINT_INTERVAL: u32 = 1_000;
pub(crate) const CFHEADERS_RANGE_LIMIT: usize = 2_000;
pub(crate) const CFILTERS_RANGE_LIMIT: usize = 1_000;
const MAX_CANDIDATES: usize = 16;
const MAX_VOTERS: usize = 8;

#[derive(Debug, Clone)]
struct FilterRecord {
    height: u32,
    filter_hash: FilterHash,
    filter_header: FilterHeader,
}

#[derive(Debug, Default)]
pub(crate) struct FilterHeaderStore {
    records: HashMap<[u8; 32], FilterRecord>,
    checkpoints: BTreeMap<u32, FilterHeader>,
    checkpoint_floor: u32,
    observed_tip: Option<[u8; 32]>,
    agreeing_groups: HashSet<NetworkGroup>,
    candidates: HashMap<([u8; 32], [u8; 32]), HashSet<NetworkGroup>>,
    candidate_order: VecDeque<([u8; 32], [u8; 32])>,
    candidate_tip: Option<[u8; 32]>,
}

impl FilterHeaderStore {
    pub(crate) fn reconcile_checkpoints(
        &mut self,
        stop_hash: [u8; 32],
        stop_height: u32,
        checkpoints: &[FilterHeader],
    ) -> Result<()> {
        let expected = (stop_height / CHECKPOINT_INTERVAL) as usize;
        if checkpoints.len() != expected {
            return Err(BtcError::Sync(format!(
                "compact-filter checkpoint count {} does not match expected {}",
                checkpoints.len(),
                expected
            )));
        }
        for (index, checkpoint) in checkpoints.iter().enumerate() {
            let height = (index + 1) as u32 * CHECKPOINT_INTERVAL;
            if height < self.checkpoint_floor {
                continue;
            }
            if let Some(existing) = self.checkpoints.get(&height) {
                if existing != checkpoint {
                    return Err(BtcError::Sync(format!(
                        "compact-filter checkpoint mismatch at height {}",
                        height
                    )));
                }
            }
            self.checkpoints.insert(height, *checkpoint);
        }
        if self.observed_tip != Some(stop_hash) {
            self.observed_tip = Some(stop_hash);
            self.agreeing_groups.clear();
        }
        Ok(())
    }

    pub(crate) fn checkpoint(&self, height: u32) -> Option<FilterHeader> {
        if height == 0 || !height.is_multiple_of(CHECKPOINT_INTERVAL) {
            return None;
        }
        self.checkpoints.get(&height).copied()
    }

    pub(crate) fn retain_checkpoints(&mut self, floor: u32, tip: u32) {
        self.checkpoint_floor = floor.saturating_sub(1) / CHECKPOINT_INTERVAL * CHECKPOINT_INTERVAL;
        self.checkpoints
            .retain(|height, _| *height >= self.checkpoint_floor && *height <= tip);
    }

    pub(crate) fn prune(&mut self, chain: &HeaderChain) {
        let floor = chain.retained_floor();
        self.records.retain(|hash, record| {
            record.height >= floor.saturating_sub(1) && chain.get(hash).is_some()
        });
        self.retain_checkpoints(floor, chain.best_height());
        if self.candidate_tip != chain.best_hash() {
            self.candidates.clear();
            self.candidate_order.clear();
            self.agreeing_groups.clear();
            self.candidate_tip = chain.best_hash();
        }
    }

    /// Drop all stored filter headers and checkpoint agreement state.
    pub(crate) fn clear(&mut self) {
        self.records.clear();
        self.checkpoints.clear();
        self.checkpoint_floor = 0;
        self.observed_tip = None;
        self.agreeing_groups.clear();
        self.candidates.clear();
        self.candidate_order.clear();
        self.candidate_tip = None;
    }

    /// Filter header stored for a block hash, if this wallet has scanned it.
    pub(crate) fn header_for(&self, block_hash: &[u8; 32]) -> Option<FilterHeader> {
        self.records
            .get(block_hash)
            .map(|record| record.filter_header)
    }

    pub(crate) fn apply_range(
        &mut self,
        start_height: u32,
        block_hashes: &[[u8; 32]],
        filter_hashes: &[FilterHash],
        mut previous_header: FilterHeader,
    ) -> Result<FilterHeader> {
        if block_hashes.len() != filter_hashes.len() {
            return Err(BtcError::Sync(format!(
                "compact-filter hash count {} does not match block count {}",
                filter_hashes.len(),
                block_hashes.len()
            )));
        }
        let mut pending = Vec::with_capacity(block_hashes.len());
        for (offset, (block_hash, filter_hash)) in
            block_hashes.iter().zip(filter_hashes).enumerate()
        {
            let height = start_height + offset as u32;
            let filter_header = filter_hash.filter_header(&previous_header);
            if let Some(existing) = self.records.get(block_hash) {
                if existing.filter_hash != *filter_hash || existing.filter_header != filter_header {
                    return Err(BtcError::Sync(format!(
                        "compact-filter header disagreement at height {}",
                        height
                    )));
                }
            }
            if let Some(checkpoint) = self.checkpoint(height) {
                if checkpoint != filter_header {
                    return Err(BtcError::Sync(format!(
                        "compact-filter header does not match checkpoint at height {}",
                        height
                    )));
                }
            }
            pending.push((
                *block_hash,
                FilterRecord {
                    height,
                    filter_hash: *filter_hash,
                    filter_header,
                },
            ));
            previous_header = filter_header;
        }
        self.records.extend(pending);
        Ok(previous_header)
    }

    pub(crate) fn verify_filter(&self, block_hash: &[u8; 32], filter: &[u8]) -> Result<()> {
        let expected = self.records.get(block_hash).ok_or_else(|| {
            BtcError::Sync(format!(
                "missing compact-filter header for block {}",
                hex::encode(block_hash)
            ))
        })?;
        let actual = FilterHash::hash(filter);
        if actual != expected.filter_hash {
            return Err(BtcError::Sync(format!(
                "compact filter does not match authenticated header for block {}",
                hex::encode(block_hash)
            )));
        }
        Ok(())
    }

    pub(crate) fn observe_candidate(
        &mut self,
        peer: SocketAddr,
        candidate: Self,
        required: usize,
    ) -> Result<usize> {
        let tip = candidate
            .observed_tip
            .ok_or_else(|| BtcError::Sync("compact-filter candidate has no tip".into()))?;
        let fingerprint = candidate.fingerprint();
        if self.candidate_tip != Some(tip) {
            self.candidates.clear();
            self.candidate_order.clear();
            self.candidate_tip = Some(tip);
        }
        let key = (tip, fingerprint);
        if !self.candidates.contains_key(&key) {
            if self.candidates.len() == MAX_CANDIDATES {
                if let Some(oldest) = self.candidate_order.pop_front() {
                    self.candidates.remove(&oldest);
                }
            }
            self.candidate_order.push_back(key);
        }
        let groups = self.candidates.entry(key).or_default();
        if groups.len() < MAX_VOTERS {
            groups.insert(network_group(peer.ip()));
        }
        let agreement = groups.len();
        let agreed_groups = groups.clone();
        if agreement < required {
            return Ok(agreement);
        }

        for (height, checkpoint) in &candidate.checkpoints {
            if let Some(existing) = self.checkpoints.get(height) {
                if existing != checkpoint {
                    return Err(BtcError::Sync(format!(
                        "agreed compact-filter candidate conflicts at checkpoint height {}",
                        height
                    )));
                }
            }
        }
        for (block_hash, record) in &candidate.records {
            if let Some(existing) = self.records.get(block_hash) {
                if existing.filter_hash != record.filter_hash
                    || existing.filter_header != record.filter_header
                {
                    return Err(BtcError::Sync(format!(
                        "agreed compact-filter candidate conflicts for block {}",
                        hex::encode(block_hash)
                    )));
                }
            }
        }

        self.checkpoints.extend(candidate.checkpoints);
        self.records.extend(candidate.records);
        self.observed_tip = Some(tip);
        self.agreeing_groups = agreed_groups;
        Ok(agreement)
    }

    fn fingerprint(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(self.observed_tip.unwrap_or_default());
        for (height, checkpoint) in &self.checkpoints {
            hasher.update(height.to_le_bytes());
            hasher.update(checkpoint.to_byte_array());
        }
        let mut records: Vec<_> = self.records.iter().collect();
        records.sort_unstable_by_key(|(block_hash, _)| **block_hash);
        for (block_hash, record) in records {
            hasher.update(block_hash);
            hasher.update(record.filter_hash.to_byte_array());
            hasher.update(record.filter_header.to_byte_array());
        }
        hasher.finalize().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_filter_that_does_not_match_committed_hash() {
        let mut store = FilterHeaderStore::default();
        let block_hash = [3u8; 32];
        let filter_hash = FilterHash::hash(b"expected");
        store
            .apply_range(0, &[block_hash], &[filter_hash], FilterHeader::all_zeros())
            .unwrap();
        assert!(store.verify_filter(&block_hash, b"tampered").is_err());
        assert!(store.verify_filter(&block_hash, b"expected").is_ok());
    }

    #[test]
    fn rejects_header_disagreement_for_same_block() {
        let mut store = FilterHeaderStore::default();
        let block_hash = [4u8; 32];
        store
            .apply_range(
                0,
                &[block_hash],
                &[FilterHash::hash(b"one")],
                FilterHeader::all_zeros(),
            )
            .unwrap();
        assert!(store
            .apply_range(
                0,
                &[block_hash],
                &[FilterHash::hash(b"two")],
                FilterHeader::all_zeros(),
            )
            .is_err());
    }

    #[test]
    fn rejects_wrong_checkpoint_count() {
        let mut store = FilterHeaderStore::default();
        assert!(store.reconcile_checkpoints([1u8; 32], 1_000, &[]).is_err());
    }

    #[test]
    fn rejected_range_does_not_partially_mutate_store() {
        let mut store = FilterHeaderStore::default();
        store
            .reconcile_checkpoints([9u8; 32], 1_000, &[FilterHeader::all_zeros()])
            .unwrap();
        let block_hashes: Vec<[u8; 32]> = (0u32..=1_000)
            .map(|height| {
                let mut hash = [0u8; 32];
                hash[..4].copy_from_slice(&height.to_le_bytes());
                hash
            })
            .collect();
        let filter_hashes: Vec<FilterHash> = (0u32..=1_000)
            .map(|height| FilterHash::hash(&height.to_le_bytes()))
            .collect();
        assert!(store
            .apply_range(0, &block_hashes, &filter_hashes, FilterHeader::all_zeros(),)
            .is_err());
        assert!(store.records.is_empty());
    }

    #[test]
    fn tracks_distinct_peer_agreement() {
        let mut store = FilterHeaderStore::default();
        let mut candidate = FilterHeaderStore::default();
        candidate.reconcile_checkpoints([8u8; 32], 0, &[]).unwrap();
        let peer: SocketAddr = "127.0.0.1:8333".parse().unwrap();
        assert_eq!(store.observe_candidate(peer, candidate, 2).unwrap(), 1);

        let mut duplicate = FilterHeaderStore::default();
        duplicate.reconcile_checkpoints([8u8; 32], 0, &[]).unwrap();
        assert_eq!(store.observe_candidate(peer, duplicate, 2).unwrap(), 1);

        let mut matching = FilterHeaderStore::default();
        matching.reconcile_checkpoints([8u8; 32], 0, &[]).unwrap();
        assert_eq!(
            store
                .observe_candidate("128.0.0.2:8333".parse().unwrap(), matching, 2)
                .unwrap(),
            2
        );
    }

    #[test]
    fn one_bad_candidate_does_not_poison_honest_agreement() {
        let mut store = FilterHeaderStore::default();
        let candidate = |filter: &'static [u8]| {
            let mut candidate = FilterHeaderStore::default();
            candidate.reconcile_checkpoints([8u8; 32], 0, &[]).unwrap();
            candidate
                .apply_range(
                    0,
                    &[[7u8; 32]],
                    &[FilterHash::hash(filter)],
                    FilterHeader::all_zeros(),
                )
                .unwrap();
            candidate
        };
        assert_eq!(
            store
                .observe_candidate("127.0.0.1:8333".parse().unwrap(), candidate(b"bad"), 2,)
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .observe_candidate("127.0.0.2:8333".parse().unwrap(), candidate(b"honest"), 2,)
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .observe_candidate("128.0.0.3:8333".parse().unwrap(), candidate(b"honest"), 2,)
                .unwrap(),
            2
        );
        assert!(store.verify_filter(&[7u8; 32], b"honest").is_ok());
    }

    #[test]
    fn same_network_group_cannot_authorize_filters() {
        for peers in [
            ["203.0.113.1:8333", "203.0.114.2:8334"],
            ["[2001:db8::1]:8333", "[2001:db8:1::2]:8334"],
            ["203.0.113.1:8333", "[::ffff:203.0.113.1]:8334"],
        ] {
            let mut store = FilterHeaderStore::default();
            for peer in peers {
                let mut candidate = FilterHeaderStore::default();
                candidate.reconcile_checkpoints([8; 32], 0, &[]).unwrap();
                candidate
                    .apply_range(
                        0,
                        &[[7; 32]],
                        &[FilterHash::hash(b"hidden transactions")],
                        FilterHeader::all_zeros(),
                    )
                    .unwrap();
                assert_eq!(
                    store
                        .observe_candidate(peer.parse().unwrap(), candidate, 2)
                        .unwrap(),
                    1
                );
            }
            assert!(store
                .verify_filter(&[7; 32], b"hidden transactions")
                .is_err());
        }
    }

    #[test]
    fn candidate_churn_and_voters_are_bounded_without_poisoning_honest_agreement() {
        fn candidate(filter: &[u8], tip: [u8; 32]) -> FilterHeaderStore {
            let mut candidate = FilterHeaderStore::default();
            candidate.reconcile_checkpoints(tip, 0, &[]).unwrap();
            candidate
                .apply_range(
                    0,
                    &[[7; 32]],
                    &[FilterHash::hash(filter)],
                    FilterHeader::all_zeros(),
                )
                .unwrap();
            candidate
        }
        let mut store = FilterHeaderStore::default();
        for n in 0u32..1_000 {
            store
                .observe_candidate(
                    "1.1.1.1:8333".parse().unwrap(),
                    candidate(&n.to_le_bytes(), [8; 32]),
                    2,
                )
                .unwrap();
            assert!(store.candidates.len() <= MAX_CANDIDATES);
            assert!(store.candidate_order.len() <= MAX_CANDIDATES);
        }
        assert_eq!(
            store
                .observe_candidate(
                    "1.1.1.1:8333".parse().unwrap(),
                    candidate(b"honest", [8; 32]),
                    2
                )
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .observe_candidate(
                    "2.2.2.2:8333".parse().unwrap(),
                    candidate(b"honest", [8; 32]),
                    2
                )
                .unwrap(),
            2
        );
        store.verify_filter(&[7; 32], b"honest").unwrap();
        for n in 3..100 {
            store
                .observe_candidate(
                    SocketAddr::from(([n, 0, 0, 1], 8333)),
                    candidate(b"honest", [8; 32]),
                    2,
                )
                .unwrap();
        }
        assert!(store
            .candidates
            .values()
            .all(|voters| voters.len() <= MAX_VOTERS));
        assert_eq!(
            store
                .observe_candidate(
                    "1.1.1.1:8333".parse().unwrap(),
                    candidate(b"honest", [9; 32]),
                    2
                )
                .unwrap(),
            1
        );
        assert_eq!(store.candidates.len(), 1);
    }

    #[test]
    fn filter_records_follow_the_retained_header_window() {
        let genesis = bitcoin::blockdata::constants::genesis_block(bitcoin::Network::Regtest);
        let mut chain = HeaderChain::unanchored_for_tests();
        chain
            .add_header(&bitcoin::consensus::serialize(&genesis.header), 0)
            .unwrap();
        chain.set_retention(32);
        let mut store = FilterHeaderStore::default();
        let mut parent = genesis.block_hash();
        let mut previous_filter = FilterHeader::all_zeros();
        for height in 1u32..=10_000 {
            let mut header = genesis.header;
            header.prev_blockhash = parent;
            header.time += height;
            header.merkle_root = bitcoin::TxMerkleNode::hash(&height.to_le_bytes());
            header.nonce = 0;
            while header.validate_pow(header.target()).is_err() {
                header.nonce += 1;
            }
            let hash = header.block_hash().to_byte_array();
            chain
                .add_header(&bitcoin::consensus::serialize(&header), height)
                .unwrap();
            let mut candidate = FilterHeaderStore {
                observed_tip: Some(hash),
                ..Default::default()
            };
            previous_filter = candidate
                .apply_range(
                    height,
                    &[hash],
                    &[FilterHash::hash(b"filter")],
                    previous_filter,
                )
                .unwrap();
            store
                .observe_candidate("1.1.1.1:8333".parse().unwrap(), candidate, 1)
                .unwrap();
            store.prune(&chain);
            assert!(store.records.len() <= 34);
            assert_eq!(store.candidates.len(), 1);
            store.verify_filter(&hash, b"filter").unwrap();
            if height > 1 {
                assert!(store.header_for(&parent.to_byte_array()).is_some());
            }
            parent = header.block_hash();
        }
        store
            .apply_range(
                10_000,
                &[[88; 32]],
                &[FilterHash::hash(b"orphan")],
                FilterHeader::all_zeros(),
            )
            .unwrap();
        store.prune(&chain);
        assert!(store.header_for(&[88; 32]).is_none());
        assert!(store.records.len() <= 34);
    }

    #[test]
    fn checkpoint_retention_preserves_the_preceding_anchor() {
        let checkpoints = vec![FilterHeader::all_zeros(); 120];
        let mut store = FilterHeaderStore::default();
        store.retain_checkpoints(100_000, 120_000);
        store
            .reconcile_checkpoints([1; 32], 120_000, &checkpoints)
            .unwrap();
        assert!(store.checkpoints.len() <= 22);
        assert!(store.checkpoint(99_000).is_some());
        assert!(store.checkpoint(98_000).is_none());
        store.retain_checkpoints(110_000, 120_000);
        store
            .reconcile_checkpoints([1; 32], 120_000, &checkpoints)
            .unwrap();
        assert!(store.checkpoints.len() <= 12);
        assert!(store.checkpoint(109_000).is_some());
        assert!(store.checkpoint(108_000).is_none());
    }
}

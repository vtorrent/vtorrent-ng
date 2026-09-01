use crate::error::{BtcError, Result};
use bitcoin::bip158::{FilterHash, FilterHeader};
use bitcoin::hashes::Hash;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;

pub(crate) const CHECKPOINT_INTERVAL: u32 = 1_000;
pub(crate) const CFHEADERS_RANGE_LIMIT: usize = 2_000;
pub(crate) const CFILTERS_RANGE_LIMIT: usize = 1_000;

#[derive(Debug, Clone)]
struct FilterRecord {
    filter_hash: FilterHash,
    filter_header: FilterHeader,
}

#[derive(Debug, Default)]
pub(crate) struct FilterHeaderStore {
    records: HashMap<[u8; 32], FilterRecord>,
    checkpoints: Vec<FilterHeader>,
    observed_tip: Option<[u8; 32]>,
    agreeing_peers: HashSet<SocketAddr>,
    candidates: HashMap<([u8; 32], [u8; 32]), HashSet<SocketAddr>>,
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
            if let Some(existing) = self.checkpoints.get(index) {
                if existing != checkpoint {
                    return Err(BtcError::Sync(format!(
                        "compact-filter checkpoint mismatch at height {}",
                        (index + 1) as u32 * CHECKPOINT_INTERVAL
                    )));
                }
            }
        }
        if checkpoints.len() > self.checkpoints.len() {
            self.checkpoints
                .extend_from_slice(&checkpoints[self.checkpoints.len()..]);
        }
        if self.observed_tip != Some(stop_hash) {
            self.observed_tip = Some(stop_hash);
            self.agreeing_peers.clear();
        }
        Ok(())
    }

    pub(crate) fn checkpoint(&self, height: u32) -> Option<FilterHeader> {
        if height == 0 || !height.is_multiple_of(CHECKPOINT_INTERVAL) {
            return None;
        }
        self.checkpoints
            .get((height / CHECKPOINT_INTERVAL - 1) as usize)
            .copied()
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
        let peers = self.candidates.entry((tip, fingerprint)).or_default();
        peers.insert(peer);
        let agreement = peers.len();
        let agreed_peers = peers.clone();
        if agreement < required {
            return Ok(agreement);
        }

        for (index, checkpoint) in candidate.checkpoints.iter().enumerate() {
            if let Some(existing) = self.checkpoints.get(index) {
                if existing != checkpoint {
                    return Err(BtcError::Sync(format!(
                        "agreed compact-filter candidate conflicts at checkpoint height {}",
                        (index + 1) as u32 * CHECKPOINT_INTERVAL
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

        if candidate.checkpoints.len() > self.checkpoints.len() {
            self.checkpoints
                .extend_from_slice(&candidate.checkpoints[self.checkpoints.len()..]);
        }
        self.records.extend(candidate.records);
        self.observed_tip = Some(tip);
        self.agreeing_peers = agreed_peers;
        Ok(agreement)
    }

    fn fingerprint(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(self.observed_tip.unwrap_or_default());
        for checkpoint in &self.checkpoints {
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
                .observe_candidate("127.0.0.2:8333".parse().unwrap(), matching, 2)
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
                .observe_candidate("127.0.0.3:8333".parse().unwrap(), candidate(b"honest"), 2,)
                .unwrap(),
            2
        );
        assert!(store.verify_filter(&[7u8; 32], b"honest").is_ok());
    }
}

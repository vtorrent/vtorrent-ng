use super::{proof_service::PROOF_WINDOW, Node};
use crate::chain::Chain;
use std::{
    collections::HashMap,
    net::SocketAddr,
    time::{Duration, Instant},
};
use vtorrent_p2p::message::{encode_for_peer, GetProofMsg, NetMessage};

pub(crate) const MAX_PENDING_PROOFS: usize = 100;
const MAX_ATTEMPTS: u8 = 4;
const MAX_RETRIES_PER_TICK: usize = 8;
pub(crate) const RETRY_DELAY: Duration = PROOF_WINDOW.saturating_add(Duration::from_secs(5));
const REQUEST_TTL: Duration = Duration::from_secs(300);

struct PendingProof {
    height: u32,
    peer: SocketAddr,
    created: Instant,
    next_attempt: Instant,
    attempts: u8,
}

#[derive(Default)]
pub(crate) struct ProofRetries {
    pending: HashMap<[u8; 32], PendingProof>,
}

impl ProofRetries {
    pub(crate) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub(crate) fn track(
        &mut self,
        hash: [u8; 32],
        height: u32,
        peer: SocketAddr,
        now: Instant,
    ) -> bool {
        if self.pending.contains_key(&hash) {
            return false;
        }
        if self.pending.len() >= MAX_PENDING_PROOFS {
            let oldest = self
                .pending
                .iter()
                .min_by_key(|(hash, entry)| (entry.height, **hash))
                .map(|(hash, _)| *hash)
                .unwrap();
            self.pending.remove(&oldest);
        }
        self.pending.insert(
            hash,
            PendingProof {
                height,
                peer,
                created: now,
                next_attempt: now + RETRY_DELAY,
                attempts: 1,
            },
        );
        true
    }

    pub(crate) fn complete(&mut self, hash: &[u8; 32]) {
        self.pending.remove(hash);
    }

    fn prune(&mut self, now: Instant, mut keep: impl FnMut(&[u8; 32], u32) -> bool) {
        self.pending.retain(|hash, entry| {
            now.duration_since(entry.created) < REQUEST_TTL
                && (entry.attempts < MAX_ATTEMPTS || now < entry.next_attempt)
                && keep(hash, entry.height)
        });
    }

    fn due(
        &mut self,
        now: Instant,
        peers: &[(SocketAddr, u32)],
    ) -> Vec<([u8; 32], SocketAddr, u32)> {
        self.prune(now, |_, _| true);
        if peers.is_empty() {
            return vec![];
        }
        let mut due: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, entry)| now >= entry.next_attempt && entry.attempts < MAX_ATTEMPTS)
            .map(|(hash, entry)| (*hash, entry.next_attempt, entry.height))
            .collect();
        due.sort_unstable_by_key(|(hash, at, height)| (*at, *height, *hash));
        due.into_iter()
            .take(MAX_RETRIES_PER_TICK)
            .map(|(hash, _, _)| {
                let entry = self.pending.get_mut(&hash).unwrap();
                let &(peer, version) = peers
                    .iter()
                    .find(|(peer, _)| *peer == entry.peer)
                    .unwrap_or(&peers[(entry.attempts as usize - 1) % peers.len()]);
                entry.attempts += 1;
                entry.next_attempt = now + RETRY_DELAY;
                (hash, peer, version)
            })
            .collect()
    }
}

fn recent_canonical(chain: &Chain, hash: &[u8; 32], height: u32) -> bool {
    height > 0
        && chain.block_hash_at_height(height) == Some(*hash)
        && chain.best_height().saturating_sub(height) < MAX_PENDING_PROOFS as u32
}

impl Node {
    async fn prune_proof_requests(&mut self, now: Instant) {
        if self.proof_retries.is_empty() {
            return;
        }
        let chain = self.chain.lock().await;
        let cached = self.stake_proofs.read().await;
        self.proof_retries.prune(now, |hash, height| {
            recent_canonical(&chain, hash, height) && !cached.contains_key(hash)
        });
    }

    pub(crate) async fn request_stake_proof(
        &mut self,
        hash: [u8; 32],
        height: u32,
        peer: SocketAddr,
        version: u32,
    ) {
        let now = Instant::now();
        self.prune_proof_requests(now).await;
        let eligible = {
            let chain = self.chain.lock().await;
            recent_canonical(&chain, &hash, height)
                && !self.stake_proofs.read().await.contains_key(&hash)
        };
        if eligible && self.proof_retries.track(hash, height, peer, now) {
            self.peer_manager
                .send_to(
                    peer,
                    NetMessage::new(
                        "getproof",
                        encode_for_peer(&GetProofMsg { block_hash: hash }, version),
                    ),
                )
                .await;
        }
    }

    pub(crate) async fn retry_stake_proofs(&mut self, now: Instant) {
        self.prune_proof_requests(now).await;
        if self.proof_retries.is_empty() {
            return;
        }
        let mut peers: Vec<_> = self
            .peer_manager
            .connected_peers()
            .into_iter()
            .filter_map(|addr| {
                self.peer_versions
                    .get(&addr)
                    .map(|version| (addr, *version))
            })
            .collect();
        peers.sort_unstable_by_key(|(addr, _)| *addr);
        for (hash, peer, version) in self.proof_retries.due(now, &peers) {
            self.peer_manager
                .send_to(
                    peer,
                    NetMessage::new(
                        "getproof",
                        encode_for_peer(&GetProofMsg { block_hash: hash }, version),
                    ),
                )
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(port: u16) -> SocketAddr {
        format!("127.0.0.1:{port}").parse().unwrap()
    }

    #[test]
    fn retries_wait_past_the_quota_window_and_stop_after_four_attempts() {
        let now = Instant::now();
        let mut queue = ProofRetries::default();
        assert!(queue.track([1; 32], 1, peer(1), now));
        assert!(queue
            .due(now + Duration::from_secs(64), &[(peer(1), 3)])
            .is_empty());
        for seconds in [65, 130, 195] {
            assert!(!queue.track([1; 32], 1, peer(2), now + Duration::from_secs(seconds)));
            assert_eq!(
                queue.due(now + Duration::from_secs(seconds), &[(peer(1), 3)]),
                vec![([1; 32], peer(1), 3)]
            );
        }
        assert!(queue
            .due(now + Duration::from_secs(260), &[(peer(1), 3)])
            .is_empty());
        assert!(queue.pending.is_empty());
    }

    #[test]
    fn disconnection_preserves_attempts_and_uses_a_replacement_peer() {
        let now = Instant::now();
        let mut queue = ProofRetries::default();
        queue.track([1; 32], 1, peer(1), now);
        for seconds in [65, 130, 195] {
            assert!(queue
                .due(now + Duration::from_secs(seconds), &[])
                .is_empty());
        }
        assert_eq!(
            queue.due(now + Duration::from_secs(240), &[(peer(2), 2)]),
            vec![([1; 32], peer(2), 2)]
        );
        assert!(queue
            .due(now + Duration::from_secs(300), &[(peer(2), 2)])
            .is_empty());
        assert!(queue.pending.is_empty());
    }

    #[test]
    fn disconnected_sources_rotate_fallbacks_and_prefer_the_original_if_it_returns() {
        let now = Instant::now();
        let mut queue = ProofRetries::default();
        queue.track([1; 32], 1, peer(1), now);
        let peers = [(peer(2), 2), (peer(3), 3)];
        assert_eq!(
            queue.due(now + Duration::from_secs(65), &peers),
            vec![([1; 32], peer(2), 2)]
        );
        assert_eq!(
            queue.due(now + Duration::from_secs(130), &peers),
            vec![([1; 32], peer(3), 3)]
        );
        assert_eq!(
            queue.due(
                now + Duration::from_secs(195),
                &[(peer(2), 2), (peer(1), 3)]
            ),
            vec![([1; 32], peer(1), 3)]
        );
    }

    #[test]
    fn queue_and_dispatch_batches_are_bounded_and_completion_cancels_work() {
        let now = Instant::now();
        let mut queue = ProofRetries::default();
        for height in 1..=101 {
            queue.track([height as u8; 32], height, peer(1), now);
        }
        assert_eq!(queue.pending.len(), 100);
        assert!(!queue.pending.contains_key(&[1; 32]));
        let due = queue.due(now + RETRY_DELAY, &[(peer(1), 3)]);
        assert_eq!(due.len(), 8);
        assert_eq!(due[0].0, [2; 32]);
        queue.complete(&[100; 32]);
        queue.prune(now + RETRY_DELAY, |hash, _| *hash == [100; 32]);
        assert!(queue.pending.is_empty());
    }
}

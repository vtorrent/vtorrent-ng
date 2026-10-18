use super::Node;
use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};
use vtorrent_spv::stake::StakeProof;

pub(crate) const PROOF_WINDOW: Duration = Duration::from_secs(60);
pub(crate) const PROOFS_PER_IP: usize = 100;
pub(crate) const PROOFS_GLOBAL: usize = 200;

#[derive(Default)]
pub(crate) struct ProofBudget {
    global: VecDeque<Instant>,
    peers: HashMap<IpAddr, VecDeque<Instant>>,
}

fn expire(times: &mut VecDeque<Instant>, now: Instant) {
    while times
        .front()
        .is_some_and(|time| now.duration_since(*time) >= PROOF_WINDOW)
    {
        times.pop_front();
    }
}

impl ProofBudget {
    pub(crate) fn try_admit(&mut self, ip: IpAddr, now: Instant) -> bool {
        expire(&mut self.global, now);
        self.peers.retain(|_, times| {
            expire(times, now);
            !times.is_empty()
        });
        let ip = ip.to_canonical();
        if self.global.len() >= PROOFS_GLOBAL
            || self
                .peers
                .get(&ip)
                .is_some_and(|times| times.len() >= PROOFS_PER_IP)
        {
            return false;
        }
        self.global.push_back(now);
        self.peers.entry(ip).or_default().push_back(now);
        true
    }
}

impl Node {
    pub(crate) async fn get_stake_proof(
        &mut self,
        peer: SocketAddr,
        hash: [u8; 32],
    ) -> Option<StakeProof> {
        let mut proof = self.stake_proofs.read().await.get(&hash).cloned();
        if proof.is_none() {
            #[cfg(feature = "test-util")]
            let requested = Instant::now();
            let chain = self.chain.lock().await;
            #[cfg(feature = "test-util")]
            let acquired = Instant::now();
            let snapshot = if !chain.has_stake_proof_history(&hash) {
                None
            } else if self.proof_budget.try_admit(peer.ip(), Instant::now()) {
                chain.stake_proof_snapshot(&hash)
            } else {
                tracing::debug!(%peer, "Stake proof reconstruction budget exhausted");
                None
            };
            #[cfg(feature = "test-util")]
            let held_us = acquired.elapsed().as_micros();
            drop(chain);
            if let Some(snapshot) = snapshot {
                proof = match tokio::task::spawn_blocking(move || snapshot.reconstruct()).await {
                    Ok(rebuilt) => rebuilt,
                    Err(error) => {
                        tracing::warn!(%error, "Stake proof reconstruction worker failed");
                        None
                    }
                };
            }
            #[cfg(feature = "test-util")]
            tracing::info!(
                "proof_reconstruction hash={} wait_us={} hold_us={} found={}",
                hex::encode(hash),
                acquired.duration_since(requested).as_micros(),
                held_us,
                proof.is_some()
            );
        }
        match proof {
            Some(candidate) => self.cache_canonical_stake_proof(hash, candidate).await,
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(last: u8) -> IpAddr {
        format!("192.0.2.{last}").parse().unwrap()
    }

    #[test]
    fn per_ip_limit_survives_address_aliases_and_preserves_other_peers() {
        let mut budget = ProofBudget::default();
        let now = Instant::now();
        for _ in 0..PROOFS_PER_IP {
            assert!(budget.try_admit(ip(1), now));
        }
        assert!(!budget.try_admit(ip(1), now));
        assert!(!budget.try_admit("::ffff:192.0.2.1".parse().unwrap(), now));
        assert!(budget.try_admit(ip(2), now));
    }

    #[test]
    fn rotating_ips_cannot_exceed_the_global_limit_or_grow_tracking() {
        let mut budget = ProofBudget::default();
        let now = Instant::now();
        for n in 1..=PROOFS_GLOBAL {
            assert!(budget.try_admit(ip(n as u8), now));
        }
        for n in 1..=250 {
            assert!(!budget.try_admit(ip(n), now));
        }
        assert_eq!(budget.global.len(), PROOFS_GLOBAL);
        assert_eq!(budget.peers.len(), PROOFS_GLOBAL);
        assert!(budget.try_admit(ip(250), now + PROOF_WINDOW));
        assert_eq!(budget.global.len(), 1);
        assert_eq!(budget.peers.len(), 1);
    }

    #[test]
    fn sliding_window_expires_entries_at_their_own_deadlines() {
        let mut budget = ProofBudget::default();
        let now = Instant::now();
        for offset in 0..2 {
            for _ in 0..PROOFS_PER_IP / 2 {
                assert!(budget.try_admit(ip(1), now + Duration::from_secs(offset)));
            }
        }
        assert!(!budget.try_admit(ip(1), now + PROOF_WINDOW - Duration::from_nanos(1)));
        for _ in 0..PROOFS_PER_IP / 2 {
            assert!(budget.try_admit(ip(1), now + PROOF_WINDOW));
        }
        assert!(!budget.try_admit(ip(1), now + PROOF_WINDOW));
        assert!(budget.try_admit(ip(1), now + PROOF_WINDOW + Duration::from_secs(1)));
    }
}

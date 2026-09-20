use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::connect_info::ConnectInfo;
use axum::extract::Request;
use axum::extract::State;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;
use tokio::sync::RwLock;
use tokio::time::Instant;

const WINDOW: Duration = Duration::from_secs(60);
const MAX_REQUESTS: u32 = 100;
/// How often stale client windows are pruned.
const PRUNE_INTERVAL: Duration = Duration::from_secs(30);
/// Maximum tracked client addresses before oldest-window eviction.
const MAX_TRACKED_CLIENTS: usize = 100_000;

pub struct RateLimiter {
    clients: HashMap<IpAddr, (u32, Instant)>,
    last_prune: Instant,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimiter {
    pub fn new() -> Self {
        RateLimiter {
            clients: HashMap::new(),
            last_prune: Instant::now(),
        }
    }

    fn is_rate_limited(&mut self, ip: IpAddr) -> bool {
        let now = Instant::now();
        // Prune periodically rather than on every request: a full `retain`
        // over the map is O(distinct IPs) per call, so doing it per request
        // makes the limiter itself a CPU amplification vector under a
        // spoofed-source flood.
        if now.duration_since(self.last_prune) >= PRUNE_INTERVAL {
            self.clients
                .retain(|_, (_, window_start)| now.duration_since(*window_start) < WINDOW);
            self.last_prune = now;
        }
        // Hard cap: even within a window, a flood of distinct source IPs must
        // not grow the map without bound. Evict the oldest windows first.
        if self.clients.len() >= MAX_TRACKED_CLIENTS && !self.clients.contains_key(&ip) {
            let mut entries: Vec<(IpAddr, Instant)> = self
                .clients
                .iter()
                .map(|(addr, (_, start))| (*addr, *start))
                .collect();
            entries.sort_by_key(|(_, start)| *start);
            let excess = entries.len() - MAX_TRACKED_CLIENTS * 3 / 4;
            for (addr, _) in entries.into_iter().take(excess) {
                self.clients.remove(&addr);
            }
        }
        let entry = self.clients.entry(ip).or_insert((0, now));
        if now.duration_since(entry.1) >= WINDOW {
            *entry = (1, now);
            false
        } else {
            entry.0 += 1;
            entry.0 > MAX_REQUESTS
        }
    }
}

pub type SharedRateLimiter = Arc<RwLock<RateLimiter>>;

pub fn new_shared_limiter() -> SharedRateLimiter {
    Arc::new(RwLock::new(RateLimiter::new()))
}

pub async fn ip_rate_limit(
    State(limiter): State<SharedRateLimiter>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let ip = request
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip());

    match ip {
        // Loopback is exempt: the default deployment binds RPC to 127.0.0.1
        // and the local UI/CLI must not be throttled. NOTE: if a reverse proxy
        // is placed in front of RPC, every client appears as loopback and this
        // limiter is bypassed entirely — the proxy must apply its own limit,
        // or RPC must be bound to a non-loopback address (which requires an
        // API key).
        Some(ip) if ip.is_loopback() => Ok(next.run(request).await),
        Some(ip) => {
            let limited = { limiter.write().await.is_rate_limited(ip) };
            if limited {
                Err(StatusCode::TOO_MANY_REQUESTS)
            } else {
                Ok(next.run(request).await)
            }
        }
        None => Ok(next.run(request).await),
    }
}

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

pub struct RateLimiter {
    clients: HashMap<IpAddr, (u32, Instant)>,
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
        }
    }

    fn is_rate_limited(&mut self, ip: IpAddr) -> bool {
        let now = Instant::now();
        self.clients
            .retain(|_, (_, window_start)| now.duration_since(*window_start) < WINDOW);
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

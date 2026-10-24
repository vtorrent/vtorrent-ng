//! Shared token-bucket rate limiter for torrent upload/download.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

struct RateState {
    tokens: f64,
    last: Instant,
}

pub struct TorrentRateLimiter {
    download_rate: AtomicU64,
    upload_rate: AtomicU64,
    download: Mutex<RateState>,
    upload: Mutex<RateState>,
}

impl TorrentRateLimiter {
    pub fn new(download_rate: u64, upload_rate: u64) -> Self {
        Self {
            download_rate: AtomicU64::new(download_rate),
            upload_rate: AtomicU64::new(upload_rate),
            download: Mutex::new(RateState {
                tokens: download_rate as f64,
                last: Instant::now(),
            }),
            upload: Mutex::new(RateState {
                tokens: upload_rate as f64,
                last: Instant::now(),
            }),
        }
    }

    pub fn set_rates(&self, download_rate: u64, upload_rate: u64) {
        self.download_rate.store(download_rate, Ordering::Relaxed);
        self.upload_rate.store(upload_rate, Ordering::Relaxed);
    }

    pub fn download_rate(&self) -> u64 {
        self.download_rate.load(Ordering::Relaxed)
    }

    pub fn upload_rate(&self) -> u64 {
        self.upload_rate.load(Ordering::Relaxed)
    }

    pub async fn consume_download(&self, bytes: u64, cancel: &CancellationToken) {
        consume(
            &self.download,
            self.download_rate.load(Ordering::Relaxed),
            bytes,
            cancel,
        )
        .await;
    }

    pub async fn consume_upload(&self, bytes: u64, cancel: &CancellationToken) {
        consume(
            &self.upload,
            self.upload_rate.load(Ordering::Relaxed),
            bytes,
            cancel,
        )
        .await;
    }
}

async fn consume(mutex: &Mutex<RateState>, rate: u64, bytes: u64, cancel: &CancellationToken) {
    if rate == 0 || bytes == 0 {
        return;
    }
    let mut state = mutex.lock().await;
    let now = Instant::now();
    let elapsed = now.saturating_duration_since(state.last).as_secs_f64();
    state.tokens = (state.tokens + elapsed * rate as f64).min(rate as f64);
    state.last = now;
    let need = bytes as f64;
    if state.tokens >= need {
        state.tokens -= need;
        return;
    }
    let wait = (need - state.tokens) / rate as f64;
    state.tokens = 0.0;
    drop(state);
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs_f64(wait)) => {}
        _ = cancel.cancelled() => return,
    }
    let mut state = mutex.lock().await;
    state.tokens = 0.0;
    state.last = Instant::now();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unlimited_limiter_does_not_sleep() {
        let limiter = TorrentRateLimiter::new(0, 0);
        let cancel = CancellationToken::new();
        let start = Instant::now();
        limiter.consume_download(10 * 1024 * 1024, &cancel).await;
        limiter.consume_upload(10 * 1024 * 1024, &cancel).await;
        assert!(start.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn limited_limiter_sleeps_for_large_transfers() {
        let limiter = TorrentRateLimiter::new(1024, 1024);
        let cancel = CancellationToken::new();
        let start = Instant::now();
        limiter.consume_download(4096, &cancel).await;
        assert!(start.elapsed() >= Duration::from_millis(100));
    }
}

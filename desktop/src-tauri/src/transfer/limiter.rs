//! One download speed limit shared by every transfer (Settings → Network → speed limit).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Bytes per second; 0 = no limit
static LIMIT: AtomicU64 = AtomicU64::new(0);
/// (bytes allowed right now, when that was last topped up)
static BUCKET: Mutex<Option<(f64, Instant)>> = Mutex::new(None);

pub fn set(bytes_per_sec: f64) {
    LIMIT.store(bytes_per_sec.max(0.0) as u64, Ordering::Relaxed);
}

/// Waits until `n` more bytes may be received (returns early when cancelled).
pub fn take(n: u64, cancel: &AtomicBool) {
    loop {
        let limit = LIMIT.load(Ordering::Relaxed);
        if limit == 0 {
            return;
        }
        let wait = {
            let mut b = BUCKET.lock().unwrap();
            let now = Instant::now();
            let (tokens, last) = b.get_or_insert((limit as f64, now));
            // Top up, holding at most one second's worth
            *tokens = (*tokens + now.duration_since(*last).as_secs_f64() * limit as f64).min(limit as f64);
            *last = now;
            if *tokens >= n as f64 || *tokens >= limit as f64 {
                *tokens -= n as f64;
                return;
            }
            Duration::from_secs_f64(((n as f64 - *tokens) / limit as f64).min(0.25))
        };
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(wait);
    }
}

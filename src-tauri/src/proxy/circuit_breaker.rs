//! Thread-safe circuit breaker.
//!
//! Ported from `src/gateway/circuit-breaker.ts`. Three states:
//! - `Closed`: requests pass normally, consecutive failures counted.
//! - `Open`: all requests rejected until cooldown expires.
//! - `HalfOpen`: limited trial requests; success closes, failure re-opens.
//!
//! Thread-safe via `std::sync::Mutex` (lock duration is microseconds - no
//! contention concern).

use crate::database::dao::proxy_providers::CircuitBreakerConfig;
use serde::Serialize;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CircuitState {
    Closed,
    Open,
    #[serde(rename = "half-open")]
    HalfOpen,
}

impl std::fmt::Display for CircuitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => write!(f, "closed"),
            Self::Open => write!(f, "open"),
            Self::HalfOpen => write!(f, "half-open"),
        }
    }
}

#[derive(Debug, Serialize, Clone)]
pub struct CircuitBreakerSnapshot {
    pub state: CircuitState,
    pub failures: u32,
    pub successes: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opened_at: Option<i64>,
    pub cooldown_ms: u64,
}

struct Inner {
    state: CircuitState,
    failures: u32,
    successes: u32,
    opened_at: i64, // unix millis, 0 if never opened
    half_open_in_flight: u32,
}

pub struct CircuitBreaker {
    cfg: CircuitBreakerConfig,
    inner: Mutex<Inner>,
}

impl CircuitBreaker {
    pub fn new(cfg: CircuitBreakerConfig) -> Self {
        Self {
            cfg,
            inner: Mutex::new(Inner {
                state: CircuitState::Closed,
                failures: 0,
                successes: 0,
                opened_at: 0,
                half_open_in_flight: 0,
            }),
        }
    }

    fn now_ms() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    /// Whether a request is allowed to pass. Implements lazy open->half-open
    /// transition when cooldown expires.
    pub fn allow(&self) -> bool {
        let mut g = self.inner.lock().unwrap();
        match g.state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                let elapsed = Self::now_ms() - g.opened_at;
                if elapsed >= self.cfg.cooldown_ms as i64 {
                    g.state = CircuitState::HalfOpen;
                    g.half_open_in_flight = 0;
                    g.successes = 0;
                    log::info!("circuit: breaker -> half-open (probing)");
                    g.half_open_in_flight < self.cfg.half_open_retries
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => g.half_open_in_flight < self.cfg.half_open_retries,
        }
    }

    pub fn record_success(&self) {
        let mut g = self.inner.lock().unwrap();
        match g.state {
            CircuitState::HalfOpen => {
                g.successes += 1;
                g.half_open_in_flight = g.half_open_in_flight.saturating_sub(1);
                if g.successes >= self.cfg.success_threshold {
                    g.state = CircuitState::Closed;
                    g.failures = 0;
                    g.successes = 0;
                    g.half_open_in_flight = 0;
                    log::info!("circuit: breaker CLOSED (recovered)");
                }
            }
            CircuitState::Closed => {
                g.failures = 0;
            }
            CircuitState::Open => {}
        }
    }

    pub fn record_failure(&self) {
        let mut g = self.inner.lock().unwrap();
        match g.state {
            CircuitState::HalfOpen => {
                g.half_open_in_flight = g.half_open_in_flight.saturating_sub(1);
                g.state = CircuitState::Open;
                g.opened_at = Self::now_ms();
                g.successes = 0;
                g.half_open_in_flight = 0;
                log::warn!("circuit: breaker re-OPENED (half-open failure)");
            }
            CircuitState::Closed => {
                g.failures += 1;
                if g.failures >= self.cfg.failure_threshold {
                    g.state = CircuitState::Open;
                    g.opened_at = Self::now_ms();
                    g.successes = 0;
                    g.half_open_in_flight = 0;
                    log::warn!("circuit: breaker OPENED (after {} failures)", g.failures);
                }
            }
            CircuitState::Open => {}
        }
    }

    pub fn snapshot(&self) -> CircuitBreakerSnapshot {
        let g = self.inner.lock().unwrap();
        CircuitBreakerSnapshot {
            state: g.state,
            failures: g.failures,
            successes: g.successes,
            opened_at: if g.opened_at > 0 { Some(g.opened_at) } else { None },
            cooldown_ms: self.cfg.cooldown_ms,
        }
    }

    #[allow(dead_code)]
    pub fn reset(&self) {
        let mut g = self.inner.lock().unwrap();
        g.state = CircuitState::Closed;
        g.failures = 0;
        g.successes = 0;
        g.half_open_in_flight = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(threshold: u32) -> CircuitBreakerConfig {
        CircuitBreakerConfig {
            failure_threshold: threshold,
            cooldown_ms: 1000,
            half_open_retries: 1,
            success_threshold: 1,
        }
    }

    #[test]
    fn opens_after_threshold_failures() {
        let b = CircuitBreaker::new(cfg(3));
        assert!(b.allow());
        for _ in 0..3 {
            b.record_failure();
        }
        assert!(!b.allow());
        assert_eq!(b.snapshot().state, CircuitState::Open);
    }

    #[test]
    fn success_resets_failures_in_closed() {
        let b = CircuitBreaker::new(cfg(5));
        b.record_failure();
        b.record_failure();
        b.record_success();
        assert_eq!(b.snapshot().failures, 0);
    }

    #[test]
    fn half_open_reopens_on_failure() {
        let b = CircuitBreaker::new(cfg(1));
        b.record_failure(); // opens immediately
        assert_eq!(b.snapshot().state, CircuitState::Open);
        // Wait for cooldown
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert!(b.allow()); // transitions to half-open, allows
        b.record_failure(); // re-opens
        assert_eq!(b.snapshot().state, CircuitState::Open);
    }
}
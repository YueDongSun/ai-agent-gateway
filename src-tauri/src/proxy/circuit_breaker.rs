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
    generation: u64,
}

/// An owned admission token. A stale attempt cannot settle or release a newer
/// half-open generation after another request has reopened the breaker.
#[allow(non_camel_case_types)]
pub struct Circuit_permit {
    generation: u64,
    half_open: bool,
}

#[allow(non_camel_case_types)]
pub enum Circuit_outcome {
    Success,
    Failure,
    Cancelled,
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
                generation: 0,
            }),
        }
    }

    fn now_ms() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    /// Readiness check retained for compatibility. Executing requests must use
    /// `acquire` so half-open concurrency has a concrete owner.
    pub fn allow(&self) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        self.admission_ready(&mut inner)
    }

    fn admission_ready(&self, inner: &mut Inner) -> bool {
        if inner.state == CircuitState::Open {
            let elapsed = Self::now_ms() - inner.opened_at;
            if elapsed < self.cfg.cooldown_ms as i64 {
                return false;
            }
            inner.state = CircuitState::HalfOpen;
            inner.generation = inner.generation.wrapping_add(1);
            inner.half_open_in_flight = 0;
            inner.successes = 0;
            log::info!("circuit: breaker -> half-open (probing)");
        }
        inner.state == CircuitState::Closed
            || inner.half_open_in_flight < self.cfg.half_open_retries
    }

    pub fn acquire(&self) -> Option<Circuit_permit> {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if !self.admission_ready(&mut inner) {
            return None;
        }
        let half_open = inner.state == CircuitState::HalfOpen;
        if half_open {
            inner.half_open_in_flight += 1;
        }
        Some(Circuit_permit {
            generation: inner.generation,
            half_open,
        })
    }

    pub fn settle(&self, permit: Circuit_permit, outcome: Circuit_outcome) {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if permit.generation != inner.generation {
            return;
        }
        if permit.half_open {
            inner.half_open_in_flight = inner.half_open_in_flight.saturating_sub(1);
        }
        match outcome {
            Circuit_outcome::Success => self.success_locked(&mut inner),
            Circuit_outcome::Failure => self.failure_locked(&mut inner),
            Circuit_outcome::Cancelled => {}
        }
    }

    pub fn record_success(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        self.success_locked(&mut inner);
    }

    fn success_locked(&self, inner: &mut Inner) {
        match inner.state {
            CircuitState::HalfOpen => {
                inner.successes += 1;
                if inner.successes >= self.cfg.success_threshold {
                    inner.state = CircuitState::Closed;
                    inner.generation = inner.generation.wrapping_add(1);
                    inner.failures = 0;
                    inner.successes = 0;
                    inner.half_open_in_flight = 0;
                    log::info!("circuit: breaker CLOSED (recovered)");
                }
            }
            CircuitState::Closed => inner.failures = 0,
            CircuitState::Open => {}
        }
    }

    pub fn record_failure(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        self.failure_locked(&mut inner);
    }

    fn failure_locked(&self, inner: &mut Inner) {
        match inner.state {
            CircuitState::HalfOpen => {
                inner.state = CircuitState::Open;
                inner.generation = inner.generation.wrapping_add(1);
                inner.opened_at = Self::now_ms();
                inner.successes = 0;
                inner.half_open_in_flight = 0;
                log::warn!("circuit: breaker re-OPENED (half-open failure)");
            }
            CircuitState::Closed => {
                inner.failures += 1;
                if inner.failures >= self.cfg.failure_threshold {
                    inner.state = CircuitState::Open;
                    inner.generation = inner.generation.wrapping_add(1);
                    inner.opened_at = Self::now_ms();
                    inner.successes = 0;
                    inner.half_open_in_flight = 0;
                    log::warn!(
                        "circuit: breaker OPENED (after {} failures)",
                        inner.failures
                    );
                }
            }
            CircuitState::Open => {}
        }
    }
    pub fn snapshot(&self) -> CircuitBreakerSnapshot {
        let g = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        CircuitBreakerSnapshot {
            state: g.state,
            failures: g.failures,
            successes: g.successes,
            opened_at: if g.opened_at > 0 {
                Some(g.opened_at)
            } else {
                None
            },
            cooldown_ms: self.cfg.cooldown_ms,
        }
    }

    #[allow(dead_code)]
    pub fn reset(&self) {
        let mut g = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        g.state = CircuitState::Closed;
        g.generation = g.generation.wrapping_add(1);
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
    #[test]
    fn stale_cancel_cannot_release_another_generation_probe() {
        let breaker = CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 1,
            cooldown_ms: 0,
            half_open_retries: 1,
            success_threshold: 1,
        });
        breaker.record_failure();
        let stale = breaker.acquire().unwrap();
        assert!(breaker.acquire().is_none());
        breaker.record_failure();
        let current = breaker.acquire().unwrap();
        breaker.settle(stale, Circuit_outcome::Cancelled);
        assert!(breaker.acquire().is_none());
        breaker.settle(current, Circuit_outcome::Cancelled);
        assert!(breaker.acquire().is_some());
        assert_eq!(breaker.snapshot().successes, 0);
    }
}

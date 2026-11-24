//! Three-state lockless atomic Circuit Breaker (Martin Fowler pattern) for upstream AI providers.
//!
//! Provides `Closed`, `Open`, and `HalfOpen` states with lockless atomic tracking:
//! - `Closed`: Normal operation. Tracks consecutive failures and overall error rate over min requests.
//!   If consecutive failures reach `failure_threshold` or error rate exceeds `error_rate_threshold`
//!   over at least `min_requests`, the circuit trips to `Open`.
//! - `Open`: Requests are rejected (`allow_request` returns `false`). After `timeout_seconds`,
//!   the next request transitions the circuit to `HalfOpen` for probing.
//! - `HalfOpen`: Allows 1 probe request through at a time. If probe requests succeed consecutively
//!   up to `success_threshold`, the circuit recovers to `Closed`. If any probe fails, it resets
//!   to `Open` with full cooldown.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU8, Ordering};
use std::sync::{Arc, RwLock};

pub const STATE_CLOSED: u8 = 0;
pub const STATE_OPEN: u8 = 1;
pub const STATE_HALF_OPEN: u8 = 2;

/// Three-state representation of upstream provider availability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum CircuitState {
    #[default]
    Closed,
    Open,
    HalfOpen,
}

impl std::fmt::Display for CircuitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CircuitState::Closed => write!(f, "closed"),
            CircuitState::Open => write!(f, "open"),
            CircuitState::HalfOpen => write!(f, "half-open"),
        }
    }
}

impl From<u8> for CircuitState {
    fn from(val: u8) -> Self {
        match val {
            STATE_OPEN => CircuitState::Open,
            STATE_HALF_OPEN => CircuitState::HalfOpen,
            _ => CircuitState::Closed,
        }
    }
}

impl From<CircuitState> for u8 {
    fn from(s: CircuitState) -> Self {
        match s {
            CircuitState::Closed => STATE_CLOSED,
            CircuitState::Open => STATE_OPEN,
            CircuitState::HalfOpen => STATE_HALF_OPEN,
        }
    }
}

/// Configuration settings for the three-state Circuit Breaker.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CircuitBreakerConfig {
    /// Number of consecutive failures in `Closed` state before tripping to `Open`.
    pub failure_threshold: u32,
    /// Number of consecutive successful probe requests in `HalfOpen` required to recover to `Closed`.
    pub success_threshold: u32,
    /// Cooldown timeout in seconds when `Open` before a probe is permitted.
    pub timeout_seconds: u64,
    /// Error rate threshold (e.g. 0.6 = 60%) to trip `Closed` to `Open`.
    pub error_rate_threshold: f64,
    /// Minimum requests in the evaluation cycle before error rate threshold is evaluated.
    pub min_requests: u32,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 3,
            success_threshold: 2,
            timeout_seconds: 60,
            error_rate_threshold: 0.5,
            min_requests: 5,
        }
    }
}

pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Lockless atomic tracking state for an individual provider.
#[derive(Debug)]
struct ProviderCircuit {
    state: AtomicU8,
    opened_at_secs: AtomicI64,
    consecutive_failures: AtomicU32,
    total_requests: AtomicU32,
    total_failures: AtomicU32,
    consecutive_successes: AtomicU32,
    probe_in_flight: AtomicBool,
}

impl ProviderCircuit {
    fn new() -> Self {
        Self {
            state: AtomicU8::new(STATE_CLOSED),
            opened_at_secs: AtomicI64::new(0),
            consecutive_failures: AtomicU32::new(0),
            total_requests: AtomicU32::new(0),
            total_failures: AtomicU32::new(0),
            consecutive_successes: AtomicU32::new(0),
            probe_in_flight: AtomicBool::new(false),
        }
    }

    fn reset(&self) {
        self.state.store(STATE_CLOSED, Ordering::Release);
        self.opened_at_secs.store(0, Ordering::Release);
        self.consecutive_failures.store(0, Ordering::Release);
        self.total_requests.store(0, Ordering::Release);
        self.total_failures.store(0, Ordering::Release);
        self.consecutive_successes.store(0, Ordering::Release);
        self.probe_in_flight.store(false, Ordering::Release);
    }

    fn release_probe(&self) {
        self.probe_in_flight.store(false, Ordering::Release);
    }

    fn allow_request(&self, config: &CircuitBreakerConfig) -> bool {
        let current_state = self.state.load(Ordering::Acquire);
        match current_state {
            STATE_CLOSED => true,
            STATE_OPEN => {
                let now = now_secs();
                let opened_at = self.opened_at_secs.load(Ordering::Acquire);
                if now >= opened_at + config.timeout_seconds as i64 {
                    // Atomically claim the probe reservation FIRST before state transition.
                    // This guarantees exactly one thread holds probe permission and eliminates
                    // the race condition where concurrent threads see STATE_HALF_OPEN with
                    // probe_in_flight not yet set to true.
                    if self
                        .probe_in_flight
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        if self
                            .state
                            .compare_exchange(
                                STATE_OPEN,
                                STATE_HALF_OPEN,
                                Ordering::AcqRel,
                                Ordering::Acquire,
                            )
                            .is_ok()
                        {
                            self.consecutive_successes.store(0, Ordering::Release);
                            true
                        } else {
                            // State changed out of OPEN (e.g. manual reset); release reservation
                            self.probe_in_flight.store(false, Ordering::Release);
                            self.state.load(Ordering::Acquire) == STATE_CLOSED
                        }
                    } else {
                        // Another thread already claimed the probe reservation
                        false
                    }
                } else {
                    false
                }
            }
            STATE_HALF_OPEN => {
                // In half-open state, allow 1 probe request through if none is currently in flight.
                self.probe_in_flight
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            }
            _ => true,
        }
    }

    fn record_success(&self, config: &CircuitBreakerConfig) {
        self.record_success_with(config, true);
    }

    /// Record a success while optionally leaving the in-flight probe slot held.
    ///
    /// The streaming path releases the slot at the commit point (via the RAII
    /// guard in `forward.rs`), then reports success asynchronously when the
    /// stream completes. That deferred success must still advance HalfOpen
    /// recovery, but it must NOT release the slot again — a newer probe may
    /// have claimed it in the meantime. `release_slot = false` skips only that
    /// release; a full recovery to `Closed` still resets everything.
    fn record_success_with(&self, config: &CircuitBreakerConfig, release_slot: bool) {
        let current_state = self.state.load(Ordering::Acquire);
        match current_state {
            STATE_HALF_OPEN => {
                let succ = self.consecutive_successes.fetch_add(1, Ordering::AcqRel) + 1;
                if succ >= config.success_threshold {
                    // Fully recovered: transition to Closed first, then reset counters and probe
                    self.state.store(STATE_CLOSED, Ordering::Release);
                    self.consecutive_failures.store(0, Ordering::Release);
                    self.total_requests.store(0, Ordering::Release);
                    self.total_failures.store(0, Ordering::Release);
                    self.consecutive_successes.store(0, Ordering::Release);
                    self.probe_in_flight.store(false, Ordering::Release);
                } else if release_slot {
                    // Still in HalfOpen, need further successful probes: release slot for next probe
                    self.probe_in_flight.store(false, Ordering::Release);
                }
            }
            STATE_CLOSED => {
                self.consecutive_failures.store(0, Ordering::Release);
                let total_r = self.total_requests.fetch_add(1, Ordering::AcqRel) + 1;
                let max_window = config.min_requests.saturating_mul(10).max(10);
                if total_r >= max_window {
                    self.total_requests.store(0, Ordering::Release);
                    self.total_failures.store(0, Ordering::Release);
                }
            }
            _ => {}
        }
    }

    fn record_failure(&self, config: &CircuitBreakerConfig) {
        let current_state = self.state.load(Ordering::Acquire);
        match current_state {
            STATE_HALF_OPEN => {
                // Any probe failure in HalfOpen immediately trips back to Open with full cooldown
                self.opened_at_secs.store(now_secs(), Ordering::Release);
                self.state.store(STATE_OPEN, Ordering::Release);
                self.probe_in_flight.store(false, Ordering::Release);
                self.consecutive_successes.store(0, Ordering::Release);
                self.consecutive_failures
                    .store(config.failure_threshold, Ordering::Release);
            }
            STATE_CLOSED => {
                let fails = self.consecutive_failures.fetch_add(1, Ordering::AcqRel) + 1;
                let total_f = self.total_failures.fetch_add(1, Ordering::AcqRel) + 1;
                let total_r = self.total_requests.fetch_add(1, Ordering::AcqRel) + 1;

                let trip_by_failures = fails >= config.failure_threshold;
                let trip_by_rate = total_r >= config.min_requests
                    && (total_f as f64 / total_r as f64) >= config.error_rate_threshold;

                if trip_by_failures || trip_by_rate {
                    self.opened_at_secs.store(now_secs(), Ordering::Release);
                    self.state.store(STATE_OPEN, Ordering::Release);
                    self.probe_in_flight.store(false, Ordering::Release);
                    self.consecutive_successes.store(0, Ordering::Release);
                    self.total_requests.store(0, Ordering::Release);
                    self.total_failures.store(0, Ordering::Release);
                } else {
                    let max_window = config.min_requests.saturating_mul(10).max(10);
                    if total_r >= max_window {
                        self.total_requests.store(0, Ordering::Release);
                        self.total_failures.store(0, Ordering::Release);
                    }
                }
            }
            STATE_OPEN => {
                // Already open
            }
            _ => {}
        }
    }
}

/// Thread-safe, lockless atomic Circuit Breaker managing upstream AI providers.
#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    config: CircuitBreakerConfig,
    providers: Arc<RwLock<HashMap<String, Arc<ProviderCircuit>>>>,
}

impl CircuitBreaker {
    /// Create a new `CircuitBreaker` with the given configuration.
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            config,
            providers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Access configuration.
    pub fn config(&self) -> &CircuitBreakerConfig {
        &self.config
    }

    fn get_or_create(&self, provider_id: &str) -> Arc<ProviderCircuit> {
        if let Ok(map) = self.providers.read() {
            if let Some(p) = map.get(provider_id) {
                return Arc::clone(p);
            }
        }
        let mut map = self.providers.write().unwrap_or_else(|e| e.into_inner());
        map.entry(provider_id.to_string())
            .or_insert_with(|| Arc::new(ProviderCircuit::new()))
            .clone()
    }

    /// Returns `true` if a request to `provider_id` is permitted.
    pub fn allow_request(&self, provider_id: &str) -> bool {
        let p = self.get_or_create(provider_id);
        p.allow_request(&self.config)
    }

    /// Record a successful request outcome for `provider_id`.
    pub fn record_success(&self, provider_id: &str) {
        let p = self.get_or_create(provider_id);
        p.record_success(&self.config);
    }

    /// Record a successful *stream completion* for `provider_id` without
    /// releasing the in-flight probe slot. The streaming path releases the
    /// slot at the commit point; the deferred success here only advances
    /// HalfOpen recovery and must not clobber a newer probe's reservation.
    pub fn record_success_streamed(&self, provider_id: &str) {
        let p = self.get_or_create(provider_id);
        p.record_success_with(&self.config, false);
    }

    /// Record a failed request outcome for `provider_id`.
    pub fn record_failure(&self, provider_id: &str) {
        let p = self.get_or_create(provider_id);
        p.record_failure(&self.config);
    }

    /// Inspect the current `CircuitState` for `provider_id` without mutating state.
    pub fn state(&self, provider_id: &str) -> CircuitState {
        if let Ok(map) = self.providers.read() {
            if let Some(p) = map.get(provider_id) {
                return CircuitState::from(p.state.load(Ordering::Acquire));
            }
        }
        CircuitState::Closed
    }

    /// Consecutive failures tracked for `provider_id`.
    pub fn consecutive_failures(&self, provider_id: &str) -> u32 {
        if let Ok(map) = self.providers.read() {
            if let Some(p) = map.get(provider_id) {
                return p.consecutive_failures.load(Ordering::Acquire);
            }
        }
        0
    }

    /// Alias for consecutive failures (compatible with status reporting).
    pub fn failure_count(&self, provider_id: &str) -> u32 {
        self.consecutive_failures(provider_id)
    }

    /// Remaining cooldown seconds when `Open`. Returns `None` if not `Open`.
    pub fn remaining_cooldown_secs(&self, provider_id: &str) -> Option<u64> {
        if let Ok(map) = self.providers.read() {
            if let Some(p) = map.get(provider_id) {
                if p.state.load(Ordering::Acquire) == STATE_OPEN {
                    let now = now_secs();
                    let opened = p.opened_at_secs.load(Ordering::Acquire);
                    let cooldown_end = opened + self.config.timeout_seconds as i64;
                    return if cooldown_end > now {
                        Some((cooldown_end - now) as u64)
                    } else {
                        Some(0)
                    };
                }
            }
        }
        None
    }

    /// Release in-flight probe reservation (e.g. when a probe request in HalfOpen
    /// completes with a client-side non-qualifying 4xx error such as 400 or 404).
    pub fn release_probe(&self, provider_id: &str) {
        if let Ok(map) = self.providers.read() {
            if let Some(p) = map.get(provider_id) {
                p.release_probe();
            }
        }
    }

    /// Reset an individual provider to `Closed` with all counters zeroed.
    pub fn reset(&self, provider_id: &str) {
        if let Ok(map) = self.providers.read() {
            if let Some(p) = map.get(provider_id) {
                p.reset();
            }
        }
    }

    /// Reset all tracked providers.
    pub fn reset_all(&self) {
        if let Ok(map) = self.providers.read() {
            for p in map.values() {
                p.reset();
            }
        }
    }

    /// List all currently tracked provider IDs.
    pub fn providers(&self) -> Vec<String> {
        let map = self.providers.read().unwrap_or_else(|e| e.into_inner());
        map.keys().cloned().collect()
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new(CircuitBreakerConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_transitions_closed_to_open_after_consecutive_failures() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 3,
            success_threshold: 2,
            timeout_seconds: 10,
            error_rate_threshold: 0.6,
            min_requests: 5,
        });

        assert_eq!(cb.state("upstream-a"), CircuitState::Closed);
        assert!(cb.allow_request("upstream-a"));

        cb.record_failure("upstream-a");
        cb.record_failure("upstream-a");
        assert_eq!(cb.state("upstream-a"), CircuitState::Closed);

        cb.record_failure("upstream-a");
        assert_eq!(cb.state("upstream-a"), CircuitState::Open);
        assert!(!cb.allow_request("upstream-a"));
    }

    #[test]
    fn test_circuit_half_open_probe_success_closes_circuit() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 2,
            success_threshold: 2,
            timeout_seconds: 0, // Instant cooldown for test
            error_rate_threshold: 0.6,
            min_requests: 5,
        });

        cb.record_failure("upstream-b");
        cb.record_failure("upstream-b");
        assert_eq!(cb.state("upstream-b"), CircuitState::Open);

        // With 0 timeout, next call enters HalfOpen probe
        assert!(cb.allow_request("upstream-b"));
        assert_eq!(cb.state("upstream-b"), CircuitState::HalfOpen);

        cb.record_success("upstream-b");
        assert_eq!(cb.state("upstream-b"), CircuitState::HalfOpen);

        cb.record_success("upstream-b");
        assert_eq!(cb.state("upstream-b"), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_half_open_probe_failure_reopens_circuit() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 2,
            success_threshold: 2,
            timeout_seconds: 0,
            error_rate_threshold: 0.6,
            min_requests: 5,
        });

        cb.record_failure("upstream-c");
        cb.record_failure("upstream-c");
        assert_eq!(cb.state("upstream-c"), CircuitState::Open);

        // Enter HalfOpen probe
        assert!(cb.allow_request("upstream-c"));
        assert_eq!(cb.state("upstream-c"), CircuitState::HalfOpen);

        // Probe fails -> should immediately trip back to Open
        cb.record_failure("upstream-c");
        assert_eq!(cb.state("upstream-c"), CircuitState::Open);
        assert_eq!(cb.consecutive_failures("upstream-c"), 2);
    }

    #[test]
    fn test_circuit_half_open_rejects_concurrent_probes() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 2,
            success_threshold: 2,
            timeout_seconds: 0,
            error_rate_threshold: 0.6,
            min_requests: 5,
        });

        cb.record_failure("upstream-concurrent");
        cb.record_failure("upstream-concurrent");
        assert_eq!(cb.state("upstream-concurrent"), CircuitState::Open);

        // First probe allowed through -> enters HalfOpen
        assert!(cb.allow_request("upstream-concurrent"));
        assert_eq!(cb.state("upstream-concurrent"), CircuitState::HalfOpen);

        // Concurrent request while probe is in flight must be rejected
        assert!(!cb.allow_request("upstream-concurrent"));
        assert!(!cb.allow_request("upstream-concurrent"));

        // First probe succeeds
        cb.record_success("upstream-concurrent");
        assert_eq!(cb.state("upstream-concurrent"), CircuitState::HalfOpen);

        // Now next probe is allowed
        assert!(cb.allow_request("upstream-concurrent"));
        // Concurrent again rejected
        assert!(!cb.allow_request("upstream-concurrent"));

        // Second probe succeeds -> circuit closes
        cb.record_success("upstream-concurrent");
        assert_eq!(cb.state("upstream-concurrent"), CircuitState::Closed);

        // Closed circuit allows requests
        assert!(cb.allow_request("upstream-concurrent"));
    }

    #[test]
    fn test_circuit_error_rate_threshold_trips_to_open() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 10, // high consecutive threshold
            success_threshold: 2,
            timeout_seconds: 60,
            error_rate_threshold: 0.5, // 50% error rate
            min_requests: 5,
        });

        // Alternating: fail, succ, fail, succ (4 requests, 2 fails = 50%, but requests < min_requests 5)
        cb.record_failure("upstream-rate");
        cb.record_success("upstream-rate");
        cb.record_failure("upstream-rate");
        cb.record_success("upstream-rate");
        assert_eq!(cb.state("upstream-rate"), CircuitState::Closed);

        // 5th request: fail (3 fails / 5 requests = 60% >= 50%)
        cb.record_failure("upstream-rate");
        assert_eq!(cb.state("upstream-rate"), CircuitState::Open);
        assert!(!cb.allow_request("upstream-rate"));
    }

    #[test]
    fn test_success_in_closed_resets_consecutive_failures() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 3,
            success_threshold: 2,
            timeout_seconds: 60,
            error_rate_threshold: 0.9,
            min_requests: 10,
        });

        cb.record_failure("upstream-d");
        cb.record_failure("upstream-d");
        assert_eq!(cb.consecutive_failures("upstream-d"), 2);

        // Success resets consecutive failures counter
        cb.record_success("upstream-d");
        assert_eq!(cb.consecutive_failures("upstream-d"), 0);

        // 2 more failures do not trip because consecutive counter was reset
        cb.record_failure("upstream-d");
        cb.record_failure("upstream-d");
        assert_eq!(cb.state("upstream-d"), CircuitState::Closed);
    }

    #[test]
    fn test_provider_isolation() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 2,
            success_threshold: 2,
            timeout_seconds: 60,
            error_rate_threshold: 0.5,
            min_requests: 5,
        });

        cb.record_failure("provider-1");
        cb.record_failure("provider-1");
        assert_eq!(cb.state("provider-1"), CircuitState::Open);

        // provider-2 should remain unaffected
        assert_eq!(cb.state("provider-2"), CircuitState::Closed);
        assert!(cb.allow_request("provider-2"));
    }

    #[test]
    fn test_serde_and_display() {
        assert_eq!(CircuitState::Closed.to_string(), "closed");
        assert_eq!(CircuitState::Open.to_string(), "open");
        assert_eq!(CircuitState::HalfOpen.to_string(), "half-open");

        let serialized = serde_json::to_string(&CircuitState::HalfOpen).unwrap();
        assert_eq!(serialized, "\"HalfOpen\"");
        let deserialized: CircuitState = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, CircuitState::HalfOpen);

        let config = CircuitBreakerConfig::default();
        let config_json = serde_json::to_string(&config).unwrap();
        let parsed_config: CircuitBreakerConfig = serde_json::from_str(&config_json).unwrap();
        assert_eq!(parsed_config, config);
    }

    #[test]
    fn test_remaining_cooldown_and_reset() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 1,
            success_threshold: 1,
            timeout_seconds: 30,
            error_rate_threshold: 0.5,
            min_requests: 5,
        });

        assert_eq!(cb.remaining_cooldown_secs("p1"), None);

        cb.record_failure("p1");
        assert_eq!(cb.state("p1"), CircuitState::Open);

        let rem = cb.remaining_cooldown_secs("p1");
        assert!(rem.is_some());
        assert!(rem.unwrap() <= 30);

        cb.reset("p1");
        assert_eq!(cb.state("p1"), CircuitState::Closed);
        assert_eq!(cb.remaining_cooldown_secs("p1"), None);
    }

    #[test]
    fn test_circuit_competing_threads_at_timeout_admit_only_one_probe() {
        use std::sync::atomic::AtomicUsize;
        use std::thread;

        let cb = Arc::new(CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 2,
            success_threshold: 2,
            timeout_seconds: 0, // instant cooldown
            error_rate_threshold: 0.6,
            min_requests: 5,
        }));

        cb.record_failure("competing-upstream");
        cb.record_failure("competing-upstream");
        assert_eq!(cb.state("competing-upstream"), CircuitState::Open);

        // Spawn multiple concurrent threads competing to call allow_request at the exact same instant
        let num_threads = 20;
        let barrier = Arc::new(std::sync::Barrier::new(num_threads));
        let admitted_count = Arc::new(AtomicUsize::new(0));

        let handles: Vec<_> = (0..num_threads)
            .map(|_| {
                let cb_clone = Arc::clone(&cb);
                let barrier_clone = Arc::clone(&barrier);
                let admitted_clone = Arc::clone(&admitted_count);
                thread::spawn(move || {
                    barrier_clone.wait();
                    if cb_clone.allow_request("competing-upstream") {
                        admitted_clone.fetch_add(1, Ordering::SeqCst);
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        // Exactly one thread should have won the probe reservation
        assert_eq!(
            admitted_count.load(Ordering::SeqCst),
            1,
            "Only 1 probe request must be admitted when multiple threads race at timeout"
        );
        assert_eq!(cb.state("competing-upstream"), CircuitState::HalfOpen);
    }

    #[test]
    fn test_circuit_closed_periodic_window_reset_prevents_dilution() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 10,
            success_threshold: 2,
            timeout_seconds: 60,
            error_rate_threshold: 0.5,
            min_requests: 5,
        });

        // Simulate 50 successful requests (hitting min_requests * 10 window reset)
        for _ in 0..50 {
            cb.record_success("p-window");
        }
        assert_eq!(cb.state("p-window"), CircuitState::Closed);

        // In the fresh window, 3 failures out of 5 requests trip the breaker
        cb.record_failure("p-window");
        cb.record_success("p-window");
        cb.record_failure("p-window");
        cb.record_success("p-window");
        cb.record_failure("p-window");

        // 3 failures / 5 requests = 60% >= 50% -> should trip to Open
        assert_eq!(cb.state("p-window"), CircuitState::Open);
    }

    #[test]
    fn test_release_probe_frees_in_flight_slot_in_half_open() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 1,
            success_threshold: 2,
            timeout_seconds: 0,
            error_rate_threshold: 0.5,
            min_requests: 1,
        });

        cb.record_failure("p-probe");
        assert_eq!(cb.state("p-probe"), CircuitState::Open);

        // Allow probe enters HalfOpen and sets probe_in_flight = true
        assert!(cb.allow_request("p-probe"));
        assert_eq!(cb.state("p-probe"), CircuitState::HalfOpen);

        // Concurrent probe rejected
        assert!(!cb.allow_request("p-probe"));

        // Release probe reservation
        cb.release_probe("p-probe");

        // Next probe is now admitted
        assert!(cb.allow_request("p-probe"));
    }

    #[test]
    fn test_record_success_streamed_advances_recovery_without_releasing_slot() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 1,
            success_threshold: 2,
            timeout_seconds: 0,
            error_rate_threshold: 0.5,
            min_requests: 1,
        });

        cb.record_failure("p-streamed");
        assert_eq!(cb.state("p-streamed"), CircuitState::Open);

        // Probe admitted -> HalfOpen, slot held.
        assert!(cb.allow_request("p-streamed"));
        assert_eq!(cb.state("p-streamed"), CircuitState::HalfOpen);

        // A deferred streamed success advances the recovery counter but must
        // NOT release the slot (a newer probe may have claimed it).
        cb.record_success_streamed("p-streamed");
        assert_eq!(cb.state("p-streamed"), CircuitState::HalfOpen);
        assert!(
            !cb.allow_request("p-streamed"),
            "streamed success must not release the in-flight probe slot"
        );

        // The slot is still owned; release it explicitly to simulate the next
        // probe being handed off, then a second streamed success hits the
        // threshold and recovers to Closed.
        cb.release_probe("p-streamed");
        cb.record_success_streamed("p-streamed");
        assert_eq!(cb.state("p-streamed"), CircuitState::Closed);
    }
}

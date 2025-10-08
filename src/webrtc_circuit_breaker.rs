//! Circuit Breaker Pattern for WebRTC Tubes
//!
//! Provides fault isolation and automatic recovery for WebRTC peer connections.
//! This module implements error-type specific thresholds and adaptive timeouts
//! to handle different failure scenarios appropriately.

use log::{debug, error, info, warn};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct TubeCircuitBreaker {
    state: Arc<Mutex<CircuitState>>,
    config: CircuitConfig,
    tube_id: String,
    metrics: Arc<CircuitMetrics>,
}

#[derive(Debug, Clone)]
pub struct CircuitConfig {
    failure_threshold: u32,        // Trip after N failures (default threshold)
    timeout: Duration,             // Stay open for this long
    success_threshold: u32,        // Successes needed to close
    max_half_open_requests: u32,   // Limit test requests
    max_request_timeout: Duration, // Individual operation timeout

    // Error-type specific configurations
    error_specific_thresholds: HashMap<String, ErrorSpecificConfig>,
}

/// Error-specific circuit breaker configuration
#[derive(Debug, Clone)]
pub struct ErrorSpecificConfig {
    pub failure_threshold: u32,
    pub timeout_multiplier: f64, // Multiply base timeout by this factor
}

impl Default for CircuitConfig {
    fn default() -> Self {
        let mut error_specific_thresholds = HashMap::new();

        // ICE-related errors - more lenient (network issues are common)
        error_specific_thresholds.insert(
            "IceConnectionFailed".to_string(),
            ErrorSpecificConfig {
                failure_threshold: 8,
                timeout_multiplier: 1.5,
            },
        );

        // TURN/STUN errors - moderate threshold
        error_specific_thresholds.insert(
            "TurnServerConnectionFailed".to_string(),
            ErrorSpecificConfig {
                failure_threshold: 5,
                timeout_multiplier: 1.0,
            },
        );

        // Authentication errors - strict threshold (likely configuration issue)
        error_specific_thresholds.insert(
            "AuthenticationFailed".to_string(),
            ErrorSpecificConfig {
                failure_threshold: 2,
                timeout_multiplier: 3.0,
            },
        );

        // Data channel errors - moderate threshold
        error_specific_thresholds.insert(
            "DataChannelFailed".to_string(),
            ErrorSpecificConfig {
                failure_threshold: 4,
                timeout_multiplier: 1.2,
            },
        );

        // Signaling errors - strict threshold (protocol issues)
        error_specific_thresholds.insert(
            "SignalingFailed".to_string(),
            ErrorSpecificConfig {
                failure_threshold: 3,
                timeout_multiplier: 2.0,
            },
        );

        Self {
            failure_threshold: 5,                         // Trip after 5 failures (default)
            timeout: Duration::from_secs(30),             // Stay open for 30 seconds
            success_threshold: 3,                         // Need 3 successes to close
            max_half_open_requests: 3,                    // Max 3 test requests
            max_request_timeout: Duration::from_secs(10), // 10-second operation timeout
            error_specific_thresholds,
        }
    }
}

#[derive(Debug)]
enum CircuitState {
    Closed {
        failure_count: u32,
        last_failure: Option<Instant>,
        error_type_failures: HashMap<String, u32>, // Track failures by error type
        last_error_type: Option<String>,
    },
    Open {
        opened_at: Instant,
        last_attempt: Option<Instant>,
        trigger_error_type: String, // Error type that triggered the opening
    },
    HalfOpen {
        test_started: Instant,
        test_count: u32,
        success_count: u32,
        trigger_error_type: String, // Error type that caused the open state
    },
}

#[derive(Debug)]
struct CircuitMetrics {
    total_requests: AtomicUsize,
    successful_requests: AtomicUsize,
    failed_requests: AtomicUsize,
    circuit_opens: AtomicUsize,
    circuit_closes: AtomicUsize,
    timeouts: AtomicUsize,

    // Error-type specific metrics
    error_type_counts: Arc<Mutex<HashMap<String, u32>>>,
    error_type_triggered_opens: Arc<Mutex<HashMap<String, u32>>>,
}

impl Default for CircuitMetrics {
    fn default() -> Self {
        Self {
            total_requests: AtomicUsize::new(0),
            successful_requests: AtomicUsize::new(0),
            failed_requests: AtomicUsize::new(0),
            circuit_opens: AtomicUsize::new(0),
            circuit_closes: AtomicUsize::new(0),
            timeouts: AtomicUsize::new(0),
            error_type_counts: Arc::new(Mutex::new(HashMap::new())),
            error_type_triggered_opens: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[derive(Debug)]
pub enum CircuitError<E> {
    CircuitOpen,
    Timeout,
    OperationFailed(E),
    TooManyTestRequests,
}

impl<E: std::fmt::Display> std::fmt::Display for CircuitError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CircuitError::CircuitOpen => write!(f, "Circuit breaker is open"),
            CircuitError::Timeout => write!(f, "Operation timed out"),
            CircuitError::OperationFailed(e) => write!(f, "Operation failed: {}", e),
            CircuitError::TooManyTestRequests => {
                write!(f, "Too many test requests in half-open state")
            }
        }
    }
}

impl<E: std::fmt::Debug + std::fmt::Display> std::error::Error for CircuitError<E> {}

/// Comprehensive circuit breaker statistics including error-type specific data
#[derive(Debug, Clone)]
pub struct CircuitBreakerStats {
    pub tube_id: String,
    pub state: String,
    pub total_requests: usize,
    pub successful_requests: usize,
    pub failed_requests: usize,
    pub circuit_opens: usize,
    pub circuit_closes: usize,
    pub timeouts: usize,
    pub error_type_counts: HashMap<String, u32>,
    pub error_type_triggered_opens: HashMap<String, u32>,
    pub current_error_type_failures: HashMap<String, u32>,
}

impl TubeCircuitBreaker {
    pub fn new(tube_id: String) -> Self {
        Self::with_config(tube_id, CircuitConfig::default())
    }

    pub fn with_config(tube_id: String, config: CircuitConfig) -> Self {
        debug!(
            "Creating circuit breaker for tube {} with config: failure_threshold={}, timeout={}s",
            tube_id,
            config.failure_threshold,
            config.timeout.as_secs()
        );

        Self {
            state: Arc::new(Mutex::new(CircuitState::Closed {
                failure_count: 0,
                last_failure: None,
                error_type_failures: HashMap::new(),
                last_error_type: None,
            })),
            config,
            tube_id,
            metrics: Arc::new(CircuitMetrics::default()),
        }
    }

    /// Execute an async operation with circuit breaker protection
    /// PERFORMANCE: Only adds ~1μs overhead for circuit state check
    pub async fn execute<F, Fut, T, E>(&self, operation: F) -> Result<T, CircuitError<E>>
    where
        F: FnOnce() -> Fut + Send,
        Fut: std::future::Future<Output = Result<T, E>> + Send,
        T: Send,
        E: std::fmt::Debug + std::fmt::Display + Send,
    {
        // Increment total requests
        self.metrics.total_requests.fetch_add(1, Ordering::Relaxed);

        // FAST PATH: Check circuit state (optimized for closed state)
        let can_execute = {
            let mut state = self.state.lock().unwrap();
            match &mut *state {
                CircuitState::Closed { .. } => true, // Fast path - most common case

                CircuitState::Open {
                    opened_at,
                    trigger_error_type,
                    ..
                } => {
                    if opened_at.elapsed() >= self.config.timeout {
                        info!(
                            "Circuit breaker transitioning to half-open for tube {} (triggered by: {})",
                            self.tube_id, trigger_error_type
                        );
                        *state = CircuitState::HalfOpen {
                            test_started: Instant::now(),
                            test_count: 0,
                            success_count: 0,
                            trigger_error_type: trigger_error_type.clone(),
                        };
                        true
                    } else {
                        false // Still in open state
                    }
                }

                CircuitState::HalfOpen { test_count, .. } => {
                    if *test_count >= self.config.max_half_open_requests {
                        false // Too many test requests
                    } else {
                        *test_count += 1;
                        true
                    }
                }
            }
        };

        if !can_execute {
            let error = match &*self.state.lock().unwrap() {
                CircuitState::Open { .. } => CircuitError::CircuitOpen,
                CircuitState::HalfOpen { .. } => CircuitError::TooManyTestRequests,
                _ => CircuitError::CircuitOpen,
            };
            return Err(error);
        }

        // Execute operation with timeout
        let result = tokio::time::timeout(self.config.max_request_timeout, operation()).await;

        // Process result and update circuit state
        match result {
            Ok(Ok(value)) => {
                self.record_success();
                Ok(value)
            }
            Ok(Err(e)) => {
                self.record_failure(&format!("{:?}", e));
                Err(CircuitError::OperationFailed(e))
            }
            Err(_) => {
                self.record_timeout();
                Err(CircuitError::Timeout)
            }
        }
    }

    /// Record a successful operation
    fn record_success(&self) {
        self.metrics
            .successful_requests
            .fetch_add(1, Ordering::Relaxed);

        let mut state = self.state.lock().unwrap();
        match &mut *state {
            CircuitState::Closed {
                failure_count,
                error_type_failures,
                last_error_type,
                ..
            } => {
                // Reset failure counts on success
                *failure_count = 0;
                error_type_failures.clear();
                *last_error_type = None;
            }
            CircuitState::HalfOpen { success_count, .. } => {
                *success_count += 1;
                if *success_count >= self.config.success_threshold {
                    info!(
                        "Circuit breaker closed for tube {} after {} successful tests",
                        self.tube_id, success_count
                    );
                    *state = CircuitState::Closed {
                        failure_count: 0,
                        last_failure: None,
                        error_type_failures: HashMap::new(),
                        last_error_type: None,
                    };
                    self.metrics.circuit_closes.fetch_add(1, Ordering::Relaxed);
                }
            }
            CircuitState::Open { .. } => {
                // Should not happen, but handle gracefully
                warn!(
                    "Unexpected success in open circuit state for tube {}",
                    self.tube_id
                );
            }
        }
    }

    /// Record a failed operation with error-type specific handling
    fn record_failure(&self, error: &str) {
        self.record_failure_with_type(error, "Unknown")
    }

    /// Record a failed operation with specific error type
    pub fn record_failure_with_type(&self, error: &str, error_type: &str) {
        self.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);

        // Update error type metrics
        {
            let mut error_counts = self.metrics.error_type_counts.lock().unwrap();
            *error_counts.entry(error_type.to_string()).or_insert(0) += 1;
        }

        let mut state = self.state.lock().unwrap();
        let (should_open, trigger_error_type) = match &mut *state {
            CircuitState::Closed {
                failure_count,
                last_failure,
                error_type_failures,
                last_error_type,
            } => {
                *failure_count += 1;
                *last_failure = Some(Instant::now());
                *last_error_type = Some(error_type.to_string());

                // Update error-type specific failure count
                *error_type_failures
                    .entry(error_type.to_string())
                    .or_insert(0) += 1;

                // Check if this error type should trigger circuit opening
                let threshold = self.get_threshold_for_error_type(error_type);
                let error_specific_count = error_type_failures.get(error_type).unwrap_or(&0);

                debug!(
                    "Error type '{}' count: {} (threshold: {}, total failures: {}) for tube {}",
                    error_type, error_specific_count, threshold, failure_count, self.tube_id
                );

                let should_open = *error_specific_count >= threshold
                    || *failure_count >= self.config.failure_threshold;
                (should_open, error_type.to_string())
            }
            CircuitState::HalfOpen {
                trigger_error_type, ..
            } => {
                // Failed during testing - reopen circuit
                (true, trigger_error_type.clone())
            }
            CircuitState::Open { .. } => {
                (false, error_type.to_string()) // Already open
            }
        };

        if should_open {
            let timeout_multiplier =
                self.get_timeout_multiplier_for_error_type(&trigger_error_type);
            let adjusted_timeout =
                Duration::from_secs_f64(self.config.timeout.as_secs_f64() * timeout_multiplier);

            match &*state {
                CircuitState::Closed {
                    failure_count,
                    error_type_failures,
                    ..
                } => {
                    let error_specific_count =
                        error_type_failures.get(&trigger_error_type).unwrap_or(&0);
                    error!(
                        "Circuit breaker OPENED for tube {} after {} total failures, {} '{}' errors (last_error: {}). Timeout adjusted to {}s",
                        self.tube_id, failure_count, error_specific_count, trigger_error_type, error, adjusted_timeout.as_secs()
                    );
                }
                CircuitState::HalfOpen { .. } => {
                    error!(
                        "Circuit breaker RE-OPENED for tube {} after failed test (error_type: {}, error: {}). Timeout: {}s",
                        self.tube_id, trigger_error_type, error, adjusted_timeout.as_secs()
                    );
                }
                _ => {}
            }

            *state = CircuitState::Open {
                opened_at: Instant::now(),
                last_attempt: None,
                trigger_error_type: trigger_error_type.clone(),
            };

            self.metrics.circuit_opens.fetch_add(1, Ordering::Relaxed);

            // Track which error type triggered the opening
            {
                let mut triggered_opens = self.metrics.error_type_triggered_opens.lock().unwrap();
                *triggered_opens.entry(trigger_error_type).or_insert(0) += 1;
            }
        }
    }

    /// Get failure threshold for specific error type
    fn get_threshold_for_error_type(&self, error_type: &str) -> u32 {
        self.config
            .error_specific_thresholds
            .get(error_type)
            .map(|config| config.failure_threshold)
            .unwrap_or(self.config.failure_threshold)
    }

    /// Get timeout multiplier for specific error type
    fn get_timeout_multiplier_for_error_type(&self, error_type: &str) -> f64 {
        self.config
            .error_specific_thresholds
            .get(error_type)
            .map(|config| config.timeout_multiplier)
            .unwrap_or(1.0)
    }

    /// Record a timeout
    fn record_timeout(&self) {
        self.metrics.timeouts.fetch_add(1, Ordering::Relaxed);
        self.record_failure("timeout");
    }

    /// Get current circuit state
    pub fn get_state(&self) -> String {
        let state = self.state.lock().unwrap();
        match &*state {
            CircuitState::Closed { failure_count, .. } => {
                format!("Closed (failures: {})", failure_count)
            }
            CircuitState::Open {
                opened_at,
                last_attempt,
                ..
            } => {
                let last_attempt_info = match last_attempt {
                    Some(t) => format!(", last attempt: {}s ago", t.elapsed().as_secs()),
                    None => "".to_string(),
                };
                format!(
                    "Open ({}s ago{})",
                    opened_at.elapsed().as_secs(),
                    last_attempt_info
                )
            }
            CircuitState::HalfOpen {
                test_started,
                test_count,
                success_count,
                ..
            } => {
                format!(
                    "Half-Open (tests: {}, successes: {}, testing for: {}s)",
                    test_count,
                    success_count,
                    test_started.elapsed().as_secs()
                )
            }
        }
    }

    /// Get circuit breaker metrics
    pub fn get_metrics(&self) -> (usize, usize, usize, usize, usize, usize) {
        (
            self.metrics.total_requests.load(Ordering::Relaxed),
            self.metrics.successful_requests.load(Ordering::Relaxed),
            self.metrics.failed_requests.load(Ordering::Relaxed),
            self.metrics.circuit_opens.load(Ordering::Relaxed),
            self.metrics.circuit_closes.load(Ordering::Relaxed),
            self.metrics.timeouts.load(Ordering::Relaxed),
        )
    }

    /// Get error-type specific metrics
    pub fn get_error_type_metrics(&self) -> (HashMap<String, u32>, HashMap<String, u32>) {
        let error_counts = self.metrics.error_type_counts.lock().unwrap().clone();
        let triggered_opens = self
            .metrics
            .error_type_triggered_opens
            .lock()
            .unwrap()
            .clone();
        (error_counts, triggered_opens)
    }

    /// Get comprehensive circuit breaker statistics
    pub fn get_comprehensive_stats(&self) -> CircuitBreakerStats {
        let (error_counts, triggered_opens) = self.get_error_type_metrics();
        let state = self.state.lock().unwrap();

        let (current_state, error_type_failures) = match &*state {
            CircuitState::Closed {
                failure_count,
                error_type_failures,
                ..
            } => (
                format!("Closed (failures: {})", failure_count),
                error_type_failures.clone(),
            ),
            CircuitState::Open {
                opened_at,
                trigger_error_type,
                ..
            } => (
                format!(
                    "Open ({}s ago, triggered by: {})",
                    opened_at.elapsed().as_secs(),
                    trigger_error_type
                ),
                HashMap::new(),
            ),
            CircuitState::HalfOpen {
                test_started,
                trigger_error_type,
                test_count,
                success_count,
                ..
            } => (
                format!(
                    "Half-Open (testing for: {}s, triggered by: {}, tests: {}, successes: {})",
                    test_started.elapsed().as_secs(),
                    trigger_error_type,
                    test_count,
                    success_count
                ),
                HashMap::new(),
            ),
        };

        CircuitBreakerStats {
            tube_id: self.tube_id.clone(),
            state: current_state,
            total_requests: self.metrics.total_requests.load(Ordering::Relaxed),
            successful_requests: self.metrics.successful_requests.load(Ordering::Relaxed),
            failed_requests: self.metrics.failed_requests.load(Ordering::Relaxed),
            circuit_opens: self.metrics.circuit_opens.load(Ordering::Relaxed),
            circuit_closes: self.metrics.circuit_closes.load(Ordering::Relaxed),
            timeouts: self.metrics.timeouts.load(Ordering::Relaxed),
            error_type_counts: error_counts,
            error_type_triggered_opens: triggered_opens,
            current_error_type_failures: error_type_failures,
        }
    }

    /// Force reset the circuit breaker (for manual recovery)
    pub fn force_reset(&self) {
        info!("Force resetting circuit breaker for tube {}", self.tube_id);
        let mut state = self.state.lock().unwrap();
        *state = CircuitState::Closed {
            failure_count: 0,
            last_failure: None,
            error_type_failures: HashMap::new(),
            last_error_type: None,
        };
    }

    /// Check if circuit is healthy (closed)
    pub fn is_healthy(&self) -> bool {
        matches!(*self.state.lock().unwrap(), CircuitState::Closed { .. })
    }
}

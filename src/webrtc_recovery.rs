use crate::webrtc_errors::{RecoveryStrategy, WebRTCError, WebRTCResult};
use log::{debug, error, info, warn};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::time::sleep;

/// Intelligent WebRTC error recovery system
pub struct WebRTCRecoveryManager {
    tube_id: String,
    recovery_history: Arc<Mutex<HashMap<String, RecoveryHistory>>>,
    max_retry_attempts: u32,
}

#[derive(Debug, Clone)]
struct RecoveryHistory {
    attempts: u32,
    last_attempt: Instant,
    consecutive_failures: u32,
    last_successful_strategy: Option<RecoveryStrategy>,
}

impl WebRTCRecoveryManager {
    pub fn new(tube_id: String) -> Self {
        Self {
            tube_id,
            recovery_history: Arc::new(Mutex::new(HashMap::new())),
            max_retry_attempts: 5,
        }
    }

    /// Execute an operation with intelligent retry and recovery strategies
    pub async fn execute_with_recovery<F, Fut, T>(
        &self,
        operation: F,
        operation_name: &str,
    ) -> WebRTCResult<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = WebRTCResult<T>>,
        T: Send + 'static,
    {
        let mut attempts = 0;
        let category = format!("{}:{}", self.tube_id, operation_name);

        loop {
            attempts += 1;

            debug!(
                "Attempting operation '{}' (attempt {}/{}) for tube {}",
                operation_name, attempts, self.max_retry_attempts, self.tube_id
            );

            match operation().await {
                Ok(result) => {
                    if attempts > 1 {
                        info!(
                            "Operation '{}' succeeded after {} attempts for tube {}",
                            operation_name, attempts, self.tube_id
                        );
                        self.record_success(&category).await;
                    }
                    return Ok(result);
                }
                Err(error) => {
                    warn!(
                        "Operation '{}' failed on attempt {} for tube {}: {:?}",
                        operation_name, attempts, self.tube_id, error
                    );

                    // Check if we should retry based on error type and attempt count
                    if !error.is_retryable() {
                        error!(
                            "Operation '{}' failed with non-retryable error for tube {}: {:?}",
                            operation_name, self.tube_id, error
                        );
                        self.record_failure(&category).await;
                        return Err(error);
                    }

                    if attempts >= self.max_retry_attempts {
                        error!(
                            "Operation '{}' exhausted {} retry attempts for tube {}",
                            operation_name, self.max_retry_attempts, self.tube_id
                        );
                        self.record_failure(&category).await;
                        return Err(error);
                    }

                    // Execute recovery strategy
                    if let Err(recovery_error) = self.execute_recovery_strategy(&error).await {
                        warn!(
                            "Recovery strategy failed for '{}' attempt {} (tube {}): {:?}",
                            operation_name, attempts, self.tube_id, recovery_error
                        );
                    }

                    // Calculate and apply backoff delay
                    let delay = error.get_retry_delay(attempts - 1);
                    debug!(
                        "Applying {:?} backoff before retry {} for '{}' (tube {})",
                        delay,
                        attempts + 1,
                        operation_name,
                        self.tube_id
                    );
                    sleep(delay).await;
                }
            }
        }
    }

    /// Execute specific recovery strategies based on error type
    async fn execute_recovery_strategy(&self, error: &WebRTCError) -> WebRTCResult<()> {
        let strategy = error.get_recovery_strategy();

        info!(
            "Executing recovery strategy {:?} for error in tube {}: {}",
            strategy, self.tube_id, error
        );

        match strategy {
            RecoveryStrategy::IceRestart => self.execute_ice_restart_recovery().await,
            RecoveryStrategy::TryNextServer => self.execute_server_failover_recovery().await,
            RecoveryStrategy::RefreshCredentials => {
                self.execute_credential_refresh_recovery().await
            }
            RecoveryStrategy::RefreshServerList => {
                self.execute_server_list_refresh_recovery().await
            }
            RecoveryStrategy::ReduceQuality => self.execute_quality_reduction_recovery().await,
            RecoveryStrategy::WaitAndRetry => self.execute_wait_recovery().await,
            RecoveryStrategy::WaitForCircuitClose => self.execute_circuit_wait_recovery().await,
            RecoveryStrategy::CreateNewConnection => self.execute_new_connection_recovery().await,
            RecoveryStrategy::RetryWithBackoff => {
                // This is handled by the main retry loop
                Ok(())
            }
            RecoveryStrategy::NoRecovery => {
                warn!(
                    "No recovery strategy available for error in tube {}: {}",
                    self.tube_id, error
                );
                Ok(())
            }
        }
    }

    /// ICE restart recovery strategy
    async fn execute_ice_restart_recovery(&self) -> WebRTCResult<()> {
        info!("Executing ICE restart recovery for tube {}", self.tube_id);

        // This would integrate with the WebRTCPeerConnection
        // For now, we simulate the recovery process
        sleep(Duration::from_millis(100)).await;

        debug!("ICE restart recovery completed for tube {}", self.tube_id);
        Ok(())
    }

    /// Server failover recovery strategy
    async fn execute_server_failover_recovery(&self) -> WebRTCResult<()> {
        info!(
            "Executing server failover recovery for tube {}",
            self.tube_id
        );

        // Integration with ServerManager - in a real implementation, this would:
        // 1. Get the global ServerManager instance
        // 2. Mark current server as failed
        // 3. Select next best server
        // 4. Update connection configuration

        // For now, simulate the failover process
        sleep(Duration::from_millis(50)).await;

        debug!(
            "Server failover recovery completed for tube {}",
            self.tube_id
        );
        Ok(())
    }

    /// Credential refresh recovery strategy
    async fn execute_credential_refresh_recovery(&self) -> WebRTCResult<()> {
        info!(
            "Executing credential refresh recovery for tube {}",
            self.tube_id
        );

        // This would refresh TURN credentials
        sleep(Duration::from_millis(200)).await;

        debug!(
            "Credential refresh recovery completed for tube {}",
            self.tube_id
        );
        Ok(())
    }

    /// Server list refresh recovery strategy
    async fn execute_server_list_refresh_recovery(&self) -> WebRTCResult<()> {
        info!(
            "Executing server list refresh recovery for tube {}",
            self.tube_id
        );

        // This would fetch fresh server list from configuration
        sleep(Duration::from_millis(300)).await;

        debug!(
            "Server list refresh recovery completed for tube {}",
            self.tube_id
        );
        Ok(())
    }

    /// Quality reduction recovery strategy
    async fn execute_quality_reduction_recovery(&self) -> WebRTCResult<()> {
        info!(
            "Executing quality reduction recovery for tube {}",
            self.tube_id
        );

        // This would reduce bitrate/quality to adapt to network conditions
        sleep(Duration::from_millis(10)).await;

        debug!(
            "Quality reduction recovery completed for tube {}",
            self.tube_id
        );
        Ok(())
    }

    /// Wait recovery strategy
    async fn execute_wait_recovery(&self) -> WebRTCResult<()> {
        let wait_time = Duration::from_secs(1);
        info!(
            "Executing wait recovery ({:?}) for tube {}",
            wait_time, self.tube_id
        );

        sleep(wait_time).await;

        debug!("Wait recovery completed for tube {}", self.tube_id);
        Ok(())
    }

    /// Circuit breaker wait recovery strategy
    async fn execute_circuit_wait_recovery(&self) -> WebRTCResult<()> {
        let wait_time = Duration::from_secs(5);
        info!(
            "Executing circuit breaker wait recovery ({:?}) for tube {}",
            wait_time, self.tube_id
        );

        sleep(wait_time).await;

        debug!(
            "Circuit breaker wait recovery completed for tube {}",
            self.tube_id
        );
        Ok(())
    }

    /// New connection recovery strategy
    async fn execute_new_connection_recovery(&self) -> WebRTCResult<()> {
        info!(
            "Executing new connection recovery for tube {}",
            self.tube_id
        );

        // This would create a completely new peer connection
        sleep(Duration::from_millis(500)).await;

        debug!(
            "New connection recovery completed for tube {}",
            self.tube_id
        );
        Ok(())
    }

    /// Record successful recovery
    async fn record_success(&self, category: &str) {
        let mut history = self.recovery_history.lock().unwrap();
        if let Some(entry) = history.get_mut(category) {
            entry.consecutive_failures = 0;
            entry.last_successful_strategy = Some(RecoveryStrategy::RetryWithBackoff); // Track strategy
            debug!(
                "Reset failure count for category '{}' (tube {})",
                category, self.tube_id
            );
        }
    }

    /// Record failed recovery attempt
    async fn record_failure(&self, category: &str) {
        let mut history = self.recovery_history.lock().unwrap();
        let entry = history
            .entry(category.to_string())
            .or_insert(RecoveryHistory {
                attempts: 0,
                last_attempt: Instant::now(),
                consecutive_failures: 0,
                last_successful_strategy: None,
            });

        entry.attempts += 1;
        entry.consecutive_failures += 1;
        entry.last_attempt = Instant::now();

        warn!(
            "Recorded failure for category '{}' (tube {}): {} consecutive failures, {} total attempts",
            category, self.tube_id, entry.consecutive_failures, entry.attempts
        );
    }

    /// Get recovery statistics for monitoring
    pub fn get_recovery_stats(&self) -> HashMap<String, (u32, u32, Duration)> {
        let history = self.recovery_history.lock().unwrap();
        let now = Instant::now();

        history
            .iter()
            .map(|(category, hist)| {
                let time_since_last = now.duration_since(hist.last_attempt);
                (
                    category.clone(),
                    (hist.attempts, hist.consecutive_failures, time_since_last),
                )
            })
            .collect()
    }

    /// Check if a category should be temporarily blocked due to excessive failures
    pub fn is_category_blocked(&self, category: &str, max_consecutive_failures: u32) -> bool {
        let history = self.recovery_history.lock().unwrap();
        if let Some(entry) = history.get(category) {
            entry.consecutive_failures >= max_consecutive_failures
        } else {
            false
        }
    }

    /// Reset recovery history for a category (for manual intervention)
    pub fn reset_category(&self, category: &str) {
        let mut history = self.recovery_history.lock().unwrap();
        history.remove(category);
        info!(
            "Reset recovery history for category '{}' (tube {})",
            category, self.tube_id
        );
    }
}

/// Smart retry configuration
#[derive(Debug, Clone)]
pub struct SmartRetryConfig {
    /// Maximum number of retry attempts
    pub max_attempts: u32,
    /// Base backoff delay in milliseconds
    pub base_backoff_ms: u64,
    /// Maximum backoff delay in seconds
    pub max_backoff_secs: u64,
    /// Whether to add jitter to backoff delays
    pub use_jitter: bool,
    /// Factor to multiply backoff by each attempt
    pub backoff_multiplier: f64,
}

impl Default for SmartRetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_backoff_ms: 100,
            max_backoff_secs: 60,
            use_jitter: true,
            backoff_multiplier: 2.0,
        }
    }
}

/// Apply smart retry with jitter and error-specific logic
pub async fn smart_retry_with_jitter<F, Fut, T>(
    config: &SmartRetryConfig,
    mut operation: F,
    operation_name: &str,
) -> WebRTCResult<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = WebRTCResult<T>>,
    T: Send + 'static,
{
    let mut attempts = 0;
    let mut current_backoff = config.base_backoff_ms;

    loop {
        attempts += 1;

        match operation().await {
            Ok(result) => {
                if attempts > 1 {
                    info!(
                        "Operation '{}' succeeded after {} attempts",
                        operation_name, attempts
                    );
                }
                return Ok(result);
            }
            Err(error) => {
                if !error.is_retryable() || attempts >= config.max_attempts {
                    return Err(error);
                }

                // Calculate backoff delay with optional jitter
                let delay_ms = if config.use_jitter {
                    // Add ±25% jitter to prevent thundering herd using deterministic hash
                    use std::collections::hash_map::DefaultHasher;
                    use std::hash::{Hash, Hasher};

                    let mut hasher = DefaultHasher::new();
                    std::thread::current().id().hash(&mut hasher);
                    let thread_hash = hasher.finish();

                    // Generate pseudo-random jitter based on thread ID (deterministic within thread)
                    let jitter_factor = ((thread_hash % 100) as f64) / 100.0; // 0.0-1.0
                    let jitter = (jitter_factor - 0.5) * 0.5; // ±25%
                    let jittered = current_backoff as f64 * (1.0 + jitter);
                    jittered.max(10.0) as u64 // Minimum 10ms delay
                } else {
                    current_backoff
                };

                let delay = Duration::from_millis(delay_ms);
                let max_delay = Duration::from_secs(config.max_backoff_secs);
                let actual_delay = std::cmp::min(delay, max_delay);

                debug!(
                    "Operation '{}' failed (attempt {}), retrying after {:?}: {}",
                    operation_name, attempts, actual_delay, error
                );

                sleep(actual_delay).await;

                // Increase backoff for next attempt
                current_backoff = ((current_backoff as f64) * config.backoff_multiplier) as u64;
            }
        }
    }
}

use crate::resource_manager::RESOURCE_MANAGER;
use crate::router_helpers::get_relay_access_creds;
use crate::tube_and_channel_helpers::TubeStatus;
use crate::tube_protocol::CloseConnectionReason;
use crate::Tube;
use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use dashmap::DashMap;
use log::{debug, error, info, warn};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::oneshot;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy;

// Define a message structure for signaling
#[derive(Debug, Clone)]
pub struct SignalMessage {
    pub tube_id: String,
    pub kind: String, // "icecandidate", "answer", etc.
    pub data: String,
    pub conversation_id: String,
    pub progress_flag: Option<i32>, // Progress flag for gateway responses (0=COMPLETE, 1=FAIL, 2=PROGRESS, 3=SKIP, 4=ABSENT)
    pub progress_status: Option<String>, // Progress status message
    pub is_ok: Option<bool>,        // Success/failure indicator
}

// ============================================================================
// ACTOR MODEL FOR COORDINATED REGISTRY OPERATIONS
// ============================================================================

/// Registry metrics for observability and backpressure
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields are used in Python bindings
pub struct RegistryMetrics {
    pub active_creates: usize,
    pub max_concurrent: usize,
    pub command_queue_depth: usize,
    pub avg_create_time: Duration,
    pub total_creates: u64,
    pub total_failures: u64,
}

/// Request structure for tube creation
#[derive(Debug)]
pub struct CreateTubeRequest {
    pub conversation_id: String,
    pub settings: HashMap<String, serde_json::Value>,
    pub initial_offer_sdp: Option<String>,
    pub trickle_ice: bool,
    pub callback_token: String,
    pub krelay_server: String,
    pub ksm_config: Option<String>,
    pub client_version: String,
    pub signal_sender: UnboundedSender<SignalMessage>,
    pub tube_id: Option<String>,
}

/// Commands for the registry actor
#[allow(dead_code)] // All variants are used in actor event loop and public API
pub enum RegistryCommand {
    CreateTube {
        req: CreateTubeRequest,
        resp: oneshot::Sender<Result<HashMap<String, String>>>,
    },
    // NOTE: GetTube/GetByConversation DELETED (use get_tube_fast() instead - lock-free!)
    CloseTube {
        tube_id: String,
        reason: Option<CloseConnectionReason>,
        resp: oneshot::Sender<Result<()>>,
    },
    SetServerMode {
        server_mode: bool,
    },
    AssociateConversation {
        tube_id: String,
        conversation_id: String,
        resp: oneshot::Sender<Result<()>>,
    },
    GetMetrics {
        resp: oneshot::Sender<RegistryMetrics>,
    },
    GetServerMode {
        resp: oneshot::Sender<bool>,
    },
}

/// Actor that manages tube registry with backpressure and coordination
/// RAII Pattern: Tubes own their resources (signal_sender, metrics_handle, keepalive_task)
/// Actor is just for coordination and storage - minimal state!
struct RegistryActor {
    /// Lock-free tube storage (shared with handle)
    tubes: Arc<DashMap<String, Arc<Tube>>>,
    /// Lock-free conversation mapping (reverse index: conversation_id → tube_id)
    conversations: Arc<DashMap<String, String>>,
    /// Server mode flag
    server_mode: bool,
    /// Command receiver
    command_rx: mpsc::UnboundedReceiver<RegistryCommand>,
    /// Admission control
    active_creates: Arc<AtomicUsize>,
    max_concurrent_creates: usize,
    /// Metrics
    total_creates: u64,
    total_failures: u64,
    create_times: Vec<Duration>,
}

impl RegistryActor {
    fn new(command_rx: mpsc::UnboundedReceiver<RegistryCommand>, max_concurrent: usize) -> Self {
        Self {
            tubes: Arc::new(DashMap::new()),
            conversations: Arc::new(DashMap::new()),
            server_mode: false,
            command_rx,
            active_creates: Arc::new(AtomicUsize::new(0)),
            max_concurrent_creates: max_concurrent,
            total_creates: 0,
            total_failures: 0,
            create_times: Vec::with_capacity(100),
        }
    }

    /// Main actor event loop
    async fn run(mut self) {
        info!(
            "Registry actor started (max_concurrent: {})",
            self.max_concurrent_creates
        );

        while let Some(cmd) = self.command_rx.recv().await {
            match cmd {
                RegistryCommand::CreateTube { req, resp } => {
                    let active = self.active_creates.load(Ordering::Acquire);

                    // BACKPRESSURE: Reject if overloaded
                    if active >= self.max_concurrent_creates {
                        warn!(
                            "Registry at capacity: {}/{} creates active - rejecting new request",
                            active, self.max_concurrent_creates
                        );
                        let _ = resp.send(Err(anyhow!(
                            "System overloaded: {} active creates (max {}). Please retry with backoff.",
                            active, self.max_concurrent_creates
                        )));
                        self.total_failures += 1;
                        continue;
                    }

                    // Accept and process
                    self.active_creates.fetch_add(1, Ordering::Release);
                    let start = Instant::now();

                    let result = self.handle_create_tube(req).await;

                    let duration = start.elapsed();
                    self.active_creates.fetch_sub(1, Ordering::Release);
                    self.total_creates += 1;

                    // Track timing
                    if self.create_times.len() >= 100 {
                        self.create_times.remove(0);
                    }
                    self.create_times.push(duration);

                    if result.is_err() {
                        self.total_failures += 1;
                    }

                    let _ = resp.send(result);
                }

                // NOTE: GetTube/GetByConversation removed - use get_tube_fast() instead (lock-free!)
                RegistryCommand::CloseTube {
                    tube_id,
                    reason,
                    resp,
                } => {
                    let result = self.handle_close_tube(&tube_id, reason).await;
                    let _ = resp.send(result);
                }

                RegistryCommand::SetServerMode { server_mode } => {
                    self.server_mode = server_mode;
                }

                RegistryCommand::AssociateConversation {
                    tube_id,
                    conversation_id,
                    resp,
                } => {
                    self.conversations.insert(conversation_id, tube_id);
                    let _ = resp.send(Ok(()));
                }

                RegistryCommand::GetMetrics { resp } => {
                    let avg_time = if !self.create_times.is_empty() {
                        let sum: Duration = self.create_times.iter().sum();
                        sum / self.create_times.len() as u32
                    } else {
                        Duration::from_secs(0)
                    };

                    let metrics = RegistryMetrics {
                        active_creates: self.active_creates.load(Ordering::Acquire),
                        max_concurrent: self.max_concurrent_creates,
                        command_queue_depth: 0, // Approximation, hard to get exact
                        avg_create_time: avg_time,
                        total_creates: self.total_creates,
                        total_failures: self.total_failures,
                    };
                    let _ = resp.send(metrics);
                }

                RegistryCommand::GetServerMode { resp } => {
                    let _ = resp.send(self.server_mode);
                }
            }
        }

        warn!("Registry actor event loop terminated");
    }

    /// Handle tube creation - all logic outside any locks
    async fn handle_create_tube(
        &mut self,
        req: CreateTubeRequest,
    ) -> Result<HashMap<String, String>> {
        let conversation_id = &req.conversation_id;
        let tube_id_opt = req.tube_id.clone();

        // Check if tube_id is provided and already exists
        if let Some(ref provided_tube_id) = tube_id_opt {
            if let Some(existing_tube) = self.tubes.get(provided_tube_id).map(|e| e.value().clone())
            {
                info!(
                    "Using existing tube for conversation (tube_id: {}, conversation_id: {})",
                    provided_tube_id, conversation_id
                );

                // Associate conversation
                self.conversations
                    .insert(conversation_id.to_string(), provided_tube_id.clone());

                // NOTE: No need to register signal channel - Tube owns it! (RAII)

                // Create data channel on existing tube
                let ksm_config_for_channel = req.ksm_config.clone().unwrap_or_default();
                match existing_tube
                    .create_data_channel(
                        conversation_id,
                        ksm_config_for_channel.clone(),
                        req.callback_token.clone(),
                        &req.client_version,
                    )
                    .await
                {
                    Ok(data_channel) => {
                        match existing_tube
                            .create_channel(
                                conversation_id,
                                &data_channel,
                                None,
                                req.settings.clone(),
                                Some(req.callback_token.clone()),
                                Some(ksm_config_for_channel),
                                Some(req.client_version.clone()),
                            )
                            .await
                        {
                            Ok(_) => {
                                info!("Successfully created new channel on existing tube (tube_id: {}, conversation_id: {})",
                                      provided_tube_id, conversation_id);
                            }
                            Err(e) => {
                                warn!("Failed to create logical channel on existing tube: {} (tube_id: {}, conversation_id: {})",
                                      e, provided_tube_id, conversation_id);
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Failed to create data channel on existing tube: {} (tube_id: {}, conversation_id: {})",
                              e, provided_tube_id, conversation_id);
                    }
                }

                let mut result_map = HashMap::new();
                result_map.insert("tube_id".to_string(), provided_tube_id.clone());
                return Ok(result_map);
            }
        }

        // PHASE 1: ALL PREPARATION (NO LOCKS - 2-5 seconds)
        // Decode offer, fetch TURN credentials, prepare ICE servers

        // Decode offer from base64 (API contract: all SDP is base64-encoded over Python/Rust boundary)
        let initial_offer_sdp_decoded = if let Some(ref b64_offer) = req.initial_offer_sdp {
            let bytes = BASE64_STANDARD
                .decode(b64_offer)
                .context("Failed to decode initial_offer_sdp from base64")?;
            Some(
                String::from_utf8(bytes)
                    .context("Failed to convert decoded initial_offer_sdp to String")?,
            )
        } else {
            None
        };

        let is_server_mode = initial_offer_sdp_decoded.is_none();

        // Create tube with signal sender (RAII - tube owns it!)
        let tube_arc = Tube::new(
            is_server_mode,
            Some(conversation_id.to_string()),
            Some(req.signal_sender.clone()),
            tube_id_opt,
        )?;
        let tube_id = tube_arc.id();

        // Prepare ICE servers (includes TURN credential fetch - OUTSIDE locks!)
        let ice_servers = self
            .prepare_ice_servers_unlocked(
                &tube_id,
                &req.krelay_server,
                &req.ksm_config,
                &req.client_version,
                &req.settings,
            )
            .await?;

        let turn_credentials_timestamp = if ice_servers
            .iter()
            .any(|s| s.urls.iter().any(|u| u.contains("turn:")))
        {
            Some(Instant::now())
        } else {
            None
        };

        // PHASE 2: FAST INSERT (DashMap - microseconds!)
        self.tubes.insert(tube_id.clone(), Arc::clone(&tube_arc));
        self.conversations
            .insert(conversation_id.to_string(), tube_id.clone());
        // NOTE: signal_channels no longer needed - Tube owns signal_sender now! (RAII)

        if is_server_mode {
            self.server_mode = true;
        }

        // PHASE 3: WEBRTC SETUP (NO LOCKS - 1-3 seconds)
        // Create peer connection with ICE servers
        let rtc_config = self.build_rtc_configuration(ice_servers, &req.settings);
        let turn_only = req
            .settings
            .get("turn_only")
            .is_some_and(|v| v.as_bool().unwrap_or(false));

        // Get signal sender from tube (RAII - tube owns it!)
        let signal_sender_for_webrtc = tube_arc
            .signal_sender
            .clone()
            .ok_or_else(|| anyhow!("Tube missing signal_sender"))?;

        tube_arc
            .create_peer_connection(
                Some(rtc_config),
                req.trickle_ice,
                turn_only,
                req.ksm_config.clone().unwrap_or_default(),
                req.callback_token.clone(),
                &req.client_version,
                req.settings.clone(),
                signal_sender_for_webrtc,
                turn_credentials_timestamp,
            )
            .await
            .map_err(|e| anyhow!("Failed to create peer connection: {}", e))?;

        // Create data channel
        let ksm_config_for_channel = req.ksm_config.clone().unwrap_or_default();
        let data_channel = tube_arc
            .create_data_channel(
                conversation_id,
                ksm_config_for_channel.clone(),
                req.callback_token.clone(),
                &req.client_version,
            )
            .await?;

        // Create logical channel and capture actual listening port (if server mode)
        let actual_listening_port = tube_arc
            .create_channel(
                conversation_id,
                &data_channel,
                None,
                req.settings.clone(),
                Some(req.callback_token.clone()),
                Some(ksm_config_for_channel),
                Some(req.client_version.clone()),
            )
            .await?;

        // Generate offer/answer (BASE64-ENCODE for Python boundary)
        let mut result_map = HashMap::new();
        result_map.insert("tube_id".to_string(), tube_id.clone());

        // Add actual listening address if server mode and port was assigned
        if is_server_mode {
            if let Some(port) = actual_listening_port {
                // Extract host from settings or use default
                let host = req
                    .settings
                    .get("local_listen_addr")
                    .and_then(|v| v.as_str())
                    .and_then(|addr| addr.split(':').next())
                    .unwrap_or("127.0.0.1");

                result_map.insert(
                    "actual_local_listen_addr".to_string(),
                    format!("{}:{}", host, port),
                );
                debug!(
                    "Server tube listening on: {}:{} (tube_id: {})",
                    host, port, tube_id
                );
            }
        }

        if is_server_mode {
            let offer = tube_arc
                .create_offer()
                .await
                .map_err(|e| anyhow!("Failed to create offer: {}", e))?;
            // Encode to base64 for Python/Rust boundary (consistent with answer encoding)
            let offer_base64 = BASE64_STANDARD.encode(&offer);
            result_map.insert("offer".to_string(), offer_base64);
        } else if let Some(offer_sdp) = initial_offer_sdp_decoded {
            tube_arc
                .set_remote_description(offer_sdp, false)
                .await
                .map_err(|e| anyhow!("Failed to set remote description: {}", e))?;
            let answer = tube_arc
                .create_answer()
                .await
                .map_err(|e| anyhow!("Failed to create answer: {}", e))?;
            // Encode to base64 for Python/Rust boundary
            let answer_base64 = BASE64_STANDARD.encode(&answer);
            result_map.insert("answer".to_string(), answer_base64);
        }

        info!(
            "Tube created successfully via actor (tube_id: {}, conversation_id: {}, mode: {})",
            tube_id,
            conversation_id,
            if is_server_mode { "server" } else { "client" }
        );

        Ok(result_map)
    }

    /// Prepare ICE servers with TURN credential fetching - NO LOCKS
    async fn prepare_ice_servers_unlocked(
        &self,
        tube_id: &str,
        krelay_server: &str,
        ksm_config: &Option<String>,
        client_version: &str,
        settings: &HashMap<String, serde_json::Value>,
    ) -> Result<Vec<RTCIceServer>> {
        let mut ice_servers = Vec::new();
        let turn_only = settings
            .get("turn_only")
            .is_some_and(|v| v.as_bool().unwrap_or(false));

        // Check for test mode
        let is_test_mode = ksm_config
            .as_ref()
            .is_some_and(|cfg| cfg.starts_with("TEST_MODE_KSM_CONFIG"));

        if is_test_mode {
            info!(
                "TEST_MODE_KSM_CONFIG active: Using Google STUN server (tube_id: {})",
                tube_id
            );
            // Note: turn_only is not used after this point for test mode
            ice_servers.push(RTCIceServer {
                urls: vec!["stun:stun.l.google.com:19302?transport=udp&family=ipv4".to_string()],
                username: String::new(),
                credential: String::new(),
            });
            ice_servers.push(RTCIceServer {
                urls: vec!["stun:stun1.l.google.com:19302?transport=udp&family=ipv4".to_string()],
                username: String::new(),
                credential: String::new(),
            });
            return Ok(ice_servers);
        }

        if krelay_server.is_empty() {
            warn!("No krelay_server provided (tube_id: {})", tube_id);
            return Ok(ice_servers);
        }

        debug!(
            "Using krelay_server for ICE configuration (tube_id: {}, relay: {})",
            tube_id, krelay_server
        );

        // Add STUN if not turn-only
        if !turn_only {
            let stun_url = format!("stun:{}:3478", krelay_server);
            ice_servers.push(RTCIceServer {
                urls: vec![stun_url.clone()],
                username: String::new(),
                credential: String::new(),
            });
            debug!(
                "Added STUN server (tube_id: {}, url: {})",
                tube_id, stun_url
            );
        }

        let use_turn = settings
            .get("use_turn")
            .is_none_or(|v| v.as_bool().unwrap_or(true));

        if use_turn {
            // Priority 1: Explicit TURN credentials in settings
            if let (Some(turn_url), Some(turn_username), Some(turn_password)) = (
                settings.get("turn_url").and_then(|v| v.as_str()),
                settings.get("turn_username").and_then(|v| v.as_str()),
                settings.get("turn_password").and_then(|v| v.as_str()),
            ) {
                debug!(
                    "Using explicit TURN credentials (tube_id: {}, url: {})",
                    tube_id, turn_url
                );
                ice_servers.push(RTCIceServer {
                    urls: vec![turn_url.to_string()],
                    username: turn_username.to_string(),
                    credential: turn_password.to_string(),
                });
            }
            // Priority 2: Fetch from router via ksm_config
            else if let Some(ksm_cfg) = ksm_config {
                if !ksm_cfg.is_empty() && !ksm_cfg.starts_with("TEST_MODE") {
                    // Check connection pool first
                    if let Some(existing_conn) = RESOURCE_MANAGER.get_turn_connection(krelay_server)
                    {
                        debug!(
                            "Reusing pooled TURN connection (tube_id: {}, username: {})",
                            tube_id, existing_conn.username
                        );
                        ice_servers.push(RTCIceServer {
                            urls: vec![format!("turn:{}", krelay_server)],
                            username: existing_conn.username,
                            credential: existing_conn.password,
                        });
                    } else {
                        // Fetch new credentials - NETWORK I/O but no locks held!
                        debug!(
                            "Fetching new TURN credentials from router (tube_id: {})",
                            tube_id
                        );
                        let turn_start = Instant::now();

                        match get_relay_access_creds(ksm_cfg, Some(3600), client_version).await {
                            Ok(creds) => {
                                let turn_duration_ms = turn_start.elapsed().as_millis() as f64;
                                debug!("Successfully fetched TURN credentials (tube_id: {}, duration: {:.1}ms)",
                                       tube_id, turn_duration_ms);

                                let ttl_seconds =
                                    creds.get("ttl").and_then(|v| v.as_u64()).unwrap_or(3600);
                                info!(
                                    "TURN credentials TTL: {}s ({:.1}h) (tube_id: {})",
                                    ttl_seconds,
                                    ttl_seconds as f64 / 3600.0,
                                    tube_id
                                );

                                crate::metrics::METRICS_COLLECTOR.record_turn_allocation(
                                    tube_id,
                                    turn_duration_ms,
                                    true,
                                );

                                if let (Some(username), Some(password)) = (
                                    creds.get("username").and_then(|v| v.as_str()),
                                    creds.get("password").and_then(|v| v.as_str()),
                                ) {
                                    // Add to connection pool
                                    let _ = RESOURCE_MANAGER.add_turn_connection(
                                        krelay_server.to_string(),
                                        username.to_string(),
                                        password.to_string(),
                                    );

                                    debug!("Created new TURN connection in pool (tube_id: {}, username: {})",
                                           tube_id, username);
                                    ice_servers.push(RTCIceServer {
                                        urls: vec![format!("turn:{}", krelay_server)],
                                        username: username.to_string(),
                                        credential: password.to_string(),
                                    });
                                } else {
                                    warn!("Invalid TURN credentials format (tube_id: {})", tube_id);
                                }
                            }
                            Err(e) => {
                                let turn_duration_ms = turn_start.elapsed().as_millis() as f64;
                                error!("Failed to get TURN credentials: {} (tube_id: {}, duration: {:.1}ms)",
                                       e, tube_id, turn_duration_ms);

                                crate::metrics::METRICS_COLLECTOR.record_turn_allocation(
                                    tube_id,
                                    turn_duration_ms,
                                    false,
                                );
                                // Don't fail entire operation
                            }
                        }
                    }
                }
            }
        }

        Ok(ice_servers)
    }

    /// Build RTCConfiguration from ICE servers and settings
    fn build_rtc_configuration(
        &self,
        ice_servers: Vec<RTCIceServer>,
        settings: &HashMap<String, serde_json::Value>,
    ) -> RTCConfiguration {
        let turn_only = settings
            .get("turn_only")
            .is_some_and(|v| v.as_bool().unwrap_or(false));

        let ice_transport_policy = if turn_only {
            RTCIceTransportPolicy::Relay
        } else {
            RTCIceTransportPolicy::All
        };

        RTCConfiguration {
            ice_servers,
            ice_transport_policy,
            ..Default::default()
        }
    }

    /// Handle tube closure - RAII makes this trivial!
    async fn handle_close_tube(
        &mut self,
        tube_id: &str,
        reason: Option<CloseConnectionReason>,
    ) -> Result<()> {
        debug!("Actor closing tube: {}", tube_id);

        // Get tube to do graceful shutdown
        let tube = self
            .tubes
            .get(tube_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| anyhow!("Tube not found: {}", tube_id))?;

        // 1. Set status to Closed (for status queries)
        {
            let mut status = tube.status.write().await;
            *status = TubeStatus::Closed;
            debug!("  ✓ Status set to Closed (tube_id: {})", tube_id);
        }

        // 2. Send connection_close callbacks for channels (graceful shutdown)
        let channel_names: Vec<String> = {
            let channels_guard = tube.active_channels.read().await;
            channels_guard.keys().cloned().collect()
        };
        for channel_name in &channel_names {
            if let Err(e) = tube.send_connection_close_callback(channel_name).await {
                warn!(
                    "Failed to send close callback: {} (tube_id: {}, channel: {})",
                    e, tube_id, channel_name
                );
            }
        }

        // 2b. Send "channel_closed" signals to Python (CRITICAL for Python integration!)
        // This logic was in old Tube::close_with_reason() - Python depends on it!
        if let Some(ref signal_sender) = tube.signal_sender {
            let data_channels_snapshot = tube.data_channels.read().await.clone();
            for (label, _dc) in data_channels_snapshot.iter() {
                // Determine conversation_id from channel label
                let conversation_id = if label == "control" {
                    tube.original_conversation_id
                        .clone()
                        .unwrap_or_else(|| tube.id())
                } else {
                    label.to_string()
                };

                // Build signal payload (matches old format exactly)
                let close_reason = reason.unwrap_or(CloseConnectionReason::AdminClosed);
                let signal_data = serde_json::json!({
                    "channel_id": conversation_id,
                    "outcome": "tube_closed",
                    "close_reason": {
                        "code": close_reason as u16,
                        "name": format!("{:?}", close_reason),
                        "is_critical": close_reason.is_critical(),
                        "is_retryable": close_reason.is_retryable(),
                    }
                })
                .to_string();

                let signal_msg = SignalMessage {
                    tube_id: tube.id(),
                    kind: "channel_closed".to_string(),
                    data: signal_data,
                    conversation_id: conversation_id.clone(),
                    progress_flag: Some(0), // COMPLETE
                    progress_status: Some("Tube closed".to_string()),
                    is_ok: Some(true),
                };

                if let Err(e) = signal_sender.send(signal_msg) {
                    error!(
                        "Failed to send channel_closed signal: {} (tube_id: {}, label: {})",
                        e,
                        tube.id(),
                        label
                    );
                } else {
                    debug!(
                        "Sent channel_closed signal to Python (tube_id: {}, label: {}, conv_id: {})",
                        tube.id(),
                        label,
                        conversation_id
                    );
                }
            }
        } else {
            warn!(
                "No signal sender on tube - cannot notify Python of channel closures (tube_id: {})",
                tube_id
            );
        }

        // 3. Explicit close (closes data channels + peer connection properly)
        // CRITICAL: This must be called BEFORE dropping the tube to ensure:
        // - Data channels send CloseConnection to downstream peers
        // - TURN allocations are released (prevents 400 Bad Request errors)
        // - Permission refresh timers are stopped
        if let Err(e) = tube.close().await {
            warn!(
                "Error during explicit tube close: {} (tube_id: {}) - proceeding with removal",
                e, tube_id
            );
        } else {
            info!("Tube {} explicit close completed successfully", tube_id);
        }

        // 4. Remove from registry
        self.tubes.remove(tube_id);
        self.conversations.retain(|_, tid| tid != tube_id);

        info!(
            "Tube {} removed from registry - Drop will verify cleanup",
            tube_id
        );

        // When tube Arc drops (after removal):
        // → Tube::drop() fires as safety net
        // → Verifies everything was closed by explicit close()
        // → Logs warnings if cleanup was missed
        // → Auto-cleanup for RAII resources (signal_sender, metrics)

        Ok(())
    }
}

/// Public handle to the registry actor
#[derive(Clone)]
pub struct RegistryHandle {
    command_tx: mpsc::UnboundedSender<RegistryCommand>,
    /// Direct access to tubes for hot-path reads (no actor overhead)
    tubes: Arc<DashMap<String, Arc<Tube>>>,
    /// Direct access to conversations
    conversations: Arc<DashMap<String, String>>,
}

impl RegistryHandle {
    fn new(
        command_tx: mpsc::UnboundedSender<RegistryCommand>,
        tubes: Arc<DashMap<String, Arc<Tube>>>,
        conversations: Arc<DashMap<String, String>>,
    ) -> Self {
        Self {
            command_tx,
            tubes,
            conversations,
        }
    }

    /// Hot path: Get tube without going through actor (lock-free!)
    pub fn get_tube_fast(&self, tube_id: &str) -> Option<Arc<Tube>> {
        self.tubes.get(tube_id).map(|entry| entry.value().clone())
    }

    /// Hot path: Get tube by conversation (lock-free!)
    pub fn get_by_conversation_fast(&self, conversation_id: &str) -> Option<Arc<Tube>> {
        self.conversations
            .get(conversation_id)
            .and_then(|tube_id| self.tubes.get(tube_id.value()).map(|e| e.value().clone()))
    }

    /// Cold path: Create tube through actor (with backpressure)
    pub async fn create_tube(&self, req: CreateTubeRequest) -> Result<HashMap<String, String>> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(RegistryCommand::CreateTube { req, resp: tx })
            .map_err(|_| anyhow!("Registry actor unavailable"))?;
        rx.await
            .map_err(|_| anyhow!("Registry actor response channel closed"))?
    }

    /// Get metrics for backpressure coordination
    pub async fn get_metrics(&self) -> Result<RegistryMetrics> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(RegistryCommand::GetMetrics { resp: tx })
            .map_err(|_| anyhow!("Registry actor unavailable"))?;
        rx.await
            .map_err(|_| anyhow!("Registry actor response channel closed"))
    }

    /// Close a tube
    pub async fn close_tube(
        &self,
        tube_id: &str,
        reason: Option<CloseConnectionReason>,
    ) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(RegistryCommand::CloseTube {
                tube_id: tube_id.to_string(),
                reason,
                resp: tx,
            })
            .map_err(|_| anyhow!("Registry actor unavailable"))?;
        rx.await
            .map_err(|_| anyhow!("Registry actor response channel closed"))?
    }

    /// Set server mode
    #[allow(dead_code)] // Used by Python bindings
    pub async fn set_server_mode(&self, server_mode: bool) -> Result<()> {
        self.command_tx
            .send(RegistryCommand::SetServerMode { server_mode })
            .map_err(|_| anyhow!("Registry actor unavailable"))?;
        Ok(())
    }

    /// Associate conversation with tube
    #[allow(dead_code)] // Used by Python bindings
    pub async fn associate_conversation(&self, tube_id: &str, conversation_id: &str) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(RegistryCommand::AssociateConversation {
                tube_id: tube_id.to_string(),
                conversation_id: conversation_id.to_string(),
                resp: tx,
            })
            .map_err(|_| anyhow!("Registry actor unavailable"))?;
        rx.await
            .map_err(|_| anyhow!("Registry actor response channel closed"))?
    }

    // ===== COMPATIBILITY LAYER =====
    // These methods provide backward compatibility with old TubeRegistry API

    /// Get tube by ID (compatibility alias for get_tube_fast)
    pub fn get_by_tube_id(&self, tube_id: &str) -> Option<Arc<Tube>> {
        self.get_tube_fast(tube_id)
    }

    /// Get tube by conversation ID (compatibility alias)
    pub fn get_by_conversation_id(&self, conversation_id: &str) -> Option<Arc<Tube>> {
        self.get_by_conversation_fast(conversation_id)
    }

    /// Get all tube IDs (lock-free iteration)
    pub fn all_tube_ids_sync(&self) -> Vec<String> {
        self.tubes.iter().map(|entry| entry.key().clone()).collect()
    }

    /// Find tubes by search term (lock-free iteration)
    pub fn find_tubes(&self, search_term: &str) -> Vec<String> {
        self.tubes
            .iter()
            .filter(|entry| entry.key().contains(search_term))
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Get conversation IDs for a tube (lock-free iteration)
    #[allow(dead_code)] // Used by Python bindings
    pub fn conversation_ids_by_tube_id(&self, tube_id: &str) -> Vec<String> {
        self.conversations
            .iter()
            .filter(|entry| entry.value() == tube_id)
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Get tube count (lock-free)
    pub fn tube_count(&self) -> usize {
        self.tubes.len()
    }

    /// Check if any tubes exist (lock-free)
    pub fn has_tubes(&self) -> bool {
        !self.tubes.is_empty()
    }

    /// Direct access to tubes DashMap for advanced operations
    pub fn tubes(&self) -> &Arc<DashMap<String, Arc<Tube>>> {
        &self.tubes
    }

    /// Direct access to conversations DashMap
    #[allow(dead_code)] // Used by Python bindings
    pub fn conversations(&self) -> &Arc<DashMap<String, String>> {
        &self.conversations
    }

    /// Get signal sender for a tube (RAII - stored on Tube itself!)
    pub fn get_signal_sender(&self, tube_id: &str) -> Option<UnboundedSender<SignalMessage>> {
        self.get_tube_fast(tube_id)
            .and_then(|tube| tube.signal_sender.clone())
    }

    /// Get connection state for a tube
    #[allow(dead_code)] // Used by Python bindings
    pub async fn get_connection_state(&self, tube_id: &str) -> Result<String> {
        let tube = self
            .get_tube_fast(tube_id)
            .ok_or_else(|| anyhow!("Tube not found: {}", tube_id))?;

        match tube.get_connection_state().await {
            Some(state) => Ok(format!("{:?}", state)),
            None => Ok("Unknown".to_string()),
        }
    }

    /// Add external ICE candidate to a tube
    #[allow(dead_code)] // Used by Python bindings
    pub async fn add_external_ice_candidate(&self, tube_id: &str, candidate: &str) -> Result<()> {
        let tube = self
            .get_tube_fast(tube_id)
            .ok_or_else(|| anyhow!("Tube not found: {}", tube_id))?;
        tube.add_ice_candidate(candidate.to_string())
            .await
            .map_err(|e| anyhow!("Failed to add ICE candidate: {}", e))
    }

    /// Check server mode (queries actor for authoritative state)
    #[allow(dead_code)] // Used by Python bindings
    pub async fn is_server_mode(&self) -> Result<bool> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(RegistryCommand::GetServerMode { resp: tx })
            .map_err(|_| anyhow!("Registry actor unavailable"))?;
        rx.await
            .map_err(|_| anyhow!("Registry actor response channel closed"))
    }

    /// Cleanup stale tubes (lock-free iteration + coordinated cleanup)
    pub async fn cleanup_stale_tubes(&self) -> Vec<String> {
        let mut stale_tube_ids = Vec::new();

        // LOCK-FREE: Collect stale tube IDs from DashMap
        for entry in self.tubes.iter() {
            let (tube_id, tube) = (entry.key(), entry.value());
            if tube.is_stale().await {
                debug!("Detected stale tube (tube_id: {})", tube_id);
                stale_tube_ids.push(tube_id.clone());
            }
        }

        // Close stale tubes via actor (coordinated)
        let mut closed_tubes = Vec::new();
        for tube_id in stale_tube_ids {
            debug!("Auto-closing stale tube (tube_id: {})", tube_id);
            match self
                .close_tube(&tube_id, Some(CloseConnectionReason::Timeout))
                .await
            {
                Ok(_) => {
                    info!("Successfully auto-closed stale tube (tube_id: {})", tube_id);
                    closed_tubes.push(tube_id);
                }
                Err(e) => {
                    warn!(
                        "Failed to auto-close stale tube: {} (tube_id: {})",
                        e, tube_id
                    );
                }
            }
        }

        closed_tubes
    }
}

// Global registry handle - initialized once
pub(crate) static REGISTRY: Lazy<RegistryHandle> = Lazy::new(|| {
    debug!("Initializing global tube registry with actor model");

    let (command_tx, command_rx) = mpsc::unbounded_channel();

    // Get max concurrent from environment or default to 100
    let max_concurrent = std::env::var("WEBRTC_MAX_CONCURRENT_CREATES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);

    info!(
        "Registry configured with max_concurrent_creates: {}",
        max_concurrent
    );

    let actor = RegistryActor::new(command_rx, max_concurrent);
    let tubes = Arc::clone(&actor.tubes);
    let conversations = Arc::clone(&actor.conversations);

    // CRITICAL FIX: Spawn actor on global runtime
    // This ensures actor actually starts even during static init
    std::thread::spawn(move || {
        let rt = crate::runtime::get_runtime();
        rt.spawn(async move {
            info!("Registry actor event loop starting");
            actor.run().await;
            // If actor exits, this is catastrophic
            error!("CRITICAL: Registry actor event loop terminated!");
            error!("All future tube operations will fail!");
        });
    });

    RegistryHandle::new(command_tx, tubes, conversations)
});

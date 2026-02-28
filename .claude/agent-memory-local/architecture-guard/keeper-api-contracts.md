# Keeper API Contracts

## WebRTC Registry API (keeper-pam-webrtc-rs)

**Source:** REF_CODE/keeper-pam-connections/crates/keeper-pam-webrtc-rs/src/tube_registry.rs

### get_registry() -> &'static RegistryHandle
Returns the global singleton RegistryHandle (Lazy<RegistryHandle> via once_cell).
Thread-safe: RegistryHandle uses DashMap for lock-free reads and an actor channel
for coordinated writes.

### RegistryHandle::create_tube(req: CreateTubeRequest) -> Result<HashMap<String, String>>
- Returns Map with tube_id and other metadata
- Enforces backpressure: max 100 concurrent creates (WEBRTC_MAX_CONCURRENT_CREATES)
- Async; goes through actor channel (RegistryCommand::CreateTube)
- Returns anyhow::Result (NOT keeper-core error types)

### RegistryHandle::close_tube(tube_id, reason) -> Result<()>
- Reason is CloseConnectionReason enum from tube_protocol module
- Normal close: CloseConnectionReason::Normal
- Spawns a separate task (non-blocking actor)

### RegistryHandle::get_connection_state(tube_id) -> Result<String>
- Returns state as String (not a typed enum)
- States: "new", "connecting", "connected", "disconnected", "failed", "closed"
- Must map to TunnelState domain enum in adapter

### CreateTubeRequest fields
- conversation_id: String (base64url)
- settings: HashMap<String, serde_json::Value> — protocol params go here
- initial_offer_sdp: Option<String> — base64-encoded SDP (use STANDARD not URL_SAFE)
- trickle_ice: bool
- callback_token: String — authentication token for KRouter
- krelay_server: String — TURN server hostname
- ksm_config: Option<String>
- client_version: String
- signal_sender: UnboundedSender<SignalMessage> — REQUIRED, cannot be None
- tube_id: Option<String> — None = auto-generate
- capabilities: tube_protocol::Capabilities
- python_handler_tx: Option<...> — None for Rust-only use

### SignalMessage fields (received from signal_sender channel)
- tube_id: String
- kind: String — "icecandidate", "answer", "offer", etc.
- data: String — SDP or candidate JSON
- conversation_id: String
- progress_flag: Option<i32> — 0=COMPLETE, 1=FAIL, 2=PROGRESS, 3=SKIP, 4=ABSENT
- progress_status: Option<String>
- is_ok: Option<bool>

### CRITICAL: SDP encoding
ALL SDP exchanged with keeper-pam-webrtc-rs MUST be base64 STANDARD encoded
(not URL_SAFE). Use base64::engine::general_purpose::STANDARD.
See keeper-pam-connections CLAUDE.md: "ALL SDP exchanged over the Python/Rust boundary
MUST be base64-encoded."

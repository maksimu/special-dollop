# Bugs and Gotchas

## Security: SecureBytes escapes into HashMap<String, Value>

**Context (2026-02-28 Phase 2 review):**
TunnelStartParams carries encryption_key: SecureBytes and turn_password: SecureString.
CreateTubeRequest.settings is HashMap<String, serde_json::Value>.

When the adapter translates TunnelStartParams -> CreateTubeRequest, the key material
must go into settings as plain JSON strings (base64-encoded bytes). At that boundary,
the zero-knowledge guarantee weakens: the key exists in heap memory as a plain String
and in a serde_json::Value::String, neither of which is zeroized.

**Severity:** Medium. Acceptable at the infrastructure/FFI boundary IF:
1. The key is SHORT-LIVED (not stored in the sessions map)
2. The HashMap is dropped immediately after create_tube() returns
3. Session map stores only the conversation_id, NOT the key

**Gotcha:** Do NOT store the key in SessionEntry. Only store the conversation_id.
The WebRTC layer takes ownership of the key via CreateTubeRequest; once create_tube()
completes, the request is dropped.

## Rust version bump: 1.86 -> 1.93

**Context (2026-02-28):** keeper-pam-webrtc-rs requires Rust 1.93+ (latest stable as
of Feb 2026). The Dockerfile must be updated. All existing crates must still compile
under 1.93. Check for any 1.86 MSRV assumptions in workspace Cargo.toml.

## Path dependency to REF_CODE/ (gitignored)

**Context (2026-02-28):** keeper-tunnel Cargo.toml will have a path dep pointing to
REF_CODE/keeper-pam-connections/crates/keeper-pam-webrtc-rs/. REF_CODE is gitignored.

**Consequence:** The crate will NOT compile in CI without REF_CODE present, AND any
developer without the REF_CODE checkout will get a build failure. This is a known
temporary state for development. The long-term fix is a git dependency pointing to
the keeper-pam-connections repo (once it has a stable release tag).

**Mitigation:** The keeper-tunnel feature should be optional (cfg feature flag) so
builds without REF_CODE still compile the rest of the workspace.

## CreateTubeRequest requires UnboundedSender<SignalMessage> (signal_sender field)

**Context (2026-02-28):** CreateTubeRequest is not a simple data bag — it requires a
live tokio::sync::mpsc::UnboundedSender<SignalMessage> for signaling callbacks.
The adapter (WebRtcTunnelPort::start()) must spawn a task to consume SignalMessage
events from the channel and route ICE candidates / SDP answers back to the signaling
port (TunnelSignalingPort). This is non-trivial and is essentially Phase 3 work.

Stubbing it with a dropped channel will cause ICE to fail silently.

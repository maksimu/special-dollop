# Video Transport Plan: H.264 over RTCRtpSender

## Goal

Replace the current JPEG-over-DataChannel screen transport with H.264-over-RTCRtpSender for graphical
protocols (RDP, VNC, RBI). Keep the Guacamole data channel for input events, cursor, clipboard, and
terminal-only protocols (SSH, Telnet, database, TN3270, TN5250).

This is the same transport split Discord uses: video content travels RTP, control/input travels a
separate data channel.

**Scope: guacr only.** The standard guacd path (Apache Guacamole daemon speaking the Guacamole wire
protocol over TCP 4822) is not affected and cannot be improved here — that protocol is fixed by the
guacd spec. These changes only apply to the guacr Rust handlers where we own the full stack.

---

## Why This Matters

### Current path (JPEG over DataChannel)
```
pixels → JPEG encode (CPU, per dirty region) → base64 → img+blob+end instructions →
SCTP data channel → browser → JS parse → canvas.drawImage()
```

Every frame is a full encode, every pixel sent over SCTP, every decode done in JavaScript on the
main thread with no GPU involvement.

### Target path (H.264 over RTCRtpSender)
```
pixels → H.264 encode (GPU) → NAL units → RTP → SRTP → browser → GPU decode → <video> compositor
```

Browser GPU decodes H.264 natively. P-frames mean only changed pixels travel the wire. REMB/TWCC
feedback automatically adjusts encoder bitrate to network capacity.

| Feature              | Current (JPEG/DataChannel)     | Target (H.264/RTCRtpSender)       |
|----------------------|--------------------------------|-----------------------------------|
| Codec                | JPEG (software, per dirty rect)| H.264 (GPU, full frame + P-frames)|
| Bandwidth            | ~15-50 Mbps at 60fps 1080p     | ~3-8 Mbps at 60fps 1080p          |
| Congestion control   | Manual backpressure only       | REMB/TWCC (automatic, real-time)  |
| Error recovery       | SCTP retransmit (HoL blocking) | FEC + NACK (per-packet)           |
| Browser render       | canvas API (JS, CPU copy)      | GPU compositor overlay            |
| Temporal compression | None (full dirty rect always)  | P-frames (changed pixels only)    |

### What does NOT change

- Security model: server renders everything, browser only receives pixels
- DLP: gateway can inspect/redact before encoding
- Session recording: recorded server-side before encoding
- Clipboard control: still enforced server-side
- Input events (keyboard, mouse): still over data channel (small, latency-sensitive)
- Cursor position: still Guacamole `cursor` instructions for zero-lag cursor
- All terminal protocols (SSH, Telnet, database, TN3270, TN5250): unchanged — see rationale below

---

## Architecture After This Change

```
                         ┌─────────────────────────────────────────────┐
                         │  RDP/VNC/RBI Handler                        │
                         │                                              │
                         │  FrameBuffer (RGBA) ──→ H.264 Encoder      │
                         │                             │ NAL units      │
                         │  cursor/clipboard ─────────────────────────┼──→ guac_tx (DataChannel)
                         │                             │                │
                         └─────────────────────────────────────────────┘
                                                       │
                                                 VideoSender
                                                       │
                         ┌─────────────────────────────────────────────┐
                         │  keeper-pam-webrtc-rs (Tube)                │
                         │                                              │
                         │  TrackLocalStaticSample ──→ RTCRtpSender   │
                         │  RTCDataChannel ───────────→ SCTP          │
                         └─────────────────────────────────────────────┘
                                         │ WebRTC (DTLS/SRTP)
                         ┌─────────────────────────────────────────────┐
                         │  Vault browser                              │
                         │                                              │
                         │  ontrack ──→ <video> (GPU decode/render)   │
                         │  onmessage → cursor overlay canvas          │
                         │  input ────→ data channel (key/mouse)      │
                         └─────────────────────────────────────────────┘
```

---

## Phase 1: H.264 Encoder Crate (guacr-encoder)

Create `crates/guacr-encoder/` as a new crate providing a platform-independent encoding API.

### Trait

```rust
pub trait VideoEncoder: Send {
    // Encode a full RGBA frame. Returns H.264 NAL units.
    // On first call after IDR request or GOP boundary: IDR frame (all intra).
    // On subsequent calls: P-frame (delta from previous).
    fn encode(&mut self, frame: &RgbaFrame) -> Result<EncodedFrame>;

    // Force next encode() to produce an IDR (keyframe).
    // Called on PLI (Picture Loss Indication) from browser.
    fn request_keyframe(&mut self);

    // Called when REMB/TWCC feedback arrives.
    fn set_target_bitrate(&mut self, bps: u32);
}

pub struct RgbaFrame {
    pub data: Bytes,   // width * height * 4 bytes, row-major
    pub width: u32,
    pub height: u32,
    pub timestamp_us: u64,
}

pub struct EncodedFrame {
    pub data: Bytes,         // Packetization-ready H.264 NAL units (Annex B)
    pub is_keyframe: bool,
    pub pts: u64,            // presentation timestamp in 90kHz RTP clock units
}
```

### Encoder selection (priority order)

1. **VideoToolbox** (macOS) — zero additional dependencies, system framework
2. **VAAPI** (Linux, AMD/Intel GPU) — requires `libva-dev`
3. **NVENC** (Linux/Windows, NVIDIA) — requires CUDA toolkit
4. **QuickSync** (Windows, Intel) — requires Intel Media SDK
5. **openh264** (software fallback) — pure software via `openh264-sys`, works everywhere, no GPU deps

Feature flags in `guacr-encoder/Cargo.toml`:
```toml
[features]
default = ["software"]
software = ["openh264-sys"]
videotoolbox = []          # macOS, no deps
vaapi = ["va-sys"]         # Linux
nvenc = ["cuda-sys"]       # NVIDIA
quicksync = ["mfx-sys"]    # Intel
```

### H.264 tuning for remote desktop (not screen share)

These settings differ from Discord/Meet because PAM needs low latency, not high compression:

```
profile:      constrained baseline (broadest browser support; Main for better compression)
level:        4.1 (covers 1080p60)
B-frames:     0 (B-frames add 1-2 frame latency — unacceptable for desktop)
GOP:          no periodic IDR; use intra refresh instead (see below)
bitrate:      ~3-6 Mbps for 1080p; hard ceiling via SDP b=TIAS (see Security section)
QP range:     18-40 (floor prevents blurring; ceiling prevents quality collapse on text)
```

**Intra refresh instead of periodic IDR**: IDR frames are 10-30x larger than P-frames. A periodic
IDR at 60fps causes a predictable bandwidth spike. Use rolling intra refresh instead — each encoder
refreshes a slice of macroblocks every N frames so every block gets refreshed within the window,
with no single large frame. This also avoids a PLI storm triggering multiple simultaneous IDRs.
Configure IDR period to `INFINITE` and rely on explicit PLI responses + intra refresh for recovery.

**SPS+PPS before every IDR — correctness requirement**: The rtp crate's `H264Payloader` bundles
SPS and PPS as a STAP-A packet before the IDR automatically, but only if those NALUs literally
precede the IDR in the Annex-B bytestream it receives. It clears them after each use. If your
encoder outputs a bare IDR without preceding SPS+PPS:
- Safari breaks immediately — it requires in-band SPS/PPS even if `sprop-parameter-sets` was in SDP
- Chrome breaks after any packet loss that drops the out-of-band parameter sets

Every keyframe output from every encoder backend must be in the form:
`[SPS NALU][PPS NALU][IDR NALU]`. This is non-negotiable.

### Per-encoder screen content settings

Screen content (flat colors, sharp text edges, repeated UI elements) encodes very differently from
camera video. Each encoder has specific modes for this.

**openh264 (software fallback)**
```rust
// The single most important flag:
params.iUsageType = SCREEN_CONTENT_REAL_TIME;  // activates intra block copy, palette mode
// Rate control: fixed QP gives most predictable quality
params.iRCMode = RC_OFF_MODE;
params.iQP = 26;           // 0-51; 22-30 for desktop text
params.bEnableFrameSkip = false;   // never skip frames for interactive use
params.iComplexityMode = HIGH_COMPLEXITY;  // better quality, more CPU
params.iTemporalLayerNum = 1;      // no temporal scaling for low latency
// Note: SCREEN_CONTENT_REAL_TIME is ~3x slower than CAMERA_VIDEO_REAL_TIME.
// Acceptable for software fallback; GPU encoders don't have this tradeoff.
```

**NVENC (NVIDIA hardware)**
```rust
// No screen-specific tuning enum; use ultra-low-latency with AQ enabled
initParams.tuningInfo = NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY;
initParams.presetGUID = NV_ENC_PRESET_P4_GUID;  // balance of speed/quality

// Intra refresh (replaces periodic IDR)
h264Config.idrPeriod = NVENC_INFINITE_GOPLENGTH;
h264Config.enableIntraRefresh = 1;
h264Config.intraRefreshPeriod = 60;  // refresh window in frames
h264Config.intraRefreshCnt = 5;      // spread refresh over 5 frames
h264Config.outputRecoveryPointSEI = 1;
h264Config.repeatSPSPPS = 1;         // SPS+PPS on every IDR response

// Adaptive quantization — critical for text quality at given bitrate
rcParams.enableAQ = 1;
rcParams.aqStrength = 8;    // 1-15; redistributes bits from flat regions to text/edges

// VBV sized for 1 frame at target fps to minimize encode latency
rcParams.vbvBufferSize = averageBitRate / 60;
rcParams.vbvInitialDelay = rcParams.vbvBufferSize;
```

**VideoToolbox (macOS hardware)**
```rust
// Low-latency rate controller MUST be set at session creation time, not post-creation
encoderSpec[kVTVideoEncoderSpecification_EnableLowLatencyRateControl] = true;

// Post-creation properties:
kVTCompressionPropertyKey_AllowFrameReordering = false  // no B-frames
kVTCompressionPropertyKey_RealTime = true
kVTCompressionPropertyKey_MaxAllowedFrameQP = 40   // prevents quality collapse on text
kVTCompressionPropertyKey_MinAllowedFrameQP = 18
kVTCompressionPropertyKey_H264EntropyMode = kVTH264EntropyMode_CABAC  // 10-15% better compression
kVTCompressionPropertyKey_MaxFrameDelayCount = 0   // one-in-one-out mandatory for low latency
// No screen content mode key exists; Apple's hardware has internal heuristics
```

**AV1 — not viable as primary codec**
AV1 has genuine screen content coding tools (intra block copy, palette mode) that would give
significantly sharper text at lower bitrates. However: Safari does not support AV1 WebRTC encode
at all, and only decodes on M3+ Mac and iPhone 15 Pro and later. M1/M2 MacBooks cannot receive AV1.
This disqualifies it as the primary codec for a general product. H.264 remains correct.
AV1 may be offered as an opt-in for Chromium-only deployments in future.

---

## Phase 2: VideoSender — Bridge Between guacr and WebRTC

Add a `VideoSender` to the guacr handler API so handlers can push H.264 frames without
knowing about WebRTC internals.

### Changes to guacr-handlers

Add to `ProtocolHandler::connect` signature:

```rust
#[async_trait]
pub trait ProtocolHandler: Send + Sync {
    async fn connect(
        &self,
        params: HashMap<String, String>,
        guac_tx: mpsc::Sender<GuacInstruction>,
        guac_rx: mpsc::Receiver<GuacInstruction>,
        video_tx: Option<VideoSender>,   // NEW: None for terminal protocols
    ) -> Result<()>;
}
```

`VideoSender` is a thin wrapper:

```rust
pub struct VideoSender {
    track: Arc<TrackLocalStaticSample>,
}

impl VideoSender {
    pub async fn send(&self, frame: EncodedFrame) -> Result<()> {
        self.track.write_sample(&Sample {
            data: frame.data,
            duration: Duration::from_millis(16),  // ~60fps
            ..Default::default()
        }).await
    }

    pub fn on_pli(&self, cb: impl Fn() + Send + 'static) {
        // Register PLI handler → rate-limited encoder.request_keyframe()
        // See PLI rate limiting below — do not call request_keyframe() on every PLI.
    }

    pub fn on_remb(&self, cb: impl Fn(u32) + Send + 'static) {
        // Register REMB handler → call encoder.set_target_bitrate()
    }
}
```

**PLI rate limiting — required for correctness and security**

A PLI (Picture Loss Indication) is an RTCP message the browser sends when it misses a keyframe.
Without throttling, a burst of packet loss causes multiple simultaneous PLIs, each triggering a
full IDR. IDR frames are 10-30x larger than P-frames. Responding to every PLI floods the link
precisely when it is already congested, causing more loss, more PLIs — a spiral.

Rate limit: respond to at most 1 PLI per 500ms. Ignore PLIs received within 200ms of a
just-sent IDR. Production SFUs (mediasoup: 1/sec, others: 2-5 seconds) use similar limits.

```rust
struct PliThrottle {
    last_keyframe_at: Instant,
}

impl PliThrottle {
    const MIN_INTERVAL: Duration = Duration::from_millis(500);
    const POST_IDR_GRACE: Duration = Duration::from_millis(200);

    fn should_respond(&mut self) -> bool {
        let elapsed = self.last_keyframe_at.elapsed();
        if elapsed >= Self::MIN_INTERVAL {
            self.last_keyframe_at = Instant::now();
            true
        } else {
            false
        }
    }
}
```

**TWCC vs REMB — webrtc-rs 0.14 reality**

webrtc-rs 0.14 includes TWCC infrastructure (the `interceptor` crate stamps outgoing packets and
collects per-packet arrival feedback) but ships **no bandwidth estimator (GCC/GCC2)**. TWCC
feedback arrives via `read_rtcp()` as `TransportLayerCc` packets but nothing consumes it to
produce a bitrate estimate.

For Phase 1: call `configure_twcc_sender_only()` when building the interceptor registry. This
stamps outgoing RTP packets with transport-wide sequence numbers (2-4 bytes overhead per packet)
and tells the browser to send feedback. The browser's own congestion control benefits from seeing
these timestamps. Defer implementing a server-side GCC estimator until after end-to-end video
is working.

REMB (Receiver Estimated Max Bitrate) feedback is simpler — the browser computes the estimate and
sends it as a single value. The encoder can directly use it. Keep the REMB RTCP feedback entry
in the SDP codec parameters as the primary adaptive bitrate signal for now.

### Changes to keeper-pam-webrtc-rs (Tube)

In `IsolatedWebRTCAPI`, before creating the SDP offer, optionally add a video track:

```rust
pub struct IsolatedWebRTCAPI {
    // existing fields...
    video_track: Option<Arc<TrackLocalStaticSample>>,
}

impl IsolatedWebRTCAPI {
    pub fn with_video() -> Self {
        let video_track = Arc::new(TrackLocalStaticSample::new(
            RTPCodecParameters {
                capability: RTPCodecCapability {
                    mime_type: MIME_TYPE_H264.to_string(),
                    clock_rate: 90000,
                    channels: 0,
                    sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f".to_string(),
                    rtcp_feedbacks: vec![
                        RTCPFeedback { typ: "nack".into(), parameter: "".into() },
                        RTCPFeedback { typ: "nack".into(), parameter: "pli".into() },
                        RTCPFeedback { typ: "goog-remb".into(), parameter: "".into() },
                    ],
                },
                payload_type: 96,
                ..Default::default()
            },
            TrackID::new("video", "guacr-video"),
        ));
        // peer_connection.add_track(video_track.clone()) before creating offer
        Self { video_track: Some(video_track), ..Default::default() }
    }
}
```

The `video_track` handle is wrapped in `VideoSender` and passed to the guacr handler at
connection time.

### RegistryActor / TubeConfig changes

Add a `connection_type` enum to `TubeConfig`:

```rust
pub enum ConnectionMode {
    GuacamoleOnly,          // SSH, Telnet, database, TN3270 — data channel only
    GuacamoleWithVideo,     // RDP, VNC, RBI — data channel + RTP video track
}
```

Gateway code selects the mode based on protocol before calling `registry.create_tube()`.

---

## Compatibility Matrix

The vault must handle two fundamentally different session types. The selection is automatic —
the vault does not need to know in advance which type it will get.

| Session type | Server | SDP offer | Vault path |
|---|---|---|---|
| guacd proxy | Apache guacd + gateway proxy | `m=application` only (data channel) | `ontrack` never fires → Guacamole canvas (unchanged) |
| guacr, old vault | guacr handler, vault without video support | `m=application` only (GuacamoleOnly mode) | `ontrack` never fires → Guacamole canvas (unchanged) |
| guacr, new vault, encoder ok | guacr handler | `m=application` + `m=video` | `ontrack` fires → `<video>` element + cursor overlay |
| guacr, new vault, encoder fails | guacr handler, encoder init failure | `m=application` only (fell back) | `ontrack` never fires → Guacamole canvas (fallback) |
| guacr terminal (SSH, Telnet, etc.) | guacr handler | `m=application` only (GuacamoleOnly always) | `ontrack` never fires → Guacamole canvas |

The vault never needs to know which row it is in. `hasVideoTrack` controls everything.

guacd sessions are forever GuacamoleOnly — the Guacamole text protocol is fixed by the guacd
spec and there is nothing to change on the gateway side.

### Encoder failure fallback

If the encoder fails to initialize (no GPU, bad driver, missing library), the handler receives
`video_tx: None` and falls back to JPEG dirty-rect. The vault gets a GuacamoleOnly SDP offer,
`ontrack` never fires, canvas path runs. No user-visible error beyond slightly higher bandwidth.

This fallback must be implemented at the `ConnectionMode` selection point in the gateway before
calling `registry.create_tube()` — if encoder init fails, use `GuacamoleOnly` rather than
`GuacamoleWithVideo`.

---

## Phase 3: RDP Handler Changes (guacr-rdp)

The RDP handler (`crates/guacr-rdp/src/handler.rs`) is the first target. VNC and RBI follow the
same pattern.

### Both paths live in the handler simultaneously

The JPEG dirty-rect code is NOT removed. The handler selects the path based on `video_tx`:

```rust
async fn handle_screen_output(
    decoded_image_rx: mpsc::Receiver<DecodedImage>,
    video_tx: Option<VideoSender>,    // Some = H.264 path, None = JPEG path
    guac_tx: mpsc::Sender<GuacInstruction>,
    encoder: Option<Box<dyn VideoEncoder>>,
) {
    match video_tx {
        Some(video_tx) => video_encode_loop(decoded_image_rx, encoder.unwrap(), video_tx, guac_tx).await,
        None => jpeg_dirty_rect_loop(decoded_image_rx, guac_tx).await,  // existing code, unchanged
    }
}
```

`jpeg_dirty_rect_loop` is the existing JPEG encoding code moved into a named function — no
logic change. It runs whenever `video_tx` is None: guacd sessions, old vault clients, encoder
failure, terminal protocols.

### What the H.264 path adds

- `VideoEncoder` instance (platform-selected at startup, None if init fails)
- Full-frame encode on each IronRDP frame update
- RTCP read loop for PLI (rate-limited) and REMB (bitrate adjustment)

### What stays the same in both paths

- `cursor` instructions over Guacamole data channel
- `clipboard` instructions over Guacamole data channel
- `sync` instructions for frame timing
- `disconnect` instruction
- `key` and `mouse` input from browser
- Stream ID tracking (already correct at `stream_id: 1`)

### H.264 encoding loop

```rust
async fn video_encode_loop(
    mut decoded_image_rx: mpsc::Receiver<DecodedImage>,
    mut encoder: Box<dyn VideoEncoder>,
    video_tx: VideoSender,
    guac_tx: mpsc::Sender<GuacInstruction>,
) {
    let mut pli_throttle = PliThrottle::new();

    // RTCP read loop runs concurrently
    video_tx.on_pli(move || {
        if pli_throttle.should_respond() {
            encoder.request_keyframe();
        }
    });

    while let Some(image) = decoded_image_rx.recv().await {
        let frame = RgbaFrame {
            data: image.as_rgba(),
            width: image.width(),
            height: image.height(),
            timestamp_us: now_us(),
        };
        match encoder.encode(&frame) {
            Ok(encoded) => {
                video_tx.send(encoded).await.ok();
                guac_tx.send(GuacInstruction::sync(frame.timestamp_us / 1000)).await.ok();
            }
            Err(e) => warn!("encode failed: {}", e),
        }
    }
}
```

### Congestion response

REMB feedback adjusts `encoder.set_target_bitrate()`. The hard ceiling from `b=TIAS` prevents
the encoder from ever exceeding the server-enforced maximum regardless of REMB signals.

---

## Phase 4: Vault UI Changes

### 4a. PrivateTunnel.ts — add video transceiver to offer

**Initial negotiation**: the vault creates the first SDP offer. It advertises video support by
adding a `recvonly` video transceiver before calling `createOffer()`. This puts `m=video` in
the offer. The gateway decides whether to activate it in the answer.

**ICE restart**: either side can send an offer for ICE restart (the system requires
`trickle_ice=True` for this to work, per existing configuration). Once the video transceiver
is established, it persists through all ICE restarts — you do not call `addTransceiver` again.
Whichever side sends the restart offer includes the existing `m=video` section automatically,
and the other side answers. The video track continues uninterrupted.

```typescript
// In setupPeerConnection() — called ONCE during initial connection setup only:

// Add the video transceiver before the first createOffer() for protocols
// that could support video (RDP, VNC, RBI). The protocol is available as
// this._protocol from the session start parameters.
// For terminal protocols (SSH, Telnet, database, etc.) skip this entirely.
if (['rdp', 'vnc', 'rbi'].includes(this._protocol)) {
    this.peerConnection.addTransceiver('video', { direction: 'recvonly' })
}
// This is not called again for ICE restarts — the transceiver persists.

// ontrack fires once when the gateway first activates the video track.
// It does not re-fire on ICE restart — the track survives renegotiation.
this.peerConnection.ontrack = (event) => {
    if (event.track.kind === 'video') {
        this.onVideoTrack?.(new MediaStream([event.track]))
    }
}
```

No change needed to the ICE/STUN/TURN configuration, trickle ICE signaling, or ICE restart logic.

**Gateway decision logic** (server side, before `registry.create_tube()`):

The gateway receives the vault's initial offer and decides `ConnectionMode`. All conditions must
be true for video:

```
is guacr session?              (not a guacd proxy)
  AND protocol supports video?  (RDP, VNC, RBI — not SSH/Telnet/database)
  AND encoder initialized ok?   (GPU available, library loaded)
  AND offer contains m=video?   (new vault client)
→ GuacamoleWithVideo: answer includes video sender, video track activated
→ else GuacamoleOnly: m=video rejected in answer, browser falls back to canvas
```

The `ConnectionMode` is fixed for the lifetime of the session — ICE restarts do not re-evaluate
it. The gateway's ICE restart offers include `m=video sendonly` if the session is
`GuacamoleWithVideo`, maintaining the established direction.

### 4b. GuacamoleViewer.tsx — add <video> element and cursor overlay

```tsx
// Dual render surface:
//   - <video> element: receives RTP H.264 stream, rendered by GPU
//   - <canvas> overlay: cursor position, transparent background
//
// The canvas sits on top with pointer-events: none, so mouse events fall
// through to the Guacamole.Mouse handler below.

<div style={{ position: 'relative' }}>
  <video
    ref={videoRef}
    autoPlay
    playsInline
    muted
    style={{ width, height, display: hasVideoTrack ? 'block' : 'none' }}
  />
  <canvas
    ref={cursorCanvasRef}
    style={{
      position: 'absolute', top: 0, left: 0,
      pointerEvents: 'none',
      display: hasVideoTrack ? 'block' : 'none',
    }}
  />
  {/* Existing Guacamole canvas — hidden when video track active */}
  <div
    ref={guacDisplayRef}
    style={{ display: hasVideoTrack ? 'none' : 'block' }}
  />
</div>
```

When `ontrack` fires:

```typescript
tunnel.onVideoTrack = (stream) => {
    videoRef.current.srcObject = stream
    setHasVideoTrack(true)
}
```

When `tunnel.onVideoTrack` is not set (terminal protocols, old gateway), the existing Guacamole
canvas path is used unchanged.

### 4c. Cursor overlay

Guacamole `cursor` instructions still arrive over the data channel. In video mode, instead of
letting guacamole-common render to the main canvas, intercept `cursor` instructions and draw to the
cursor overlay canvas:

```typescript
guacClient.getDisplay().oncursor = (canvas, x, y) => {
    if (hasVideoTrack) {
        renderCursorToOverlay(cursorCanvasRef.current, canvas, mouseX - x, mouseY - y)
    }
    // else: let guacamole-common handle it normally
}
```

Mouse position tracking already exists in GuacamoleViewer.tsx; cursor overlay just needs the
hot-spot offset applied.

### 4d. img instruction suppression

When video track is active, the gateway no longer sends `img` instructions for screen content.
No change needed in the parser — the instructions simply won't arrive.

If the gateway falls back to Guacamole img (e.g., encoder initialization failure), the existing
canvas path handles it transparently.

### 4e. Jitter buffer target

Browsers buffer incoming video to smooth out network jitter. For remote desktop, latency matters
more than smoothness. Set `jitterBufferTarget` (Chrome 124+, Firefox) to minimize buffering.
Safari does not support it; the call is a graceful no-op there.

API note: Chrome renamed this from `playoutDelayHint` (seconds, float) to `jitterBufferTarget`
(milliseconds, integer) in Chrome 124. Handle both:

```typescript
function setJitterBufferTarget(receiver: RTCRtpReceiver, targetMs: number) {
    if ('jitterBufferTarget' in receiver) {
        (receiver as any).jitterBufferTarget = targetMs;       // Chrome 124+, Firefox
    } else if ('playoutDelayHint' in receiver) {
        (receiver as any).playoutDelayHint = targetMs / 1000;  // older Chrome (seconds)
    }
    // Safari: no-op
}

// Set immediately after ontrack fires:
peerConnection.ontrack = (event) => {
    if (event.track.kind === 'video') {
        setJitterBufferTarget(event.receiver, 0);  // minimize jitter buffer for RDP
        this.onVideoTrack?.(new MediaStream([event.track]))
    }
}
```

### 4f. Protocol routing on the gateway

The gateway needs to know whether to route a session to guacr or guacd. This is a gateway
configuration/routing concern, not a vault concern. Options:

- **Per-protocol routing**: SSH/RDP/VNC → guacr; legacy sessions → guacd
- **Feature flag**: `GUACR_ENABLED=true` environment variable gates guacr usage
- **Per-connection parameter**: vault can pass a `backend` field in the session start data

The vault does not need to know which backend is selected. It always includes `m=video` in the
offer for protocols that could support it (RDP, VNC, RBI). The gateway's answer activates or
rejects the video track based on its routing decision and encoder availability.

This means zero vault changes are needed when a gateway is upgraded from guacd-proxy to guacr —
the vault offer already has `m=video`, and the new gateway starts answering with a video sender.

---

## Phase 5: Session Recording

### How the current system works (guacd)

guacd writes recording bytes to a named pipe. Python `RecordingReader` reads from the pipe,
stream-encrypts with AES-GCM using a random per-session key, and uploads to the Keeper router
via WebSocket in real time. The recording key is wrapped with the resource and user record keys
so only the record owner can decrypt it. The upload URL encodes the recording type:

```
/api/device/recording/{conversation_uid}/{protocol}/{r_type}
```

Current r_types: `ses` (Guacamole visual), `tys` (typescript/terminal), `timing`, `sum` (AI).

### guacr approach: Rust owns the full recording pipeline

For guacr sessions, the Python gateway does **not** manage recording. No named pipes. The Rust
guacr handler handles capture → encrypt → upload directly. Python passes recording configuration
to the handler at session start and is done.

This is more reliable than named pipes: no cross-language IPC, no pipe buffer management, no
risk of the pipe filling and blocking the encoder.

### Recording configuration passed from Python to Rust at session start

The interface mirrors guacd params deliberately — a flat `HashMap<String, String>` of
recording parameters. This keeps the calling convention consistent and leaves the door open
for a future service that generates guacr params the same way guacd params are generated today.

```rust
// Same keys as guacd recording params, extended with guacr-specific fields:
// "recordingenabled"       → "true" / "false"
// "recordingname"          → filename stem (e.g., "2026-01-01T00-00-00-abc123")
// "recordingpath"          → directory path OR empty for router-only
// "recording_secret"       → base64(os.urandom(32)) — derived by Python, passed to Rust
// "recording_nonce"        → base64(os.urandom(12))
// "recording_associated"   → base64(JSON metadata: conversationUid, resourceUid, etc.)
// "recording_router_url"   → WebSocket upload endpoint
// "recording_auth_*"       → auth header values (Challenge, Signature, Authorization)
// "typescriptrecordingenabled" → "true" / "false" (terminal protocols only)
// "typescriptname"         → filename stem for .tys recording
// "typescriptpath"         → directory path for typescript recording
```

Python derives the keys and metadata exactly as it does today for guacd, then passes these
params to the guacr handler at session start via the Python bindings. The handler constructs
its `RecordingSink` from these params. No structured type crosses the Python/Rust boundary —
just the same flat string map.

The metadata format and encryption scheme are identical to the Python implementation so the
router and vault handle guacr recordings the same way as guacd recordings.

### Rust recording pipeline

```
guacr handler
    ├── live path:   encoder → VideoSender → RTCRtpSender → browser
    └── record path: encoder → RecordingSink (trait) → encrypted bytes → destination
```

`RecordingSink` is a trait. The handler writes to it without knowing the destination:

```rust
#[async_trait]
pub trait RecordingSink: Send {
    async fn write(&mut self, data: &[u8]) -> Result<()>;
    async fn finalize(self: Box<Self>) -> Result<()>;
}
```

Three implementations, selected at session start from `RecordingConfig`:

**`WebSocketSink`** — streams to router (production default)
```rust
pub struct WebSocketSink {
    encryptor: StreamEncryptor,
    ws_tx: SplitSink<WebSocket, Message>,
    buffer: BytesMut,
    buffer_threshold: usize,  // flush at KROUTER_FRAME_SIZE
}
```

**`FileSink`** — writes to a local file (debug / fallback)
```rust
pub struct FileSink {
    encryptor: StreamEncryptor,
    file: tokio::fs::File,
}
// Writes the same encrypted format as WebSocketSink.
// The resulting file can be uploaded manually or replayed by the vault.
```

**`DualSink`** — writes to both simultaneously
```rust
pub struct DualSink {
    ws: WebSocketSink,
    file: FileSink,
}
// If the WebSocket upload fails, the file copy is still intact.
// Useful for high-reliability deployments or debugging upload issues.
```

`RecordingConfig` selects the sink:

```rust
pub enum RecordingDestination {
    Router { ws_url: String, auth_headers: HashMap<String, String> },
    File   { path: PathBuf },
    Both   { ws_url: String, auth_headers: HashMap<String, String>, path: PathBuf },
}
```

`StreamEncryptor` in Rust uses the `aes-gcm` crate with the same AES-256-GCM streaming approach
as the Python `StreamEncryptor`. The wire format must match exactly so the router can decrypt
using the same key material. The file format is identical to the WebSocket format — the same
encrypted bytes, so a file recording can be uploaded to the router later using the same tooling.

**Upload failure handling**: `WebSocketSink` must handle mid-session connection drops. The Python
`log_recording` has a `TUNNEL_CLOSE_TIMEOUT` grace period and retries. The Rust equivalent needs:
- Reconnect with exponential backoff on WebSocket disconnect
- Buffer unacknowledged frames in memory during reconnect attempts
- If reconnect fails: **terminate the session**

Recording failure is a hard stop for PAM. If the recording cannot be delivered, the session
must not continue — a session with no audit trail is not an authorized session. The handler
signals session termination via the existing `tunnel_close_callback` with reason
`CloseConnectionReason::RecordingFailed`.

### What gets recorded per protocol type

**Graphical protocols (RDP, VNC, RBI) — r_type: `mp4`**

The recording encoder runs alongside the live encoder, both receiving the same `RgbaFrame`:

```
RgbaFrame → live encoder (60fps, ~4 Mbps)  → VideoSender → browser
          → record encoder (15fps, ~1 Mbps) → RecordingSink → router
```

H.264's P-frame compression applies to the recording stream too. The recording encoder shares
the `VideoEncoder` trait but configured for storage (lower fps, lower bitrate, keyframe every
2 seconds for seeking).

Input events (`key`, `mouse`) from the data channel are embedded as a timed data track inside
the MP4 container — no separate upload. MP4 supports multiple tracks natively; the key/mouse
events go in as a timed text/data track synchronized to the video PTS. One file, one upload.
The vault player extracts the data track for the keystroke log and heatmap.

The same `key` instruction intercept also feeds the threat detection system — one intercept,
two consumers (recording + threat detection). See Phase 6.

**Terminal protocols (SSH, Telnet, TN3270, TN5250) — r_type: `tys` + `timing`**

SSH and Telnet handlers have access to raw PTY bytes — the actual text before rendering. Write
those to the recording sink in typescript format (same as guacd produces). The router already
accepts `tys` and `timing` r_types. No new infrastructure needed on the server side.

The typescript format is: raw PTY output bytes, with a separate `.timing` file recording elapsed
time per chunk. The vault's existing typescript player handles this.

**Database handlers**: database sessions speak SQL protocol, not PTY. They do not have a raw
byte stream equivalent to PTY output. Recording for database sessions is a separate question
not covered by this plan — use the Guacamole `.ses` fallback path for now (JPEG dirty-rect
recording via the data channel, same as current).

### New recording types to register on the router

| r_type | Format | Source | Vault player |
|--------|--------|--------|--------------|
| `ses` | Guacamole | guacd (unchanged) | GuacamolePlayer (unchanged) |
| `tys` + `timing` | Typescript | guacd OR guacr terminal | Existing typescript player |
| `mp4` | H.264 + embedded data track | guacr graphical | New VideoSessionPlayer |

Only `mp4` is a new r_type. The upload WebSocket pattern is identical to existing r_types.
Key/mouse events are embedded inside the MP4 — no separate file or upload needed.

### Vault player for mp4 recordings

The vault downloads `{session_uid}.mp4` from the router, then:
- Plays the video track in a `<video>` element (seek via `video.currentTime`)
- Extracts the embedded data track for the keystroke log and activity heatmap
- Playback speed via `video.playbackRate`

The existing `GuacamolePlayer` is unchanged. `VideoSessionPlayer` is a new component for
`mp4` recordings only.

### What does NOT change

- guacd recording pipeline — named pipes, Python `RecordingReader`, upload — all unchanged
- Encryption scheme — same AES-GCM, same key wrapping, same metadata format
- Router recording storage — same endpoint, same pattern, two new r_types added
- Vault `GuacamolePlayer` — unchanged for `ses` and `tys` recordings

---

## Phase 6: DLP and Threat Detection

### Threat detection

`guacr-threat-detection` is entirely text-based. It analyzes keystroke sequences and terminal
output by sending them to a BAML REST API (LLM). It has zero interaction with images, pixels,
or video frames.

For graphical protocols (RDP, VNC, RBI), the `key` instructions from the data channel are
intercepted at the handler level for recording (embedded in the MP4 data track). This same
intercept point is the correct place to also feed threat detection. One keystroke intercept,
two consumers:

```
key/mouse instructions (DataChannel)
    ├── → MP4 data track (recording)
    └── → ThreatDetector → BAML API (LLM) → risk score → block/allow/terminate

pixels → H.264 encoder → VideoSender → browser  (separate pipeline, unaffected)
```

Threat detection is not yet integrated into any handler (it's feature-gated, production-ready,
waiting to be wired). The intercept point established for recording in this plan is the natural
integration point for threat detection on graphical protocols when that work happens.

### DLP (Data Loss Prevention)

DLP operates server-side before encoding, which is unchanged by this plan. The gateway renders
the screen to RGBA pixels, can inspect/redact them at that point, and then passes them to the
encoder. The security boundary is preserved: the browser only ever receives pixels (or H.264
frames derived from those pixels), never raw text or credentials.

For graphical protocols (RDP/VNC/RBI), DLP at the pixel level (blurring regions before encode)
remains valid and is unaffected by the transport change. The integration point is still:
```
RDP pixels → DLP inspect/redact → H.264 encoder → VideoSender
```

---

## Security Hardening

### Hard bitrate ceiling

The REMB adaptive bitrate path takes browser-reported bandwidth as input. A client sending fake
REMB claiming very high bandwidth would drive the encoder to maximum quality, spiking server CPU
and producing oversized frames. Enforce a hard server-side ceiling regardless of REMB signals.

**Server side — SDP `b=TIAS` injection (most reliable, enforced at transport):**

Inject `b=TIAS` (Transport Independent Application Specific, bits/sec) into the SDP answer before
setting it as the local description. This is enforced at the RTP layer and works in all browsers
including Firefox (which ignores `setParameters()` unreliably).

```rust
// In IsolatedWebRTCAPI, after generating the answer SDP:
fn inject_bitrate_ceiling(sdp: &str, max_bps: u32) -> String {
    // Insert b=TIAS after the complete m=video line (not mid-line).
    // SDP lines end with \r\n. Split on line boundaries, insert after
    // any line that starts with "m=video".
    let insertion = format!("b=TIAS:{}\r\n", max_bps);
    let mut result = String::with_capacity(sdp.len() + insertion.len());
    for line in sdp.split_inclusive("\r\n") {
        result.push_str(line);
        if line.starts_with("m=video") {
            result.push_str(&insertion);
        }
    }
    result
}
```

The vault has no video sender — it is `recvonly`. `RTCRtpSender.setParameters()` is not
applicable here. SDP `b=TIAS` injected server-side is the only enforcement mechanism needed.

### E2E encryption

Standard WebRTC uses DTLS-SRTP, which encrypts between the browser and the WebRTC endpoint
(the Keeper gateway). In this architecture that **is** end-to-end: the gateway is also the source
of the pixels. There is no relay or SFU that sees decrypted media. DTLS-SRTP is sufficient.

Discord's DAVE protocol (Insertable Streams + MLS key exchange) addresses a different threat model:
an SFU relay that can see DTLS-decrypted media. That threat does not exist here because the gateway
that terminates DTLS is the same process that generates the screen content. If a relay or SFU is
ever added to this architecture, the security model must be revisited.

---

## Implementation Order

1. `crates/guacr-encoder/` with openh264 software encoder only — no GPU deps, works in CI
2. `VideoSender` and `ConnectionMode` in `keeper-pam-webrtc-rs`
3. Wire `VideoSender` into `guacr-rdp` handler, test against xrdp dev container
4. Vault changes: `addTransceiver` in `PrivateTunnel.ts`, `<video>` element + cursor overlay in `GuacamoleViewer.tsx`, `jitterBufferTarget`
5. End-to-end test: RDP session with H.264 transport, no recording yet
6. Hardware encoder backends: VideoToolbox first (dev machines), then VAAPI, then NVENC
7. Session recording:
   a. `crates/guacr-recorder/` — `RecordingSink` trait, `StreamEncryptor`, `WebSocketSink`, `FileSink`, `DualSink`, MP4 muxer with embedded data track
   b. Python gateway changes — pass recording params (flat map, same style as guacd params) at guacr session start
   c. Wire recording into `guacr-rdp` — dual encoder (live + record), key/mouse intercept → MP4 data track + threat detection
   d. Router changes — accept `mp4` r_type
   e. Vault `VideoSessionPlayer` component — MP4 playback + embedded data track for keystroke log
8. VNC and RBI handlers (same pattern as RDP)
9. Terminal recording — `tys`/`timing` from guacr SSH and Telnet handlers

---

## What Stays the Same (Do Not Touch)

- Guacamole text protocol for all control messages (`key`, `mouse`, `clipboard`, `cursor`, `size`, `sync`, `disconnect`) — replacing this is a separate future effort, see `crates/guacr-protocol/docs/BINARY_PROTOCOL_SPEC.md`
- WebRTC ICE/STUN/TURN configuration
- Python bindings public API surface for WebRTC tubes (note: Python bindings DO need a new internal call to pass `RecordingConfig` at session start — this is an addition, not a breaking change)
- Data channel encryption and session key derivation
- `TCP_NODELAY` and no-flush pattern on data channels
- Registry actor, RAII tube lifecycle, backpressure

### Why terminal protocols are excluded from video transport

SSH, Telnet, TN3270, TN5250, and database handlers keep JPEG dirty-rect for screen content.

**Bandwidth savings are small.** Terminal sessions already send very little data. When nothing
changes, nothing is sent. When a character appears, you send a small JPEG of a few character cells.
The current access pattern is well-matched to dirty-rect encoding. H.264 P-frames help with
scrolling but the savings are modest compared to graphical protocols.

**Text quality is more sensitive to compression artifacts.** H.264 quantization causes ringing on
sharp character edges. On a graphical desktop a slightly soft button shadow goes unnoticed; a
slightly blurry monospace font at a shell prompt is immediately visible.

**TN3270/TN5250** is the hardest case — mainframe screens have fixed-position labeled fields that
operators read precisely. Character accuracy is not negotiable there.

The risk/reward ratio is poor. Revisit only if bandwidth for terminal sessions becomes a real
problem.

---

## Files To Create

| File | Purpose |
|------|---------|
| `crates/guacr-encoder/Cargo.toml` | New encoder crate |
| `crates/guacr-encoder/src/lib.rs` | `VideoEncoder` trait, `RgbaFrame`, `EncodedFrame` |
| `crates/guacr-encoder/src/openh264.rs` | Software fallback encoder |
| `crates/guacr-encoder/src/videotoolbox.rs` | macOS hardware encoder |
| `crates/guacr-encoder/src/vaapi.rs` | Linux GPU encoder |
| `crates/guacr-encoder/src/nvenc.rs` | NVIDIA encoder |
| `crates/guacr-recorder/Cargo.toml` | New recording crate |
| `crates/guacr-recorder/src/lib.rs` | `RecordingSink` trait, flat params constructor, `StreamEncryptor` |
| `crates/guacr-recorder/src/websocket_sink.rs` | `WebSocketSink` — upload to router |
| `crates/guacr-recorder/src/file_sink.rs` | `FileSink` — write to local file |
| `crates/guacr-recorder/src/dual_sink.rs` | `DualSink` — both simultaneously |
| `vault/.../VideoSessionPlayer.tsx` | New player for mp4 recordings (video + embedded data track) |

## Files To Modify

| File | Change |
|------|--------|
| `Cargo.toml` (workspace) | Add `crates/guacr-encoder` and `crates/guacr-recorder` members |
| `crates/guacr-handlers/src/lib.rs` | Add `video_tx: Option<VideoSender>` to `ProtocolHandler::connect` |
| `crates/keeper-pam-webrtc-rs/src/webrtc_core.rs` | Add video track to `IsolatedWebRTCAPI`, `b=TIAS` injection |
| `crates/keeper-pam-webrtc-rs/src/tube.rs` | Add `VideoSender` field, expose via `TubeHandle` |
| `crates/keeper-pam-webrtc-rs/src/tube_registry.rs` | Accept `ConnectionMode` in tube config |
| `crates/guacr-rdp/src/handler.rs` | Add H.264 encode loop alongside JPEG path, recording encoder, events capture |
| `crates/guacr/Cargo.toml` | Enable encoder feature flags |
| `vault/.../PrivateTunnel.ts` | `addTransceiver('video', {direction:'recvonly'})` for RDP/VNC/RBI, `ontrack` handler |
| `vault/.../GuacamoleViewer.tsx` | `<video>` element, cursor overlay canvas, `jitterBufferTarget` |
| `keeper_pam_gateway/...` (Python) | Pass `RecordingConfig` to guacr session start |
| Router | Accept `mp4` r_type at recording endpoint |

# ICE Restart & Trickle ICE Integration Guide

## WebRTC Connection Recovery Integration

This guide covers integrating with the library's ICE restart and trickle ICE features:
- **Python**: Using the library's Python bindings directly
- **JavaScript**: Peer-side WebRTC implementation connecting to this library

---

## 🎯 **What This Solves**

### **Network Reliability Issues:**
- ❌ **NAT timeout disconnections** (19-minute NAT timeout)
- ❌ **Network topology changes** (ethernet ↔ wifi switching) 
- ❌ **Connection hangs** on network interruption
- ❌ **Manual reconnection required** after network issues

### **Solution Provided:**
- ✅ **Automatic NAT timeout prevention** (5-minute keepalive)
- ✅ **Smart ICE restart** with exponential backoff
- ✅ **Network change recovery** (automatic reconnection)
- ✅ **Production-grade reliability** with comprehensive logging

---

## ⚠️ **Prerequisites & Requirements**

### **CRITICAL: Trickle ICE Required for ICE Restart**

ICE restart functionality **requires** `trickle_ice=True`. This is a fundamental requirement due to RFC 5245 ICE restart protocol:

**❌ Will NOT Work:**
```python
tube_info = registry.create_tube(
    trickle_ice=False,  # ❌ ICE restart disabled!
    # ... other params
)
# Network changes → Connection closes gracefully (no restart attempted)
```

**✅ Correct Configuration:**
```python
tube_info = registry.create_tube(
    trickle_ice=True,   # ✅ ICE restart enabled
    # ... other params
)
# Network changes → ICE restart attempted → Connection recovers
```

**Why Trickle ICE is Required:**
- ICE restart generates new offer with fresh ICE credentials
- Offer must be sent to remote peer BEFORE candidate gathering completes
- Non-trickle ICE waits for ALL candidates (15-30 seconds) before returning SDP
- By the time SDP is ready, connection may be dead and timing is unpredictable
- Trickle ICE returns SDP immediately while gathering candidates in parallel

**What Happens Without Trickle ICE:**
- Network changes are detected ✅
- ICE restart is **blocked** with clear log message ✅
- Connection closes gracefully without restart attempts ✅
- No "pingAllCandidates" spam or hanging connections ✅

---

## 🚀 **Quick Start**

### **Python Integration**

```python
import keeper_pam_webrtc_rs

class WebRTCManager:
    def __init__(self):
        self.registry = keeper_pam_webrtc_rs.PyTubeRegistry()
        self.peer_connections = {}
        self.connection_states = {}
        
        # Configure for production
        self.registry.set_resource_limits({
            'max_concurrent_sockets': 2000,
            'max_concurrent_ice_agents': 1000,
            'ice_keepalive_enabled': True,        # NAT timeout prevention
            'ice_keepalive_interval': 300,        # 5 minutes
            'session_timeout': 3600,              # 1 hour
            'connection_health_check_interval': 120  # 2 minutes
        })
    
    def create_connection(self, conversation_id, is_server=True, offer_sdp=None):
        """Create WebRTC connection with ICE restart enabled"""
        settings = {
            "conversationType": "tunnel",
            "local_listen_addr": "127.0.0.1:0" if is_server else None
        }
        
        tube_info = self.registry.create_tube(
            conversation_id=conversation_id,
            settings=settings,
            trickle_ice=True,  # CRITICAL: Enable for best performance
            callback_token="your-callback-token",
            krelay_server="your.relay.server.com",
            client_version="your-client-v1.0",
            ksm_config="your-ksm-config",
            offer=offer_sdp,  # None for server, SDP for client
            signal_callback=self.handle_webrtc_signal
        )
        
        tube_id = tube_info['tube_id']
        self.connection_states[tube_id] = 'initializing'
        
        return tube_info
    
    def handle_webrtc_signal(self, signal_dict):
        """Handle WebRTC signaling with recovery monitoring"""
        tube_id = signal_dict.get('tube_id')
        kind = signal_dict.get('kind')
        data = signal_dict.get('data')
        
        if kind == "connection_state_changed":
            old_state = self.connection_states.get(tube_id, 'unknown')
            new_state = data.lower()
            self.connection_states[tube_id] = new_state
            
            print(f"🔄 Connection {tube_id}: {old_state} → {new_state}")
            
            if new_state == "connected":
                print(f"✅ Connected: {tube_id} - Keepalive automatically activated")
                self.on_connection_established(tube_id)
                
            elif new_state == "disconnected":
                print(f"⚠️  Disconnected: {tube_id} - ICE restart will attempt recovery")
                self.on_connection_lost(tube_id)
                
            elif new_state == "failed":
                print(f"❌ Failed: {tube_id} - Check network/configuration")
                self.on_connection_failed(tube_id)
        
        elif kind == "icecandidate":
            # ICE candidate exchange (automatic in trickle mode)
            peer_tube_id = self.get_peer_tube_id(tube_id)
            if peer_tube_id:
                try:
                    self.registry.add_ice_candidate(peer_tube_id, data)
                    if data:
                        print(f"🧊 ICE candidate: {tube_id} → {peer_tube_id}")
                    else:
                        print(f"🏁 ICE gathering complete: {tube_id}")
                except Exception as e:
                    print(f"❌ ICE candidate relay failed: {e}")

        elif kind == "ice_restart_offer":
            # ⚡ NEW: ICE restart offer - forward to remote peer
            conversation_id = signal_dict.get('conversation_id')
            offer_sdp_base64 = data  # Already base64 encoded

            print(f"🔄 ICE restart offer received (tube: {tube_id})")

            # Forward to remote peer via your signaling mechanism
            self.send_to_remote_peer({
                'action': 'ice_restart',
                'tube_id': tube_id,
                'conversation_id': conversation_id,
                'offer': offer_sdp_base64
            })

    async def handle_ice_restart_answer(self, tube_id, answer_sdp_base64):
        """Handle ICE restart answer from remote peer"""
        print(f"📥 Applying ICE restart answer (tube: {tube_id})")
        try:
            # Rust auto-detects this completes ICE restart
            self.registry.set_remote_description(tube_id, answer_sdp_base64, is_answer=True)
            print(f"✅ ICE restart completed (tube: {tube_id})")
        except Exception as e:
            print(f"❌ ICE restart answer failed: {e}")

---

### **Signal Types Reference**

The library sends these signal types to your Python callback:

| Signal Type | Description | Data Format | Required Action |
|------------|-------------|-------------|-----------------|
| `connection_state_changed` | Peer connection state changed | `"connected"`, `"disconnected"`, `"failed"`, etc. | Update UI/monitoring |
| `icecandidate` | ICE candidate discovered | SDP candidate string (or empty for end) | Forward to remote peer immediately |
| `ice_restart_offer` | **ICE restart offer generated** | Base64-encoded SDP offer | **Forward to remote peer, apply answer** |
| `channel_closed` | Data channel closed | JSON with close reason | Cleanup resources |

#### **ice_restart_offer Signal Structure:**
```python
{
    'tube_id': 'fef8e337-423f-4071-9d43-ae4fb0b9172f',
    'kind': 'ice_restart_offer',
    'data': 'dmV...==',  # Base64-encoded offer SDP
    'conversation_id': 'M0xZ4rCngyOpC4KNvz83Cw==',
    'progress_flag': 2,  # PROGRESS - awaiting answer
    'progress_status': 'ICE restart offer sent, awaiting answer',
    'is_ok': True
}
```

**Critical:** You have **15 seconds** to forward the offer and apply the answer, or the connection will timeout.

---

    def monitor_connection_health(self, tube_id):
        """Monitor connection health and recovery"""
        import threading
        import time
        
        def health_check():
            recovery_start = None
            while tube_id in self.connection_states:
                try:
                    state = self.registry.get_connection_state(tube_id)
                    current_state = self.connection_states.get(tube_id, 'unknown')
                    
                    if state.lower() == "connected" and recovery_start:
                        recovery_time = time.time() - recovery_start
                        print(f"🚀 Recovery successful: {tube_id} in {recovery_time:.2f}s")
                        recovery_start = None
                    elif state.lower() in ["disconnected", "failed"] and not recovery_start:
                        recovery_start = time.time()
                        print(f"🔧 Recovery attempting: {tube_id}")
                    
                    time.sleep(30)  # Check every 30 seconds
                except Exception as e:
                    print(f"Health check error for {tube_id}: {e}")
                    break
        
        threading.Thread(target=health_check, daemon=True).start()
```

### **JavaScript Peer Integration**

**JavaScript runs as the WebRTC peer connecting TO this library (not using it)**

```javascript
class RustLibraryWebRTCPeer {
    constructor() {
        this.peerConnection = null;
        this.dataChannel = null;
        this.connectionState = 'disconnected';
    }
    
    async connectToRustLibrary(offerSdp) {
        // JavaScript peer connects to the Rust library
        // The Rust library generates offers, JavaScript responds with answers
        this.peerConnection = new RTCPeerConnection({
            iceServers: [
                { urls: 'stun:stun.l.google.com:19302' },
                { urls: 'stun:stun1.l.google.com:19302' }
            ],
            iceCandidatePoolSize: 10,  // Enable candidate pre-gathering for trickle ICE
            bundlePolicy: 'max-bundle',
            rtcpMuxPolicy: 'require'
        });
        
        // Set up event handlers for connecting to Rust library
        this.setupEventHandlers();
        
        // Process offer from Rust library
        await this.peerConnection.setRemoteDescription({
            type: 'offer',
            sdp: offerSdp
        });
        
        // Create answer to send back to Rust library
        const answer = await this.peerConnection.createAnswer();
        await this.peerConnection.setLocalDescription(answer);
        
        console.log('📤 Answer SDP ready to send to Rust library');
        
        return {
            answerSdp: answer.sdp,
            peerConnection: this.peerConnection
        };
    }
    
    setupEventHandlers() {
        // Connection state monitoring (Rust library handles ICE restart automatically)
        this.peerConnection.onconnectionstatechange = () => {
            const oldState = this.connectionState;
            const newState = this.peerConnection.connectionState;
            this.connectionState = newState;
            
            console.log(`🔄 Connection to Rust library: ${oldState} → ${newState}`);
            
            switch (newState) {
                case 'connected':
                    console.log('✅ Connected to Rust library - NAT keepalive automatically activated');
                    this.onConnectedToRustLibrary();
                    break;
                case 'disconnected':
                    console.log('⚠️  Disconnected from Rust library - Library will attempt ICE restart');
                    this.onDisconnectedFromRustLibrary();
                    break;
                case 'failed':
                    console.log('❌ Connection to Rust library failed');
                    this.onConnectionFailedToRustLibrary();
                    break;
            }
        };
        
        // Trickle ICE: Send candidates to Rust library as they're discovered
        this.peerConnection.onicecandidate = (event) => {
            if (event.candidate) {
                console.log('🧊 Sending ICE candidate to Rust library');
                this.sendIceCandidateToRustLibrary(event.candidate.candidate);
            } else {
                console.log('🏁 ICE gathering complete - notifying Rust library');
                this.sendIceCandidateToRustLibrary(''); // End-of-candidates
            }
        };
        
        // ICE connection state monitoring
        this.peerConnection.oniceconnectionstatechange = () => {
            console.log(`🧊 ICE connection state: ${this.peerConnection.iceConnectionState}`);
            
            if (this.peerConnection.iceConnectionState === 'failed') {
                console.log('🔧 ICE connection failed - Rust library will handle restart');
                // Note: JavaScript peer doesn't initiate ICE restart
                // The Rust library detects the failure and handles restart automatically
            }
        };
        
        // Data channel from Rust library
        this.peerConnection.ondatachannel = (event) => {
            this.dataChannel = event.channel;
            console.log('📡 Received data channel from Rust library');
            this.setupDataChannelHandlers();
        };
    }
    
    // Handle ICE restart initiated by Rust library
    // This is called when you receive an 'ice_restart_offer' signal from Python
    async handleIceRestartOfferFromRustLibrary(newOfferSdp) {
        try {
            console.log('📥 Received ICE restart offer from Rust library (via Python ice_restart_offer signal)');

            // Process new offer from Rust library
            await this.peerConnection.setRemoteDescription({
                type: 'offer',
                sdp: newOfferSdp
            });

            // Generate new answer
            const answer = await this.peerConnection.createAnswer();
            await this.peerConnection.setLocalDescription(answer);

            // Send answer back to Rust library through your signaling mechanism
            // Python will call registry.set_remote_description(tube_id, answer, is_answer=True)
            this.sendAnswerToRustLibrary(answer.sdp);

            console.log('📤 ICE restart answer sent back to Rust library');
            console.log('⏱️  Answer must reach Rust within 15 seconds or connection will timeout');
        } catch (error) {
            console.error('❌ Failed to handle ICE restart from Rust library:', error);
        }
    }
    
    // Add ICE candidate received from Rust library
    addIceCandidateFromRustLibrary(candidateString) {
        if (!candidateString) {
            // End-of-candidates signal from Rust library
            console.log('🏁 End of candidates from Rust library');
            return;
        }
        
        try {
            const candidate = new RTCIceCandidate({
                candidate: candidateString,
                sdpMid: '0',
                sdpMLineIndex: 0
            });
            
            this.peerConnection.addIceCandidate(candidate);
            console.log('➕ Added ICE candidate from Rust library');
        } catch (error) {
            console.error('❌ Failed to add ICE candidate from Rust library:', error);
        }
    }
    
    setupDataChannelHandlers() {
        this.dataChannel.onopen = () => {
            console.log('📡 Data channel to Rust library opened');
        };
        
        this.dataChannel.onclose = () => {
            console.log('📡 Data channel to Rust library closed');
        };
        
        this.dataChannel.onerror = (error) => {
            console.error('❌ Data channel error with Rust library:', error);
        };
        
        this.dataChannel.onmessage = (event) => {
            // Handle data received from Rust library
            this.handleDataFromRustLibrary(event.data);
        };
    }
    
    // Integration points - implement these based on your signaling mechanism
    sendIceCandidateToRustLibrary(candidate) {
        // Send ICE candidate to Rust library through your signaling system
        // This could be WebSocket, HTTP API, or other signaling mechanism
        console.log('→ Sending ICE candidate to Rust library');
        // Implementation depends on your signaling system
    }
    
    sendAnswerToRustLibrary(answerSdp) {
        // Send SDP answer back to Rust library
        console.log('→ Sending answer SDP to Rust library');
        // Implementation depends on your signaling system
    }
    
    sendDataToRustLibrary(data) {
        // Send data through data channel to Rust library
        if (this.dataChannel && this.dataChannel.readyState === 'open') {
            this.dataChannel.send(data);
        }
    }
    
    handleDataFromRustLibrary(data) {
        // Process data received from Rust library through data channel
        console.log('← Received data from Rust library:', data.byteLength, 'bytes');
        // Handle the data based on your application needs
    }
    
    // Event handlers - implement based on your application needs
    onConnectedToRustLibrary() {
        console.log('🎉 Successfully connected to Rust library');
        // Rust library now handles keepalive automatically
    }
    
    onDisconnectedFromRustLibrary() {
        console.log('🔄 Disconnected from Rust library - automatic recovery in progress');
        // Rust library will attempt ICE restart automatically
    }
    
    onConnectionFailedToRustLibrary() {
        console.log('💥 Connection to Rust library failed');
        // Handle connection failure (may need to restart signaling process)
    }
}

// Usage example - JavaScript peer connecting to Rust library
async function connectToRustLibrary() {
    const peer = new RustLibraryWebRTCPeer();
    
    // 1. Receive offer from Rust library through your signaling system
    const offerFromRustLibrary = await receiveOfferFromSignalingSystem();
    
    // 2. Process offer and generate answer
    const result = await peer.connectToRustLibrary(offerFromRustLibrary.sdp);
    
    // 3. Send answer back to Rust library
    await sendAnswerToSignalingSystem(result.answerSdp);
    
    // 4. Handle ICE candidates (trickle ICE)
    peer.onIceCandidate = (candidate) => {
        sendIceCandidateToSignalingSystem(candidate);
    };
    
    // 5. Connection established - Rust library handles keepalive and recovery
    console.log('🎉 Connected to Rust library with automatic recovery enabled');
}

// Signaling system implementation (you implement these)
async function receiveOfferFromSignalingSystem() {
    // Receive offer from Rust library through WebSocket, HTTP, etc.
    return { sdp: "..." };
}

async function sendAnswerToSignalingSystem(answerSdp) {
    // Send answer back to Rust library
}

async function sendIceCandidateToSignalingSystem(candidate) {
    // Send ICE candidate to Rust library
}

async function receiveIceCandidateFromSignalingSystem() {
    // Receive ICE candidate from Rust library
}
```

---

## 🔄 **Trickle ICE Best Practices**

### **Why Trickle ICE?**

**Traditional ICE (Non-Trickle):**
```
[Gather ALL candidates] → [Complete SDP] → [Send offer] → [Wait for answer]
⏱️  Slow: 5-15 seconds before connection attempt
```

**Trickle ICE (Recommended):**
```
[Send initial SDP] → [Stream candidates as found] → [Start connecting immediately]
⚡ Fast: Connection starts in <1 second
```

### **Implementation Pattern**

**Python (using the library):**
```python
# ✅ CORRECT: Enable trickle ICE
tube_info = registry.create_tube(
    conversation_id="my-connection",
    settings=settings,
    trickle_ice=True,  # Enable streaming candidates
    # ... other params
)

# Handle streaming candidates
def handle_webrtc_signal(signal_dict):
    if signal_dict.get('kind') == 'icecandidate':
        candidate = signal_dict.get('data')
        peer_tube_id = get_peer_tube_id(signal_dict.get('tube_id'))
        
        if peer_tube_id:
            # Forward immediately to JavaScript peer - don't buffer
            send_candidate_to_javascript_peer(candidate)
```

**JavaScript (connecting to the library):**
```javascript
// ✅ CORRECT: Handle streaming candidates from Rust library
peerConnection.onicecandidate = (event) => {
    if (event.candidate) {
        // Send immediately to Rust library - don't wait for all candidates
        sendCandidateToRustLibrary(event.candidate.candidate);
    } else {
        // Signal end of candidates to Rust library
        sendCandidateToRustLibrary('');
    }
};

// Handle candidates received from Rust library
function onIceCandidateFromRustLibrary(candidateString) {
    if (candidateString) {
        const candidate = new RTCIceCandidate({
            candidate: candidateString,
            sdpMid: '0',
            sdpMLineIndex: 0
        });
        peerConnection.addIceCandidate(candidate);
    }
}
```

### **Common Mistakes**

**Python (using the library):**
```python
# ❌ WRONG: Disabling trickle ICE
trickle_ice=False  # Slow connection establishment

# ❌ WRONG: Buffering candidates
candidates = []
if signal_dict.get('kind') == 'icecandidate':
    candidates.append(signal_dict.get('data'))  # Don't buffer!

# ❌ WRONG: Waiting for all candidates
if len(candidates) >= 10:  # Don't wait!
    for candidate in candidates:
        send_candidate_to_javascript_peer(candidate)
```

**JavaScript (connecting to the library):**
```javascript
// ❌ WRONG: Buffering candidates before sending to Rust library
const candidateBuffer = [];
peerConnection.onicecandidate = (event) => {
    if (event.candidate) {
        candidateBuffer.push(event.candidate.candidate);  // Don't buffer!
    }
};

// ❌ WRONG: Waiting for all candidates
setTimeout(() => {
    candidateBuffer.forEach(candidate => {  // Don't wait!
        sendCandidateToRustLibrary(candidate);
    });
}, 5000);

// ❌ WRONG: Not handling ICE restart from Rust library
// JavaScript peers must be ready to process new offers from Rust library for ICE restart
```

---

## 🔧 **ICE Restart Integration**

### **Automatic Recovery**

The Rust library handles ICE restart automatically:

**Triggers:**
- Network interface changes (ethernet ↔ wifi)
- NAT timeout (prevented by keepalive)
- Connection degradation (poor quality)
- Peer connection failures

**Behavior:**
- **Exponential backoff**: 5s → 10s → 20s → 60s intervals
- **Attempt limiting**: Maximum 10 restart attempts
- **Smart timing**: Waits for network stability
- **Comprehensive logging**: Detailed restart decision logging

### **Monitoring Recovery**

```python
def enhanced_signal_handler(self, signal_dict):
    tube_id = signal_dict.get('tube_id')
    kind = signal_dict.get('kind')
    data = signal_dict.get('data')
    
    if kind == "connection_state_changed":
        timestamp = time.time()
        state = data.lower()
        
        # Track state transitions for recovery timing
        if not hasattr(self, 'state_transitions'):
            self.state_transitions = {}
        if tube_id not in self.state_transitions:
            self.state_transitions[tube_id] = []
        
        self.state_transitions[tube_id].append((timestamp, state))
        
        if state == "connected":
            # Check if this is a recovery
            transitions = self.state_transitions[tube_id]
            if len(transitions) >= 2:
                prev_state = transitions[-2][1]
                if prev_state in ["disconnected", "failed"]:
                    recovery_time = timestamp - transitions[-2][0]
                    print(f"🚀 RECOVERY: {tube_id} recovered in {recovery_time:.2f}s")
        
        elif state in ["disconnected", "failed"]:
            print(f"🔧 RESTART: {tube_id} will attempt ICE restart (automatic)")
```

### **ICE Restart Timeout Behavior**

The library implements comprehensive timeout protection to prevent hanging connections:

#### **ICE Gathering Timeout**
- **Default:** 30 seconds (configurable via `ice_gather_timeout_seconds`)
- **Purpose:** Prevent infinite wait during candidate gathering
- **Applies to:** Non-trickle ICE offer/answer generation
- **Failure:** Connection fails with clear error if timeout exceeded

#### **ICE Restart Answer Timeout**
- **Duration:** 15 seconds (**not configurable**)
- **Purpose:** Prevent hanging after sending ICE restart offer
- **Trigger:** Starts when `ice_restart_offer` signal is sent
- **If Timeout Occurs:**
  - Connection marked as closing
  - No further restart attempts allowed
  - Log message: `"ICE restart answer timeout - no response from remote peer after 15s"`
- **To Prevent:** Handle `ice_restart_offer` signals and apply answers promptly

#### **Circuit Breaker Protection**
- **Failure Threshold:** 5 failed ICE restart attempts
- **Breaker Opens:** After 5th consecutive failure
- **When Open:** All restart attempts blocked for 30 seconds
- **After Breaker Trips:** Connection closes **immediately** (doesn't wait 30s)
- **Reset:** Breaker closes after 3 successful operations

**Important:** If you don't implement `ice_restart_offer` signal handling, ICE restart will:
1. Send offer signal (ignored by Python)
2. Wait 15 seconds for answer
3. Timeout and close connection
4. Prevent "pingAllCandidates" spam ✅

### **Concurrent Restart Protection**

The library prevents multiple simultaneous ICE restart attempts:

**Example Timeline:**
```
T+0s:  Connection: Disconnected → ICE restart #1 triggered
T+2s:  Connection: Failed → ICE restart #2 attempted
T+2s:  ✅ Blocked: "ICE restart already in progress, skipping duplicate request"
T+7s:  Restart #1 completes (answer received) → Flag cleared
T+10s: New restart attempts now allowed if needed
```

**Protection Mechanism:**
- Internal `ice_restart_in_progress` atomic flag
- Set when restart starts, cleared when complete or timeout
- Duplicate attempts logged and skipped
- No manual management required

### **Automatic Restart Completion**

When remote peer sends ICE restart answer, the library **automatically** completes the restart:

**Flow:**
1. ICE restart triggered → `ice_restart_in_progress = true`
2. Offer sent via `ice_restart_offer` signal
3. Python forwards to remote peer
4. Remote peer sends answer back
5. Python calls `registry.set_remote_description(tube_id, answer, is_answer=True)`
6. **Library auto-detects:** answer + restart in progress
7. **Automatically calls:** internal `complete_ice_restart()`
8. **Clears flag:** `ice_restart_in_progress = false`
9. **Cancels timeout:** 15s timeout task becomes no-op

**No Explicit Call Needed:**
```python
# ❌ DON'T DO THIS (method not exposed to Python):
# registry.complete_ice_restart(tube_id)

# ✅ DO THIS (auto-completion built-in):
registry.set_remote_description(tube_id, answer, is_answer=True)
# → Rust automatically detects and completes ICE restart
```

**JavaScript (monitoring recovery from peer side):**
```javascript
class RustLibraryConnectionMonitor {
    constructor() {
        this.stateTransitions = [];
        this.recoveryMetrics = [];
    }
    
    trackStateChange(oldState, newState) {
        const timestamp = Date.now();
        
        this.stateTransitions.push({ timestamp, state: newState });
        
        if (newState === 'connected') {
            // Check if this is a recovery
            const prevTransition = this.stateTransitions[this.stateTransitions.length - 2];
            if (prevTransition && ['disconnected', 'failed'].includes(prevTransition.state)) {
                const recoveryTime = (timestamp - prevTransition.timestamp) / 1000;
                console.log(`🚀 RUST LIBRARY RECOVERY: Connection recovered in ${recoveryTime.toFixed(2)}s`);
                
                // Note: Recovery was handled automatically by Rust library
                this.recoveryMetrics.push(recoveryTime);
            }
        } else if (['disconnected', 'failed'].includes(newState)) {
            console.log('🔧 RUST LIBRARY: Connection lost - automatic recovery initiated by library');
        }
        
        // Clean up old transitions (keep last 10)
        if (this.stateTransitions.length > 10) {
            this.stateTransitions.splice(0, this.stateTransitions.length - 10);
        }
    }
    
    getRecoveryStats() {
        if (this.recoveryMetrics.length === 0) return null;
        
        const avgTime = this.recoveryMetrics.reduce((a, b) => a + b) / this.recoveryMetrics.length;
        const maxTime = Math.max(...this.recoveryMetrics);
        const minTime = Math.min(...this.recoveryMetrics);
        
        return {
            totalRecoveries: this.recoveryMetrics.length,
            averageTime: avgTime.toFixed(2),
            maxTime: maxTime.toFixed(2),
            minTime: minTime.toFixed(2)
        };
    }
}
```

---

## 📊 **Connection Management APIs**

### **Manual ICE Restart**

For advanced use cases, you can manually trigger ICE restart:

```python
# Manual ICE restart (usually not needed - automatic restart is preferred)
def manual_ice_restart(self, tube_id):
    """Manually trigger ICE restart for a specific connection"""
    try:
        restart_sdp = self.registry.restart_ice(tube_id)
        print(f"🔄 Manual ICE restart initiated for {tube_id}")
        # Send restart_sdp to remote peer through your signaling mechanism
        self.send_restart_offer_to_peer(restart_sdp, tube_id)
    except Exception as e:
        print(f"❌ Manual ICE restart failed for {tube_id}: {e}")
```

### **Connection Statistics**

Monitor connection quality with real-time statistics:

```python
def monitor_connection_quality(self, tube_id):
    """Get real-time connection statistics"""
    try:
        stats = self.registry.get_connection_stats(tube_id)
        
        print(f"📊 Connection Stats for {tube_id}:")
        print(f"   📈 Bytes sent: {stats['bytes_sent']:,}")
        print(f"   📉 Bytes received: {stats['bytes_received']:,}")
        print(f"   📡 Packet loss: {stats['packet_loss_rate']:.2%}")
        
        if stats['rtt_ms'] is not None:
            print(f"   ⏱️  Round-trip time: {stats['rtt_ms']:.1f}ms")
        
        # Quality assessment
        if stats['packet_loss_rate'] > 0.05:  # >5% loss
            print(f"⚠️  High packet loss detected - ICE restart may be beneficial")
        if stats['rtt_ms'] and stats['rtt_ms'] > 500:  # >500ms RTT
            print(f"⚠️  High latency detected - network issues possible")
            
        return stats
    except Exception as e:
        print(f"❌ Failed to get connection stats for {tube_id}: {e}")
        return None

def connection_health_monitor(self, tube_id):
    """Continuous connection health monitoring"""
    import threading
    import time
    
    def health_loop():
        consecutive_poor_quality = 0
        
        while tube_id in self.connection_states:
            try:
                stats = self.monitor_connection_quality(tube_id)
                if stats:
                    # Check for poor connection quality
                    poor_quality = (
                        stats['packet_loss_rate'] > 0.03 or  # >3% loss
                        (stats['rtt_ms'] and stats['rtt_ms'] > 300)  # >300ms RTT
                    )
                    
                    if poor_quality:
                        consecutive_poor_quality += 1
                        if consecutive_poor_quality >= 3:  # 3 consecutive poor readings
                            print(f"🔧 Poor connection quality detected - manual restart recommended")
                            self.manual_ice_restart(tube_id)
                            consecutive_poor_quality = 0
                    else:
                        consecutive_poor_quality = 0
                
                time.sleep(30)  # Check every 30 seconds
            except Exception as e:
                print(f"Health monitor error for {tube_id}: {e}")
                break
    
    threading.Thread(target=health_loop, daemon=True).start()
```

### **Quality-Based Recovery**

Implement proactive recovery based on connection quality:

```python
class QualityBasedRecoveryManager:
    def __init__(self, registry):
        self.registry = registry
        self.quality_thresholds = {
            'packet_loss_restart_threshold': 0.05,    # 5% loss triggers restart
            'rtt_restart_threshold': 800,             # 800ms RTT triggers restart
            'poor_quality_count_threshold': 3         # 3 consecutive poor readings
        }
        self.quality_history = {}
    
    def assess_connection_quality(self, tube_id):
        """Assess connection quality and trigger recovery if needed"""
        try:
            stats = self.registry.get_connection_stats(tube_id)
            
            # Quality scoring
            quality_score = 1.0
            
            # Packet loss penalty
            if stats['packet_loss_rate'] > 0:
                quality_score -= min(stats['packet_loss_rate'] * 2, 0.5)
            
            # RTT penalty
            if stats['rtt_ms']:
                if stats['rtt_ms'] > 200:
                    quality_score -= min((stats['rtt_ms'] - 200) / 1000, 0.3)
            
            # Track quality history
            if tube_id not in self.quality_history:
                self.quality_history[tube_id] = []
            
            self.quality_history[tube_id].append(quality_score)
            
            # Keep last 10 measurements
            if len(self.quality_history[tube_id]) > 10:
                self.quality_history[tube_id].pop(0)
            
            # Check if restart is needed
            recent_quality = self.quality_history[tube_id][-3:]  # Last 3 measurements
            if len(recent_quality) >= 3 and all(q < 0.7 for q in recent_quality):
                print(f"🔧 Poor connection quality detected - triggering ICE restart")
                self.registry.restart_ice(tube_id)
                self.quality_history[tube_id] = []  # Reset history after restart
            
            return {
                'quality_score': quality_score,
                'packet_loss_rate': stats['packet_loss_rate'],
                'rtt_ms': stats['rtt_ms'],
                'bytes_sent': stats['bytes_sent'],
                'bytes_received': stats['bytes_received']
            }
            
        except Exception as e:
            print(f"Quality assessment failed for {tube_id}: {e}")
            return None
```

---

## 📊 **Production Monitoring**

### **Key Metrics to Track**

```python
class ProductionMetrics:
    def __init__(self):
        self.connection_attempts = 0
        self.successful_connections = 0
        self.recovery_attempts = 0
        self.successful_recoveries = 0
        self.average_connection_time = 0
        self.average_recovery_time = 0
        self.ice_restart_count = 0
        
    def record_connection_attempt(self):
        self.connection_attempts += 1
        
    def record_successful_connection(self, duration_seconds):
        self.successful_connections += 1
        # Update rolling average
        self.average_connection_time = (
            self.average_connection_time * (self.successful_connections - 1) + duration_seconds
        ) / self.successful_connections
        
    def record_recovery_attempt(self):
        self.recovery_attempts += 1
        
    def record_successful_recovery(self, duration_seconds):
        self.successful_recoveries += 1
        self.average_recovery_time = (
            self.average_recovery_time * (self.successful_recoveries - 1) + duration_seconds
        ) / self.successful_recoveries
        
    def get_success_rates(self):
        return {
            'connection_success_rate': (
                self.successful_connections / max(self.connection_attempts, 1)
            ) * 100,
            'recovery_success_rate': (
                self.successful_recoveries / max(self.recovery_attempts, 1)
            ) * 100,
            'average_connection_time': self.average_connection_time,
            'average_recovery_time': self.average_recovery_time,
            'ice_restarts': self.ice_restart_count
        }
```

### **Logging Configuration**

```python
import logging

def setup_webrtc_logging():
    """Configure logging for WebRTC debugging"""
    
    # Key log targets from Rust library
    loggers = [
        'webrtc_keepalive',      # NAT timeout prevention
        'webrtc_ice_restart',    # ICE restart events
        'webrtc_activity',       # Connection activity
        'webrtc_ice',           # ICE candidate processing
        'resource_management',   # Resource usage
        'webrtc_lifecycle'       # Connection lifecycle
    ]
    
    for logger_name in loggers:
        logger = logging.getLogger(logger_name)
        logger.setLevel(logging.INFO)
        
        # Add handler if not already present
        if not logger.handlers:
            handler = logging.StreamHandler()
            formatter = logging.Formatter(
                '%(asctime)s - %(name)s - %(levelname)s - %(message)s'
            )
            handler.setFormatter(formatter)
            logger.addHandler(handler)

# Enable in production
setup_webrtc_logging()
```

---

## 🎯 **Testing Network Changes**

### **Manual Testing**

```bash
# Test network interface switching
# 1. Start connection on ethernet
# 2. Switch to wifi
# 3. Verify automatic recovery

# Monitor connection during switch:
tail -f application.log | grep "webrtc_ice_restart\|connection_state_changed"
```

### **Automated Testing**

```python
import time
import threading

def simulate_network_change_test():
    """Simulate network change scenarios"""
    manager = WebRTCManager()
    
    # Establish connection
    tube_info = manager.create_connection("test-recovery")
    tube_id = tube_info['tube_id']
    
    # Wait for connection
    time.sleep(5)
    assert manager.registry.get_connection_state(tube_id) == "connected"
    
    # Simulate network degradation by monitoring recovery
    recovery_detected = threading.Event()
    
    def monitor_recovery():
        prev_state = "connected"
        while not recovery_detected.is_set():
            current_state = manager.registry.get_connection_state(tube_id)
            if prev_state != "connected" and current_state == "connected":
                print("✅ Automatic recovery detected!")
                recovery_detected.set()
            prev_state = current_state
            time.sleep(1)
    
    monitor_thread = threading.Thread(target=monitor_recovery)
    monitor_thread.start()
    
    # In real test, you'd trigger actual network change here
    # For simulation, just wait and verify the system handles it
    recovery_detected.wait(timeout=60)  # Wait up to 60 seconds
    
    assert recovery_detected.is_set(), "Recovery not detected within timeout"
    print("🎉 Network change recovery test passed!")
```

---

## 🔧 **Configuration Tuning**

### **For Different Environments**

```python
# High-reliability (corporate networks)
CORPORATE_CONFIG = {
    'ice_keepalive_interval': 240,        # 4 minutes (aggressive)
    'session_timeout': 7200,              # 2 hours
    'connection_health_check_interval': 60, # 1 minute checks
    'max_concurrent_ice_agents': 2000,    # Scale for many users
}

# Mobile/unstable networks
MOBILE_CONFIG = {
    'ice_keepalive_interval': 180,        # 3 minutes (very aggressive)  
    'session_timeout': 1800,              # 30 minutes
    'connection_health_check_interval': 30, # 30 second checks
    'ice_gather_timeout': 20,             # Longer gathering time
}

# Low-latency applications
LOW_LATENCY_CONFIG = {
    'ice_keepalive_interval': 300,        # Standard 5 minutes
    'connection_health_check_interval': 15, # Frequent checks
    'ice_gather_timeout': 5,              # Quick gathering
}

def configure_for_environment(registry, config_type="corporate"):
    configs = {
        "corporate": CORPORATE_CONFIG,
        "mobile": MOBILE_CONFIG, 
        "low_latency": LOW_LATENCY_CONFIG
    }
    
    registry.set_resource_limits(configs[config_type])
```

---

## 🎯 **Summary**

### **Key Integration Points:**

**Python (using the library):**
1. **Enable Trickle ICE**: **REQUIRED** for ICE restart - always use `trickle_ice=True`
2. **Handle Signaling**: Process `connection_state_changed`, `icecandidate`, **and `ice_restart_offer`** events
3. **Forward ICE Restart Offers**: Send `ice_restart_offer` signal data to remote peer (you have 15 seconds)
4. **Apply ICE Restart Answers**: Call `set_remote_description(tube_id, answer, is_answer=True)` when remote peer responds
5. **Forward ICE Candidates**: Send candidates to JavaScript peer immediately during normal operation
6. **Monitor Recovery**: Track state transitions for recovery timing
7. **Use Connection APIs**: `restart_ice()` for manual restart, `get_connection_stats()` for quality monitoring
8. **Configure Logging**: Enable WebRTC logging targets for debugging

**JavaScript (connecting to the library):**
1. **Handle Trickle ICE**: Send candidates to Rust library immediately as discovered
2. **Process ICE Restart Offers**: Handle `ice_restart_offer` signals forwarded from Python (respond within 15 seconds)
3. **Send ICE Restart Answers**: Generate answer and send back to Python for application
4. **Monitor Connection State**: Track connection health from peer perspective
5. **Implement Signaling**: Handle SDP and candidate exchange with Rust library
6. **Test Network Changes**: Validate that Rust library recovery works

### **What You Get:**

- ✅ **Sub-second connection establishment** with trickle ICE
- ✅ **Automatic network change recovery** (Rust library handles ICE restart)
- ✅ **NAT timeout prevention** for long-running connections (5-minute keepalive)
- ✅ **Production-grade reliability** with comprehensive monitoring
- ✅ **Peer compatibility** (JavaScript WebRTC works seamlessly with Rust library)

The Rust library provides enterprise-grade WebRTC reliability that handles real-world network conditions automatically, while JavaScript peers simply need to implement standard WebRTC signaling.
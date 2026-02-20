# Python API Contract - Base64 SDP Encoding

**Version**: 1.0.4
**Date**: October 13, 2025
**Status**: ✅ CLEAN API (Backward compatibility removed)

---

## 🎯 **API Contract: ALL SDP is Base64-Encoded**

### **The Rule**

**ALL SDP (Session Description Protocol) data exchanged over the Python/Rust boundary MUST be base64-encoded.**

This includes:
- **Offers** (`create_tube()` returns `result['offer']`)
- **Answers** (`create_tube()` with offer returns `result['answer']`)
- **Remote descriptions** (`set_remote_description(sdp=...)` expects base64)
- **ICE restart offers** (sent via signals)

---

## ✅ **Correct Usage**

### **Server-Side (Creates Offer)**

```python
import keeper_pam_webrtc_rs
import base64

registry = keeper_pam_webrtc_rs.PyTubeRegistry()

# Create server tube (no offer = server mode)
server_result = registry.create_tube(
    conversation_id="session-123",
    settings={"conversationType": "tunnel"},
    trickle_ice=True,
    callback_token="server-token",
    krelay_server="relay.keeper.io",
    client_version="ms16.5.0",
    ksm_config="<ksm_config_json>"
)

# Offer is BASE64-ENCODED
offer_base64 = server_result['offer']
server_tube_id = server_result['tube_id']

# Send offer_base64 to client (already base64 - send as-is!)
send_to_client(offer_base64)
```

### **Client-Side (Uses Offer, Creates Answer)**

```python
# Receive base64-encoded offer from server
offer_base64 = receive_from_server()

# Create client tube WITH offer (client mode)
client_result = registry.create_tube(
    conversation_id="session-456",
    settings={"conversationType": "tunnel"},
    trickle_ice=True,
    callback_token="client-token",
    krelay_server="relay.keeper.io",
    client_version="ms16.5.0",
    ksm_config="<ksm_config_json>",
    offer=offer_base64  # ← PASS BASE64 DIRECTLY (no decoding!)
)

# Answer is BASE64-ENCODED
answer_base64 = client_result['answer']
client_tube_id = client_result['tube_id']

# Send answer back to server (already base64 - send as-is!)
send_to_server(answer_base64)
```

### **Completing Connection**

```python
# Server receives base64-encoded answer
answer_base64 = receive_from_client()

# Set remote description (expects base64)
registry.set_remote_description(
    server_tube_id,
    answer_base64,  # ← PASS BASE64 DIRECTLY
    is_answer=True
)

# Connection established!
```

---

## ❌ **Common Mistakes (Don't Do This)**

### **Mistake 1: Decoding SDP Before Passing**

```python
# ❌ WRONG - Don't decode base64 before passing to Rust!
offer_base64 = server_result['offer']
offer_raw = base64.b64decode(offer_base64).decode('utf-8')  # ❌ NO!
client_result = registry.create_tube(..., offer=offer_raw)  # ❌ WILL FAIL!
```

**Why it fails**: Rust expects base64, will try to decode raw SDP, fails with "Failed to decode from base64"

### **Mistake 2: Encoding Raw SDP**

```python
# ❌ WRONG - Rust already returns base64!
offer_base64 = server_result['offer']
double_encoded = base64.b64encode(offer_base64.encode())  # ❌ DOUBLE-ENCODED!
client_result = registry.create_tube(..., offer=double_encoded)  # ❌ WILL FAIL!
```

**Why it fails**: Rust decodes once, gets base64 string, tries to parse as SDP, fails

---

## 🔧 **Migration Guide: Update Old Code**

### **Before (Old API - Raw SDP)**

```python
# OLD CODE (if you had this):
server_result = registry.create_tube(...)
offer_raw = server_result['offer']  # Was raw SDP: "v=0\r\no=- ..."

client_result = registry.create_tube(..., offer=offer_raw)  # Raw SDP
```

### **After (Clean API - Base64)**

```python
# NEW CODE (current):
server_result = registry.create_tube(...)
offer_base64 = server_result['offer']  # Now base64: "dj0wDQpvPS0g..."

client_result = registry.create_tube(..., offer=offer_base64)  # Base64
```

**No changes needed!** The API contract is the same - just use what Rust returns.

---

## 🧪 **Testing: How to Update Tests**

### **Old Test Pattern** (if you have raw SDP)

```python
def test_something(self):
    # If your test hard-codes SDP:
    raw_sdp = "v=0\\r\\no=- 123..."  # ❌ OLD
    client_info = registry.create_tube(..., offer=raw_sdp)  # ❌ FAILS
```

### **New Test Pattern** (base64)

```python
import base64

def test_something(self):
    # Encode raw SDP to base64 for testing:
    raw_sdp = "v=0\\r\\no=- 123..."
    offer_base64 = base64.b64encode(raw_sdp.encode()).decode()  # ✅ CORRECT

    client_info = registry.create_tube(..., offer=offer_base64)  # ✅ WORKS
```

**Better yet**: Don't hard-code SDP - use real offers from server tubes!

```python
def test_something(self):
    # ✅ BEST: Use actual offer from server tube
    server_info = registry.create_tube(...)  # Server mode
    client_info = registry.create_tube(..., offer=server_info['offer'])  # ✅ WORKS
```

---

## 📚 **Why Base64?**

### **Benefits**

1. **Encoding Safety**: SDP contains special characters (\\r\\n, whitespace, binary-safe)
2. **Consistency**: Same encoding for all data over FFI boundary
3. **JSON-Friendly**: Base64 strings can be embedded in JSON without escaping
4. **Network-Safe**: Base64 is URL-safe and transport-agnostic

### **Performance**

- Encoding overhead: ~100-200ns per SDP (~500 bytes)
- Decoding overhead: ~100-200ns per SDP
- **Total cost**: <1μs per tube create (negligible)

---

## 🔍 **Debugging**

### **Error: "Failed to decode from base64"**

```
RuntimeError: Failed to decode initial_offer_sdp from base64
```

**Cause**: You passed raw SDP instead of base64-encoded SDP

**Solution**:
```python
# If you have raw SDP, encode it:
import base64
offer_base64 = base64.b64encode(raw_sdp.encode()).decode()
registry.create_tube(..., offer=offer_base64)
```

### **Error: "Invalid SDP format"**

```
RuntimeError: Failed to set remote description: Invalid SDP
```

**Cause**: You double-encoded (base64 of base64)

**Solution**:
```python
# Don't encode what Rust returns - it's already base64!
offer = server_result['offer']  # Already base64
client_result = registry.create_tube(..., offer=offer)  # ✅ Use directly
```

---

## 📊 **API Summary**

### **SDP Encoding**

| Method | Parameter | Type | Encoding |
|--------|-----------|------|----------|
| `create_tube()` | `offer=...` (optional) | Input | **Base64** |
| `create_tube()` | Returns `result['offer']` | Output | **Base64** |
| `create_tube()` | Returns `result['answer']` | Output | **Base64** |
| `create_offer(tube_id)` | N/A | Output | **Base64** |
| `create_answer(tube_id)` | N/A | Output | **Base64** |
| `set_remote_description()` | `sdp=...` | Input | **Base64** |
| `set_remote_description()` | Returns answer | Output | **Base64** |
| `restart_ice(tube_id)` | N/A | Output | **Base64** |

**Rule of thumb**: If it contains SDP, it's base64!

**Note**: All methods now correctly encode SDP to base64. Previous versions had bugs where `create_offer()`, `create_answer()`, and `restart_ice()` returned raw SDP - this is now fixed.

---

### **create_tube() Return Values**

**All tubes return**:
- `tube_id` (string): Unique identifier for the tube
- `offer` (string, base64): WebRTC offer SDP (server mode only)
- `answer` (string, base64): WebRTC answer SDP (client mode only)

**Server mode tubes also return**:
- `actual_local_listen_addr` (string, optional): Actual TCP listening address
  - Format: `"host:port"` (e.g., `"127.0.0.1:59194"`)
  - Only present if `settings` contains `"local_listen_addr": "0.0.0.0:0"`
  - The port is dynamically assigned by the OS

**Example**:
```python
# Server mode (creates offer):
server_info = registry.create_tube(
    conversation_id="tunnel-session",
    settings={
        "conversationType": "tunnel",
        "local_listen_addr": "127.0.0.1:0"  # Dynamic port
    },
    ...
)

# Returns:
{
    "tube_id": "08c9a70e-294a-4cef-9d6f-8cf7d139cb5c",
    "offer": "dj0wDQpvPS0g...",  # Base64-encoded SDP
    "actual_local_listen_addr": "127.0.0.1:59194"  # Actual port assigned
}

# Use the actual address for external connections:
host, port = server_info['actual_local_listen_addr'].split(':')
external_client.connect((host, int(port)))
```

---

## ✅ **Verification**

### **Check Your Offers**

```python
offer = server_result['offer']

# Should start with base64 characters (dj0w, dj0K, etc.)
print(f"Offer: {offer[:20]}")  # e.g., "dj0wDQpvPS0gODUxNT..."

# Should decode to SDP
import base64
decoded = base64.b64decode(offer).decode()
print(f"Decoded: {decoded[:20]}")  # e.g., "v=0\r\no=- 8515716..."

# If offer starts with "v=0", you're using old/broken code!
assert not offer.startswith("v="), "Offer should be base64, not raw SDP!"
```

---

## 🎯 **Summary**

**API Contract**: ALL SDP exchanged with Rust is BASE64-encoded

**Your responsibility**:
- ✅ Use what Rust returns directly (already base64)
- ❌ Don't decode before passing to Rust
- ❌ Don't double-encode

**Rust's responsibility**:
- ✅ Encode all SDP outputs to base64
- ✅ Decode all SDP inputs from base64
- ✅ Validate base64 format

**Simple rule**: Treat SDP as opaque base64 strings!

---

Generated: October 13, 2025
Author: keeper-pam-webrtc-rs development team
Status: Production API contract

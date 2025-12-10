# Apache guacd Feature Comparison

**Status:** Feature parity assessment for SSH/Telnet handlers
**Date:** 2025-11-14

## What We Have vs What guacd Has

### Core Terminal Features

| Feature | guacd | Our Implementation | Status |
|---------|-------|-------------------|--------|
| **Text rendering** | Pango + PNG | fontdue + PNG | ✅ Same approach |
| **Font support** | System fonts via Pango | Noto Sans Mono embedded | ✅ Working |
| **Dirty regions** | Yes (Cairo surfaces) | Yes (cell hashing) | ✅ Implemented |
| **Terminal emulation** | libvterm (VT100/ANSI) | vt100 crate | ✅ Working |
| **Keyboard input** | Full X11 keysym mapping | x11_keysym_to_bytes | ✅ Working |
| **Terminal resize** | Yes | Yes | ✅ Working |
| **Session recording** | typescript format | asciicast format | ✅ Different but working |

### User Interaction Features

| Feature | guacd | Our Implementation | Priority | Complexity |
|---------|-------|-------------------|----------|------------|
| **Clipboard (copy/paste)** | ✅ Bidirectional | ✅ Working | - | - |
| **Mouse events** | ✅ xterm mouse protocol | ✅ Working | - | - |
| **Scrollback buffer** | ✅ Configurable history | ✅ Working | - | - |
| **Audio (BEL)** | ✅ Plays beep | ✅ Working | - | - |

### File Transfer Features

| Feature | guacd | Our Implementation | Priority | Complexity |
|---------|-------|-------------------|----------|------------|
| **SFTP integration** | ✅ Full filesystem | ✅ Working | - | - |
| **File download** | ✅ Browser download | ✅ Working | - | - |
| **File upload** | ✅ Drag & drop | ✅ Working | - | - |

### Authentication & Security

| Feature | guacd | Our Implementation | Priority | Complexity |
|---------|-------|-------------------|----------|------------|
| **Password auth** | ✅ Yes | ✅ Working | - | - |
| **Private key auth** | ✅ Yes | ✅ Working | - | - |
| **Agent forwarding** | ✅ Yes | ❌ Missing | LOW | Medium (~100 lines) |
| **Host key verification** | ✅ known_hosts | ✅ Working | - | - |

### Configuration & Customization

| Feature | guacd | Our Implementation | Priority | Complexity |
|---------|-------|-------------------|----------|------------|
| **Color schemes** | ✅ Configurable | ✅ Working | - | - |
| **Font selection** | ✅ System fonts | ✅ Noto Sans Mono (embedded) | - | - |
| **Locale/timezone** | ✅ ENV vars | ✅ Working | - | - |
| **Command execution** | ✅ Run specific command | ✅ Working | - | - |

## Implementation Roadmap

### Phase 1: Essential Features (Week 1)
**Goal:** Feature parity for basic SSH usage

1. **Fix current rendering issues** (BLOCKING)
   - Debug why typed characters don't appear
   - Verify dirty regions work correctly
   - Ensure no overlapping layers

2. **Private key authentication** (~40 lines)
   ```rust
   if let Some(key_data) = params.get("private_key") {
       let key = russh_keys::decode_secret_key(key_data, passphrase)?;
       sh.authenticate_publickey(username, Arc::new(key)).await?;
   }
   ```

3. **Clipboard integration** (~100 lines)
   ```rust
   // Handle clipboard instruction from browser
   if msg_str.contains(".clipboard,") {
       parse_clipboard_data();
       // TODO: Forward to SSH server (not all SSH servers support clipboard)
   }
   ```

### Phase 2: File Transfer (Week 2)
**Goal:** Enable file upload/download

4. **SFTP integration** (~500 lines)
   - Use existing `guacr-sftp` crate stub
   - Implement russh-sftp client calls
   - Map to Guacamole file transfer protocol
   - Browser file browser UI integration

### Phase 3: Advanced Features (Week 3)
**Goal:** Power user features

5. **Scrollback buffer** (~150 lines)
   - Maintain history of scrolled-off lines
   - Handle scroll requests from browser
   - Render historical content on demand

6. **Mouse events** (~80 lines)
   - Convert Guacamole mouse instructions
   - Send xterm mouse escape codes
   - Enable for vim/tmux/etc

7. **Host key verification** ✅ COMPLETE
   - Check against known_hosts file
   - Pinned fingerprint support (`host-key` parameter)
   - Configurable: `ignore-host-key`, `allow-unknown-hosts`
   - MITM attack protection with clear error messages

### Phase 4: Polish (Week 4)
**Goal:** Production ready

8. **Color schemes** (~30 lines)
9. **Locale/timezone** (~10 lines)
10. **Command execution mode** (~20 lines)
11. **Audio/beep** (~20 lines)

## Bandwidth Comparison with guacd

### Our Implementation with Dirty Regions

| Scenario | Bandwidth | Notes |
|----------|-----------|-------|
| Login banner | ~500KB | Full screens (80-100% dirty) |
| Typing | ~200 bytes/char | Dirty regions (0.03% of screen) |
| Editing code | ~2-3KB/line | Single line changes |
| Scrolling (ls output) | ~500KB | Full screen redraws |

**vs guacd:** Comparable (guacd uses same PNG approach with dirty regions)

### Potential Optimizations

1. **Custom glyph protocol:** 7-25× bandwidth reduction (future)
2. **Better dirty region algorithm:** Detect line-only changes
3. **Frame skipping:** Drop to 5fps when idle
4. **JPEG for photos:** If terminal displays images

## Current Status

**Working:**
- Core rendering (PNG + fonts)
- Dirty region tracking (30% threshold)
- Keyboard input
- Terminal resize
- Session recording

**Needs fixing:**
- Typed characters not visible (critical bug)
- Possible layer positioning issue with dirty regions

**Next to implement:**
1. Fix visibility bug
2. Private key auth
3. Clipboard

**Future:**
- SFTP, scrollback, mouse, etc.

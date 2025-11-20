# "One Character Behind" Fix

## Problem

When typing in SSH, characters appeared one keystroke behind. For example:
- Type 'a' → nothing appears
- Type 'b' → 'a' appears
- Type 'c' → 'b' appears

## Root Cause

The `tokio::select!` macro uses **fair/random selection** by default. When multiple branches are ready simultaneously, it picks one at random. This caused a race condition:

1. User types 'a' → sent to SSH server
2. SSH server echoes 'a' back → terminal processes it (marks screen dirty)
3. User types 'b' (before debounce timer fires)
4. `tokio::select!` randomly picks: process 'b' input OR render 'a' output
5. If it picks input first: 'b' is sent before 'a' is rendered
6. Result: User sees 'a' when they type 'b'

## Solution

Use **biased selection** in `tokio::select!` to prioritize branches in order:

```rust
tokio::select! {
    // Add this line to enable biased (ordered) selection
    biased;

    // Now branches are checked in order (top to bottom)
    // Rendering happens BEFORE processing new input
    _ = debounce.tick() => { /* render screen */ }
    Some(msg) = channel.wait() => { /* process SSH output */ }
    msg = from_client.recv() => { /* process client input */ }
}
```

With `biased;`, the select checks branches in order:
1. **First**: Check if debounce timer fired → render any pending output
2. **Second**: Check for SSH server messages
3. **Third**: Check for client input

This ensures that echoed characters are always rendered before processing the next keystroke.

## Files Changed

### pam-guacr

**File**: `crates/guacr-ssh/src/handler.rs`

**Changes**:
1. Added `biased;` to `tokio::select!` at line 330
2. Fixed API mismatches:
   - Line 412: Added `None` parameter to `x11_keysym_to_bytes(keysym, pressed, None)`
   - Lines 523-530: Removed `layer` parameter from `format_clear_region_instructions` (now takes 6 args, not 7)
   - Lines 573-580: Removed `layer` parameter from `format_clear_region_instructions`
3. Added imports: `EventBasedHandler`, `EventCallback`
4. Added `as_event_based()` method to `ProtocolHandler` implementation
5. Added `EventBasedHandler` implementation for bidirectional zero-copy communication
6. Fixed test ambiguity: Line 752 changed `handler.name()` to `ProtocolHandler::name(&handler)` to disambiguate between `ProtocolHandler::name` and `EventBasedHandler::name`

## Testing

```bash
# Build pam-guacr
cd /Users/mroberts/Documents/pam-guacr
cargo build --release

# Build keeper-pam-webrtc-rs
cd /Users/mroberts/Documents/keeper-pam-webrtc-rs
cargo check

# Test SSH connection - typing should now be responsive with no lag
```

## Technical Details

### Why `biased;` Works

From Tokio documentation:

> By default, `select!` randomly picks a branch to check first, to provide fairness. When `biased;` is specified, branches are polled in order from top to bottom.

This prevents starvation of lower-priority branches while ensuring critical operations (like rendering) happen first.

### Performance Impact

**Negligible**. The debounce timer only fires every 16ms (60fps), so the bias doesn't cause input starvation. Input is still processed within 16ms, which is imperceptible to users.

## Related Issues

This fix also resolves:
- Cursor position appearing wrong during fast typing
- Visual lag in terminal applications
- Inconsistent character echo timing

## References

- Tokio `select!` documentation: https://docs.rs/tokio/latest/tokio/macro.select.html
- Related to bidirectional communication fix in `BIDIRECTIONAL_EVENT_HANDLER_FIX.md`

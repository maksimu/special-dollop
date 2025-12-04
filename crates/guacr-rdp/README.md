# guacr-rdp - RDP Protocol Handler

## Status: ✅ WORKING

**Implementation**: ✅ Complete
**Compilation**: ✅ Working (using local IronRDP fork)

### Current Setup

Using local IronRDP fork at `~/Documents/IronRDP` with Update PDU workaround:

```toml
# In workspace Cargo.toml
[patch.crates-io]
ironrdp = { path = "../IronRDP/crates/ironrdp" }
ironrdp-async = { path = "../IronRDP/crates/ironrdp-async" }
ironrdp-tokio = { path = "../IronRDP/crates/ironrdp-tokio" }
ironrdp-pdu = { path = "../IronRDP/crates/ironrdp-pdu" }
ironrdp-session = { path = "../IronRDP/crates/ironrdp-session" }
```

### IronRDP Fork Status

- **PR #1043** (static dispatch): Merged to upstream master
- **Update PDU workaround**: Added in local fork on `feature/handle-update-pdu` branch
- **GitHub**: `miroberts/IronRDP`

### For Distribution

Switch back to git URL when building for distribution:

```toml
[patch.crates-io]
ironrdp = { git = "https://github.com/miroberts/IronRDP.git", branch = "feature/handle-update-pdu" }
# ... etc
```

### Details

See [IRONRDP_STATUS.md](./IRONRDP_STATUS.md) for full technical details.

---
*Last updated: 2025-12-03*

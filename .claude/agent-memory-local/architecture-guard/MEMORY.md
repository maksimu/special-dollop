# Architecture Guard Memory

## Project
Keeper Commander Rust — clean-arch Rust rewrite of Python Commander.
Codebase: /Users/mustinov/Source/Commander-rust/
REF_CODE (read-only): /Users/mustinov/Source/Commander-rust/REF_CODE/

## Topic Files
- [architecture-decisions.md](architecture-decisions.md) — Design decisions and alternatives
- [keeper-api-contracts.md](keeper-api-contracts.md) — API endpoint behaviors, wire formats
- [keeper-crypto-knowledge.md](keeper-crypto-knowledge.md) — Crypto patterns
- [bugs-and-gotchas.md](bugs-and-gotchas.md) — Known bugs, root causes, fixes

## Layer Map (dependencies point INWARD)
```
keeper-core       Domain (entities, traits, errors)
keeper-pam        Use case (TunnelPort/TunnelSignalingPort traits live here)
keeper-tunnel     Infrastructure (implements TunnelPort via keeper-pam-webrtc-rs)
keeper-sdk        Facade (wires everything)
keeper-cli/tui    Consumer (thin wrappers)
```

## Critical Patterns Confirmed
- TunnelPort / TunnelSignalingPort traits are in keeper-pam (use-case layer)
- keeper-pam-webrtc-rs global singleton: get_registry() -> &'static RegistryHandle
- SecureBytes / SecureString used for all secret material in TunnelStartParams
- CreateTubeRequest.settings is HashMap<String, serde_json::Value> — secrets WILL escape here
- Rule 1 violation pattern: infra depending on use-case for a trait is an INWARD dependency inversion problem

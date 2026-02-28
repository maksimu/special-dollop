# Architecture Decisions

## Phase 2 keeper-tunnel: TunnelPort trait placement

**Decision context (2026-02-28):** TunnelPort and TunnelSignalingPort traits were placed
in keeper-pam (use-case layer). keeper-tunnel (infrastructure) must implement TunnelPort.
This creates an infra -> use-case dependency.

**Problem:** Clean Architecture says infra should NOT depend on use-case layers.
The standard fix is to move the port trait to keeper-core (domain layer), where infra
can depend on it without crossing inward-only dependency rule.

**Current state:** Traits are in keeper-pam::use_cases::tunneling. The plan proposes
keeper-tunnel depends on keeper-pam. This is a Rule 1 violation.

**Correct fix:** Move TunnelPort to keeper-core::traits (domain ports). Then:
- keeper-pam depends on keeper-core (already does)
- keeper-tunnel depends on keeper-core (already fine for infra)
- No infra -> use-case dependency

**Alternative rejected:** Keeping traits in keeper-pam and allowing infra -> use-case
is a pragmatic shortcut but sets a bad precedent. Other infra crates (keeper-api,
keeper-crypto) correctly depend only on keeper-core.

## Global REGISTRY in keeper-pam-webrtc-rs

**Decision context (2026-02-28):** keeper-pam-webrtc-rs exposes get_registry() returning
&'static RegistryHandle backed by a once_cell::Lazy singleton.

**Acceptable encapsulation:** The global is fully contained inside the external crate.
WebRtcTunnelPort wraps it behind the TunnelPort trait. From the Commander-rust
architecture's perspective, this is the correct approach — the singleton is an
infrastructure implementation detail invisible to use-case and domain layers.

**NOT acceptable:** Exposing the RegistryHandle type or get_registry() through
keeper-tunnel's public API. The adapter must be the only caller.

# ROADMAP

v0.1 delivers the full strict-checkout P2P sync engine: internet-native NAT
traversal, exclusive leases, delta sync, version history, quarantine-based
tamper handling, and a complete offline integration test suite. Everything
below is **out of scope for v0.1** and tracked here for later milestones.

## Post-v0.1

- [x] **P1 — Throughput & UX polish**
  - [x] Parallel chunking with `rayon`
  - [x] Progress bars with `indicatif`
  - [x] Terminal QR-code invite rendering
- [ ] **P2 — Connectivity insight**
  - [ ] Live path-telemetry panel
  - [ ] `doctor` NAT report
  - [ ] Reconnect polish
- [ ] **P3 — Relay & LAN**
  - [ ] Self-hosted relay guide (docker compose)
  - [ ] LAN mDNS auto-fallback mode
- [ ] **P4 — Lease ergonomics**
  - [ ] Opt-in auto-lock-on-first-write
  - [ ] Lock waitlist notifications
  - [ ] Configurable TTL
- [ ] **P5 — Windows & service**
  - [ ] Windows hardening (long paths, read-only attribute edges)
  - [ ] Background service (systemd / launchd / Task Scheduler)
  - [ ] Code signing
- [ ] **P6 — Security pass**
  - [ ] `cargo-fuzz` targets (frame decoder, ticket parser, manifest parser)
  - [ ] Handshake replay tests
  - [ ] Threat model document
- [ ] **P7 — Mobile companion**
  - [ ] FFI bindings from the lib crate (Swift / Kotlin, mirroring iroh's own
        binding path)

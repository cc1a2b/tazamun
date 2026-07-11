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
- [x] **P2 — Connectivity insight**
  - [x] Live path-telemetry panel
  - [x] `doctor` NAT report
  - [x] Reconnect polish
- [x] **P3 — Relay & LAN**
  - [x] Self-hosted relay kit (one-command docker compose + auto TLS)
  - [x] LAN mDNS discovery on by default (secured, `via LAN` status tag)
  - [x] Airgap / closed-network mode + persistent per-session net config
- [x] **P4 — Lease ergonomics**
  - [x] Opt-in auto-lock-on-first-write (preserve-first; 60s idle auto-release)
  - [x] Lock waitlist notifications (`lock --wait`, LockInterest/LockFreed)
  - [x] Configurable TTL (lease-scoped on the wire; `locks` command; humantime config)
- [x] **P5 — Windows & service**
  - [x] Windows hardening (long paths in `\\?\` form at every choke-point,
        read-only attribute ordering, bounded retry for contended ops,
        non-portable remote paths held unapplied)
  - [x] Background service (systemd user unit / launchd LaunchAgent / Windows
        Scheduled Task) with a size-rotated daemon log
  - [x] Code signing groundwork (release provenance attestations; certificate
        signing deferred and documented in docs/SIGNING.md)
- [x] **P6 — Security pass**
  - [x] `cargo-fuzz` targets (frame decoder, ticket parser, manifest parser,
        full `Msg` deserializer) — ~75.7M executions, zero crashers
  - [x] Handshake replay + wrong-secret-matrix + nonce-freshness tests
  - [x] Malicious-insider + wire-traversal tests (nothing un-verified written)
  - [x] DoS/resource bounds across the wire surface (handshakes, peers, pulls,
        waitlist, lease table, manifest-blob size)
  - [x] Threat model document + pentest playbook + runnable hostile-peer kit
- [ ] **P7 — User surface**
  - [ ] Local web dashboard served by the daemon: live members & health from
        the `status` JSON schema-1 contract, file & lock table with one-click
        lock/unlock, conflicts browser, version history + restore, invite QR
  - [ ] CLI polish: shell completions, man page
  - [ ] Portability polish: opt-in name-mangling so Windows nodes can
        materialize non-portable paths under an escaped name (today they are
        held as "unapplied" — never guessed, never mangled silently)

## Final acceptance (after P7, before the single v0.1.0 tag)

Recorded 2026-07-11; updated after the local-only freeze was **rescinded** for
the push-freely policy (DECISIONS.md). Pushes now happen at every phase close,
so history reaches GitHub incrementally and `ci.yml` stays
`workflow_dispatch`-only. What still must be cleared before the release tag:

- [x] **a.** Full history on GitHub with clean-repo gates green (`git grep -i
      claude` AND `-i anthropic` empty, single cc1a2b identity on every
      commit). An ongoing invariant, kept green on every push.
- [ ] **b.** One full cold 3-OS pass via `workflow_dispatch` (ci.yml stays
      dispatch-only): restart the two self-hosted runner services for the
      linux + windows legs, plus one paid `macos-full` under the $5 cap.
- [ ] **c.** The deferred platform debt, all of it: P5 macOS LaunchAgent live
      bootstrap check · P3 two-network Relayed proof (two machines, different
      networks, self-hosted relay, status shows Relayed + hostname) · the
      full SMOKE ladder P0→P7 on the release binary.
- [ ] **d.** Only when a–c are all green: the single annotated v0.1.0 tag,
      pushed, which fires the parked release.yml.

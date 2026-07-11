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
- [ ] **P6 — Security pass**
  - [ ] `cargo-fuzz` targets (frame decoder, ticket parser, manifest parser)
  - [ ] Handshake replay tests
  - [ ] Threat model document
- [ ] **P7 — User surface**
  - [ ] Local web dashboard served by the daemon: live members & health from
        the `status` JSON schema-1 contract, file & lock table with one-click
        lock/unlock, conflicts browser, version history + restore, invite QR
  - [ ] CLI polish: shell completions, man page
  - [ ] Portability polish: opt-in name-mangling so Windows nodes can
        materialize non-portable paths under an escaped name (today they are
        held as "unapplied" — never guessed, never mangled silently)

## Final acceptance (after P7, before the single v0.1.0 tag)

Recorded 2026-07-11 under the local-only development policy (owner decision;
the policy is verbatim in DECISIONS.md). Until final acceptance: no pushes and
no GitHub Actions — the local gates and the SMOKE ladder are the only gates.
In item (a) the scrub pattern is spelled `<assistant-name>` so this file can
never trip the very gate it records.

- [ ] **a.** ONE push of the complete local history to GitHub (after
      clean-repo gates over the FULL accumulated history: `git grep -i
      <assistant-name>` empty, `git ls-files` empty, `git log --all --grep`
      empty, single cc1a2b identity on every commit).
- [ ] **b.** Restore ci.yml push/pull_request triggers; restart both
      self-hosted runner services; one full cold 3-OS pass (self-hosted linux
      + windows, one paid macos-full under the existing $5 cap).
- [ ] **c.** The deferred platform debt, all of it: P5 macOS LaunchAgent live
      bootstrap check · P3 two-network Relayed proof (two machines, different
      networks, self-hosted relay, status shows Relayed + hostname) · the
      full SMOKE ladder P0→P7 on the release binary.
- [ ] **d.** Only when a–c are all green: the single annotated v0.1.0 tag,
      pushed, which fires the parked release.yml.

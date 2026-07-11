# tazamun v0.1.0

**تزامُن — strict-checkout P2P folder sync. No server ever reads your files.**

tazamun keeps a folder in sync across machines over an encrypted peer-to-peer
link, with a discipline borrowed from version control instead of the usual
"last write wins" free-for-all: **every synced file is read-only until you take
an exclusive lease on it.** No file is ever silently overwritten, and no server
ever sees your bytes.

## What it is

- **Strict exclusive checkout.** Files are read-only on disk. `tazamun lock
  <path>` takes a cluster-wide exclusive lease (granted only when every
  connected peer agrees and your copy is up to date), makes the file writable,
  and `tazamun unlock` publishes the edit and returns it to read-only. An
  un-leased edit is never propagated — it is quarantined and the indexed version
  is restored.
- **The Golden Invariant.** Never overwrite data a peer has not seen; never
  silently delete user bytes. Every ambiguous situation preserves both copies
  under `.tazamun/conflicts/` and warns loudly.
- **Internet-native, no server.** Built on [iroh](https://iroh.computer): NAT
  traversal and hole-punching from an invite ticket alone. Direct connections
  are end-to-end encrypted (QUIC/TLS); a relay is only an encrypted fallback and
  **never sees file content**. Run your own relay (`deploy/relay/`) to keep even
  metadata in-house, or go fully closed-network with `--airgap`.
- **Delta sync + history.** Content-defined chunking transfers only what
  changed; the last few versions of every file are kept and restorable.
- **Web dashboard.** A local, loopback-only control panel the daemon serves —
  members & health, one-click lock/unlock with inline diagnosis, conflicts,
  version history + restore, and an invite QR. For people who would rather click
  than type.

## The privacy promise

No account, no cloud, no telemetry. The session secret lives only on member
machines (mode 0600, zeroized in memory). Relays and any on-path observer see
opaque ciphertext, never content. What is *not* hidden: that two endpoints talk,
their IP addresses, and traffic timing/volume — inherent to peer-to-peer. See
`docs/THREAT_MODEL.md` for the full model and the explicit "not defended" list.

## Install

Prebuilt, checksummed binaries with build-provenance attestations for
`x86_64-linux-gnu`, `x86_64-windows-msvc`, and `aarch64`/`x86_64` macOS are
attached to this release.

```bash
# Linux / macOS (shell installer)
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/cc1a2b/tazamun/releases/download/v0.1.0/tazamun-installer.sh | sh
# Windows (PowerShell)
powershell -c "irm https://github.com/cc1a2b/tazamun/releases/download/v0.1.0/tazamun-installer.ps1 | iex"
# Homebrew
brew install cc1a2b/tap/tazamun
# npm wrapper
npm install -g @cc1a2b/tazamun
# From source (Rust stable, edition 2024, MSRV 1.91)
cargo build --release
```

Shell completions: `tazamun completions <bash|zsh|fish|powershell|elvish>`.
Man page: `tazamun man`.

## Dashboard API (`api:1`)

The dashboard is backed by a small, versioned, **loopback-only, token-guarded**
JSON API (a stable contract for any future client):

| Method | Endpoint | Token | Purpose |
| --- | --- | --- | --- |
| GET | `/api/state` | no | One snapshot: members+health, files & leases, pulls, conflicts, version entries, id, mode, config |
| GET | `/api/invite` · `/api/invite/qr` | no | ticket · SVG QR |
| POST | `/api/lock` · `/api/unlock` | yes | acquire / release a lease |
| POST | `/api/restore` | yes | restore a kept version |
| POST | `/api/config` | yes | live subset: autolock, lease-ttl, dashboard-port |

Bind is `127.0.0.1` only; mutations require the per-start token (delivered in
the URL fragment); a non-loopback `Host` is refused (anti-DNS-rebinding); strict
CSP with a per-response nonce. See the README "Web dashboard" section.

## Verified on

<!-- MATRIX: finalized at tag time from the acceptance SMOKE + CI runs. -->

| Platform | How | Status |
| --- | --- | --- |
| Linux x86_64 (WSL2) | full test suite + release-binary SMOKE ladder P0→P7 | ✅ verified |
| Linux x86_64 | self-hosted CI (fmt · clippy · `cargo test --all-targets`) | ✅ verified |
| Windows x86_64 (NTFS) | self-hosted CI (native `cargo test --all-targets`) | ✅ verified |
| macOS (aarch64/x86_64) | hosted CI `macos-full` | ⚠️ **not run** — the account's Actions run was refused for billing ("recent account payments have failed / spending limit"); resolve in GitHub Billing and re-dispatch. Shared-Unix paths are covered by the Linux suite; the macOS-only artifact (the LaunchAgent plist) is golden-file unit-tested cross-platform. |
| Relayed path, two networks | self-hosted relay + two machines, `status` shows `Relayed` + hostname | ⚠️ **manual** — see `deploy/relay/acceptance-drill.sh`; recorded in `SMOKE.md` once run. |

Security: `cargo audit` reports **0 vulnerabilities** (three transitive
build-time "unmaintained crate" advisories are accepted and documented in
`DECISIONS.md`). ~75.7M fuzz executions across four targets in P6 found zero
crashers.

## Thanks

Built by cc1a2b. Bug reports and security issues: see `docs/THREAT_MODEL.md`
§ "Reporting a security issue".

# Tazamun v0.1.0

**Strict-checkout P2P folder sync. No server ever reads your files.**

The first public release. A plain folder stays in lockstep across machines over
an authenticated, end-to-end-encrypted QUIC link, and to change a file you check
it out — an exclusive, network-granted lease, so two people can never quietly
overwrite each other.

## The three commitments

- **One writer at a time.** Every synced file is read-only on disk. A lease is
  granted only when all three preconditions hold — reachability, freshness, and
  no live lease — computed identically on every node.
- **Nobody in the middle can read it.** Content is chunked, BLAKE3-addressed and
  streamed over authenticated QUIC. Relays forward sealed packets and cannot
  open them; even presence beacons are encrypted under the session key.
- **Your bytes are never silently lost.** The Golden Invariant: never overwrite
  data a peer has not seen, never silently delete user bytes. Every ambiguous
  case resolves the same way — preserve both copies, warn loudly.

## What is in it

- **Sync engine** — FastCDC chunking, delta transfer, version vectors, kept
  history with tags and pins, quarantine-based conflict handling, and an
  append-only audit log that reads offline.
- **Networking** — NAT hole-punching with an end-to-end-encrypted relay
  fallback, LAN mDNS discovery, self-hosted relay support, and an airgap mode
  that talks to nothing outside your network.
- **Command line** — `init`, `join`, `start`, `status`, `lock`/`unlock`,
  `versions`/`restore`, `conflicts`, `log`, `doctor`, `setup`, and a one-shot
  `send`/`receive` that needs no session at all. A refusal names the
  precondition that blocked it, the peers consulted, and what to do next.
- **Desktop app** — `tazamun gui` opens a real native window on Windows, macOS
  and Linux, compiled into the same binary. No browser, no webview, nothing
  extra to install.
- **Web dashboard** — `tazamun dashboard` serves a loopback-only, token-guarded
  panel on demand; nothing binds until you ask for it.
- **Policy** — per-folder roles (editor / viewer / archive) enforced on the wire
  through signed capability grants, strict and easy modes, an ignore engine with
  selective sync, and a device-wide service that hosts every folder in one
  process.

## Platforms

Prebuilt binaries for Linux and Windows. macOS builds from source with
`cargo build --release` — the one platform this project cannot cross-compile.

Release artifacts carry **SLSA build-provenance attestations**; verify with
`gh attestation verify <file> --repo cc1a2b/tazamun`. They are not
Authenticode-signed (Windows) or Developer-ID-signed and notarized (macOS), so
SmartScreen's "unknown publisher" warning and macOS Gatekeeper quarantine still
apply. Code signing needs paid certificates and is deferred.

## Honest limitations

- **A member you invited is inside the trust boundary.** Anyone holding the
  session secret can read, write and publish. Revocation is `tazamun rekey`,
  which mints a new key for the members you keep. There is no defence against
  someone you chose to trust.
- **A compromised machine is a compromised session.** The secret lives in
  `state.json` at 0600; whoever can read your disk has the session.
- **Traffic analysis is not addressed.** Your files cannot be read in transit,
  but sizes and timing are not hidden.
- **Known-unverified:** the macOS hardware path, and a two-network Relayed-path
  proof. Both are documented rather than claimed.
- **Two `quick-xml` denial-of-service advisories** (RUSTSEC-2026-0194 and
  RUSTSEC-2026-0195) are present in the dependency tree and are deliberately not
  silenced. One path is a build-time proc-macro no attacker can reach; the other
  is `self_update` parsing release metadata during `tazamun update`. No version
  of this tree resolves them yet — `self_update` 0.44.0 is the latest release
  and still requires `quick-xml ^0.38`. Reasoning in `.cargo/audit.toml`.

Security reporting: [SECURITY.md](SECURITY.md). The full adversary analysis is
in [docs/THREAT_MODEL.md](docs/THREAT_MODEL.md), and every load-bearing design
decision — including the ones that turned out to be wrong — is recorded in
[DECISIONS.md](DECISIONS.md).

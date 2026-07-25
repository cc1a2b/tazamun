# Tazamun v0.1.8

A one-command rename: `tazamun mv <old> <new>`. Renaming a synced file with a
plain `mv` leaves the old name behind on every peer, because a rename is a
delete plus a create and tazamun reverts an un-leased delete on purpose (so an
accidental `rm` can't wipe a file off every peer). This does the safe dance for
you in one step.

- `tazamun mv a.pdf b.pdf` leases the old name, renames on disk, publishes the
  new name, and publishes the removal of the old — so peers end up with only
  `b.pdf`, no duplicate, and you never touch `lock`/`unlock` yourself.
- It refuses to clobber: an existing destination, a missing or non-file source,
  and a same-name move are all rejected. Verified by a two-peer test that
  renames a file and asserts the peer gains the new name and loses the old.

Plain shell `mv` still reverts by design — deletes need a lease. `tazamun mv`
is the safe way to rename without that being manual. No engine changes; the
delete-protection core is untouched.

---

# Tazamun v0.1.7

Clearer messaging when a synced file is deleted or renamed without a lease.
Renaming a synced file looked like it duplicated it: a rename is a delete of
the old name plus a create of the new one, and tazamun reverts an un-leased
delete on purpose (the Golden Invariant — a bare `rm` must not silently wipe a
file from every peer), so the old name came back while the new one published.

- The revert message now tells the truth for a delete. It no longer claims
  "offending bytes quarantined" (a delete has none) and instead explains the
  file was restored and gives the exact way to delete or rename a synced file:
  lock it, delete/rename, unlock — the unlock publishes the removal to peers.

This is a message-only change; the delete-protection behaviour is unchanged and
deliberate. To rename `a` to `b` so it propagates cleanly:
`tazamun lock a`, `mv a b`, `tazamun unlock a` (and `b` auto-publishes). No
engine changes.

---

# Tazamun v0.1.6

`tazamun send <folder>` no longer aborts when the folder carries tazamun's own
metadata. A folder that was ever `tazamun init`'d keeps a `.tazamun/` directory
(state, audit log, history); `send` was scooping that into the transfer
manifest, and the receiver correctly refused it — `.tazamun` is reserved and
accepting untrusted paths there would be a security hole — which killed the
whole transfer with "manifest has a hostile path: .tazamun/audit.jsonl".

- The send walk now skips `.tazamun` (and the receiver-staging `.tazamun-recv`)
  the same way the session sync does — it is tooling metadata, not your files.
- Fixed the underlying logic error too: the manifest filter *included* any path
  its own sanitizer rejected, instead of dropping it. A send can now never
  offer a path the receiver will refuse. Both are covered by a test that
  reproduces the exact folder-with-`.tazamun` case, Arabic filenames and all.

The receiver's strict rejection is unchanged — it stays as a security backstop
against a hostile sender. No engine changes.

---

# Tazamun v0.1.5

`tazamun doctor` now names the one environment where a healthy daemon still
cannot reach its peers: **WSL2 in the default NAT networking mode**. Two WSL
machines there each sit on their own isolated `172.16/12` subnet, so neither a
session join nor a one-shot `send`/`receive` can connect — both just time out,
with nothing explaining why.

- Doctor detects WSL2 NAT mode (WSL kernel plus a `172.16/12` outbound address)
  and prints the fix inline: enable WSL **mirrored networking** on both
  machines (`networkingMode=mirrored` in `.wslconfig`, then `wsl --shutdown`),
  which needs Windows 11 22H2+; on Windows 10, run the native Windows build
  rather than the one inside WSL. The check is pure and unit-tested.

If your peers won't connect, run `tazamun doctor` — it will now tell you
whether this is why. No engine changes.

---

# Tazamun v0.1.4

The second half of the Windows self-update fix. v0.1.2 taught the updater
where the binary lives inside the zip; this teaches it to decompress the zip
at all. A Windows `tazamun update` downloaded the release, found the binary,
and then died with `ZipError: Compression method not supported` — because the
updater was built to handle zip archives but without the DEFLATE decompressor,
and every release zip is DEFLATE (the universal zip compression).

- Added the `compression-zip-deflate` feature to `self_update`. The build
  already had `compression-flate2` — but that is the gzip decoder for the unix
  `.tar.gz`, not the DEFLATE decoder for the Windows `.zip`. Both are needed;
  a Cargo.toml comment now says so, and a test extracts a real DEFLATE zip
  through self_update's own extractor so the feature can't silently regress.

Windows binaries at v0.1.0–v0.1.3 cannot self-update past this: their updater
still lacks the decompressor. Reinstall once — `npm update -g tazamun`, re-run
the installer, or grab the zip — to reach v0.1.4, and every later update works
in place. No engine changes.

---

# Tazamun v0.1.3

The WSL drive-mount fix. `tazamun init` inside a Windows drive mounted in WSL
(`/mnt/c`, `/mnt/e`, …) used to succeed, mint a real invite, and then `tazamun
start` failed with a bare `ipc io: Operation not supported (os error 95)` and
no explanation. Those 9p mounts support neither Unix sockets nor reliable
change events, so a session there can never run.

- **`init` now refuses up front**, before writing any session state, when the
  folder's filesystem cannot host the daemon — it probes by binding a
  throwaway socket and removing it. The message names the cause and two real
  fixes: keep the session on your native Linux home, or sync the Windows drive
  with the native Windows build as its own peer.
- **`start` gives the same clear message** instead of the raw errno, so an
  older session created before this release explains itself too.

If you have a session stranded on a `/mnt` drive: re-init it under your Linux
home (`~/tazamun/<folder>`) and re-share the invite. No engine changes.

---

# Tazamun v0.1.2

The Windows self-update fix. If you are on Windows with v0.1.0 or v0.1.1,
`tazamun update` cannot carry you here — reinstall once
(`npm update -g tazamun`, or re-run the installer, or grab the zip) and every
later update works in place.

- **`tazamun update` on Windows died extracting** ("specified file not found
  in archive"). The two archive formats have different layouts — the unix
  tar.gz nests the binary under `tazamun-<target>/`, the Windows zip is flat —
  and the updater assumed the tar shape for both. Each format now gets its own
  path, pinned by tests against the live release layouts.
- **Updates no longer stop to ask.** The confirm prompt and the step-by-step
  chatter are gone: `tazamun update` states what it found, shows the download,
  and reports the swap. The old `-y` flag remains accepted.
- **Package-manager installs are recognised.** When the running binary lives
  inside npm's or Homebrew's tree, a successful self-update now says so and
  names the manager's own command — the manager's records still hold the old
  version, and its next operation may roll the file back.
- **A release without the Homebrew tap token now skips the formula job**
  instead of failing it.

No engine changes.

---

# Tazamun v0.1.1

A plumbing release, one day after v0.1.0 — no engine changes. Its purpose is
to exercise the one path a first release cannot prove about itself:
`tazamun update` from an installed v0.1.0 to a newer version, end to end.

- The Homebrew tap is initialized, so the formula publish lands once the tap
  token is in place; `npm install -g tazamun` and both one-line installers are
  already live and verified against v0.1.0.
- Release automation runs entirely on GitHub-hosted runners; every platform
  archive carries a `.sha256` and a build-provenance attestation, as before.

Nothing about the sync engine, the protocol, the CLI surface, or the desktop
app changed. If you are on v0.1.0: `tazamun update`.

---

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

Prebuilt binaries for x86_64 Linux, Intel and Apple-silicon macOS, and x86_64
Windows — plus a one-line installer for each, a Homebrew tap
(`brew install cc1a2b/tap/tazamun`), an npm package (`npm install -g tazamun`),
and the crate on crates.io (`cargo install tazamun`).

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

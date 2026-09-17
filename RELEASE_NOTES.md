# Tazamun v0.2.0

A menu bar in the head of the window, the high-contrast palette as the one it
opens in, and the in-app updater that was CLI-only until now.

## The bar

`session · tools · display · help`, ruled across the title bar in the register's
own hand — lowercase tracked heads with a hairline between them, exactly the way
a column heading is set, not a File/Edit/View strip borrowed from somewhere else.

- **session** — start, stop, pause, resume, open folder, copy path, rename.
- **tools** — diagnostics, the web dashboard, reclaim disk space, rotate the
  session key, delete preserved copies, and the machine's supervisor. Split by
  what the operation belongs to, which is also where the enablement boundary
  falls: the first block needs a running daemon, rekey does not, and the
  supervisor is a property of the machine.
- **display** — palette, density, motion and text size, which were previously
  reachable only from Home. A reader inside a session had to leave it to make
  the text bigger.
- **help** — the keyboard sheet, the colophon, and **updates**.

Every item is operable from the keyboard (F10 opens the bar, arrows move,
Escape closes, Tab never traps), every unavailable item says why on hover rather
than disappearing, and the chords printed beside items are read out of the one
shortcut registry so the menu can never advertise a key that does nothing. At a
narrow window the heads fold from the right into a `more` head; a menu carrying
news — an update waiting — is pinned and never folds away.

The session header's row of five ghost buttons is gone: the bar carries those
verbs now, with their keyboard routes and their reasons, and at a large text
size the buttons and the folder path used to collide.

## Updates, in the window

`tazamun update` existed only as a CLI command, so a window left open for weeks
had no way to learn it was stale. **help → Check for updates** now reports its
state rather than just offering a verb — not checked yet, checking, "0.1.9 is
the newest release, checked 4 minutes ago", "0.2.1 is ready to install", or the
reason the last check could not finish. When there is something to say, the
`help` head carries a mark so the bar says it without being opened.

A check never installs. An install reuses the same path the CLI does, so the
archive-layout and self-replace contracts are untouched. A copy owned by npm or
Homebrew refuses to replace itself and tells you the manager's own command
instead — updating underneath a package manager leaves its records naming a
version that is no longer there.

## Contrast is the default palette

It was built for low vision and bright rooms, and it turned out to be the
clearest statement of the whole design: ink-black ground, white text, and the
brand gold carrying every mark that means something. An existing `gui.json`
names its own palette and still means exactly what it said — only a preferences
file that never chose one takes the new default.

## Fixed

- **Two-line settings rows were sliced through the middle.** A register row is a
  fixed height because the body is virtualised, and the Display rows stack a
  name over its hint. They now declare that stack and are ruled at the height
  the type needs, derived from the scale rather than from a constant that
  happened to fit at 100%.
- **Fixed columns did not grow with the text size**, so at 200% a column held
  about a third of the text it held at 100% and the rest was clipped. They now
  scale with the type they carry — and a flexible column has a floor, so the
  column an entry is identified by can never be squeezed to two letters by the
  fixed ones beside it.
- The column ruler, the empty state, the loading skeleton, the folio margin and
  the custody seal were all sized from constants and are now derived from the
  type scale; each one clipped or collided somewhere above ~1.2x.
- The running version moved from the sidebar, where a long session list scrolled
  it out of sight, to the foot of the window, which never scrolls.

# Tazamun v0.1.9

The window is rebuilt as a register of custody. The GUI read as a generic dark
dashboard: the codebase already owned a real Islamic-geometric language —
khatam stars, girih strapwork, the house diamond — and modules named for a
manuscript, but the chassis underneath was cards and capsules and the ornament
was decoration sprinkled on top. This makes that language the structure of the
page instead.

## What you will notice

- **Entries are ruled, not boxed.** Files, Peers, History, Conflicts and Audit
  were stacks of cards, each free to disagree with the next about where its
  columns sat. They are now one ruled register each, under a single column
  ruler, with a folio in the margin and a khatam seal beside anything under
  lease. A file list that showed five rows now shows twenty-plus.
- **Three palettes.** Night (ink on a dark desk), Paper (ink on warm stock) and
  Contrast, switchable in Settings alongside register density and a
  reduced-motion setting. All four persist.
- **New typography.** Inter and Noto Sans Arabic are replaced by the IBM Plex
  superfamily — nine faces for 1.87 MB against the old four for 2.09 MB. Mono
  carries every figure, so columns of bytes and timings actually line up.
- **The Overview answers the question.** It opens with a sentence — "5 files,
  nothing is held, in step with 1 peer." — and shows what needs you only when
  something does, instead of a strip of numbers to assemble yourself.

## Things that were wrong, not merely plain

- **Conflict resolution had its severity inverted.** "Keep mine" overwrites the
  file on every peer *and* deletes the preserved copy, yet it was the
  unconfirmed primary button while the lesser "keep theirs" wore the danger
  styling and asked first. Both destructive verbs now warn and confirm.
- **`keep both` was missing entirely** — the one resolution that deletes
  nothing. Worse, for a preserved copy whose original path was never recorded,
  the only action the window offered was the one that destroyed it.
- **A failed read rendered as reassurance.** An unreadable conflicts directory
  displayed "No conflicts waiting — every preserved copy is resolved". The view
  layer can no longer express "empty" for a source it could not read.
- **Lease refusals threw away the daemon's diagnosis.** It names which of the
  three preconditions blocked the edit, what clears it, and who holds the
  lease; the window kept the one-line message and dropped the rest. The words
  REACHABILITY, FRESHNESS and LEASE now appear where they are needed.
- **Accessibility was compiled out.** `default-features = false` dropped
  AccessKit from eframe's defaults, so every screen-reader annotation in the
  app — including those that predate this release — was inert. It is on, and
  the painted figures (the balance, the peer mesh, the status strip, the invite
  ticket) now describe themselves in a sentence.

## Under it

- One wedged daemon used to freeze the whole window for the full 30-second IPC
  timeout, because the worker awaited every command inline. Commands now run
  concurrently behind a gate per folder, sessions are polled in parallel on a
  3-second timeout, and every click raises a ticket you can see and cancel.
- Eleven operations the CLI has always had are reachable from the window for
  the first time: `lock --wait`, `mv`, `diff`, `doctor`, `dashboard`, `gc`,
  `conflicts prune`, role- and TTL-scoped `invite`, `rekey`, and the supervisor.
- Files past the daemon's 1000-path cap were invisible to any UI. A server-side
  query makes them searchable, pageable and lockable, and the same is true of
  the audit ledger.

## Fixed in the test suite

Three timing races in the conflict tests and two Windows-only path assertions
had been failing since before v0.1.4 without anyone seeing them: the `full`
matrix, the only job that builds on Windows and macOS, is skipped for pushes to
main. One of the races presented as the product overwriting bytes it had in
fact preserved correctly — a test accusing the engine of losing data when it
had not. All five are fixed and all three platforms are green.

No engine changes. The Golden Invariant, the lease state machine and the path
sanitizer are untouched.

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

# TAZAMUN · تزامُن

<div align="center">

[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.91+-B7410E?style=flat&logo=rust)](https://www.rust-lang.org)
[![Release](https://img.shields.io/github/release/cc1a2b/tazamun.svg)](https://github.com/cc1a2b/tazamun/releases)
[![GitHub stars](https://img.shields.io/github/stars/cc1a2b/tazamun)](https://github.com/cc1a2b/tazamun/stargazers)
[![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-lightgrey)](https://github.com/cc1a2b/tazamun/releases)

**🔒 مزامنة مجلّد لحظية بين الأنداد — بدون خادم يقرأ ملفّاتك.**

*Real-time peer-to-peer folder sync. No server ever reads your files.*

</div>

> **Development status:** v0.1 is in active development. Work is tracked phase by
> phase in [ROADMAP.md](ROADMAP.md); there is **no released version yet**.

## 📖 About · نبذة

**تزامُن** (tazamun, "synchronization") is a single-binary CLI that lets remote
collaborators share one folder with real-time peer-to-peer sync — over the open
internet from the very first minute. Two people behind different NATs connect
with nothing but an invite ticket; [iroh](https://iroh.computer) hole-punches a
direct encrypted QUIC path and falls back to an end-to-end-encrypted relay only
when it must. No central server ever stores or reads a single byte of file
content. No Git, no commits, no merges — just files, of every kind.

**تزامُن** أداة سطر أوامر بملفّ تنفيذي واحد تتيح لمتعاونين عن بُعد مشاركة مجلّد
واحد مع مزامنة لحظية بين الأنداد عبر الإنترنت مباشرةً. يتّصل شخصان خلف شبكتين
مختلفتين بمجرّد تذكرة دعوة، دون أن يقرأ أيّ خادم مركزي بايتًا واحدًا من محتوى
ملفّاتك.

`tazamun status` shows every member, how you are connected to each
(Direct/Relayed + live RTT), and what is currently leased or syncing:

```text
peer id : 7b3f2a9c1d8e4f60a5b2c7d9e0f1a2b3c4d5e6f708192a3b4c5d6e7f80912a3b4
folder  : /home/hassan/project
files   : 128 (94371840 bytes)

members (2):
  9f2c4a7e10  online   Direct   rtt 24 ms
  3d8b1f60ca  online   Relayed  rtt 118 ms

active leases (1):
  designs/logo.psd  held by 9f2c4a7e10  expires in 74s

pending pulls (1):
  src/render/pipeline.rs
```

---

## 📑 Table of Contents

- [The Promise](#-the-promise--الوعد)
- [Features](#-features)
- [Installation](#-installation)
- [Quick Start](#-quick-start--البداية-السريعة)
- [Usage Examples](#-usage-examples)
- [Command Reference](#-command-reference)
- [How It Works](#-how-it-works)
- [Internet Acceptance Checklist](#-internet-acceptance-checklist)
- [Advanced Usage](#-advanced-usage)
- [Contributing](#-contributing)
- [License](#-license)
- [Support](#-support)

---

## 🔐 The Promise · الوعد

> **No server can read your files.** Connections are direct whenever possible;
> the encrypted relay fallback is self-hostable with `--relay` or disabled
> entirely with `--no-relay`.

- **🛡️ Zero-knowledge transport:** file content is chunked, BLAKE3-addressed, and
  streamed peer-to-peer over iroh's authenticated, encrypted QUIC. Relays only
  ever forward opaque, end-to-end-encrypted bytes.
- **🔑 Secret-gated membership:** knowing the gossip topic is not enough. Every
  control connection proves knowledge of the session secret in both directions
  before a single message is processed.
- **🕵️ Unreadable metadata:** even presence beacons (who is online, at what
  address) are XChaCha20-Poly1305 sealed under the session key.

---

## ✨ Features

- **🌍 Internet-native from day one:** NAT-to-NAT via hole-punching, no port
  forwarding, no VPN, no config.
- **🔒 Strict exclusive checkout:** every synced file sits read-only; editing
  requires an exclusive, network-granted lease, so two people never clobber
  each other's work.
- **🧩 All file types:** text, code, images, design files, game assets, binaries
  — no exceptions, no diff/merge assumptions.
- **⚡ Delta sync:** content-defined chunking means a one-line edit in a 2 GB file
  moves only the changed chunks.

<details>
<summary><b>More features</b></summary>

- **🧷 The Golden Invariant:** never overwrite data a peer has not seen, never
  silently delete user bytes. Every ambiguous situation preserves both copies
  and warns loudly.
- **🗂️ Built-in version history:** the last 5 versions of every path are kept
  locally; `restore` re-publishes any of them under a lease.
- **📡 Connection-strength surface:** `status` shows each peer as Direct or
  Relayed with live RTT — you always know how you are connected.
- **🩹 Tamper-evident:** a forced write to a read-only file is quarantined
  (never deleted) and the indexed version is restored, with a loud warning.
- **♻️ Self-healing membership:** a full mesh of authenticated control
  connections with exponential-backoff redial.
- **🔌 Self-hostable or serverless:** bring your own relay with `--relay`, or run
  fully relay-free on a LAN with `--no-relay --lan`.

</details>

> note: `tazamun` is strict by design. With **zero** connected peers, every edit
> path (lock, restore, new file) is refused — because with no one to coordinate
> with, there is no safe way to guarantee the Golden Invariant.

---

## 📦 Installation

### Build from source (all platforms)

```bash
# Requires the Rust stable toolchain (edition 2024, MSRV 1.91)
git clone https://github.com/cc1a2b/tazamun
cd tazamun
cargo build --release
# → one self-contained binary at target/release/tazamun
```

### System requirements

- **Rust:** stable ≥ 1.91 (only to build; end users install nothing else).
- **OS:** Linux, macOS, or Windows.
- **Network:** any internet connection. No inbound ports, no static IP.

> **WSL note:** on Windows Subsystem for Linux, keep your session folder on the
> **native Linux filesystem** (e.g. `~/projects/…`), not on a `/mnt/c` or
> `/mnt/e` Windows mount. The Windows mounts do not deliver reliable file-change
> notifications, so live edits would be missed by the watcher.

---

## 🚀 Quick Start · البداية السريعة

Two people, two machines, under five minutes. **Alice** shares, **Basma** joins.

```bash
# ── Alice (initiator) ───────────────────────────────
mkdir project && cd project
tazamun init                 # prints a peer id and an invite ticket (tzm1…)
tazamun start                # foreground daemon — keep it running

# In a second terminal (same folder):
tazamun invite               # a fresh ticket carrying live addresses — send it to Basma

# ── Basma (joiner) ──────────────────────────────────
mkdir project && cd project
tazamun join tzm1…           # paste Alice's ticket into an EMPTY folder
tazamun start                # foreground daemon

# ── Either side, to edit a file ─────────────────────
tazamun lock report.md       # acquire the exclusive lease → file becomes writable
$EDITOR report.md            # edit freely
tazamun unlock report.md     # publish + release → syncs to everyone, back to read-only
```

خطوات المزامنة: `init` ← `invite` ← `join` ← `start` ← `lock` / تعديل / `unlock`.

---

## 💡 Usage Examples

```bash
# Initialize a brand-new session in the current folder
tazamun init

# Join someone else's session (the folder must be empty)
tazamun join tzm1qy…

# Run the sync daemon in the foreground (Ctrl-C stops it cleanly)
tazamun start

# Run relay-free on a local network with mDNS discovery
tazamun start --no-relay --lan

# Route through your own self-hosted relay instead of the public ones
tazamun start --relay https://relay.example.com

# See who is connected, how (Direct/Relayed), and what is locked
tazamun status

# Print a fresh invite ticket with your current live addresses
tazamun invite

# Same, rendered as a scannable QR code in the terminal
tazamun invite --qr

# Take the exclusive lease on a path, edit, then release + sync
tazamun lock assets/logo.png
tazamun unlock assets/logo.png

# Inspect and roll back local history (restore needs a held lease)
tazamun versions assets/logo.png
tazamun lock assets/logo.png
tazamun restore assets/logo.png 1
tazamun unlock assets/logo.png

# Refresh the blob garbage-collection protection set
tazamun gc

# Operate on a folder other than the current directory
tazamun --dir ~/work/project status
```

---

## 📋 Command Reference

```text
tazamun — strict-checkout P2P folder sync. No server ever reads your files.

Usage: tazamun [OPTIONS] <COMMAND>

Commands:
  init                 Initialize this folder as a new sync session and print an invite
  join   <TICKET>      Join an existing session from an invite ticket
  start  [--relay URL] [--no-relay] [--lan]
                       Run the sync daemon in the foreground
  status               Show members, connections (Direct/Relayed + RTT), leases, pulls
  invite               Print a fresh invite ticket carrying live addresses
  lock   <PATH>        Acquire an exclusive lease and make the file writable
  unlock <PATH>        Publish pending edits and release the lease
  versions <PATH>      List kept historical versions of a path
  restore  <PATH> <N>  Restore version N of a path (requires a held lease)
  gc                   Refresh the unreferenced-blob protection set

Options:
      --dir <PATH>     Session folder (defaults to the current directory)
  -v, --verbose        Verbose logging (-v: debug; RUST_LOG is respected)
  -h, --help           Print help
  -V, --version        Print version

Exit codes: 0 success · 1 runtime error · 2 usage error
```

---

## 🔧 How It Works

**Strict exclusive checkout.** Every synced file is read-only on disk. To edit,
a node must obtain an exclusive *lease*, granted only when **all three** hold:

1. **Reachability** — at least one authenticated peer is connected, and every
   connected peer grants the request.
2. **Freshness** — the requester's version vector for the path is up to date
   against every peer, and no newer version is still being pulled.
3. **Lease** — no active, unexpired lease already exists on the path.

Concurrent lock requests resolve deterministically on the total order
`(lamport, endpoint-id)`, so every node computes the same winner. Leases expire
after a TTL (auto-renewed while held), so a crashed holder never freezes a file
forever.

**Data plane.** Files are split with FastCDC content-defined chunking, each
chunk BLAKE3-hashed and stored in a local [iroh-blobs](https://iroh.computer)
store. Syncing a change fetches only the chunks the receiver is missing, every
byte verified on arrival, assembled into a staging file, fsynced, and swapped in
by atomic rename. A failed transfer leaves your folder untouched.

**Control plane.** Peers meet over an encrypted gossip topic derived from the
session secret, then open authenticated QUIC control connections carrying the
index-exchange, lease, and metadata protocol.

---

## 📶 Connection Health

The founding idea of tazamun is *check your connection strength before you
edit*. `status` makes the network legible, and every lock failure explains
itself in network terms.

### The status panel

```text
peer id : 7b3f2a9c1d8e4f60a5b2c7d9e0f1a2b3c4d5e6f708192a3b4c5d6e7f80912a3b4
folder  : /home/hassan/project
files   : 128 (94371840 bytes)

members (2):
  ● Good   9f2c4a7e10 Direct  24±3ms       Δ0
  ● Fair   3d8b1f60ca Relayed 118±9ms      Δ1 via euw-1.relay.n0.iroh.link  ↓1.2 ↑0.0 MB/s

active leases (1):
  designs/logo.psd  held by 9f2c4a7e10  expires in 74s

transfers (1):
  ⇣ src/render/pipeline.rs   62%  8.4 MB/s

recent events:
  • peer 3d8b1f60ca upgraded Relayed→Direct (rtt 34ms)
```

Each member row is: **grade dot** · grade · id · connection type · `rtt±jitter`
· path-change count (`Δ`) · relay hostname when relayed · live rates.

- `tazamun status --watch` — a live panel refreshing every second (press `q` or
  Ctrl-C to exit). Falls back to a single snapshot when stdout is not a TTY.
- `tazamun status --json` — the full telemetry snapshot as stable JSON (schema
  below), the contract for any GUI or script.

### Grade thresholds

| Grade | Dot | Meaning |
|---|---|---|
| **Good** | 🟢 green | Direct path, RTT < 80 ms, jitter < 20 ms |
| **Fair** | 🟡 yellow | stable Relayed, or Direct with elevated RTT/jitter |
| **Poor** | 🔴 red | flapping paths (> 3/min), RTT ≥ 300 ms, or a presence gap on a live connection |
| **Offline** | ⚪ gray | no connection and nothing heard within 30 s |

Colors auto-disable when `NO_COLOR` is set or output is not a terminal.

### `status --json` schema (v1)

```json
{
  "schema": 1,
  "id": "<64-hex endpoint id>",
  "dir": "<absolute path>",
  "members": [
    {
      "id": "<hex>", "online": true, "synced": true,
      "conn": "Direct|Relayed|None",
      "grade": "Good|Fair|Poor|Offline",
      "rtt_ms": 24.0, "rtt_jitter_ms": 3.1,
      "path_changes": 0, "flaps_per_min": 0,
      "relay_url": null,
      "rate_rx_bps": 0.0, "rate_tx_bps": 0.0,
      "bytes_rx": 0, "bytes_tx": 0,
      "time_to_direct_ms": 42
    }
  ],
  "leases": [ { "path": "...", "holder": "<hex>", "mine": false, "expires_in_ms": 74000 } ],
  "pending_pulls": [ { "path": "...", "percent": 62, "bytes_done": 0, "bytes_total": 0, "rate_bytes_per_sec": 0 } ],
  "events": [ { "seq": 7, "text": "peer 3d8b1f60ca upgraded Relayed→Direct (rtt 34ms)" } ],
  "file_count": 128, "total_bytes": 94371840
}
```

### Reading a lock failure

When a lock is refused, tazamun tells you **which precondition failed**, the
**peers it consulted** with their grades, and **what to do**:

```console
$ tazamun lock report.md
✗ could not lock report.md: peer 3d8b1f60ca disconnected while voting on the lease
  blocked precondition : REACHABILITY
  what to do           : the peer whose grant was required went offline — retry once it reconnects
  peers consulted      : 3d8b1f60ca (Offline, None)
  (use -v for the full per-peer table)
```

Add `-v` for the full per-peer table (grade, conn, rtt, answered). If a
consulted peer is on a degraded link, the lock still proceeds (strict
preconditions are the only gate) but prints an advisory first:

```console
⚠ acquiring via a degraded link to 3d8b1f60ca (Relayed, 412ms) — sync may lag behind edits
```

### `tazamun doctor`

A one-shot NAT & environment report — identity and bound sockets, relay
reachability, per-peer connectivity and hole-punch status, filesystem sanity
(watcher backend, native-FS vs WSL `/mnt` warning, read-only-enforcement
probe), and IPC health — each with an `OK` / `WARN` / `FAIL` verdict and an
actionable line. Exit code is `0` (all OK), `1` (any WARN), or `2` (any FAIL);
`--json` for machine output.

```text
$ tazamun doctor
tazamun doctor

[OK  ] identity  [from daemon]
     peer id            : 7b3f2a9c1d8e4f60…
     bound socket       : 0.0.0.0:50794
[OK  ] relay
     policy             : disabled by flag (--no-relay)
     relays             : not used — direct/LAN only
[OK  ] connectivity  [from daemon]
     peer 9f2c4a7e10      : Direct (Good, 24ms, direct in 42ms)
[OK  ] filesystem
     watcher backend    : inotify
     session folder     : /home/hassan/project (native FS)
     read-only enforce  : working (create+chmod probe passed)
[OK  ] ipc
     socket             : /home/hassan/project/.tazamun/daemon.sock
     daemon             : responding

summary: OK
```

---

## ✅ Internet Acceptance Checklist

Run this once on **two machines on different networks** to confirm a real
internet round-trip (not just localhost):

- [ ] On machine A: `tazamun init` then `tazamun start`; in a second shell
      `tazamun invite` and send the ticket to machine B.
- [ ] On machine B (empty folder): `tazamun join <ticket>` then `tazamun start`.
- [ ] On both: `tazamun status` shows the other member **online** with a
      connection type of **Direct** or **Relayed** and a real **RTT**.
- [ ] On A: `tazamun lock demo.txt`, edit the file, `tazamun unlock demo.txt`.
- [ ] On B: `demo.txt` appears with the new content, **read-only**.
- [ ] On B: `tazamun lock demo.txt` succeeds only after the file has synced
      (freshness), edit, `tazamun unlock demo.txt`; A receives the change.
- [ ] Kill A's daemon mid-lease; after the TTL, B can `lock` the same path.

---

## 🧰 Advanced Usage

<details>
<summary><b>Self-hosted relay</b></summary>

Replace the public relay map with your own relay server:

```bash
tazamun start --relay https://relay.mycompany.com
```

All peers in the session should use the same relay for the fallback path.
Direct hole-punched connections still bypass it whenever possible.

</details>

<details>
<summary><b>LAN-only / air-gapped</b></summary>

Disable relays and use local mDNS discovery — nothing leaves the local network:

```bash
tazamun start --no-relay --lan
```

</details>

<details>
<summary><b>Invite by QR</b></summary>

Hand a phone-to-laptop invite across the room without copy-pasting: render the
exact `tzm1…` ticket as a terminal QR code and scan it.

```console
$ tazamun invite --qr
Scan to join this session:

█████████████████████████████
████ ▄▄▄▄▄ █▄▀ ▄██ █ ▄▄▄▄▄ ████
████ █   █ █▀▀█▄▀▄██ █   █ ████
████ █▄▄▄█ █ ▄█▀▄▀ █ █▄▄▄█ ████
████▄▄▄▄▄▄▄█▄█▄█▄█▄█▄▄▄▄▄▄▄████
█████████████████████████████
        …(full code)…

Same ticket as text:

  tzm1kkfcqzhqmshy4ilzmw…
```

The QR encodes the ticket string and nothing else. If the terminal is too
narrow for a scannable code, the plain ticket is printed with a note instead.

</details>

<details>
<summary><b>Performance tuning</b></summary>

Publishing chunks and hashes files with a small worker pool sized
automatically from the machine (hashing saturates the content-defined scan
with very few threads, so more is not better). Override it explicitly with:

```bash
TAZAMUN_THREADS=2 tazamun start
```

Any positive integer works; unset means the measured default
(`min(cores, 4)`).

</details>

<details>
<summary><b>Recovering a quarantined file</b></summary>

If someone force-writes a read-only file (or edits offline without a lease),
tazamun copies the offending bytes to `.tazamun/conflicts/<timestamp>__<path>`
and restores the indexed version. Nothing is ever deleted — retrieve your bytes
from the `conflicts/` directory, then lock the path properly to re-publish them.

</details>

---

## 🤝 Contributing

Contributions are welcome.

- **🐛 Bug reports:** open an issue with your OS, `tazamun --version`, and the
  `-v` log around the problem.
- **✨ Features:** check [`ROADMAP.md`](ROADMAP.md) first — many ideas are already
  planned and scoped.
- **📝 Docs:** clarity fixes and translations (Arabic especially) are appreciated.
- **🔧 Pull requests:** the CI gate is `cargo fmt --check`, `cargo clippy
  --all-targets -- -D warnings`, and `cargo test`. Please keep it green.

```bash
# Dev setup
git clone https://github.com/cc1a2b/tazamun && cd tazamun
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

---

## 📄 License

Released under the [MIT License](LICENSE).

```
Copyright (c) 2025-2026 Hussain Alsharman
```

---

## ☕ Support

If tazamun saved you a headache, consider supporting the work:

[![Buy Me A Coffee](https://img.shields.io/badge/Buy%20Me%20A%20Coffee-support-FFDD00?style=flat&logo=buymeacoffee&logoColor=black)](https://www.buymeacoffee.com/cc1a2b)

⭐ **Star** the repo, **follow** [@cc1a2b](https://github.com/cc1a2b), and
**share** tazamun with anyone who needs private, serverless folder sync.

---

<div align="center">

**🔒 مزامنة بين الأنداد — بدون خادم يقرأ ملفّاتك.**

*Peer-to-peer sync. No server reads your files.*

Built with ❤️ by [cc1a2b](https://github.com/cc1a2b)

</div>

# TAZAMUN · تزامُن

<div align="center">

[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.91+-B7410E?style=flat&logo=rust)](https://www.rust-lang.org)
[![Release](https://img.shields.io/github/release/cc1a2b/tazamun.svg)](https://github.com/cc1a2b/tazamun/releases)
[![GitHub stars](https://img.shields.io/github/stars/cc1a2b/tazamun)](https://github.com/cc1a2b/tazamun/stargazers)
[![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-lightgrey)](https://github.com/cc1a2b/tazamun/releases)

**مزامنة مجلّد لحظية بين الأنداد — بدون خادم يقرأ ملفّاتك.**

*Real-time peer-to-peer folder sync. No server ever reads your files.*

</div>

> **Development status:** v0.1 is in active development. Work is tracked phase by
> phase in [ROADMAP.md](ROADMAP.md); there is **no released version yet**.

## About · نبذة

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

`tazamun status` grades how you are connected to each member (green/yellow/red
dot), and shows what is currently leased or syncing. See
[Connection Health](#connection-health) for the full panel, `--watch`, and
`--json`:

```text
members (2):
  ● Good   9f2c4a7e10 Direct  24±3ms       Δ0
  ● Fair   3d8b1f60ca Relayed 118±9ms      Δ1 via euw-1.relay.n0.iroh.link
```

---

## Table of Contents

- [The Promise](#the-promise--الوعد)
- [Features](#features)
- [Installation](#installation)
- [Quick Start](#quick-start--البداية-السريعة)
- [Usage Examples](#usage-examples)
- [Command Reference](#command-reference)
- [How It Works](#how-it-works)
- [Connection Health](#connection-health)
- [Internet Acceptance Checklist](#internet-acceptance-checklist)
- [Advanced Usage](#advanced-usage)
- [Contributing](#contributing)
- [License](#license)
- [Support](#support)

---

## The Promise · الوعد

> **No server can read your files.** Connections are direct whenever possible;
> the encrypted relay fallback is self-hostable with `--relay` or disabled
> entirely with `--no-relay`.

- **Zero-knowledge transport:** file content is chunked, BLAKE3-addressed, and
  streamed peer-to-peer over iroh's authenticated, encrypted QUIC. Relays only
  ever forward opaque, end-to-end-encrypted bytes.
- **Secret-gated membership:** knowing the gossip topic is not enough. Every
  control connection proves knowledge of the session secret in both directions
  before a single message is processed.
- **Unreadable metadata:** even presence beacons (who is online, at what
  address) are XChaCha20-Poly1305 sealed under the session key.

---

## Features

- **Internet-native from day one:** NAT-to-NAT via hole-punching, no port
  forwarding, no VPN, no config.
- **Strict exclusive checkout:** every synced file sits read-only; editing
  requires an exclusive, network-granted lease, so two people never clobber
  each other's work.
- **All file types:** text, code, images, design files, game assets, binaries
  — no exceptions, no diff/merge assumptions.
- **Delta sync:** content-defined chunking means a one-line edit in a 2 GB file
  moves only the changed chunks.

<details>
<summary><b>More features</b></summary>

- **The Golden Invariant:** never overwrite data a peer has not seen, never
  silently delete user bytes. Every ambiguous situation preserves both copies
  and warns loudly.
- **Built-in version history:** the last 5 versions of every path are kept
  locally; `restore` re-publishes any of them under a lease.
- **Connection-strength surface:** `status` shows each peer as Direct or
  Relayed with live RTT — you always know how you are connected.
- **Tamper-evident:** a forced write to a read-only file is quarantined
  (never deleted) and the indexed version is restored, with a loud warning.
- **Self-healing membership:** a full mesh of authenticated control
  connections with exponential-backoff redial.
- **Sovereign by choice:** bring your own relay with `--relay` (a one-command
  Docker relay ships in [`deploy/relay/`](deploy/relay)), find same-LAN members
  over mDNS with no external network, or run fully closed with `--airgap` — no
  relays and no external discovery of any kind. Every preference persists per
  session via `tazamun config`.

</details>

> note: `tazamun` is strict by design. With **zero** connected peers, every edit
> path (lock, restore, new file) is refused — because with no one to coordinate
> with, there is no safe way to guarantee the Golden Invariant.

---

## Installation

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

## Quick Start · البداية السريعة

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

## Usage Examples

```bash
# Initialize a brand-new session in the current folder
tazamun init

# Join someone else's session (the folder must be empty)
tazamun join tzm1qy…

# Run the sync daemon in the foreground (Ctrl-C stops it cleanly)
tazamun start

# Run relay-free on a local network with mDNS discovery
tazamun start --no-relay

# Route through your own self-hosted relay instead of the public ones
tazamun start --relay https://relay.example.com

# Fully closed network: no relays, no external discovery — LAN only
tazamun start --airgap

# Persist network preferences (applied on the next start)
tazamun config show
tazamun config set relay https://relay.example.com
tazamun config set airgap on

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

## Command Reference

```text
tazamun — strict-checkout P2P folder sync. No server ever reads your files.

Usage: tazamun [OPTIONS] <COMMAND>

Commands:
  init                 Initialize this folder as a new sync session and print an invite
  join   <TICKET>      Join an existing session from an invite ticket
  start                Run the sync daemon in the foreground
  config <show|set>    Show or change persisted per-session preferences
  status               Show members, connections (Direct/Relayed/via LAN + RTT), leases, pulls
  invite               Print a fresh invite ticket carrying live addresses
  doctor               One-shot NAT & environment health report
  locks                List active leases: holder, age, expiry countdown
  lock   <PATH> [--wait]   Acquire an exclusive lease and make the file writable
  unlock <PATH>        Publish pending edits and release the lease
  versions <PATH>      List kept historical versions of a path
  restore  <PATH> <N>  Restore version N of a path (requires a held lease)
  gc                   Delete unreferenced blobs from the local store
  dashboard [--open]   Open the local web control panel (loopback only)
  completions <SHELL>  Print a shell completion script (bash/zsh/fish/powershell/elvish)
  man                  Print the roff man page to stdout

Options:
      --dir <PATH>     Session folder (defaults to the current directory)
  -v, --verbose        Verbose logging (-v: debug; RUST_LOG is respected)
      --relay <URL>    Use a self-hosted relay instead of the public ones (this run)
      --no-relay       Disable relays entirely — LAN / manually routed setups
      --no-lan         Disable LAN mDNS discovery (enabled by default)
      --airgap         Closed-network mode: no relays, no external discovery at all
  -h, --help           Print help
  -V, --version        Print version

Exit codes: 0 success · 1 runtime error · 2 usage error
```

The four network options are **global** (they work on any subcommand). On
`start` they override the persisted preferences for that run only; the durable
setting lives in `state.json` and is managed with `tazamun config`. Precedence
is always **flag → persisted config → default**.

---

## How It Works

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

## Connection Health

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
| **Good** | green | Direct path, RTT < 80 ms, jitter < 20 ms |
| **Fair** | yellow | stable Relayed, or Direct with elevated RTT/jitter |
| **Poor** | red | flapping paths (> 3/min), RTT ≥ 300 ms, or a presence gap on a live connection |
| **Offline** | gray | no connection and nothing heard within 30 s |

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
could not lock report.md: peer 3d8b1f60ca disconnected while voting on the lease
  blocked precondition : REACHABILITY
  what to do           : the peer whose grant was required went offline — retry once it reconnects
  peers consulted      : 3d8b1f60ca (Offline, None)
  (use -v for the full per-peer table)
```

Add `-v` for the full per-peer table (grade, conn, rtt, answered). If a
consulted peer is on a degraded link, the lock still proceeds (strict
preconditions are the only gate) but prints an advisory first:

```console
acquiring via a degraded link to 3d8b1f60ca (Relayed, 412ms) — sync may lag behind edits
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

## Web dashboard

Prefer a browser to the terminal? The daemon serves a local control panel — a
visual view of the same session, read-write. It is meant for designers and game
teams who would rather click than type.

```bash
tazamun start                 # in one terminal (the daemon serves the panel)
tazamun dashboard --open      # in another: prints the URL and opens your browser
```

It is a single hand-written HTML/CSS/JS page **embedded in the binary** — no
npm, no build step, no CDN, no external font, no telemetry. Panels:

- **Members** — live health dot (Good/Fair/Poor/Offline), Direct/Relayed + *via
  LAN*, RTT ± jitter, relay hostname.
- **Files & locks** — every file with its lease state and a one-click
  lock/unlock; a refused lock shows the **diagnosis inline** (which
  precondition, which peer, its grade) with a soft banner when you would acquire
  through a Poor-grade link.
- **Conflicts** — the `.tazamun/conflicts` quarantine copies, so preserved bytes
  are visible, not buried on disk.
- **Version history** — per file, the kept versions with timestamps and a
  restore-to-#N button (the API enforces the hold-the-lease rule).
- **Invite** — the `tzm1…` ticket, a rendered QR, and a copy button.

Dark mode by default; respects `prefers-color-scheme`.

### Security model

This is a local **write** surface, and it is built like one:

- **Loopback only.** The listener binds `127.0.0.1` — never `0.0.0.0`. It is
  unreachable from the network. The bind address is not configurable.
- **Session token.** The daemon mints a random 32-byte token at start. `tazamun
  dashboard` hands it to the browser in the URL **fragment**
  (`http://127.0.0.1:8787/#<token>`), which browsers never send back to the
  server. The page presents it as `X-Tazamun-Token` on every change; tokens are
  compared in constant time. Reads are tokenless; **every mutation needs it**.
- **Anti-DNS-rebinding.** Every request's `Host` must be a loopback name, so a
  malicious web page that rebinds a hostname to `127.0.0.1` is refused — reads
  included.
- **Strict CSP.** `default-src 'none'`; the one inline script/style run under a
  per-response nonce; `connect-src 'self'`; no external origins, no `eval`. Plus
  `X-Frame-Options: DENY`, `nosniff`, `Referrer-Policy: no-referrer`.
- **No second control path.** Every endpoint forwards to the *same* daemon actor
  message the CLI uses over IPC — same preconditions, same errors, no duplicated
  logic.

The port is configurable: `tazamun config set dashboard-port 9000` (or
`tazamun dashboard --port 9000` for the URL). Do not share the URL — the token
in it authorizes changes on your machine.

### API contract (`api:1`)

The panel is backed by a small, versioned JSON API. Every response carries
`"api": 1` and the `{ok, data?, error?}` envelope. It is a stable contract for
any future client.

| Method | Endpoint | Token | Purpose |
| --- | --- | --- | --- |
| GET | `/api/state` | no | One snapshot: members+health (status schema-1), files & leases, pending pulls, conflicts, per-path version entries, session id, mode, config summary. Poll ~1 s. |
| GET | `/api/invite` | no | The current `tzm1…` invite ticket. |
| GET | `/api/invite/qr` | no | The ticket as an SVG QR (`image/svg+xml`). |
| POST | `/api/lock` | **yes** | `{path}` → acquire a lease (same preconditions as `tazamun lock`; failure returns the diagnosis). |
| POST | `/api/unlock` | **yes** | `{path}` → publish edits and release. |
| POST | `/api/restore` | **yes** | `{path, n}` → restore version *n* (requires a held lease). |
| POST | `/api/config` | **yes** | `{key, value}` for the live subset: `autolock`, `lease-ttl`, `dashboard-port`. |

Mutations without a valid token return `401`; a non-loopback `Host` returns
`403`; a daemon-level refusal returns `409` with the structured error.

### Shell completions & man page

```bash
tazamun completions bash  > /etc/bash_completion.d/tazamun         # bash
tazamun completions zsh   > "${fpath[1]}/_tazamun"                 # zsh
tazamun completions fish  > ~/.config/fish/completions/tazamun.fish
tazamun completions powershell >> $PROFILE                         # PowerShell
tazamun man > /usr/share/man/man1/tazamun.1                        # man page
```

`tazamun --version` prints the version plus a short build id (the git commit)
when built from a checkout.

---

## Internet Acceptance Checklist

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

## Advanced Usage

<details>
<summary><b>Self-hosted relay</b></summary>

A relay never sees your file contents (everything on it is end-to-end
encrypted) — it only forwards packets when two peers cannot hole-punch a direct
path. Running your own keeps even that fallback traffic on infrastructure you
control.

**Stand one up.** [`deploy/relay/`](deploy/relay) is a complete, self-contained
kit: point a DNS record at your host, set one variable, and bring up an HTTPS
relay with automatic Let's Encrypt TLS:

```bash
cd deploy/relay
cp .env.example .env          # set TZM_RELAY_DOMAIN=relay.mycompany.com
docker compose up -d          # ACME provisions the cert on first request
```

Open TCP **80** and **443** (ACME + relay) and UDP **7842** (QUIC address
discovery). See [`deploy/relay/README.md`](deploy/relay/README.md) for the DNS
and firewall checklist, resource footprint, and upgrade steps.

**Point clients at it.** Every member of the session should use the same relay:

```bash
tazamun config set relay https://relay.mycompany.com   # persist it
tazamun start                                          # …or --relay for one run
```

Direct hole-punched connections still bypass the relay whenever possible;
`tazamun status` tags any peer still on the fallback path as **Relayed**, and
`tazamun doctor` probes the configured relay for reachability.

</details>

<details>
<summary><b>Same-LAN discovery (mDNS)</b></summary>

LAN discovery is **on by default**. Members on the same local network find each
other over mDNS with no relay and no external lookup — a ticket carrying only
the session secret is enough, no addresses required. The proof-of-secret
handshake still applies, so only genuine session members are ever dialed.
`tazamun status` tags a locally-reached peer **via LAN**.

To run entirely relay-free on a trusted network (still using mDNS to meet):

```bash
tazamun start --no-relay
```

Turn LAN discovery off with `--no-lan` (or `tazamun config set lan off`).

</details>

<details>
<summary><b>Closed networks (airgap)</b></summary>

`--airgap` is the sovereign extreme: **relays disabled, every form of external
address discovery disabled, LAN discovery the only way peers meet.** The
endpoint contacts nothing off the local network — suitable for airgapped labs,
regulated environments, and offline events.

```bash
tazamun start --airgap                 # one run
tazamun config set airgap on           # or persist it
```

`tazamun doctor` reports `mode: airgap` with an empty relay status so you can
confirm the closed-network guarantee at a glance. Airgap forces relays off and
LAN on regardless of the other flags, so a single option is all you need.

</details>

<details>
<summary><b>Configuration reference (<code>tazamun config</code>)</b></summary>

Every session stores its preferences in `state.json`. `tazamun config show`
prints the effective (clamped) values; `tazamun config set <key> <value>`
changes one and applies it on the next `start`.

| Key | Values | Default | Meaning |
| --- | --- | --- | --- |
| `relay` | `default` · `none` · an `https://…` URL | `default` | Relay policy (public relays, none, or your own). |
| `lan` | `on` · `off` | `on` | LAN mDNS discovery. |
| `airgap` | `on` · `off` | `off` | Closed network: no relays, no external discovery. |
| `lease-ttl` | duration `10s`–`24h` | `90s` | How long a lease you take lasts before it must renew. |
| `acquire-timeout` | duration `2s`–`60s` | `8s` | How long a lock request waits for every peer to answer. |
| `autolock` | `on` · `off` | `off` | Auto-lock-on-first-write (see below). |
| `wait-timeout` | duration | `10m` | How long `lock --wait` keeps waiting. |

Durations use humantime forms (`90s`, `15m`, `2h`). Out-of-range values are
clamped with a note. **`lease-ttl` is lease-scoped**: the value *you* set rides
the wire and governs the leases *you* take, so peers can run different TTLs
without disagreeing — a receiver honors the holder's TTL (clamped to
`[10s, 24h]` defensively). The renew interval is always `ttl/3`, derived.

```bash
tazamun config show
tazamun config set lease-ttl 15m
tazamun config set autolock on
```

</details>

<details>
<summary><b>Autolock (auto-lock-on-first-write)</b></summary>

Off by default. With `tazamun config set autolock on`, editing an un-leased
file **tries to take the lease for you** instead of rejecting the edit:

1. Your bytes are copied to `conflicts/` first — always, before anything else.
2. The normal three-precondition acquire runs (reachability, freshness, no
   active lease).
3. **On success** the edit publishes and the lease is held with a 60-second
   idle timer (each further edit resets it, then it auto-releases).
4. **On failure** (a peer holds it, you're offline, or you're not fresh) the
   file reverts to the synced version read-only and you get an
   `autolock could not acquire: <precondition>` note — your bytes are safe in
   `conflicts/`.

**Honest tradeoffs.** Autolock trades explicitness for convenience: two people
editing the same file at the same moment still resolve to exactly one winner —
the other's edit becomes a **quarantine**, never a silent overwrite. It does
**not** relax strict mode: with zero connected peers every edit is still
refused (there is no one to coordinate with). Leave it off if you prefer to
lock deliberately; turn it on for solo-ish workflows where locking is friction.

</details>

<details>
<summary><b>Waiting for a busy file (<code>lock --wait</code>)</b></summary>

If a path is already held, `tazamun lock <path> --wait` registers your interest
and auto-acquires the moment it frees (or when the holder's lease expires),
then rings the terminal bell:

```bash
tazamun lock report.md --wait
# … report.md is held by 7cff24643f; waiting (auto-acquires when free, Ctrl-C to stop)
```

The holder — and anyone running `tazamun status` / `tazamun locks` — sees you
listed as a waiter. **First-come is not guaranteed:** if several nodes wait,
the winner is decided by the same deterministic `(lamport, id)` rule as any
lock race, not by who asked first. Waiting gives up after `wait-timeout`
(default 10m).

</details>

<details>
<summary><b>Running as a service</b></summary>

One command pins a per-folder background daemon to the OS-native autostart
facility (one folder = one instance; repeat installs are idempotent):

```bash
tazamun service install      # start at login and keep running
tazamun service status       # platform state + IPC liveness + last log lines
tazamun service uninstall    # stop and remove
```

- **Linux:** a systemd **user** unit (`~/.config/systemd/user/tazamun-<id>.service`,
  `Restart=on-failure`), enabled and started immediately. For the service to
  keep running while you are logged out, enable lingering once:
  `loginctl enable-linger $USER`.
- **macOS:** a LaunchAgent (`~/Library/LaunchAgents/io.tazamun.<id>.plist`,
  RunAtLoad + KeepAlive-on-failure), bootstrapped into your GUI session;
  launchd's own stdout/err land in `.tazamun/logs/`.
- **Windows:** a logon Scheduled Task (limited privileges, hidden window).
  Start it immediately with `schtasks /Run /TN tazamun-<id>`; the hidden
  PowerShell host exists only to suppress the console flash at logon.

A service daemon (any daemon without a terminal) also writes
`.tazamun/logs/daemon.log`, size-rotated at 5 MiB keeping 3 generations —
that's what `service status` tails. Starting `tazamun start` manually while
the service runs (or vice versa) refuses cleanly with
"a daemon is already running for this folder".

</details>

<details>
<summary><b>Windows notes</b></summary>

- **Long paths:** tazamun works past the legacy 260-character `MAX_PATH` out
  of the box — every filesystem call uses `\\?\` extended-length paths, and
  the binary carries a `longPathAware` manifest. `tazamun doctor` reports the
  system-wide `LongPathsEnabled` registry switch: enabling it (the exact
  PowerShell command is in the doctor output) lets *other* programs — editors,
  Explorer — handle your deep session paths too.
- **Locked files:** antivirus scanners and editors briefly hold files open;
  Windows refuses renames/deletes during that window. tazamun retries such
  operations automatically (exponential backoff, ≤ 3.5 s) before reporting an
  error, so transient handle contention self-heals.
- **Non-portable names:** files created on Linux/macOS with names Windows
  cannot represent — `< > : " | ? *`, control characters, reserved device
  names (`CON`, `AUX`, `COM1`…), trailing dots/spaces, or names differing only
  by case from an existing file — are **held back** on Windows nodes:
  acknowledged, listed under "unapplied" in `tazamun status` (and counted by
  `doctor`), but never materialized and never renamed silently. Rename them on
  the originating node to sync them.
- **Logs:** service-mode daemons write `.tazamun\logs\daemon.log` (5 MiB × 3
  rotation).

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

## Contributing

Contributions are welcome.

- **Bug reports:** open an issue with your OS, `tazamun --version`, and the
  `-v` log around the problem.
- **Features:** check [`ROADMAP.md`](ROADMAP.md) first — many ideas are already
  planned and scoped.
- **Docs:** clarity fixes and translations (Arabic especially) are appreciated.
- **Pull requests:** the CI gate is `cargo fmt --check`, `cargo clippy
  --all-targets -- -D warnings`, and `cargo test`. Please keep it green.

```bash
# Dev setup
git clone https://github.com/cc1a2b/tazamun && cd tazamun
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

---

## License

Released under the [MIT License](LICENSE).

```
Copyright (c) 2025-2026 Hussain Alsharman
```

---

## Support

**Star** the repo, **follow** [@cc1a2b](https://github.com/cc1a2b), and
**share** tazamun with anyone who needs private, serverless folder sync.

---

<div align="center">

**مزامنة بين الأنداد — بدون خادم يقرأ ملفّاتك.**

*Peer-to-peer sync. No server reads your files.*

Built with by [cc1a2b](https://github.com/cc1a2b)

</div>

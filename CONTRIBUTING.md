# Contributing to tazamun

The authoritative engineering guide for this crate. Read it before touching
sync, locking, or the daemon, and before opening a pull request.

## Golden Invariant (never violate)

> Never overwrite data a peer has not seen. Never silently delete user bytes.
> Every ambiguous situation resolves to: **preserve both copies, warn loudly.**

Concretely: pulls assemble into a staging file and atomic-rename (a failed pull
touches nothing); the version being replaced is pushed to history first; forced
writes and offline edits are quarantined (copied, never deleted) before the
indexed version is restored; concurrent version vectors quarantine the local
copy rather than merging.

## The three lease preconditions (strict exclusive checkout)

Every synced file is read-only. A lease is granted only when **all three** hold:

1. **REACHABILITY** — ≥1 authenticated peer connected; a grant is required from
   *every* peer holding an authenticated control connection at request time.
2. **FRESHNESS** — the requester's version vector for the path is `Equal` or
   `After` versus every connected peer's advertised record, and the path is not
   in the pending-pulls set.
3. **LEASE** — no active, unexpired lease on the path.

**Offline policy:** zero authenticated peers → every edit path (lock, restore,
new file) is refused with a strict-mode error. After reconnect, index exchange
and pending pulls complete before any lease can be granted (freshness enforces
this). Competing requests resolve on the total order `(lamport, endpoint-id)` —
every node computes the same winner.

**Lease-TTL consistency (nodes may run different configs):** the lease TTL is
**lease-scoped, never global**. The holder's configured TTL travels on the wire
(`ttl_ms` in `LockReq`/`LockRenew`, `expires_in_ms` in `Index` leases) and
governs each lease; a receiver honors the wire value, clamped defensively to the
absolute `[MIN_LEASE_TTL, MAX_LEASE_TTL]` band in `locks::ttl_from_ms`. Because
the clamp is absolute — not relative to the receiver's own config — two nodes
with different `lease-ttl` settings never disagree about a lease's lifetime, and
a hostile `ttl_ms = 0` or an enormous value is bounded identically everywhere.
Do **not** reintroduce any TTL rule that depends on the *receiver's* config.

## Module map

| File | Invariant it owns |
| --- | --- |
| `src/lib.rs` | `consts` — every tuning constant lives here |
| `src/state.rs` | atomic `state.json` persistence; `RelPath` newtype; 0600/0700 modes; `SessionConfig` (relay/lan/airgap) with defaulting in-place upgrade |
| `src/session.rs` | HKDF key derivation, `tzm1…` ticket encode/decode; zeroize-on-drop |
| `src/proto.rs` | control framing (`u32` len + postcard, reject 0 / > 4 MiB) + `Msg` |
| `src/sync/vclock.rs` | pure version-vector algebra (no I/O) |
| `src/sync/index.rs` | `sanitize_rel_path` (the only untrusted-path gate) + Windows `portability_violation` corpus + `diff` (no I/O) |
| `src/sync/chunker.rs` | FastCDC — deterministic cut function; parallel hash pipeline (`chunk_file`) |
| `src/sync/transfer.rs` | iroh-blobs store; publish / pull-stage / materialize / GC-protect |
| `src/locks.rs` | pure lease state machine (injected clock, zero I/O); lease-scoped TTL clamp (`ttl_from_ms`), `since`/age tracking |
| `src/guard.rs` | read-only enforcement + quarantine (never deletes) |
| `src/versions.rs` | history push/list/entry over `AppState` |
| `src/watcher.rs` | debounced FS events, ignores `.tazamun`, mute set for own writes |
| `src/net/endpoint.rs` | iroh Endpoint build (N0 default; Minimal for airgap/relay-test) + `RelayChoice` + pure `relay_mode_for` + LAN mDNS + `is_lan_addr` + `path_info` |
| `src/net/control.rs` | mutual proof-of-secret handshake + `PeerHandle` reader/writer |
| `src/net/membership.rs` | encrypted presence gossip + mesh dialer |
| `src/net/telemetry.rs` | pure per-peer health sampling + grade function (zero I/O) |
| `src/doctor.rs` | `doctor` sections + verdicts; injectable mount classifier (zero state I/O) |
| `src/ui/progress.rs` | terminal-only presentation: bars, spinners, tracing bridge (no protocol/state) |
| `src/ipc.rs` | local socket / named pipe, one JSON line per request |
| `src/daemon.rs` | the single state-owning actor; **all** mutation happens here; autolock flow + waitlist (`my_waits`/`interest`) live here |
| `src/win_fs.rs` | `\\?\` extended-length conversion (long paths) + bounded retry for contended Windows file ops + RO-clear ordering rule |
| `src/service.rs` | per-folder background service (systemd user unit / LaunchAgent / Scheduled Task) + size-rotated `daemon.log` writer |
| `src/cli.rs` / `src/main.rs` | clap surface + thin binary; global net flags, `config`/`service` commands, flag→config→default precedence, non-TTY log tee |
| `build.rs` | Windows-target `longPathAware` manifest embed (no effect elsewhere) |
| `deploy/relay/` | self-contained self-hosted iroh-relay kit (Docker Compose + ACME TLS); not part of the crate build |

**Architecture rule:** `AppState`, `LockTable`, and the member table are only
ever mutated inside the daemon actor task (message passing, no shared-state
locking). Heavy I/O runs in spawned tasks that report completion events back.
The lock state machine and the path sanitizer contain **zero I/O** — keep them
that way so they stay exhaustively unit-testable.

## Gate commands (must pass before every commit and merge)

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

`cargo build --release` must produce one self-contained binary per OS with no
extra steps. No `.unwrap()` / `.expect()` outside tests unless provably
infallible with a justifying comment. `#![forbid(unsafe_code)]` at the crate
root — do not remove it.

## CI policy (cost-aware, self-hosted-first)

CI runs on **self-hosted runners** on the maintainer's machine to keep GitHub
Actions minutes near zero. Two runners are registered on the repo:

- `wsl2-linux` — labels `self-hosted, linux, x64` (a WSL2 service).
- `host-windows` — labels `self-hosted, windows, x64` (a Windows service).

`.github/workflows/ci.yml` maps events to cost tiers:

| Event | Job | Runner | Scope |
| --- | --- | --- | --- |
| `push` (any branch) | `light` | self-hosted linux | fmt + clippy + `cargo test --lib` (unit only) |
| `pull_request` | `full` (matrix) | self-hosted linux **and** windows | fmt + clippy + `cargo test --all-targets` |
| `workflow_dispatch` | `macos-full` | **hosted** `macos-14` (billed) | full suite |

- **macOS is not on the PR path.** Trigger `macos-full` manually (Actions → CI →
  *Run workflow*) before merging a phase that changes **platform-sensitive
  code** — the file watcher, guard/quarantine, path handling, or IPC. **P4 does
  not require a macOS run** (it touches none of those); **P5 will.**
- **`concurrency`** is grouped per ref with `cancel-in-progress: true`, so a new
  push cancels the superseded run — the main silent minute-burner.
- **No `actions/cache`** on the self-hosted jobs: the cargo cache lives on the
  runner's local disk, which is faster and avoids restore/save minutes and
  cache-eviction flakes. Only the hosted `macos-full` job caches.
- The self-hosted runners already have the **Rust toolchain** (`rustup` with
  `clippy` + `rustfmt`) installed on the machine, so the workflow installs no
  toolchain for them.

### Self-hosted runner security

Self-hosted runners **execute repository workflow code on the maintainer's
machine**. Because this repo is **private and single-author**, the blast radius
is limited, but it is hardened regardless:

- **Require approval for all outside collaborators** (Settings → Actions →
  General) so a workflow never runs from an untrusted change.
- The default `GITHUB_TOKEN` is **read-only** (`default_workflow_permissions:
  read`); `release.yml` self-elevates to `contents: write` only when a tag is
  pushed.
- Each runner uses a **dedicated `_work` folder** and runs as a service under
  the maintainer's account — not root/Administrator beyond what the service
  install needs.
- Workflows **never `echo` secrets**; there are no secrets in `ci.yml`.

## Development environment

Keep the source and any session/smoke folders on the **native Linux
filesystem** (e.g. `~/projects/tazamun`), never on a `/mnt/*` Windows mount
under WSL. DrvFS/9p does not deliver inotify events reliably, so the file
watcher misses changes there, and cargo is much slower.

## Branch & release discipline

- **One branch per roadmap phase:** `phase/pN-<slug>` (e.g. `phase/p1-throughput`).
- **Conventional commits** (`feat:`, `fix:`, `docs:`, `refactor:`, `test:`,
  `chore:`), imperative mood, scoped where useful.
- **Merge to `main` only when** the three gates are green **and** the clean-repo
  check passes (see below). Flip the matching `ROADMAP.md` checkbox in the merge
  commit.
- **No tags at phase boundaries.** `Cargo.toml` stays at `0.1.0` throughout
  development — the version is not a release marker. The single `v0.1.0` tag is
  created only after the final roadmap phase passes acceptance; that tag is what
  triggers `release.yml`.

## Authorship policy

Every commit is authored solely by the project owner. Configure the local repo
identity once and never add co-author or attribution trailers of any kind:

```bash
git config user.name  "cc1a2b"
git config user.email "101569980+cc1a2b@users.noreply.github.com"
```

Verify after committing with `git log --format='%an %ae %cn %ce'` — author and
committer must both be the owner, with no additional trailers.

## Repository hygiene

Tracked files, file names, commit messages, branch names, and the repository
description stay free of references to local development tooling or assistants.
Local-only working notes live in git-ignored files (`*.local.md`) and
directories and never reach the remote. Before every push or merge to `main`,
verify the working tree and full history are clean of such references (a
case-insensitive grep over tracked files and `git log --all` must come back
empty).

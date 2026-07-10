# DECISIONS

Version pins and design choices, each with one line of rationale. Update this
file whenever a dependency is added or a load-bearing design decision is made.

## Toolchain

- **Rust edition 2024, MSRV 1.91** — required by the iroh 1.x line and modern
  async ergonomics; builds on current stable (verified on 1.92).
- **`#![forbid(unsafe_code)]`** — this is a data-integrity tool; no module needs
  unsafe, so the compiler enforces its absence.

## Networking (the load-bearing pins)

- **`iroh = "1"` (resolves 1.0.2)** — 1.0 is the first API- and wire-stable iroh
  release; the endpoint `presets::N0` gives NAT traversal + relays from a ticket
  alone. Pinned to the 1.x major so patch/minor updates flow in.
- **`iroh-blobs = "0.103.0"`** — the iroh-1.x-compatible content-addressed blob
  store; `fs-store` (default) gives the persistent `.tazamun/blobs` store and
  the `BlobsProtocol` data-plane handler. GC is driven through the store's
  built-in `GcConfig` protect-callback rather than an ad-hoc sweep.
- **`iroh-gossip = "0.101.0"`** — the iroh-1.x-compatible gossip overlay used for
  encrypted presence beacons and peer discovery on the session topic.
- **`iroh-mdns-address-lookup = "0.4.0"`** — optional local mDNS discovery for
  `--lan`; kept out of the default path so nothing is broadcast unless asked.
- **`n0-future = "0.3.2"`** — the `Stream` extension trait iroh-gossip's receiver
  is consumed through; already in the iroh dependency tree, no new transitive
  surface.

## Crypto & encoding

- **`chacha20poly1305 = "0.11"`** — XChaCha20-Poly1305 for gossip payloads; the
  24-byte nonce lets us prepend a random nonce per message without a counter.
- **`hkdf = "0.13"` + `sha2 = "0.11"`** — HKDF-SHA256 derives topic/auth/gossip
  keys from the one session secret, so a single 32-byte secret is all a ticket
  must carry.
- **`hmac = "0.13"`** — HMAC-SHA256 for the mutual proof-of-secret handshake.
- **`subtle = "2.6"`** — constant-time proof comparison; a timing side-channel on
  the handshake would be an auth oracle.
- **`blake3 = "1.8"`** — chunk and manifest content addressing; fast, and the
  same hash iroh-blobs verifies against, so publish and store agree by
  construction.
- **`data-encoding = "2.11"`** — BASE32_NOPAD lowercase for tickets (URL/paste
  safe) and HEXLOWER for on-disk secret material.
- **`postcard = "1.1"` (`use-std`)** — compact deterministic wire format for
  frames, tickets, and manifest blobs; no schema drift with serde.
- **`zeroize = "1.9"`** — session secret and derived keys wipe on drop.

## Sync engine

- **`fastcdc = "4.0"`** — content-defined chunking (v2020) gives the delta-sync
  property: a localized edit re-transmits only the changed chunks. Cut function
  is deterministic, so both peers agree on boundaries.
- **Inline vs. blob manifests at 256 chunks** — small files carry their chunk
  list inline in messages; larger ones spill the list into a BLAKE3-referenced
  blob, bounding control-frame size.

## Runtime & process plumbing

- **`tokio = "1.52"`** (multi-thread, macros, sync, time, fs, io-util, signal) —
  the async runtime; `signal` powers the graceful ctrl-c shutdown.
- **`notify = "8.2"` + `notify-debouncer-full = "0.7"`** — recommended watcher
  with debouncing; 0.7 is the released line matching notify 8. (0.8 is still a
  release-candidate and intentionally avoided for a stable build.)
- **`interprocess = "2.4"` (`tokio`)** — one abstraction over Unix domain
  sockets and Windows named pipes for the CLI↔daemon IPC.
- **`clap = "4"` (derive)** — the CLI surface.
- **`serde`, `serde_json`** — state file is pretty JSON (human-inspectable);
  IPC is one JSON object per line.
- **`thiserror = "2"` per-module errors, `anyhow = "1"` only at the binary edge**
  — typed errors internally, ergonomic bubbling in `main`.
- **`tracing` + `tracing-subscriber` (env-filter)** — structured logs with
  `#[instrument]` on protocol handlers; `RUST_LOG` respected.
- **`tempfile = "3.27"`** — atomic-write staging for `state.json` and assembled
  pulls; also the integration-test scratch dirs.

## Design choices

- **Single state-owning actor** — all `AppState` / `LockTable` / member-table
  mutation happens in one task via message passing; no shared-state locks, so
  the concurrency model is auditable in one file (`daemon.rs`).
- **Strict mode with zero peers refuses edits** — with no one to coordinate
  with, there is no way to guarantee the Golden Invariant, so we fail closed
  rather than risk a silent overwrite on reconnect.
- **Quarantine over merge** — tazamun never merges file content. Concurrent or
  forced changes preserve both copies under `.tazamun/conflicts/` and restore
  the causal version; the user resolves intent, not the tool.
- **GC as a protect-set refresh** — instead of an on-demand destructive sweep,
  the daemon keeps the store's protected-hash snapshot in lockstep with
  committed state after every commit; the store sweeps unreferenced blobs on its
  own interval. In-flight operations hold temp tags, so a sweep can never take
  bytes being staged.
- **Ticket carries only a secret + bootstrap addrs** — identity, topic, and keys
  all derive from the secret, so any member can mint a valid invite and the
  ticket stays short.

## Phase 4 — lease ergonomics + CI cost overhaul

### CI cost overhaul (self-hosted runners)

- **Why:** the account sat at ~90% of the 2,000 free Actions minutes, and the
  old 3-OS-every-push matrix was the cause (the P3 PR alone burned macOS 9m57s +
  Ubuntu 20m47s + Windows **46m21s** — one PR ≈ 77 minutes). Windows hosted is
  the dominant cost.
- **New model** (`.github/workflows/ci.yml`): `push` → a light self-hosted-Linux
  job (fmt + clippy + `cargo test --lib`); `pull_request` → the full suite on
  self-hosted Linux **and** Windows; macOS demoted to a manual
  `workflow_dispatch` job on hosted `macos-14`, run only before merging a phase
  that touches watcher/guard/paths/IPC. Per-ref `concurrency` with
  `cancel-in-progress` kills superseded runs (the silent burner). No
  `actions/cache` on self-hosted — the cargo cache is local disk.
- **Projected hosted burn for the rest of v0.1: ≈ 0 minutes**, except explicit
  `macos-full` dispatches (P4 needs none; P5 will).
- **Security:** self-hosted runners execute repo code on the maintainer's
  machine; acceptable because the repo is private and single-author. Hardened
  anyway: default `GITHUB_TOKEN` already read-only (`release.yml` self-elevates
  on tags only); require-approval-for-outside-collaborators enabled; dedicated
  `_work` folders; no secrets in `ci.yml`.
- **Judgment call (runner registration timing):** runner registration is an
  interactive step on the maintainer's machine (a per-runner token from the repo
  UI) that cannot be automated from here. The self-hosted `ci.yml` and its policy
  docs were committed to `main` ahead of the runners coming online; until both
  runners show `Idle`, self-hosted jobs queue (they burn no minutes and do not
  fail). The step-0 verification (one light push + one throwaway PR, wall-times
  recorded here) and every phase's PR-green merge gate are therefore satisfied
  once the runners are up — an inherent dependency of the self-hosted design, not
  a regression. Feature work proceeds in parallel, gated locally by the three
  gates.

<!-- P4.0d verification (filled once runners are Idle):
     light push run: <url> <wall-time>
     full PR run (linux): <url> <wall-time>   (windows): <url> <wall-time> -->

### Configurable lease timings

### Autolock (auto-lock-on-first-write)

### Lock waitlist

## Phase 3 — sovereignty (self-hosted relay, LAN, airgap)

### Test strategy for the three sovereignty modes

- **LAN rendezvous is proven automatically** (`tests/sovereignty.rs`): two
  daemons with LAN discovery on, relays off, and a **secret-only invite ticket
  (zero bootstrap addresses)** find each other purely over mDNS and complete a
  lease/edit/sync. It auto-skips (with a logged reason, never a flake) if the
  runner lacks multicast.
- **Airgap is proven automatically**: a pure `relay_mode_for(cfg)` helper lets
  the test assert `airgap → relay_map().is_empty()` (zero external relay URLs)
  vs. the default config's non-empty map, and a live airgap endpoint binds with
  no home relay; the daemon's `doctor` snapshot reports `mode=airgap` with an
  empty relay-status list. The SMOKE run adds an `ss` egress sweep for
  belt-and-braces.
- **The relay path is proven in SMOKE, not in-process — deliberately.** Two
  facts make an automated forced-relay-path test impractical on a single host:
  (1) loopback is always directly reachable, so any IP transport that reaches
  the relay *also* enables direct hole-punching, and clearing the IP transport
  (`clear_ip_transports`) severs the relay connection too; (2) `iroh
  test_utils::run_relay_server()` serves a **self-signed** TLS cert that
  production endpoints correctly reject — trusting it needs a test-utils-gated
  insecure-verify flag we will not add to shipping code. So the automated tests
  prove the *telemetry pipeline* (a relayed `PathSample` yields conn=Relayed +
  the relay hostname + a non-Offline grade — the exact `status --json` fields),
  and the forced relay path (`status` shows `Relayed` + hostname against a real
  localhost relay) is a SMOKE section. `iroh` with the `test-utils` feature is a
  **dev-dependency only**; the edition-2024 resolver keeps it out of the release
  binary.

### iroh-relay 1.0.2 — server facts (from crate sources)

- **Binary:** the crate ships a `iroh-relay` binary (behind the `server`
  feature) driven by a **TOML config file** (`--config-path`). Key fields:
  `enable_relay` (bool), `http_bind_addr`, `enable_quic_addr_discovery` (the
  QUIC address-discovery / STUN-equivalent service), `enable_metrics`,
  `metrics_bind_addr`, and a `[tls]` section.
- **TLS:** `[tls].cert_mode` is one of `Manual`, `LetsEncrypt`, or `Reloading`.
  **`LetsEncrypt` gives built-in ACME** (with `prod_tls` prod/staging toggle),
  so a self-hosted relay obtains and renews its own certificate — no reverse
  proxy required. `Manual` reads `manual_cert_path`/`manual_key_path`.
  `[tls].hostname` is the ACME domain; `https_bind_addr` and `quic_bind_addr`
  default off `http_bind_addr`.
- **Default ports:** HTTP `80`, HTTPS `443`, QUIC address-discovery `7842`,
  metrics `9090`. The relay speaks HTTPS (relay protocol + captive-portal) and,
  when address discovery is on, QUIC on 7842.
- **Client relay policy** is set with `RelayMode`: `Default` (n0 prod map),
  `Custom(RelayMap)`, or `Disabled`. `Endpoint::relay_map()` returns the live
  `RelayMap`, which exposes `is_empty()`/`len()`/`urls()`/`contains()` — the
  concrete hook for the airgap "zero external relay URLs" assertion.
- **Local discovery** is the already-present `iroh-mdns-address-lookup` crate
  (v0.4), added to the endpoint via `.address_lookup(MdnsAddressLookup::
  builder())`. It publishes/resolves endpoint addresses over mDNS on the LAN
  with no external network. So **no new client dependency** is needed for any
  of relay/LAN/airgap.
- **Airgap construction:** `presets::Minimal` (sets only the crypto provider —
  no `DnsAddressLookup`/`PkarrPublisher`) + `RelayMode::Disabled` (empty relay
  map) + only the mDNS address-lookup. This contacts nothing off the LAN; the
  test asserts `endpoint.relay_map().is_empty()` and the SMOKE run adds an `ss`
  egress sweep.

- **One authorized history rewrite (Phase 3, step 0).** Two operator web-edit
  commits carried off-policy identities — `1b9553b` as `cc1a2b
  <cc1a2bb@gmail.com>`, and a later one as `Hussain Alsharman
  <101569980+cc1a2b@users.noreply.github.com>` (name variant). With the
  operator's explicit authorization, `git-filter-repo --mailmap` folded both
  into the single canonical identity `cc1a2b
  <101569980+cc1a2b@users.noreply.github.com>`; `main` was force-pushed and the
  merged phase branches were deleted from the remote. `git log --all
  --format='%an %ae %cn %ce' | sort -u` now yields exactly one line, and the
  clean-repo gates pass over the rewritten history. **Consequence:** every
  commit SHA quoted in the Phase 0–2 closing reports is pre-rewrite and now
  historical; the equivalent post-rewrite commits carry the same messages and
  content under new SHAs.

## Phase 2 — connection health & observability

- **Test harness retries explicitly-transient lock states.** The 32 MiB delta
  test writes a large file and immediately unlocks; on a slow runner the
  watcher-driven publish is still in flight, so `unlock` correctly returns
  `busy` ("retry in a moment"). The harness's `lock_ok`/`unlock_ok` now retry
  the `busy`/`syncing` codes for up to the standard wait budget — exactly what
  a real script would do — instead of failing on the first transient. The
  daemon behaviour is unchanged; only the test's expectation of instant
  success was wrong. (A future phase may let the CLI auto-retry these for
  large-file ergonomics; out of scope here.)


- **Zero new dependencies.** Telemetry, grading, the status panel, `--watch`,
  `doctor`, and JSON output are all built on the existing `indicatif`/`console`
  stack from P1 plus `serde_json`. No crate was added.
- **No new wire messages.** Lock explainability is derived entirely from
  existing grants/denies plus local telemetry; the control protocol
  (`proto::Msg`) is unchanged, so P2 is fully wire-compatible with P1 peers.
  Had a wire change been needed it would have been an append-only postcard
  enum variant — none was.
- **Telemetry is a pure module** (`net/telemetry.rs`): samples in, grade out,
  `now` injected, no I/O — exhaustively unit-tested over synthetic sample
  matrices (all four grades, exact threshold boundaries, jitter/rate EWMAs,
  time-to-direct). The daemon actor owns every `PeerHealth` and feeds it from
  `endpoint::sample_connection` on a 2 s tick and on path events; no shared
  locks, same message-passing pattern as the rest of the actor.
- **Grade thresholds live in one place** (`consts`): Good = Direct & RTT < 80 ms
  & jitter < 20 ms; Poor = flaps > 3/min or RTT ≥ 300 ms or a presence gap on a
  live connection; Offline = no connection and silence past `ONLINE_WINDOW`;
  Fair = everything else. Chosen as human-legible round numbers for a
  first-cut; they are data, easy to retune.
- **Control connection is authoritative for liveness.** A peer missing presence
  beacons but holding a live control connection stays online; the divergence is
  logged at debug. Presence only refreshes `last_seen` for the snapshot.
- **`status --json` is a stable contract (schema = 1).** The integration suite
  asserts the required top-level and per-member keys so the schema can't drift
  silently; any addition must bump `schema` and is documented in the README.
- **Reconnect polish.** On path loss the daemon does one immediate redial
  before entering the jittered exponential backoff (fast-path for transient
  blips); peers stuck on a relay get a 60 s re-hole-punch probe
  (`add_external_addr` of the known direct addresses), and Direct↔Relayed
  transitions are logged and pushed to the status event ring.
- **`doctor` never opens its own endpoint.** It reads the running daemon's live
  view over IPC (labelled "from daemon") and adds only local, side-effect-free
  probes (mount classification, a temp-file read-only probe, IPC path). The
  mount classifier is injected so the WSL `/mnt` warning is unit-tested without
  a real `/mnt`. Exit code encodes the worst verdict (0/1/2).

## Phase 1 — performance & terminal UX

### New dependencies

- **`rayon` (1.12)** — the per-chunk BLAKE3 hash/copy stage of publishing runs
  as order-preserving parallel batches on a small dedicated pool.
- **`indicatif` (0.18)** — terminal progress bars/spinners for pulls and big
  publishes in the foreground daemon; multi-bar via `MultiProgress`.
- **`qrcode` (0.14)** — renders the invite ticket as a terminal QR code
  (unicode half-blocks); pure encoding, no I/O.
- **`console` (0.16)** — terminal size/TTY introspection for the QR fallback;
  already in the tree transitively via indicatif, so this adds no new code to
  the dependency graph.
- **`criterion` (0.8, dev-only)** — statistics-backed benches for the chunking
  path; `[[bench]] harness = false`, never part of the shipped binary.
- **`blake3` gains the `rayon` feature** — needed only to *evaluate*
  `Hasher::update_rayon` as a candidate (see below; it lost decisively).

### Parallel chunking — measurements (i9-14900HX, 16 logical CPUs, WSL2)

Bench: `benches/chunking.rs`, seeded synthetic files generated at bench start
(never committed), page-cache-warm reads, criterion medians.

Baseline (sequential `StreamCDC` cut + inline BLAKE3, pre-change):

| input | time | throughput |
|---|---|---|
| 4 MiB | 2.650 ms | 1.474 GiB/s |
| 64 MiB | 44.157 ms | 1.415 GiB/s |
| 512 MiB | 342.20 ms | 1.461 GiB/s |

Decision inputs:

- **Pure sequential scan floor** (`scan_only_slice`, in-memory FastCDC scan,
  no I/O/hash/copy): **22.24 ms / 64 MiB (2.81 GiB/s)**. The cut scan is
  mandated sequential, so by Amdahl the hard ceiling for any parallel-hash
  scheme on this machine is 44.16 / 22.24 = **1.99×** — the 2× acceptance
  target is exactly at, not above, the theoretical limit.
- **`blake3::Hasher::update_rayon` per chunk: rejected.** 390.5 ms / 64 MiB —
  **8.8× slower than baseline**; per-call rayon dispatch swamps 64–256 KiB
  chunks.
- **Hash-pool sizing measured, not assumed:** with the overlapped pipeline the
  64 MiB time was 31.7 ms with 16 hash threads, 27.9 ms with 8, and flat at
  ~26.0–26.1 ms for 1–4 — BLAKE3 (~4.7 GiB/s/thread) saturates a 2.8 GiB/s
  scan with 1–2 threads, and extra hashers only steal cycles from the scan
  thread. Default pool = `min(cores, 4)`, overridable with `TAZAMUN_THREADS`.

Final design: `chunk_bytes`/`chunk_stream` keep their exact signatures with
windowed slice-semantics scanning + order-preserving parallel hash batches; a
new `chunk_file` fast path (used by `publish_local` and `disk_matches`) adds a
reader thread with three recycled 4 MiB window buffers so the caller thread
runs only the sequential scan plus in-order emission. Cut points are
byte-identical across all three entry points (window cuts are finalized only
with ≥ `CDC_MAX` lookahead or EOF, which provably matches whole-slice
semantics; unit tests pin equality including tiny-window and trickle-read
cases).

After (default pool):

| input | time | throughput | speedup |
|---|---|---|---|
| 4 MiB | 2.607 ms | 1.498 GiB/s | 1.02× |
| 64 MiB | 26.607 ms | 2.349 GiB/s | **1.66×** |
| 512 MiB | 208.15 ms | 2.402 GiB/s | **1.64×** |

**Acceptance note (≥2× target):** not reachable on this machine — the
sequential scan alone is 50.3% of the baseline, capping any hashing
parallelism at 1.99×; the achieved 26.6 ms sits within 16% of the 22.2 ms
floor, the residual being carry copies, cross-core cache handoff of freshly
read windows, and emit bookkeeping. Going past this requires making the *scan*
faster (SIMD gear hash or segment-parallel CDC), which changes or risks the
cut-point contract and is out of scope for this phase. The 4 MiB case is flat
by design: pipeline startup roughly equals the savings at that size.

**Memory bound:** peak RSS of the full 512 MiB bench process = **44 MiB**
(budget: 256 MiB). Method: kernel high-water mark `VmHWM` from
`/proc/<pid>/status` polled to process exit — VmHWM is monotonic and
kernel-maintained, so the final reading is the true peak (GNU time is not
installed in this WSL image). The pipeline holds 3 × ~4.5 MiB recycled window
buffers plus in-flight batch copies regardless of file size.

### CI heavy-test headroom (Windows)

The 32 MiB `delta_edit_transfers_under_20_percent` test recurs as a slow-runner
flake **only** on GitHub's shared `windows-latest` instances: it passes every
run on Linux/macOS and on 4-CPU-pinned Linux in ~5 s, and passed the P2 PR
Windows job, but a pathologically slow Windows instance occasionally exceeds
the sync wait (once at 132 s total). Its two convergence budgets were raised
120 s → 180 s so the test stops being a coin-flip on the worst runners.
`wait_until` returns as soon as the file matches, so the larger budget costs
nothing on healthy runners.

### CI observation (watched, not root-caused)

One `windows-latest` run of the P1 branch failed `delta_edit_transfers_under_
20_percent` with "delta edit did not sync" after its full 120 s wait; the
identical code passed Windows on the next run, passes 4-CPU-pinned Linux in
~1.7 s across repeated runs, and every other suite on the failing runner ran
at normal speed. Verdict: slow-runner flakiness, not a product defect. Rather
than papering over it with a bigger timeout, the test now dumps both daemons'
full `status` (members, leases, pending pulls with progress, per-file version
vectors) whenever either 120 s wait expires, so any recurrence is directly
diagnosable from the CI log.

### Terminal UX decisions

- **Progress is presentation-only.** Pull bars and the publish spinner live in
  `src/ui/progress.rs`; the transfer layer only increments an optional shared
  byte meter. No protocol, state, or transfer semantics changed — headless runs
  (`Ui::disabled()`, non-TTY stdout, CI) behave byte-identically to before.
- **Bars and logs coexist through a suspending writer.** tracing output is
  routed through a `MakeWriter` that wraps each write in
  `MultiProgress::suspend`, so a log line never tears through a rendering bar.
  Side effect: daemon logs now go to stderr in all modes (previously stdout) —
  consistent streams regardless of whether bars are active.
- **Bars auto-disable off-TTY and honor `NO_COLOR`** (colorless templates when
  set). Detection via `std::io::IsTerminal` on stdout and stderr.
- **`status` transfer rows reuse the bar meters.** Active pulls report
  percentage, bytes, and average rate from the same atomics that drive the
  bars; `pending_pulls` entries became objects (`path`/`percent`/`bytes_*`/
  `rate_bytes_per_sec`).
- **QR invite encodes the exact ticket string, nothing else**, rendered as
  unicode half-blocks (inverted polarity for dark terminals — phone scanners
  read both). Falls back to the plain ticket with a note when the terminal is
  narrower than the code.
- **Unix IPC socket falls back to a short hashed path for deep folders**
  (found during live verification): `sockaddr_un` caps socket paths at ~107
  bytes, so `.tazamun/daemon.sock` cannot bind under deeply nested session
  folders. When the in-folder path exceeds a conservative 100-byte budget,
  daemon and CLI both derive `$XDG_RUNTIME_DIR/tazamun-<blake3-16hex>.sock`
  (or the temp dir) from the absolute folder path — same fallback on both
  sides, so they always meet.

## Phase 0 — bootstrap decisions

- **Source lives on the native Linux filesystem (`~/projects/tazamun`), not a
  `/mnt/*` Windows mount** — DrvFS/9p does not deliver inotify events reliably,
  so the file watcher silently misses changes there, and cargo is markedly
  slower. The WSL vdisk has ~840 GB free, so the full move was taken rather than
  the `CARGO_TARGET_DIR` fallback. A stale pre-move copy may remain under
  `/mnt/e/Programming/tazamun` (its removal was declined by the sandbox); it is
  abandoned and safe to delete manually.
- **Release profile: `lto = "thin"`, `codegen-units = 1`, `strip = true`,
  `panic = "abort"`** — thin LTO plus a single codegen unit trade a little
  compile time for a smaller, faster binary; `strip` drops symbols; `panic =
  "abort"` removes unwinding tables and shrinks the binary further. Tradeoff
  noted: with `panic = "abort"` a panicking spawned task aborts the whole
  daemon instead of unwinding just that task. This is acceptable and arguably
  aligned with the fail-loud philosophy because production code carries no bare
  `unwrap`/`expect`; every fallible path returns a typed error. The gates run in
  the dev/test profile, so unwinding-based test behaviour is unaffected.
- **Distribution stays parked until v0.1.0** — `release.yml` triggers only on
  `v*` tags, and no tag is created until every roadmap phase (P1–P7) has merged
  and passed final acceptance. `Cargo.toml` stays at `0.1.0` throughout
  development; the version is not a release marker.

## Known limitations (deferred fixes)

- **Watcher mute-window race** — after the daemon writes a path itself (pull,
  restore, violation-recovery), it suppresses watch events for that path for
  `MUTE_WINDOW` (2 s) so its own writes are not misread as user edits. A user
  force-write to that same path *within* those 2 s is therefore swallowed and
  not immediately quarantined. It is not lost: the forced bytes stay on disk and
  are caught by the startup divergence scan on the next daemon start. The clean
  fix is content-hash-scoped muting (suppress only an event whose on-disk hash
  equals the bytes the daemon just wrote) or a periodic disk-vs-index
  reconciliation sweep for un-leased paths; both are deferred to a later roadmap
  phase rather than added during bootstrap. The Phase 0 acceptance smoke test
  waits out the window so it exercises the violation path directly.

## Portability

- **Watcher relative-path mapping tries multiple roots** — the 3-OS CI matrix
  caught a macOS-only failure: temp/session folders under `/var/folders/…` are
  symlinks to `/private/var/folders/…`, and macOS FSEvents reports the canonical
  `/private/var` path, so stripping the session root failed and every watch
  event was dropped (deletions went undetected). The fix maps each event path
  against both the original root **and** its canonicalized form. It deliberately
  does *not* canonicalize the path that is watched (which on Windows would become
  a `\\?\` extended-length path and risk regressing the already-passing Windows
  job) — only the strip-prefix comparison is made permissive. Linux/Windows hit
  the original root on the first try; macOS falls through to the canonical one.

## CI stability (Phase 0)

The 3-OS CI matrix surfaced two environment issues (not product bugs), each
fixed at the root:

- **Dependencies are optimized in debug/test builds** (`[profile.dev.package."*"]
  opt-level = 2`). The 32 MiB delta-sync test chunks and BLAKE3-hashes the file
  several times; in a fully unoptimized build that is CPU-bound and timed out on
  a slow Windows runner (~72 s). Optimizing dependency code (while keeping
  tazamun's own crate unoptimized for fast compiles and good backtraces) brings
  the whole `sync_flow` suite to ~1.5 s locally, with generous headroom on any
  runner. The heavy test's wait budgets were also raised to 120 s belt-and-
  suspenders.
- **Convergence poll budgets raised for slow runners.** `wait_until` returns as
  soon as its predicate holds, so a larger timeout only adds slack when a runner
  is slow — it never slows the passing path. The shared budget went from 10 s to
  30 s, and three-node gossip mesh formation (where two joiners discover each
  other only through presence beacons) gets a dedicated 60 s budget. Multi-node
  lock tests also wait until the acquiring node has received every peer's index
  (`synced` in `status`), so lease acquisition is gated on the real FRESHNESS
  precondition rather than on a peer merely being "online".
- **macOS pinned to `macos-14` + cache prefix bumped.** A macOS run failed to
  execute the `iroh-relay` build script ("cannot execute binary file", exit
  126) — a stale build artifact restored across an architecture change in the
  floating `macos-latest` runner pool. Pinning a fixed-arch runner and bumping
  the `rust-cache` prefix key make the build cache architecture-consistent.

## Dependency audit (Phase 0)

`cargo audit` reports **zero security vulnerabilities** across the 495-crate
lockfile. Three informational *unmaintained-crate* advisories remain, all in
transitive dependencies of the iroh networking stack — not direct dependencies,
and none is an exploitable vulnerability. They are accepted (and ignored in
`.cargo/audit.toml`, so `cargo audit` stays clean) with the rationale below;
each should be re-checked whenever the iroh tree is bumped, since the fix is an
upstream dependency update, not a change we can make here:

- **RUSTSEC-2023-0089 — `atomic-polyfill` unmaintained.** Not present in the
  host build graph at all (`cargo tree -i` finds no edge for our targets); it is
  a platform-gated entry in the lockfile only. Zero runtime impact.
- **RUSTSEC-2024-0436 — `paste` unmaintained.** Pulled in via
  `iroh → netwatch → netdev → netlink-packet-core`. A proc-macro used at build
  time only; no runtime surface.
- **RUSTSEC-2024-0370 — `proc-macro-error` unmaintained.** Pulled in via
  `iroh-blobs → bao-tree → genawaiter → genawaiter-proc-macro`. Also a
  build-time proc-macro dependency.

`cargo tree --duplicates` lists 24 crates present at more than one version
(e.g. `aead` 0.5 / 0.6, `cipher` 0.4 / 0.5). This is benign version skew: the
iroh QUIC/crypto stack pins the older majors while our direct crypto
dependencies (`chacha20poly1305` 0.11) pull the newer ones. It slightly
increases binary size but raises no correctness or supply-chain concern; all are
well-known RustCrypto/iroh crates. No action taken.

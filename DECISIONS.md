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

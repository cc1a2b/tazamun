# Real-binary acceptance smoke test

This is the recorded acceptance run of the compiled **release** binary driving
two independent daemons through the full lifecycle: init → invite → join →
start → lock/edit/unlock (both directions) → un-leased-write violation →
status → clean shutdown.

- **Binary:** `target/release/tazamun` (release profile: `lto=thin`,
  `codegen-units=1`, `strip`, `panic=abort`).
- **Filesystem:** native Linux (`~/tazamun-smoke`), never a `/mnt/*` mount —
  the file watcher needs real inotify.
- **Transport:** `--no-relay`; the two nodes hole-punch a **Direct** QUIC path
  from the direct addresses embedded in the live invite ticket, so the run is
  fully local and needs no relay or internet.
- **Result:** every assertion passed.

The driver script lives at `~/tazamun-smoke/run.sh` (outside the repo). It prints
`PASS`/`FAIL` per assertion and exits non-zero on the first failure.

## Assertion transcript

```text
=== STEP 1 — folder A: init ===
PASS: A initialized

=== STEP 2 — folder A: start daemon (backgrounded, --no-relay) ===
PASS: A daemon answering IPC

=== STEP 3 — folder A: live invite ticket ===
PASS: invite produced ticket (126 chars)

=== STEP 4 — folder B: join <ticket> then start daemon ===
PASS: B joined
PASS: B connected to A
PASS: A sees B online

=== STEP 5 — A: lock notes.txt, write content, unlock ===
PASS: A acquired lease on notes.txt (path did not exist yet — valid)
PASS: A unlocked notes.txt

=== STEP 6 — assert notes.txt lands on B with exact bytes, read-only ===
PASS: notes.txt reached B
PASS: byte-exact copy on B
PASS: notes.txt is read-only on B

=== STEP 7 — B: lock/edit/unlock notes.txt (round-trip back to A) ===
PASS: B acquired lease after sync (freshness satisfied)
PASS: B unlocked notes.txt
PASS: B's edit round-tripped to A (byte-exact)
PASS: notes.txt read-only on A after remote edit

=== STEP 8 — B: force-write an un-leased file (violation → quarantine + restore) ===
PASS: quarantine copy created under B/.tazamun/conflicts/
PASS: notes.txt restored to the indexed version
PASS: restored notes.txt is read-only again
PASS: quarantine holds the exact forced bytes (nothing deleted)

=== STEP 9 — status on both nodes shows conn type + RTT ===
----- A status -----
peer id : 7cff24643f7f29e6dcbdb67a1248f4991a92c816fa3b2e85a97c28124832164a
folder  : /home/cc1a2b/tazamun-smoke/A
files   : 1 (34 bytes)

members (1):
  5ec3b43f93  online   Direct   rtt 0 ms

active leases (0):
----- B status -----
peer id : 5ec3b43f932bf0d96b2ab63cfbc8bed53e7cc358df280d284ee64a932ff2b0cc
folder  : /home/cc1a2b/tazamun-smoke/B
files   : 1 (34 bytes)

members (1):
  7cff24643f  online   Direct   rtt 0 ms

active leases (0):
PASS: A status row shows conn type + RTT
PASS: B status row shows conn type + RTT

=== STEP 10 — SIGINT both daemons: clean shutdown + lease release in logs ===
PASS: A daemon exited after SIGINT
PASS: B daemon exited after SIGINT
PASS: A logged clean shutdown
PASS: B logged clean shutdown
PASS: A log records lease release
PASS: B log records lease release

=== RESULT ===
ALL ASSERTIONS PASSED
```

## Violation evidence (the Golden Invariant, observed)

When node B force-wrote a read-only, un-leased file, the daemon **preserved the
forced bytes** in quarantine and **restored the indexed version** read-only —
nothing was overwritten or deleted:

```text
$ ls -l B/.tazamun/conflicts/
-rw-r--r-- 1  52  20260708T150725864Z__notes.txt

$ cat B/.tazamun/conflicts/20260708T150725864Z__notes.txt
FORCED UNLEASED OVERWRITE — should be quarantined

$ ls -l B/notes.txt
-r--r--r-- 1  34  B/notes.txt      # restored, read-only (0444)

$ cat B/notes.txt
hello from A
line two
EDITED BY B
```

## Note on watcher timing

The violation flow is driven by the filesystem watcher. The daemon suppresses
watch events for a path for a short window (`MUTE_WINDOW = 2 s`) after it writes
that path itself, so its own sync/restore writes are not misread as user edits.
A force-write must therefore fall outside that window to be seen as a fresh user
edit; the acceptance run waits it out before forcing the overwrite. The narrow
race this implies (a user force-write within 2 s of a daemon write to the same
path) is recorded as a known limitation in `DECISIONS.md`.

---

# P1 addendum — performance & terminal UX

Re-run of the full acceptance script with the Phase 1 release binary
(parallel chunk hashing, progress bars, QR invites): **all 27 assertions
passed unchanged** — same transcript shape as above, exit code 0.

New behavior exercised on top of the 27 assertions:

## Live progress bars (pull of a 200 MiB file)

Both daemons ran under a pseudo-terminal so `Ui::detect()` enabled bars; the
joiner pulled a 200 MiB genesis file over loopback QUIC. Captured frames from
the puller's terminal (color codes stripped):

```text
⇣ big-asset.bin · 2597 chunks    147.13 MiB / 200.00 MiB   96.03 MiB/s [====================>        ]  74%
⇣ big-asset.bin · 2597 chunks    163.69 MiB / 200.00 MiB   99.34 MiB/s [======================>      ]  82%
⇣ big-asset.bin · 2597 chunks    192.72 MiB / 200.00 MiB  104.88 MiB/s [==========================>  ]  96%
2026-07-09T14:36:01Z  INFO applied remote version path=big-asset.bin peer=ef80caf509
```

The final line is ordinary tracing output landing *after* the bar cleared —
log lines and bars coexist through the suspending writer, no torn frames.
The pulled file was byte-identical to the source (`cmp` clean), and with the
bars disabled (non-TTY logs-to-file daemons) the same pull produces plain logs
only — presentation never touches transfer semantics.

## `status` transfer rows (live percentage + rate)

Snapshots taken from a second shell while the same 200 MiB pull was in
flight on a fresh joiner:

```text
pending pulls (1):
  big-asset.bin    0%  0.0 MB/s

pending pulls (1):
  big-asset.bin   48%  126.5 MB/s
```

## QR invite

`tazamun invite --qr` renders the exact `tzm1…` ticket as a unicode
half-block QR (scannable, inverted polarity for dark terminals) followed by
the same ticket as text; plain `tazamun invite` output is unchanged, and a
too-narrow terminal falls back to the plain ticket with a note.

## Publish path (parallel chunker) under the same script

The genesis import of the 200 MiB file and every lock/edit/unlock in the
script went through the new `chunk_file` pipeline (reader thread + sequential
FastCDC scan + rayon hash batches) — byte-identical cut points and hashes,
measured 1.66× faster on the 64 MiB benchmark (details in `DECISIONS.md`).

---

# P2 addendum — connection health & observability

Re-run of the full acceptance script with the Phase 2 release binary: **all 27
assertions passed** (exit 0). The status-panel assertions now match the new
grade-based rows (`(Direct|Relayed) NN±Mms`). New health surfaces exercised
live with two `--no-relay` daemons on the native filesystem:

## `status` panel (grade dots, jitter, path Δ, events)

```text
peer id : 5446…
folder  : <dir>
files   : 0 (0 bytes)

members (1):
  ● Good   <peer> Direct  0±0ms        Δ0

active leases (0):

recent events:
  • peer <peer> connected (Direct, rtt 0ms)
```

`● Good` is a green dot on a TTY (shown here with `NO_COLOR=1`). `--watch`
renders this same panel refreshing every second and exits on `q`/Ctrl-C;
`--json` emits the full schema-1 snapshot.

## Forced lock-failure diagnosis

Node B was killed abruptly (SIGKILL, no goodbye); once A graded it Offline, a
lock on A failed with a full network-terms diagnosis (`-v` table shown):

```text
$ tazamun -v lock report.md
✗ could not lock report.md: strict mode: no peer is currently reachable (last known: <peer>)
  blocked precondition : REACHABILITY
  what to do           : wait for at least one peer to reconnect (check `tazamun status`)
  peers consulted:
    id           grade    conn          rtt  answered
    <peer>   Offline  None          0ms  NO
```

The failed acquire names the unreachable peer, states the blocked precondition,
and gives an actionable next step.

## `doctor` (both nodes, `--no-relay`)

```text
$ tazamun doctor
tazamun doctor

[OK  ] identity  [from daemon]
     peer id            : 5446…
     bound socket       : 0.0.0.0:<port>
     bound socket       : [::]:<port>
[OK  ] relay
     policy             : disabled by flag (--no-relay)
     relays             : not used — direct/LAN only
[OK  ] connectivity  [from daemon]
     peer <peer>      : Direct (Good, 0ms, direct in 0ms)
[OK  ] filesystem
     watcher backend    : inotify
     session folder     : <dir> (native FS)
     read-only enforce  : working (create+chmod probe passed)
[OK  ] ipc
     socket             : <dir>/.tazamun/daemon.sock
     daemon             : responding

summary: OK
```

`--no-relay` correctly reports the relay section as **OK / disabled by flag**
(not an error), the hole-punched **Direct** link is reported under
connectivity, and the process exits `0` (all sections OK). Identifiers and
paths above are redacted; the run is otherwise verbatim.

---

# P3 addendum — sovereignty (self-hosted relay, LAN, airgap)

Three scenarios driven by the Phase 3 release binary, each with its own driver
script under `~/tazamun-smoke-p3/`. Every assertion passed; transcripts are
verbatim (peer ids abbreviated). Together they exercise the two automatable
sovereignty guarantees end-to-end on real infrastructure, plus the client half
of the self-hosted-relay path against a genuine relay.

## LAN rendezvous — meet over mDNS from a zero-address ticket

Node A `init`s and hands B a ticket carrying **only the session secret** (the
`init`-time ticket has no live addresses — the daemon is not running yet). Both
start with `--no-relay`, so with no relay and no address in the ticket the only
way to meet is local mDNS discovery.

```text
=== STEP 1 — A: init (ticket carries NO live addresses) ===
PASS: A initialized, secret-only ticket (113 chars)
=== STEP 2 — A: start --no-relay (LAN discovery on by default) ===
PASS: A daemon answering IPC
=== STEP 3 — B: join the address-less ticket, start --no-relay ===
PASS: B joined (no addresses known)
PASS: B daemon answering IPC
=== STEP 4 — they must meet over mDNS alone (no ticket addresses) ===
PASS: peers discovered each other via LAN mDNS
=== STEP 5 — status shows the peer as reached via LAN ===
----- A status -----
members (1):
  ● Good   ca40b82acc Direct  0±0ms        Δ0 via LAN

active leases (0):
PASS: A tags the peer 'via LAN'
=== STEP 6 — a lease + edit round-trips over the LAN-only link ===
PASS: A locked lan.txt
PASS: A unlocked lan.txt
PASS: lan.txt synced to B over the LAN link
PASS: byte-exact copy on B
=== RESULT ===
LAN RENDEZVOUS SMOKE PASSED
```

The proof-of-secret handshake still gates the connection (`peer authenticated`
in both logs), so only a genuine session member is ever dialed — mDNS supplies
the address, the secret supplies the trust. The **via LAN** tag on the status
row is the observable signal that the peer was reached over a private-network
Direct path rather than a relay. On a CI runner without multicast this scenario
self-skips (STEP 4 fails fast with the "no multicast" note) rather than hanging.

## Airgap — closed network, egress sweep with `ss`

A single `--airgap` daemon; `doctor` states the closed-network guarantees, then
`ss` enumerates every socket the daemon owns and asserts none reaches a public
address.

```text
=== STEP 1 — A: init + start --airgap ===
PASS: airgap daemon answering IPC (pid 29373)
=== STEP 2 — doctor reports a closed network ===
PASS: doctor: mode = AIRGAP (closed network)
PASS: doctor: guarantees = no relays / no DNS-pkarr / LAN only
PASS: doctor: relays not used
=== STEP 3 — let the endpoint settle, then sweep its sockets with ss ===
----- ss -tunap for pid 29373 -----
  udp 0.0.0.0:53539 -> peer 0.0.0.0:*
  udp 0.0.0.0:5353 -> peer 0.0.0.0:*
  udp [::]:55930 -> peer [::]:*
  udp *:5353 -> peer *:*
PASS: ss egress sweep: 0 sockets reach a public address
=== STEP 4 — corroborate: no established TCP to any host at all ===
PASS: no established outbound TCP (relays + pkarr truly off)
=== RESULT ===
AIRGAP EGRESS SMOKE PASSED
```

The only sockets are the endpoint's own wildcard UDP binds plus the mDNS group
on **:5353** — i.e. local discovery is the one thing still listening, and there
is no relay/pkarr traffic and no established outbound TCP at all. `doctor`'s
`AIRGAP (closed network)` section makes the guarantee legible at a glance.

## Self-hosted relay — client path against a real relay

The forced *Relayed peer path* needs two peers whose only route is the relay; on
a single host loopback is always directly reachable, so that assertion is the
two-machine procedure below. What one host **can** prove — and this run does,
against a genuine relay — is the full client wiring: the official
`n0computer/iroh-relay` image in `--dev` (plain-HTTP) mode stands in for the
[`deploy/relay/`](deploy/relay) HTTPS kit, the client persists it via `config`,
and the endpoint adopts it as its home relay with `doctor` probing it reachable.

```text
=== STEP 1 — bring up a real iroh-relay (dev/HTTP) in Docker ===
PASS: relay listening on http://localhost:3340
=== STEP 2 — persist the self-hosted relay, then start the daemon ===
PASS: config set relay http://localhost:3340
PASS: config show reflects the self-hosted relay
PASS: daemon answering IPC (pid 31479)
=== STEP 3 — the endpoint adopts the self-hosted relay as its home relay ===
----- doctor relay section -----
     policy             : custom: http://localhost:3340/
     home relay         : http://localhost:3340/
     reachability       : reachable (relay link up)

PASS: endpoint adopted the self-hosted relay as its home relay
PASS: doctor probe: relay link up (reachable)
=== RESULT ===
SELF-HOSTED RELAY CLIENT SMOKE PASSED
```

The daemon's `iroh` debug log shows the mechanism end-to-end: `net_report`
selects the relay (`home is now relay http://localhost:3340/, was None`), the
relay actor dials it by websocket (`ws://localhost:3340/relay`, TCP to
`127.0.0.1:3340`), and the keepalive ping/pong round-trips at ~100 µs. Any peer
that later cannot hole-punch would fall back through exactly this link.

**Two-machine step (Relayed peer path + hostname).** Run the
[`deploy/relay/`](deploy/relay) kit on a public host, then on two machines on
**different** networks: `tazamun config set relay https://relay.example.com` on
both, `init`/`invite`/`join`/`start`, and confirm `tazamun status` shows the
peer as **Relayed** with the relay hostname and a real RTT (the same fields the
automated `relayed_sample_surfaces_conn_and_hostname` unit test asserts over the
telemetry pipeline). This is the standard two-network setup from the *Internet
Acceptance Checklist*, pinned to a self-hosted relay. It is a manual step
because a single host cannot force the relay path — see `DECISIONS.md`.

---

# P4 addendum — lease ergonomics (autolock race, waitlist handoff)

Two scenarios driven by the Phase 4 release binary (scripts under
`~/tazamun-smoke-p4/`). Transcripts are verbatim (peer ids abbreviated); every
assertion passed.

## Autolock race — one winner, the loser's bytes preserved

Both nodes turn autolock on and force-write the same un-leased file at the same
moment. Exactly one edit is published; the other is quarantined — never
silently overwritten.

```text
=== STEP 1 — A: genesis race.txt, autolock on, start ===
PASS: A up with autolock, live ticket minted
=== STEP 2 — B: join, autolock on, start; sync the base ===
PASS: B synced the base version
=== STEP 3 — both force-write race.txt at once (un-leased) ===
PASS: both nodes force-wrote an un-leased edit
=== STEP 4 — converge to ONE winner on both nodes ===
PASS: both nodes converged to a single winner: 'from-A'
=== STEP 5 — the loser's bytes are preserved in quarantine ===
  A conflicts: 1   B conflicts: 1
  quarantined: 20260710T134343730Z__race.txt = 'from-A'
  quarantined: 20260710T134343737Z__race.txt = 'from-B'
PASS: the losing write is quarantined (nothing silently overwritten)
=== RESULT ===
AUTOLOCK RACE SMOKE PASSED (winner='from-A')
```

Both nodes converge to `from-A`, and **both written variants remain
recoverable**: the winner on disk, and each node's own pre-edit bytes in
`conflicts/` (`from-A` on A, `from-B` on B). This exercised — and the fix for —
a real Golden-Invariant gap the smoke surfaced: a remote apply used to overwrite
the on-disk file without checking for an un-indexed local edit, so in the tight
simultaneous-write race the loser's bytes could be lost (their watcher event was
swallowed by the apply's mute). Since a synced file is read-only, `apply_remote`
now treats a *writable* file as an un-leased edit and quarantines it before
applying the incoming version.

## Waitlist handoff — A holds, B waits, B auto-acquires

```text
=== STEP 2 — A takes the lease ===
PASS: A holds the lease
=== STEP 3 — B waits for it (lock --wait, backgrounded) ===
PASS: B is waiting: … report.md is held by 341bbdc079; waiting (auto-acquires when free, Ctrl-C to stop)
=== STEP 4 — the holder lists B as a waiter ===
  A sees "waiters":["5cd638ad34975362f041d1249a85c6d82dee9cf4c93b4b21ac70f0c6db83abec"]
PASS: holder lists B as a waiter
=== STEP 5 — A unlocks; B auto-acquires ===
PASS: B's --wait resolved (auto-acquired)
PASS: B acquired: ✔ report.md is now writable (lease TTL 90s, auto-renewed)
=== RESULT ===
WAITLIST HANDOFF SMOKE PASSED
```

`tazamun lock report.md --wait` registered B's interest (the holder listed it as
a `waiter` in `status --json`), and the moment A unlocked, B's waiting client
re-attempted the acquire and won it — `report.md` became writable on B with the
holder's own 90s TTL.

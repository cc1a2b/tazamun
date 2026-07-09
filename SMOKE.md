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

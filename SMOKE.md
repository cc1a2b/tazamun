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

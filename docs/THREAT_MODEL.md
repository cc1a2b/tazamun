# tazamun threat model (v0.1)

What tazamun defends, from whom, where the defense lives, and — just as
important — what it deliberately does **not** defend against. This is the P6
security-pass artifact; it is meant to be read alongside the code, so every
mitigation names its enforcement site.

The one rule everything else serves is the **Golden Invariant** (stated in the
crate-level docs of `src/lib.rs`, enforced at the sites in §8): *never overwrite
data a peer has not seen; never silently delete user bytes; every ambiguous
situation resolves to "preserve both copies, warn loudly."*

---

## 1. Assets

| Asset | Why it matters |
| --- | --- |
| **File bytes** (content in the synced folder + history) | The user's data. Loss or silent corruption is the worst outcome. |
| **Session secret** (32 bytes) | The single root of trust: topic id, handshake auth key, and gossip key all derive from it (`session.rs::SessionKeys::derive`). Whoever has it is a full member. |
| **Lease integrity** | Strict exclusive checkout depends on exactly one writer per path. A forged/stolen lease could enable a silent overwrite. |
| **Availability** | The daemon must keep serving a legitimate user even while a peer misbehaves. |
| **Membership metadata** (who is online, addresses) | Lower-sensitivity, but should not leak to non-members. |

---

## 2. Trust boundaries & adversaries

Four boundaries, from outermost to innermost:

1. **Non-member with the gossip topic** — knows or guesses the topic id (it is
   derivable only from the secret, but assume it leaks) but **not** the session
   secret. Can open QUIC connections, join the gossip overlay, and send bytes.
2. **Authenticated insider** — a current or former member who holds (or held)
   the session secret / an invite ticket. Completed the handshake. Everything
   it sends is "authenticated" but may be hostile. *This is the hardest and
   most important boundary.*
3. **Relay / network** — a relay server (n0's or self-hosted) and any on-path
   network observer. Forwards packets; may drop, delay, reorder, or inspect
   ciphertext.
4. **Local OS / filesystem** — other processes and users on the same machine.

Assumption for all boundaries: **everything on the wire is hostile.** Every
inbound byte is untrusted until a specific check passes.

---

## 3. Boundary 1 — non-member with the topic

| Capability | Mitigation | Enforcement site |
| --- | --- | --- |
| Open a control connection and try to act as a member | **Mutual proof-of-secret handshake**: both sides prove knowledge of the secret via `HMAC-SHA256(auth_key, label ‖ id_min ‖ id_max ‖ nonce_a ‖ nonce_b)` before any application message; failure closes with one generic reason. | `net/control.rs::{proof, handshake_initiator, handshake_acceptor}` |
| Replay a captured handshake | The proof binds **both** nonces; a fresh connection uses a fresh `nonce_b`, so a recorded proof never re-verifies. | `net/control.rs::proof`; test `security.rs::recorded_proof_replayed_into_fresh_session_is_rejected` |
| Probe for an auth oracle (bad-proof vs bad-peer) | Constant-time compare, single generic `HandshakeError::Failed`, identical close code/reason for every failure. | `net/control.rs::verify`; test `security.rs::wrong_secret_matrix_fails_closed_without_oracle` |
| Read membership gossip | Presence beacons are XChaCha20-Poly1305 sealed under the gossip key (topic id as AAD); undecryptable gossip is dropped silently. | `net/membership.rs::{seal, open}` |
| Exhaust resources with half-open handshakes | **`MAX_INFLIGHT_HANDSHAKES`** semaphore: beyond the cap the accept side closes immediately instead of tying up a task/stream for the handshake deadline. | `daemon.rs::CtlAccept::accept` |

A non-member cannot become a peer, cannot read gossip, and cannot force
unbounded handshake work. It can still *connect* to the QUIC endpoint (that is
inherent to being internet-reachable) — see "not defended".

---

## 4. Boundary 2 — authenticated insider (the collaboration boundary)

An insider legitimately holds the secret. The model here is **integrity of the
protocol and the Golden Invariant**, not keeping the insider out — a member can
legitimately edit files they lock. The line is: a malicious member must not be
able to make another node **silently lose data**, **write unverified bytes**,
**wedge the sync loop**, or **exhaust resources**.

| Hostile action | Mitigation | Enforcement site |
| --- | --- | --- |
| Wire path traversal (`../`, absolute, drive-letter, backslash, NUL, `.tazamun`, overlong) in `Index`/`FileMeta`/lock messages | **`sanitize_rel_path`** runs on *every* remote path at the wire boundary; a failing path drops its whole record (never partially applied, never queued). | `sync/index.rs::sanitize_rel_path`, called in `daemon.rs::on_ctl` for every path-bearing variant; tests `security.rs::hostile_paths_in_*` |
| Non-representable path (Windows reserved/case-collision) | Portability validator; violating records are held **unapplied** on Windows (never guessed, never mangled), warn-only on Unix. | `sync/index.rs::portability_violation`, `daemon.rs::portability_reason` / `maybe_pull` |
| `LockGrant` for a path we never requested | Ignored: a grant only counts from a peer in the request's voter set. | `locks.rs::on_grant` (`needed.contains(from)`) |
| `LockRenew` / `LockRelease` for someone else's lease | Ignored: holder identity is checked. | `locks.rs::{on_renew, on_release}` |
| `FileMeta` with a version vector that skips causal history / concurrent clocks | Concurrent clocks are treated as tampering: the deterministic loser **quarantines** its bytes and pulls the winner; nothing is merged. | `daemon.rs::on_concurrent`; `sync/index.rs::diff` |
| Advertise content, then serve wrong/absent bytes | Every chunk is **BLAKE3-verified** by iroh-blobs on fetch and length-checked on assembly into a staging file; a failed pull leaves the folder untouched (atomic rename). Unverifiable content is never written. | `sync/transfer.rs::{pull_stage, assemble}`; tests `security.rs::insider_illegal_control_sequences_stay_healthy` |
| Push an un-leased edit race to overwrite a loser's bytes | A synced file is read-only (0444); a *writable* file on disk is an un-leased edit and is quarantined **before** any apply overwrites it. | `daemon.rs::apply_remote` (preserve-first); `guard.rs::quarantine` |
| Manifest / chunk bomb (millions of chunks, petabyte size, u64-overflow fold) | Chunk count capped at **`MAX_CHUNKS_PER_FILE`**; length fold uses **checked** arithmetic (overflow-audited); size must equal the declared record size; a manifest **blob** larger than **`MAX_MANIFEST_BYTES`** is rejected before it is loaded into memory. | `sync/manifest.rs::{decode_blob, folded_size, check}`; `sync/transfer.rs::resolve_manifest`; tests in `sync::manifest` |
| `Index` flooding thousands of paths → unbounded pull tasks | **`MAX_CONCURRENT_PULLS`** running pulls with a bounded backlog (**`MAX_PULL_BACKLOG`**); excess records stay in the peer index (FRESHNESS still gates edits) and drain as slots free. | `daemon.rs::{maybe_pull, drain_pull_backlog}`; test `security.rs::hostile_index_flood_respects_pull_cap` |
| `Index` flooding thousands of leases, or a `LockReq` storm | **`MAX_TRACKED_LEASES`** cap: a new path is refused at capacity (`DenyReason::Unavailable`); existing leases still update. | `locks.rs::{on_remote_request, observe_lease}` (`at_capacity_for_new`) |
| `LockInterest` flooding the waitlist | **`MAX_WAITLIST_ENTRIES`** cap on distinct waitlisted paths. | `daemon.rs::on_ctl` (`Msg::LockInterest`) |
| Spin up many identities to exhaust the peer table | **`MAX_PEERS`** cap on authenticated peers. | `daemon.rs::on_authed` |
| Oversized / malformed control frame | `u32` length prefix rejected if `0` or `> MAX_FRAME` **before** the body read; any decode error is fatal for that connection. | `proto.rs::{read_msg, decode_frame}`; test `security.rs::oversized_frame_closes_connection` |
| Forged / tampered invite ticket | Prefix, base32, postcard, and version are all validated; tampering the secret region yields a different secret the handshake rejects. | `session.rs::Ticket::decode`; test `security.rs::tampered_ticket_rejected` |

**Accepted risk at this boundary (by design):** an insider *can* corrupt a
file **they legitimately hold the lease on** — that is the collaboration model,
not a break. It is bounded by version **history** (`versions.rs`,
`HISTORY_KEEP` prior versions kept and restorable) and by the fact that the
change is causal and attributable, never silent.

---

## 5. Boundary 3 — relay / network

| Capability | Mitigation / reality |
| --- | --- |
| Read file content in transit | **Cannot.** All transport is QUIC/TLS 1.3 between endpoints; the relay only forwards opaque, end-to-end-encrypted packets. iroh relays never see plaintext. |
| Read control/gossip payloads | Control frames ride the encrypted QUIC channel; gossip payloads are separately sealed (§3). |
| See metadata (that two endpoints talk, their addresses, volume, timing) | **Accepted.** A relay necessarily sees endpoint ids and traffic patterns. Run a self-hosted relay (`docs`/P3) to keep even that in-house. |
| Drop / delay / reorder | Degrades availability only; QUIC handles reordering, and the sync loop is convergent and retry-driven. Cannot cause data loss (atomic staging + Golden Invariant). |

---

## 6. Boundary 4 — local OS / filesystem

| Capability | Mitigation / reality |
| --- | --- |
| Another process reads the session secret | Secret material is mode **0600**, the metadata dir **0700**, and keys **zeroize on drop**. A root/same-user process can still read process memory — see "not defended". | `state.rs` (`set_secret_mode`, `create_meta_dirs`), `session.rs` (`Zeroize`/`ZeroizeOnDrop`) |
| Tamper with on-disk files behind the daemon | Detected: read-only enforcement + startup divergence scan quarantine offending bytes and restore the causal version. | `guard.rs`, `daemon.rs::startup_scan` |
| Local IPC abuse | One JSON line per request, `IPC_LINE_MAX` bounded; the socket lives under the 0700 metadata dir. | `ipc.rs` |

---

## 7. What v0.1 does **NOT** defend against (explicit)

- **A compromised member / leaked secret.** Anyone with the session secret is a
  full member: they can read all content and add/edit files under leases. There
  is no per-member authorization, revocation, or key rotation in v0.1. Rotating
  a session means starting a new one with a new secret.
- **Malicious code on a member's machine**, a root/same-user local attacker, or
  memory scraping. Zeroize reduces the window; it is not a defense against a
  privileged local adversary.
- **Traffic-analysis / metadata privacy** against a relay or on-path observer
  (who-talks-to-whom, timing, volume). Content is encrypted; metadata is not
  hidden.
- **Endpoint IP exposure to peers.** Direct P2P connections reveal your IP to
  the peers you connect to. This is inherent to hole-punched P2P; use a relay
  path (accept the metadata trade-off) to avoid it.
- **Denial of availability by a determined insider.** The bounds in §4 stop
  memory/FD exhaustion and keep the daemon responsive, but a member can still
  waste bandwidth/CPU. The trust model assumes members are mostly honest; the
  goal is *integrity under a hostile member*, not *availability against one*.
- **Rollback/equivocation by an insider** (advertising different histories to
  different peers). Detected as concurrent clocks → quarantine, not prevented.
- **Post-quantum adversaries.** Crypto is classical (X25519/QUIC-TLS,
  HMAC-SHA256, XChaCha20-Poly1305, BLAKE3).

---

## 8. Cross-reference: invariants → enforcement

| Invariant / control | Lives in |
| --- | --- |
| Golden Invariant (preserve both, never silently delete) | `guard.rs::quarantine`, `daemon.rs::{apply_remote, on_concurrent, on_violation_staged}`, `versions.rs` |
| Proof-of-secret handshake (both nonces bound, constant-time, generic failure) | `net/control.rs` |
| BLAKE3 content verification + atomic staging | `sync/transfer.rs::{pull_stage, assemble}` |
| Untrusted-path sanitizer (the only `RelPath`-from-wire gate) | `sync/index.rs::sanitize_rel_path` |
| Portability validator (Windows representability) | `sync/index.rs::portability_violation` |
| Gossip confidentiality | `net/membership.rs::{seal, open}` |
| Key separation (HKDF, one secret → topic/auth/gossip) | `session.rs::SessionKeys::derive` |
| Lease integrity (voter grants, holder checks, TTL clamp, tie-break) | `locks.rs` |
| DoS bounds (all of them) | `lib.rs::consts` (values) + the sites in §3–§4 |

---

## 9. Reporting a security issue

Found a way to make tazamun lose or corrupt data, write unverified bytes, wedge
the sync loop, or authenticate without the secret? Please report it privately
to **cc1a2b** at `renhusa9@gmail.com` (or open a GitHub **security advisory** on
`cc1a2b/tazamun`). Include the version/commit, a minimal reproduction, and the
observed vs. expected behavior. Please do not open a public issue for an
unpatched data-loss or auth-bypass bug. There is no bounty program; credit is
given in the release notes unless you prefer otherwise.

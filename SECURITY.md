# Security policy

tazamun moves people's files between machines and refuses to let a server read
them. That makes correctness and confidentiality the product, not a feature of
it, so security reports are welcome and taken seriously.

## Reporting a vulnerability

**Use GitHub's private vulnerability reporting:**
[Report a vulnerability](https://github.com/cc1a2b/tazamun/security/advisories/new).
It is private between you and the maintainer, it lets us work on a fix and
publish an advisory together, and it does not expose the issue while it is
unpatched.

Please do **not** open a public issue for an unpatched data-loss, key-handling,
or authentication-bypass bug.

A good report includes:

- the version (`tazamun --version` prints the crate version and the commit),
- the platform, and whether the peers were direct, relayed, or on a LAN,
- a minimal reproduction — ideally a sequence of commands,
- what you observed against what you expected.

You should get an acknowledgement within a few days. There is no bounty
programme; credit goes in the release notes unless you would rather it did not.

## What is in scope

Anything that breaks one of the guarantees the project makes about itself:

- **Data loss or corruption.** The Golden Invariant is *never overwrite data a
  peer has not seen; never silently delete user bytes.* A way to make tazamun
  drop, truncate, or clobber a file without quarantining it first is the most
  serious class of bug here — more serious than a crash.
- **Writing unverified bytes.** Content is BLAKE3-addressed; anything that
  lands on disk without matching its hash is in scope.
- **Authentication bypass.** Joining a session, reading gossip, or being granted
  a lease without proving knowledge of the session secret.
- **Confidentiality against a relay.** Relays forward sealed packets. Anything
  that leaks plaintext, file names, or folder structure to one is in scope.
- **Lease-model breakage.** Two nodes convinced they both hold an exclusive
  lease on the same path.
- **Remote resource exhaustion** that a peer can trigger against another peer.

## What is out of scope

These are documented limitations, not bugs — see
[docs/THREAT_MODEL.md](docs/THREAT_MODEL.md) for the full reasoning:

- **A malicious member.** Anyone holding the session secret is inside the trust
  boundary by design: they can read, write, and publish. Revocation is
  `tazamun rekey`, which mints a new key for the members you keep. There is no
  defence against someone you invited.
- **A compromised local machine.** The session secret lives in `state.json` with
  0600 permissions. An attacker who can read your disk or your process memory
  has the session.
- **Denial of service by a member** — the same trust boundary applies.
- **Traffic analysis.** A network observer cannot read your files, but sizes and
  timing are not hidden.

## Supported versions

Development is on `main`, and fixes land there first. Until 1.0, only the latest
tagged release and `main` receive security fixes.

## Advisories in dependencies

`cargo audit` runs against the tree and its accepted-advisory list is kept in
`.cargo/audit.toml`, with the reasoning for each acceptance recorded in
[DECISIONS.md](DECISIONS.md). An advisory in a transitive dependency that is
genuinely reachable from tazamun's own code paths is in scope; one that is not
reachable is documented rather than silently ignored.

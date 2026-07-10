# Self-hosted tazamun relay

Run your own [iroh](https://iroh.computer) relay so your sessions never touch
the public relay infrastructure. Direct hole-punched connections always bypass
the relay; it is only the encrypted fallback for peers that cannot reach each
other directly. **The relay never sees file content** — it forwards opaque,
end-to-end-encrypted bytes.

## One command

On any VPS with a public IP and a domain:

```bash
git clone https://github.com/cc1a2b/tazamun && cd tazamun/deploy/relay
cp .env.example .env      # set TZM_RELAY_DOMAIN and TZM_ACME_CONTACT
docker compose up -d
```

The relay compiles the `iroh-relay` binary, obtains a Let's Encrypt certificate
automatically (built-in ACME — no reverse proxy), and starts serving. First
start takes a few minutes to build and provision TLS.

> **Skip the build.** To avoid compiling from source, point the compose
> `image:` at the official prebuilt `n0computer/iroh-relay` (matched to the
> `DECISIONS.md` pin) and drop the `build:` block — the config, ports, and ACME
> setup are identical.

## Local test drive (no domain, no TLS)

To verify the client wiring before you provision a public host, run the same
relay in localhost dev mode (plain HTTP, no certificate) and point one node at
it:

```bash
docker run --rm --network host n0computer/iroh-relay --dev   # serves :3340
tazamun config set relay http://localhost:3340
tazamun start &                       # in the session folder
tazamun doctor                        # relay section → home relay + "reachable"
```

`tazamun doctor` should report the relay as the home relay and the link
reachable. (Two nodes on one host still connect **Direct** over loopback, so the
`Relayed` peer path only appears across two real networks — see `SMOKE.md`.)

## DNS + open-ports checklist

Before `docker compose up -d`:

- [ ] **DNS:** an `A` (and `AAAA` if you have IPv6) record for
      `TZM_RELAY_DOMAIN` pointing at this host's public IP.
- [ ] **TCP 80** open inbound — the ACME http-01 challenge (certificate
      issuance/renewal).
- [ ] **TCP 443** open inbound — the relay HTTPS endpoint (the relay protocol).
- [ ] **UDP 7842** open inbound — QUIC address discovery (helps peers learn
      their public address so direct hole-punching succeeds).
- [ ] (optional) **TCP 9090** — Prometheus metrics; bound to `127.0.0.1` by
      default, expose only behind your own auth.

## Point tazamun at it

On every member's machine, persist the relay once:

```bash
tazamun config set relay https://relay.example.com
tazamun config show          # verify
tazamun start                # uses your relay for the fallback path
```

Per-run override without changing the saved config:

```bash
tazamun start --relay https://relay.example.com
```

Invite tickets automatically embed each node's current relay in its address, so
a freshly invited peer learns your relay from the ticket alone. Verify a
relayed connection with `tazamun status` (the peer row shows `Relayed` and the
relay hostname) or `tazamun doctor` (the relay section probes your relay's live
connection).

## Resource footprint

A relay is lightweight — it forwards bytes and answers address-discovery
probes. A 1 vCPU / 512 MB VPS comfortably serves a small team; bandwidth is the
main cost and only for sessions that fall back to relaying (direct connections
use none of it). The `relay-certs` volume holds a few KB of certificates.

## Upgrading

```bash
cd tazamun/deploy/relay
git pull                       # pick up a newer pinned iroh-relay version
docker compose build --no-cache
docker compose up -d           # certs persist in the named volume
```

The relay and clients must stay on the same iroh 1.x wire-compatible line;
`DECISIONS.md` records the exact pin.

#!/usr/bin/env bash
# Two-machine Relayed-path acceptance drill (P3 debt for v0.1.0).
#
# Proves a real relayed connection across TWO machines on DIFFERENT networks,
# through YOUR self-hosted relay — the one thing a single host cannot show
# (loopback always hole-punches Direct). Run one role per machine.
#
#   Relay host (VPS, public IP + domain):
#     cd tazamun/deploy/relay && cp .env.example .env   # set domain + ACME email
#     docker compose up -d                              # brings up iroh-relay + TLS
#
#   Machine 1 (network A):   ./acceptance-drill.sh node1 https://relay.example.com
#   Machine 2 (network B):   ./acceptance-drill.sh node2 https://relay.example.com <TICKET-from-node1>
#
# Each node forces the relay as its only relay and disables LAN discovery, so
# the only paths are direct hole-punch or your relay. Across two real NATs the
# path settles Relayed; the drill prints the evidence (conn=Relayed + relay
# hostname + RTT) or FAILs.
set -u
ROLE="${1:-}"
RELAY="${2:-}"
TICKET="${3:-}"
BIN="${TAZAMUN:-tazamun}"

pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*"; exit 1; }

case "$ROLE" in
  node1)
    [ -n "$RELAY" ] || fail "usage: acceptance-drill.sh node1 <relay-url>"
    DIR="${DIR:-$HOME/tzm-relay-node1}"
    mkdir -p "$DIR"
    "$BIN" --dir "$DIR" init || true
    "$BIN" --dir "$DIR" config set relay "$RELAY"
    echo "salam from network A" > "$DIR/hello.txt"
    "$BIN" --dir "$DIR" start --no-lan --relay "$RELAY" >/tmp/tzm-node1.log 2>&1 &
    echo "$!" > /tmp/tzm-node1.pid
    for _ in $(seq 1 40); do "$BIN" --dir "$DIR" status --json >/dev/null 2>&1 && break; sleep 0.5; done
    echo "=== node1 up. Give this ticket to machine 2: ==="
    "$BIN" --dir "$DIR" invite
    echo "=== then watch: tazamun --dir $DIR status ==="
    ;;

  node2)
    [ -n "$RELAY" ] && [ -n "$TICKET" ] || fail "usage: acceptance-drill.sh node2 <relay-url> <ticket>"
    DIR="${DIR:-$HOME/tzm-relay-node2}"
    mkdir -p "$DIR"
    "$BIN" --dir "$DIR" join "$TICKET"
    "$BIN" --dir "$DIR" config set relay "$RELAY"
    "$BIN" --dir "$DIR" start --no-lan --relay "$RELAY" >/tmp/tzm-node2.log 2>&1 &
    echo "$!" > /tmp/tzm-node2.pid
    echo "=== waiting for a Relayed peer path (up to 90s) ==="
    for _ in $(seq 1 180); do
      SNAP=$("$BIN" --dir "$DIR" status --json 2>/dev/null)
      # A member row with conn=Relayed and a relay_url is the proof.
      ROW=$(echo "$SNAP" | tr -d ' \n' | grep -oE '"conn":"Relayed"[^}]*"relay_url":"[^"]+"' | head -1)
      if [ -n "$ROW" ]; then
        echo "=== EVIDENCE (from status --json) ==="
        echo "$SNAP" | grep -A12 '"members"' | grep -E '"conn"|"relay_url"|"rtt_ms"|"grade"|"via_lan"' | head -20
        pass "peer path is Relayed through your relay"
        echo "Copy the block above back to close the two-network Relayed proof."
        exit 0
      fi
      sleep 0.5
    done
    echo "=== last snapshot ==="
    "$BIN" --dir "$DIR" status 2>/dev/null | head -20
    fail "no Relayed peer path within 90s — check the relay is reachable (tazamun doctor) and that the two machines are on different networks (a shared LAN hole-punches Direct)"
    ;;

  *)
    echo "usage: $0 <node1|node2> <relay-url> [ticket]"
    echo "  and bring up the relay first: cd deploy/relay && docker compose up -d"
    exit 2
    ;;
esac

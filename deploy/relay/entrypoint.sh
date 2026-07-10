#!/bin/sh
# Renders the templated relay config from the environment, then execs the relay.
set -eu

: "${TZM_RELAY_DOMAIN:?set TZM_RELAY_DOMAIN}"
: "${TZM_ACME_CONTACT:?set TZM_ACME_CONTACT}"

RENDERED=/tmp/relay.toml
# Only the domain placeholder appears in config.toml; fill it in.
envsubst '${TZM_RELAY_DOMAIN}' < /etc/tazamun-relay/config.toml > "$RENDERED"

echo "tazamun relay starting for ${TZM_RELAY_DOMAIN} (ACME contact ${TZM_ACME_CONTACT})"
exec iroh-relay --config-path "$RENDERED"

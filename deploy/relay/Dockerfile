# Builds the iroh-relay server binary (iroh 1.x line) with the `server`
# feature. Pinned to the same iroh-relay version tazamun depends on so the
# wire protocol matches exactly.
FROM rust:1-slim-bookworm AS build
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates \
 && rm -rf /var/lib/apt/lists/*
# Compile just the relay binary from crates.io (no source checkout needed).
RUN cargo install iroh-relay --version 1.0.2 --features server --bin iroh-relay --root /out

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl gettext-base \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /out/bin/iroh-relay /usr/local/bin/iroh-relay
# entrypoint.sh renders config.toml (envsubst) and execs the relay.
EXPOSE 80 443 7842/udp 9090
ENTRYPOINT ["/entrypoint.sh"]

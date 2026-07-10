//! iroh endpoint construction and connection-path introspection.
//!
//! Invariant: the default configuration is the n0 production preset (public
//! relays + DNS/pkarr address lookup) plus LAN mDNS, so two strangers behind
//! different NATs connect from a ticket alone while same-LAN members find each
//! other with no external network. Every deviation (`--relay`, `--no-relay`,
//! `--no-lan`, `--airgap`, test binds) narrows this and is an explicit opt-in;
//! airgap and the test bind reach nothing off the local network.

use std::net::SocketAddr;
use std::time::Duration;

use iroh::endpoint::{Connection, presets};
use iroh::{Endpoint, RelayMode, RelayUrl, SecretKey};

use crate::consts::CTL_ALPN;

/// Relay policy selected on the command line.
#[derive(Debug, Clone, Default)]
pub enum RelayChoice {
    /// n0 production relays + address lookup (the default).
    #[default]
    Default,
    /// Self-hosted relay: replaces the relay map.
    Custom(RelayUrl),
    /// No relays at all (LAN / manually routed setups).
    Disabled,
}

impl RelayChoice {
    /// Parses the persisted/CLI relay string: `default`, `none`/`disabled`, or
    /// an `https://…` relay URL. Rejects anything else with a clear message.
    pub fn parse(s: &str) -> Result<RelayChoice, String> {
        match s.trim() {
            "default" | "" => Ok(RelayChoice::Default),
            "none" | "disabled" | "off" => Ok(RelayChoice::Disabled),
            url => url
                .parse::<RelayUrl>()
                .map(RelayChoice::Custom)
                .map_err(|e| {
                    format!(
                        "invalid relay setting {url:?}: {e} (use default, none, or an https URL)"
                    )
                }),
        }
    }

    /// The canonical string form persisted in `state.json`.
    pub fn to_config_string(&self) -> String {
        match self {
            RelayChoice::Default => "default".to_string(),
            RelayChoice::Disabled => "none".to_string(),
            RelayChoice::Custom(url) => url.to_string(),
        }
    }
}

/// Network configuration for one daemon.
#[derive(Debug, Clone, Default)]
pub struct NetConfig {
    pub relay: RelayChoice,
    /// Enable local mDNS address lookup (on by default; `--no-lan` disables).
    pub lan: bool,
    /// Airgap: relays off + all external discovery off + LAN only. The
    /// endpoint contacts nothing outside the local network.
    pub airgap: bool,
    /// Disable global address lookup and relays entirely and bind to this
    /// address — used by the offline integration tests.
    pub test_bind: Option<SocketAddr>,
    /// Integration-test hook: bind to loopback and use this relay map as the
    /// only relay, with an address filter that keeps the path relayed. Lets a
    /// test exercise the real relay path without external infrastructure.
    pub test_relay: Option<iroh::RelayMap>,
}

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("endpoint bind: {0}")]
    Bind(String),
}

/// The relay map an endpoint would use for the given config. Pure and
/// introspectable: `relay_mode_for(cfg).relay_map().is_empty()` is the concrete
/// "reaches no external relay" check for airgap (and any relay-disabled mode).
pub fn relay_mode_for(cfg: &NetConfig) -> RelayMode {
    if cfg.airgap || cfg.test_bind.is_some() {
        return RelayMode::Disabled;
    }
    if let Some(map) = &cfg.test_relay {
        return RelayMode::Custom(map.clone());
    }
    match &cfg.relay {
        RelayChoice::Default => RelayMode::Default,
        RelayChoice::Custom(url) => RelayMode::custom([url.clone()]),
        RelayChoice::Disabled => RelayMode::Disabled,
    }
}

/// Builds the iroh endpoint from the persisted secret key.
///
/// Discovery/relay topology by mode:
/// - **airgap** (or `test_bind`): `Minimal` preset — no DNS/pkarr address
///   lookup of any kind — plus `RelayMode::Disabled`, so the relay map is
///   empty and the only address source is LAN mDNS (airgap) or the explicit
///   test bind. The endpoint reaches nothing off the local network.
/// - **normal**: the `N0` preset (public relays + DNS/pkarr address lookup),
///   with the relay map set from [`RelayChoice`]; LAN mDNS is layered on unless
///   `--no-lan`.
pub async fn build_endpoint(secret_key: SecretKey, cfg: &NetConfig) -> Result<Endpoint, NetError> {
    let alpns = vec![CTL_ALPN.to_vec(), iroh_blobs::ALPN.to_vec()];

    // Integration-test relay hook: Minimal preset with the test relay as the
    // only relay. Bound to loopback so no external network is touched. (The IP
    // transport is kept — it is how the endpoint reaches the relay itself; two
    // same-host endpoints therefore still connect directly, which is why the
    // *forced* relay-path proof lives in SMOKE against a real relay, while the
    // automated tests prove relay reachability and the telemetry pipeline.)
    if let Some(relay_map) = &cfg.test_relay {
        let builder = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Custom(relay_map.clone()))
            .secret_key(secret_key)
            .alpns(alpns)
            .bind_addr("127.0.0.1:0".parse::<SocketAddr>().expect("valid loopback"))
            .map_err(|e| NetError::Bind(e.to_string()))?;
        return builder
            .bind()
            .await
            .map_err(|e| NetError::Bind(e.to_string()));
    }

    let sovereign = cfg.airgap || cfg.test_bind.is_some();
    let mut builder = if sovereign {
        // Sovereign: the Minimal preset adds no external address-lookup
        // service, and relays are off, so nothing external is contacted.
        Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled)
    } else {
        let b = Endpoint::builder(presets::N0);
        match &cfg.relay {
            RelayChoice::Default => b,
            RelayChoice::Custom(url) => b.relay_mode(RelayMode::custom([url.clone()])),
            RelayChoice::Disabled => b.relay_mode(RelayMode::Disabled),
        }
    };
    // LAN mDNS discovery: enabled by default, and it is the *only* discovery in
    // airgap. Never added for the offline test bind (explicit direct addrs).
    if cfg.lan && cfg.test_bind.is_none() {
        builder = builder.address_lookup(iroh_mdns_address_lookup::MdnsAddressLookup::builder());
    }
    builder = builder.secret_key(secret_key).alpns(alpns);
    if let Some(addr) = cfg.test_bind {
        builder = builder
            .bind_addr(addr)
            .map_err(|e| NetError::Bind(e.to_string()))?;
    }
    builder
        .bind()
        .await
        .map_err(|e| NetError::Bind(e.to_string()))
}

/// How a live connection currently reaches its peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnKind {
    Direct,
    Relayed,
}

impl std::fmt::Display for ConnKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnKind::Direct => f.write_str("Direct"),
            ConnKind::Relayed => f.write_str("Relayed"),
        }
    }
}

/// Reads the selected path of a connection: Direct vs Relayed plus RTT.
pub fn path_info(conn: &Connection) -> Option<(ConnKind, Duration)> {
    let paths = conn.paths();
    let selected = paths.iter().find(|p| p.is_selected()).or_else(|| {
        let mut iter = paths.iter();
        iter.next()
    })?;
    let kind = if selected.is_relay() {
        ConnKind::Relayed
    } else {
        ConnKind::Direct
    };
    Some((kind, selected.rtt()))
}

/// Whether a socket address is on the local network (loopback, RFC1918
/// private, link-local, or IPv6 unique-local/link-local). A Direct path to
/// such an address is a LAN path — the observable signal that a peer was
/// reached via local (mDNS) discovery rather than a relay/global address.
pub fn is_lan_addr(addr: &SocketAddr) -> bool {
    use std::net::IpAddr;
    match addr.ip() {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // unique-local fc00::/7
        }
    }
}

/// Takes one telemetry reading from a live connection: selected-path state,
/// RTT, the relay URL when relayed, whether the path is on the LAN, and
/// cumulative UDP byte counters.
pub fn sample_connection(conn: &Connection) -> crate::net::telemetry::PathSample {
    use crate::net::telemetry::{ConnState, PathSample};
    use iroh::TransportAddr;

    let paths = conn.paths();
    let selected = paths.iter().find(|p| p.is_selected()).or_else(|| {
        let mut iter = paths.iter();
        iter.next()
    });
    let (state, rtt_ms, relay_url, on_lan) = match selected {
        Some(p) => {
            let state = if p.is_relay() {
                ConnState::Relayed
            } else {
                ConnState::Direct
            };
            let (relay, on_lan) = match p.remote_addr() {
                TransportAddr::Relay(url) => (Some(url.to_string()), false),
                TransportAddr::Ip(sock) => (None, is_lan_addr(sock)),
                _ => (None, false),
            };
            (state, p.rtt().as_secs_f64() * 1000.0, relay, on_lan)
        }
        None => (ConnState::None, 0.0, None, false),
    };
    let stats = conn.stats();
    PathSample {
        conn: state,
        rtt_ms,
        relay_url,
        on_lan,
        bytes_tx: stats.udp_tx.bytes,
        bytes_rx: stats.udp_rx.bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_choice_parses_all_forms() {
        assert!(matches!(
            RelayChoice::parse("default"),
            Ok(RelayChoice::Default)
        ));
        assert!(matches!(RelayChoice::parse(""), Ok(RelayChoice::Default)));
        assert!(matches!(
            RelayChoice::parse("none"),
            Ok(RelayChoice::Disabled)
        ));
        assert!(matches!(
            RelayChoice::parse("off"),
            Ok(RelayChoice::Disabled)
        ));
        match RelayChoice::parse("https://relay.example.com./") {
            Ok(RelayChoice::Custom(url)) => {
                assert!(url.to_string().contains("relay.example.com"));
            }
            other => panic!("expected custom url, got {other:?}"),
        }
        // Garbage is rejected with a helpful message.
        let err = RelayChoice::parse("not a url").unwrap_err();
        assert!(err.contains("invalid relay setting"));
    }

    #[test]
    fn relay_choice_config_string_roundtrips() {
        assert_eq!(RelayChoice::Default.to_config_string(), "default");
        assert_eq!(RelayChoice::Disabled.to_config_string(), "none");
        let c = RelayChoice::parse("https://r.example./").unwrap();
        let s = c.to_config_string();
        // Re-parsing the canonical string yields the same custom choice.
        assert!(matches!(RelayChoice::parse(&s), Ok(RelayChoice::Custom(_))));
    }
}

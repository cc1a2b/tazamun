//! iroh endpoint construction and connection-path introspection.
//!
//! Invariant: the default configuration is the n0 production preset (public
//! relays + address lookup on), so two strangers behind different NATs connect
//! from a ticket alone; every deviation (`--relay`, `--no-relay`, `--lan`,
//! test binds) is an explicit opt-in.

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

/// Network configuration for one daemon.
#[derive(Debug, Clone, Default)]
pub struct NetConfig {
    pub relay: RelayChoice,
    /// Enable local mDNS address lookup (`--lan`).
    pub lan: bool,
    /// Disable global address lookup and relays entirely and bind to this
    /// address — used by the offline integration tests.
    pub test_bind: Option<SocketAddr>,
}

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("endpoint bind: {0}")]
    Bind(String),
}

/// Builds the iroh endpoint from the persisted secret key.
pub async fn build_endpoint(secret_key: SecretKey, cfg: &NetConfig) -> Result<Endpoint, NetError> {
    let alpns = vec![CTL_ALPN.to_vec(), iroh_blobs::ALPN.to_vec()];
    let mut builder = if cfg.test_bind.is_some() {
        // Fully offline: no relays, no address lookup services.
        Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled)
    } else {
        let b = Endpoint::builder(presets::N0);
        match &cfg.relay {
            RelayChoice::Default => b,
            RelayChoice::Custom(url) => b.relay_mode(RelayMode::custom([url.clone()])),
            RelayChoice::Disabled => b.relay_mode(RelayMode::Disabled),
        }
    };
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

/// Takes one telemetry reading from a live connection: selected-path state,
/// RTT, the relay URL when relayed, and cumulative UDP byte counters.
pub fn sample_connection(conn: &Connection) -> crate::net::telemetry::PathSample {
    use crate::net::telemetry::{ConnState, PathSample};
    use iroh::TransportAddr;

    let paths = conn.paths();
    let selected = paths.iter().find(|p| p.is_selected()).or_else(|| {
        let mut iter = paths.iter();
        iter.next()
    });
    let (state, rtt_ms, relay_url) = match selected {
        Some(p) => {
            let state = if p.is_relay() {
                ConnState::Relayed
            } else {
                ConnState::Direct
            };
            let relay = match p.remote_addr() {
                TransportAddr::Relay(url) => Some(url.to_string()),
                _ => None,
            };
            (state, p.rtt().as_secs_f64() * 1000.0, relay)
        }
        None => (ConnState::None, 0.0, None),
    };
    let stats = conn.stats();
    PathSample {
        conn: state,
        rtt_ms,
        relay_url,
        bytes_tx: stats.udp_tx.bytes,
        bytes_rx: stats.udp_rx.bytes,
    }
}

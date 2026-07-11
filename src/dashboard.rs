//! Local web dashboard, served by the daemon on loopback.
//!
//! # Security model (this is a local *write* surface — treat it as one)
//!
//! - **Loopback only.** The listener binds `127.0.0.1` (never `0.0.0.0`), so it
//!   is unreachable from the network. The bind address is not configurable.
//! - **Session token.** The daemon mints a random [`DASHBOARD_TOKEN_BYTES`]-byte
//!   token at start. The `tazamun dashboard` command hands it to the browser in
//!   the URL *fragment* (`http://127.0.0.1:<port>/#<token>`), which browsers do
//!   not send back to the server, so it never lands in a request log. The page
//!   script reads `location.hash` and presents the token on every mutation as
//!   the `X-Tazamun-Token` header. Tokens are compared in constant time.
//!   Read (`GET`) endpoints are tokenless; **every mutation requires the token**.
//! - **Anti-DNS-rebinding.** Every request's `Host` header must be a loopback
//!   name (`127.0.0.1`/`localhost`/`[::1]`, with or without the port). A
//!   malicious web page that rebinds a hostname to `127.0.0.1` sends its own
//!   `Host` and is refused — this protects the tokenless reads too.
//! - **Strict CSP.** `default-src 'none'`; the one inline script and inline
//!   style each run under a per-response nonce; `connect-src 'self'` for the
//!   API; no external origins, no `eval`. Plus `X-Frame-Options: DENY`,
//!   `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`.
//! - **Thin adapter.** Every endpoint forwards to the *same* daemon actor
//!   message the IPC socket uses (`ipc_tx`), so there is no second control path
//!   with its own logic, preconditions, or bugs.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};

use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::consts::DASHBOARD_MAX_REQUEST;
use crate::ipc::{IpcRequest, IpcResponse};

/// The channel shared with the IPC socket server: every dashboard request is
/// forwarded to the daemon actor as `(request, reply)` exactly like a socket
/// client, so the HTTP layer owns zero control logic.
type IpcTx = mpsc::Sender<(IpcRequest, oneshot::Sender<IpcResponse>)>;

const INDEX_HTML: &str = include_str!("dashboard.html");

struct Server {
    ipc: IpcTx,
    token: String,
    port: u16,
}

/// Binds `127.0.0.1:port` and serves the dashboard until the process exits.
/// `port` may be `0` for an OS-assigned port; the actual bound port is written
/// to `bound` (so the daemon reports the real port over `DashboardInfo`). A
/// bind failure is logged and the daemon continues without a dashboard — it is
/// never fatal.
pub async fn serve(ipc: IpcTx, token: String, port: u16, bound: Arc<AtomicU16>) {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            warn!("dashboard disabled: cannot bind {addr}: {e}");
            return;
        }
    };
    let actual = listener.local_addr().map(|a| a.port()).unwrap_or(port);
    bound.store(actual, Ordering::Relaxed);
    info!("dashboard on http://127.0.0.1:{actual} (open with `tazamun dashboard`)");
    let server = Arc::new(Server {
        ipc,
        token,
        port: actual,
    });
    loop {
        match listener.accept().await {
            Ok((stream, _peer)) => {
                let server = server.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_conn(stream, server).await {
                        debug!("dashboard conn ended: {e}");
                    }
                });
            }
            Err(e) => {
                warn!("dashboard accept error: {e}");
            }
        }
    }
}

struct Request {
    method: String,
    path: String,
    query: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

async fn handle_conn(mut stream: TcpStream, server: Arc<Server>) -> std::io::Result<()> {
    let Some(req) = read_request(&mut stream).await? else {
        return Ok(());
    };
    let bytes = route(&server, &req).await;
    stream.write_all(&bytes).await?;
    stream.flush().await
}

/// Reads one HTTP/1.1 request, bounded by [`DASHBOARD_MAX_REQUEST`]. Returns
/// `None` on a malformed/oversized request (the caller just closes).
async fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<Request>> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos;
        }
        if buf.len() > DASHBOARD_MAX_REQUEST {
            return Ok(None);
        }
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let Ok(head) = std::str::from_utf8(&buf[..header_end]) else {
        return Ok(None);
    };
    let mut lines = head.split("\r\n");
    let Some(request_line) = lines.next() else {
        return Ok(None);
    };
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Ok(None);
    };
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let content_len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if content_len > DASHBOARD_MAX_REQUEST {
        return Ok(None);
    }
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < content_len {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
        if body.len() > DASHBOARD_MAX_REQUEST {
            return Ok(None);
        }
    }
    body.truncate(content_len);
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.to_string(), String::new()),
    };
    Ok(Some(Request {
        method: method.to_string(),
        path,
        query,
        headers,
        body,
    }))
}

async fn route(server: &Server, req: &Request) -> Vec<u8> {
    // Anti-DNS-rebinding: reject any non-loopback Host, protecting reads too.
    if !host_ok(&req.headers, server.port) {
        return json_response(
            "403 Forbidden",
            &serde_json::json!({"api": 1, "ok": false,
                "error": {"code": "bad_host", "message": "loopback Host required"}}),
        );
    }
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => index_response(),
        ("GET", "/api/state") => api_json(server, IpcRequest::DashboardState).await,
        ("GET", "/api/invite") => api_json(server, IpcRequest::Invite).await,
        ("GET", "/api/invite/qr") => api_invite_qr(server).await,
        ("POST", "/api/lock") => guarded(server, req, parse_path_op(&req.body, PathOp::Lock)).await,
        ("POST", "/api/unlock") => {
            guarded(server, req, parse_path_op(&req.body, PathOp::Unlock)).await
        }
        ("POST", "/api/restore") => guarded(server, req, parse_restore(&req.body)).await,
        ("POST", "/api/config") => guarded(server, req, parse_config(&req.body)).await,
        _ => json_response(
            "404 Not Found",
            &serde_json::json!({"api": 1, "ok": false,
                "error": {"code": "not_found", "message": "no such endpoint"}}),
        ),
    }
}

enum PathOp {
    Lock,
    Unlock,
}

/// A parsed mutation ready to forward, or a client error to return as 400.
type ParsedReq = Result<IpcRequest, String>;

fn parse_path_op(body: &[u8], op: PathOp) -> ParsedReq {
    let v: serde_json::Value = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    let path = v
        .get("path")
        .and_then(|p| p.as_str())
        .ok_or("missing string field `path`")?
        .to_string();
    Ok(match op {
        PathOp::Lock => IpcRequest::Lock { path },
        PathOp::Unlock => IpcRequest::Unlock { path },
    })
}

fn parse_restore(body: &[u8]) -> ParsedReq {
    let v: serde_json::Value = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    let path = v
        .get("path")
        .and_then(|p| p.as_str())
        .ok_or("missing string field `path`")?
        .to_string();
    let n = v
        .get("n")
        .and_then(|n| n.as_u64())
        .ok_or("missing integer field `n`")? as usize;
    Ok(IpcRequest::Restore { path, n })
}

fn parse_config(body: &[u8]) -> ParsedReq {
    let v: serde_json::Value = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    let key = v
        .get("key")
        .and_then(|k| k.as_str())
        .ok_or("missing string field `key`")?
        .to_string();
    let value = v
        .get("value")
        .and_then(|k| k.as_str())
        .ok_or("missing string field `value`")?
        .to_string();
    Ok(IpcRequest::ConfigSet { key, value })
}

/// Runs a mutation only if the request carries the valid token; otherwise 401.
async fn guarded(server: &Server, req: &Request, parsed: ParsedReq) -> Vec<u8> {
    if !token_ok(server, req) {
        return json_response(
            "401 Unauthorized",
            &serde_json::json!({"api": 1, "ok": false,
                "error": {"code": "unauthorized", "message": "missing or invalid dashboard token"}}),
        );
    }
    let request = match parsed {
        Ok(r) => r,
        Err(e) => {
            return json_response(
                "400 Bad Request",
                &serde_json::json!({"api": 1, "ok": false,
                    "error": {"code": "bad_request", "message": e}}),
            );
        }
    };
    let resp = forward(&server.ipc, request).await;
    let status = if resp.ok { "200 OK" } else { "409 Conflict" };
    json_response(status, &with_api(&resp))
}

/// A read endpoint that forwards one IPC request and returns its JSON payload.
async fn api_json(server: &Server, request: IpcRequest) -> Vec<u8> {
    let resp = forward(&server.ipc, request).await;
    json_response(
        if resp.ok { "200 OK" } else { "409 Conflict" },
        &with_api(&resp),
    )
}

/// `GET /api/invite/qr` → the current ticket rendered as an SVG QR code.
async fn api_invite_qr(server: &Server) -> Vec<u8> {
    let resp = forward(&server.ipc, IpcRequest::Invite).await;
    let ticket = resp
        .data
        .as_ref()
        .and_then(|d| d.get("ticket"))
        .and_then(|t| t.as_str())
        .unwrap_or_default();
    match render_qr_svg(ticket) {
        Some(svg) => http_response(
            "200 OK",
            "image/svg+xml",
            "default-src 'none'; style-src 'unsafe-inline'",
            svg.as_bytes(),
        ),
        None => json_response(
            "409 Conflict",
            &serde_json::json!({"api": 1, "ok": false,
                "error": {"code": "no_ticket", "message": "no invite available"}}),
        ),
    }
}

fn render_qr_svg(ticket: &str) -> Option<String> {
    if ticket.is_empty() {
        return None;
    }
    use qrcode::QrCode;
    use qrcode::render::svg;
    let code = QrCode::new(ticket.as_bytes()).ok()?;
    Some(
        code.render::<svg::Color>()
            .min_dimensions(220, 220)
            .quiet_zone(true)
            .dark_color(svg::Color("#0b0f14"))
            .light_color(svg::Color("#e6edf3"))
            .build(),
    )
}

async fn forward(ipc: &IpcTx, request: IpcRequest) -> IpcResponse {
    let (tx, rx) = oneshot::channel();
    if ipc.send((request, tx)).await.is_err() {
        return IpcResponse::err("shutting_down", "daemon is shutting down");
    }
    rx.await
        .unwrap_or_else(|_| IpcResponse::err("internal", "daemon dropped the request"))
}

/// Wraps an [`IpcResponse`] into the stable `api:1` envelope the UI consumes.
fn with_api(resp: &IpcResponse) -> serde_json::Value {
    let mut out = serde_json::json!({ "api": 1, "ok": resp.ok });
    if let Some(data) = &resp.data {
        out["data"] = data.clone();
    }
    if let Some(err) = &resp.error {
        out["error"] = serde_json::json!({ "code": err.code, "message": err.message });
    }
    out
}

/// Constant-time token check from the `X-Tazamun-Token` header or `?token=`.
fn token_ok(server: &Server, req: &Request) -> bool {
    let provided = req
        .headers
        .get("x-tazamun-token")
        .cloned()
        .or_else(|| {
            req.query
                .split('&')
                .find_map(|kv| kv.strip_prefix("token=").map(str::to_string))
        })
        .unwrap_or_default();
    let a = provided.as_bytes();
    let b = server.token.as_bytes();
    a.len() == b.len() && a.ct_eq(b).into()
}

fn host_ok(headers: &HashMap<String, String>, port: u16) -> bool {
    let Some(host) = headers.get("host") else {
        return false;
    };
    let host = host.trim();
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
        || host == format!("127.0.0.1:{port}")
        || host == format!("localhost:{port}")
        || host == format!("[::1]:{port}")
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn nonce() -> String {
    let bytes: [u8; 16] = rand::random();
    data_encoding::HEXLOWER.encode(&bytes)
}

/// Serves the embedded single-file UI with a per-response CSP nonce.
fn index_response() -> Vec<u8> {
    let n = nonce();
    let html = INDEX_HTML.replace("__NONCE__", &n);
    let csp = format!(
        "default-src 'none'; script-src 'nonce-{n}'; style-src 'nonce-{n}'; \
         img-src 'self' data:; connect-src 'self'; base-uri 'none'; \
         form-action 'none'; frame-ancestors 'none'"
    );
    http_response("200 OK", "text/html; charset=utf-8", &csp, html.as_bytes())
}

fn json_response(status: &str, value: &serde_json::Value) -> Vec<u8> {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    http_response(
        status,
        "application/json",
        "default-src 'none'; frame-ancestors 'none'; base-uri 'none'",
        &body,
    )
}

fn http_response(status: &str, content_type: &str, csp: &str, body: &[u8]) -> Vec<u8> {
    let mut head = String::new();
    head.push_str(&format!("HTTP/1.1 {status}\r\n"));
    head.push_str(&format!("Content-Type: {content_type}\r\n"));
    head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    head.push_str(&format!("Content-Security-Policy: {csp}\r\n"));
    head.push_str("X-Content-Type-Options: nosniff\r\n");
    head.push_str("X-Frame-Options: DENY\r\n");
    head.push_str("Referrer-Policy: no-referrer\r\n");
    head.push_str("Cache-Control: no-store\r\n");
    head.push_str("Connection: close\r\n\r\n");
    let mut out = head.into_bytes();
    out.extend_from_slice(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(token: &str) -> Server {
        let (tx, _rx) = mpsc::channel(1);
        Server {
            ipc: tx,
            token: token.to_string(),
            port: 8787,
        }
    }

    fn req_with(headers: &[(&str, &str)], query: &str) -> Request {
        Request {
            method: "POST".into(),
            path: "/api/lock".into(),
            query: query.into(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), v.to_string()))
                .collect(),
            body: Vec::new(),
        }
    }

    #[test]
    fn token_required_and_constant_time_checked() {
        let s = server("abc123");
        // Correct token via header.
        assert!(token_ok(
            &s,
            &req_with(&[("X-Tazamun-Token", "abc123")], "")
        ));
        // Correct token via query.
        assert!(token_ok(&s, &req_with(&[], "token=abc123")));
        // Wrong token, missing token, and wrong length all fail.
        assert!(!token_ok(
            &s,
            &req_with(&[("X-Tazamun-Token", "abc124")], "")
        ));
        assert!(!token_ok(&s, &req_with(&[], "")));
        assert!(!token_ok(&s, &req_with(&[("X-Tazamun-Token", "abc")], "")));
    }

    #[test]
    fn host_allowlist_blocks_dns_rebinding() {
        let ok = |h: &str| {
            host_ok(
                &[("host".to_string(), h.to_string())].into_iter().collect(),
                8787,
            )
        };
        assert!(ok("127.0.0.1:8787"));
        assert!(ok("localhost:8787"));
        assert!(ok("127.0.0.1"));
        assert!(ok("[::1]:8787"));
        // A rebound attacker hostname is refused.
        assert!(!ok("evil.example.com"));
        assert!(!ok("evil.example.com:8787"));
        assert!(!ok("127.0.0.1:9999")); // wrong port
        // Missing Host is refused.
        assert!(!host_ok(&HashMap::new(), 8787));
    }

    #[test]
    fn index_sets_csp_nonce_and_no_frame() {
        let bytes = index_response();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("Content-Security-Policy: default-src 'none'"));
        assert!(text.contains("script-src 'nonce-"));
        assert!(text.contains("X-Frame-Options: DENY"));
        // The served body carries the same nonce the header declared.
        let header_nonce = text
            .split("script-src 'nonce-")
            .nth(1)
            .and_then(|s| s.split('\'').next())
            .unwrap();
        assert!(
            text.matches(header_nonce).count() >= 2,
            "nonce must be injected into the page"
        );
        // The placeholder is fully substituted.
        assert!(!text.contains("__NONCE__"));
    }

    #[test]
    fn api_envelope_shape() {
        let ok = with_api(&IpcResponse::ok(serde_json::json!({"x": 1})));
        assert_eq!(ok["api"], 1);
        assert_eq!(ok["ok"], true);
        assert_eq!(ok["data"]["x"], 1);
        let err = with_api(&IpcResponse::err("lease_held", "held by peer"));
        assert_eq!(err["ok"], false);
        assert_eq!(err["error"]["code"], "lease_held");
    }

    #[test]
    fn mutation_parsers_validate_input() {
        assert!(matches!(
            parse_path_op(br#"{"path":"a.txt"}"#, PathOp::Lock),
            Ok(IpcRequest::Lock { .. })
        ));
        assert!(parse_path_op(b"{}", PathOp::Lock).is_err());
        assert!(matches!(
            parse_restore(br#"{"path":"a","n":2}"#),
            Ok(IpcRequest::Restore { n: 2, .. })
        ));
        assert!(parse_restore(br#"{"path":"a"}"#).is_err());
        assert!(matches!(
            parse_config(br#"{"key":"autolock","value":"on"}"#),
            Ok(IpcRequest::ConfigSet { .. })
        ));
        assert!(parse_config(b"not json").is_err());
    }

    #[test]
    fn qr_svg_renders_for_a_ticket() {
        let svg = render_qr_svg("tzm1abcdef").expect("qr renders");
        assert!(svg.contains("<svg"));
        assert!(render_qr_svg("").is_none());
    }
}

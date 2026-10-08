//! Generic HTTP fetch API for sandboxed plugins.
//!
//! Rhai plugins run with no I/O capabilities by construction. A plugin that
//! declares `permissions = ["network.fetch"]` in its `plugin.toml` is granted
//! a small, hardened HTTP surface — completely generic, nothing here knows
//! about any particular upstream service:
//!
//! ```text
//! http_get(url)                          -> #{status, body, content_type, error}
//! http_get(url, headers)                 -> ditto
//! http_post(url, headers, body)          -> ditto
//! http_request(method, url, headers, body, timeout_secs) -> ditto
//! ```
//!
//! On transport failure `status` is `0` and `error` explains why; scripts are
//! expected to fall back gracefully (no exceptions are raised).
//!
//! Hardening (all enforced host-side, beyond script control):
//! - only `http` / `https` URLs, bounded length;
//! - **SSRF guard**: the target host must resolve to public addresses —
//!   loopback, private, link-local, shared/benchmark/documentation, multicast
//!   and IPv4-mapped ranges are rejected before any connection is made;
//! - redirects are never followed;
//! - header names/values are validated (no CRLF injection, no `Host` /
//!   `Content-Length` / `Connection` overrides);
//! - request and response bodies are size-capped, the timeout is clamped
//!   and requests run via `block_in_place` so the async runtime keeps
//!   scheduling while a fetch is in flight.
//!
//! Known limitation (documented trade-off): the SSRF guard resolves the host
//! once and the connection resolves it again — a hostile authoritative DNS
//! can in principle rebind between the two (TOCTOU). Full mitigation requires
//! connecting to the validated socket address directly, which ureq 2.x does
//! not support with correct TLS/SNI; plugin authors are admin-vetted, so the
//! guard is treated as best-effort hardening.

use std::io::Read;
use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;

use rhai::{Dynamic, Engine, Map};

/// Manifest permission required before `http_*` functions are registered.
pub const PERMISSION: &str = "network.fetch";

const MAX_URL_LEN: usize = 2048;
const MAX_BODY_OUT: usize = 1024 * 1024;
const MAX_BODY_IN: usize = 1024 * 1024;
const DEFAULT_TIMEOUT_SECS: u64 = 10;
const MAX_TIMEOUT_SECS: u64 = 30;
const MAX_HEADERS: usize = 32;

/// Register the `http_*` host functions on a plugin engine. Called only when
/// the plugin manifest declares [`PERMISSION`].
pub fn register(engine: &mut Engine) {
    engine.register_fn("http_get", |url: &str| -> Dynamic {
        request("GET", url, None, "", DEFAULT_TIMEOUT_SECS as i64)
    });
    engine.register_fn("http_get", |url: &str, headers: Map| -> Dynamic {
        request("GET", url, Some(headers), "", DEFAULT_TIMEOUT_SECS as i64)
    });
    engine.register_fn(
        "http_post",
        |url: &str, headers: Map, body: &str| -> Dynamic {
            request(
                "POST",
                url,
                Some(headers),
                body,
                DEFAULT_TIMEOUT_SECS as i64,
            )
        },
    );
    engine.register_fn(
        "http_request",
        |method: &str, url: &str, headers: Map, body: &str, timeout_secs: i64| -> Dynamic {
            request(method, url, Some(headers), body, timeout_secs)
        },
    );
}

fn err_map(status: i64, error: &str) -> Dynamic {
    let mut m = Map::new();
    m.insert("status".into(), Dynamic::from(status));
    m.insert("body".into(), Dynamic::from(String::new()));
    m.insert("content_type".into(), Dynamic::from(String::new()));
    m.insert("error".into(), Dynamic::from(error.to_string()));
    Dynamic::from(m)
}

fn ok_map(status: i64, body: String, content_type: String) -> Dynamic {
    let mut m = Map::new();
    m.insert("status".into(), Dynamic::from(status));
    m.insert("body".into(), Dynamic::from(body));
    m.insert("content_type".into(), Dynamic::from(content_type));
    m.insert("error".into(), Dynamic::from(String::new()));
    Dynamic::from(m)
}

/// One validated, SSRF-guarded request. Never panics; every failure mode is
/// reported as `{status: 0, error: …}`.
fn request(
    method: &str,
    url: &str,
    headers: Option<Map>,
    body: &str,
    timeout_secs: i64,
) -> Dynamic {
    let method = method.trim().to_ascii_uppercase();
    if !matches!(
        method.as_str(),
        "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD"
    ) {
        return err_map(0, "unsupported method");
    }

    let url = url.trim();
    if url.is_empty() || url.len() > MAX_URL_LEN {
        return err_map(0, "invalid url");
    }
    let Some((_scheme, host, port)) = parse_url(url) else {
        return err_map(0, "invalid url (only http/https is allowed)");
    };

    let mut hdrs: Vec<(String, String)> = Vec::new();
    if let Some(map) = headers {
        for (k, v) in map.iter().take(MAX_HEADERS) {
            let key = k.to_string();
            let val = v.clone().try_cast::<String>().unwrap_or_default();
            if valid_header(&key, &val) {
                hdrs.push((key, val));
            }
        }
    }
    let body = truncate_utf8(body, MAX_BODY_OUT).to_string();
    let timeout = timeout_secs.clamp(1, MAX_TIMEOUT_SECS as i64) as u64;
    let url = url.to_string();

    run_blocking(move || {
        // DNS resolution is blocking I/O — keep it inside run_blocking so a
        // slow resolver cannot stall an async worker thread.
        if !resolves_public(&host, port) {
            return err_map(0, "host is not a public address");
        }
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(timeout))
            .redirects(0)
            .user_agent(concat!("polaris-plugin-http/", env!("CARGO_PKG_VERSION")))
            .build();
        let mut req = agent.request(&method, &url);
        for (k, v) in &hdrs {
            req = req.set(k, v);
        }
        let outcome = if matches!(method.as_str(), "POST" | "PUT" | "PATCH") && !body.is_empty() {
            req.send_string(&body)
        } else {
            req.call()
        };
        match outcome {
            Ok(resp) => read_response(resp),
            Err(ureq::Error::Status(_, resp)) => read_response(resp),
            Err(ureq::Error::Transport(t)) => err_map(0, &format!("request failed: {t}")),
        }
    })
}

/// Byte-length cap that never splits a UTF-8 character (a plain `&s[..max]`
/// panics when the boundary lands inside a multi-byte sequence).
fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Consume a response into `(status, body, content_type)` with a hard cap on
/// the body size. A read failure mid-body is reported honestly instead of
/// handing the plugin a silently truncated 200.
fn read_response(resp: ureq::Response) -> Dynamic {
    let status = resp.status() as i64;
    let content_type = resp.content_type().to_string();
    let mut reader = resp.into_reader().take((MAX_BODY_IN + 1) as u64);
    let mut buf = Vec::new();
    let read_result = reader.read_to_end(&mut buf);
    let truncated = buf.len() > MAX_BODY_IN;
    buf.truncate(MAX_BODY_IN);
    if let Err(e) = read_result {
        return err_map(status, &format!("body read failed: {e}"));
    }
    let mut body = String::from_utf8_lossy(&buf).into_owned();
    if truncated {
        body.push_str("\n…[truncated]");
    }
    ok_map(status, body, content_type)
}

/// Parse `scheme://[user@]host[:port]/…` into `(scheme, host, port)`.
fn parse_url(url: &str) -> Option<(String, String, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    // Userinfo (anything before the last '@') is stripped — credentials in
    // URLs are not supported and confuse host extraction.
    let hostport = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);
    let default_port: u16 = if scheme == "https" { 443 } else { 80 };
    let (host, port) = if let Some(inner) = hostport.strip_prefix('[') {
        // IPv6 literal: [::1]:8080
        let close = inner.find(']')?;
        let host = &inner[..close];
        let after = &inner[close + 1..];
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse::<u16>().ok()?,
            None if after.is_empty() => default_port,
            None => return None,
        };
        (host, port)
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) => (h, p.parse::<u16>().ok()?),
            None => (hostport, default_port),
        }
    };
    if host.is_empty()
        || host
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return None;
    }
    Some((scheme, host.to_string(), port))
}

/// Resolve `host:port` and require every resolved address to be public.
fn resolves_public(host: &str, port: u16) -> bool {
    match format!("{host}:{port}").to_socket_addrs() {
        Ok(mut addrs) => addrs.all(|a| ip_allowed(a.ip())),
        Err(_) => false,
    }
}

/// Public-address check for the SSRF guard.
fn ip_allowed(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || (o[0] == 100 && (o[1] & 0xC0) == 64) // 100.64.0.0/10 CGNAT
                || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24
                || (o[0] == 198 && (o[1] & 0xFE) == 18) // 198.18.0.0/15 benchmark
                || o[0] >= 224) // multicast + reserved
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return ip_allowed(IpAddr::V4(mapped));
            }
            let seg = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || (seg[0] & 0xFE00) == 0xFC00 // fc00::/7 unique local
                || (seg[0] & 0xFFC0) == 0xFE80 // fe80::/10 link local
                || (seg[0] & 0xFF00) == 0xFF00 // multicast
                || (seg[0] == 0x2001 && seg[1] == 0x0DB8)) // documentation
        }
    }
}

/// Header names must be plain tokens, values free of CR/LF/NUL, and a small
/// set of hop-by-hop / connection-level fields may not be overridden.
fn valid_header(key: &str, value: &str) -> bool {
    if key.is_empty()
        || key.len() > 64
        || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return false;
    }
    if matches!(
        key.to_ascii_lowercase().as_str(),
        "host" | "content-length" | "connection" | "transfer-encoding" | "upgrade"
    ) {
        return false;
    }
    value.len() <= 4096 && !value.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0)
}

/// Run a blocking operation without stalling the async runtime: inside the
/// multi-thread server runtime the worker is handed back to the scheduler
/// for the duration (same pattern as `jobs.rs`); outside a runtime the call
/// simply runs.
fn run_blocking<T>(f: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_of(d: Dynamic) -> i64 {
        d.try_cast::<Map>()
            .expect("result must be a map")
            .get("status")
            .and_then(|v| v.clone().try_cast::<i64>())
            .expect("status must be an int")
    }

    fn error_of(d: Dynamic) -> String {
        d.try_cast::<Map>()
            .expect("result must be a map")
            .get("error")
            .and_then(|v| v.clone().try_cast::<String>())
            .expect("error must be a string")
    }

    #[test]
    fn parse_url_shapes() {
        assert_eq!(
            parse_url("https://api.github.com/repos/x/y"),
            Some(("https".into(), "api.github.com".into(), 443))
        );
        assert_eq!(
            parse_url("http://example.com"),
            Some(("http".into(), "example.com".into(), 80))
        );
        assert_eq!(
            parse_url("http://example.com:8080/a?b#c"),
            Some(("http".into(), "example.com".into(), 8080))
        );
        assert_eq!(
            parse_url("HTTP://Example.com/"),
            Some(("http".into(), "Example.com".into(), 80))
        );
        assert_eq!(
            parse_url("http://user:pass@example.com/").unwrap().1,
            "example.com"
        );
        assert!(parse_url("ftp://example.com/").is_none());
        assert!(parse_url("example.com").is_none());
        assert!(parse_url("http://").is_none());
        assert!(parse_url("http://example.com:notaport/").is_none());
        assert!(parse_url("http://[::1]:8080/").is_some());
    }

    #[test]
    fn ssrf_guard_blocks_internal_targets() {
        assert!(ip_allowed("8.8.8.8".parse().unwrap()));
        assert!(ip_allowed("2606:4700::1111".parse().unwrap()));
        let blocked = [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.9",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.1.1",
            "0.0.0.0",
            "100.64.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "::1",
            "::",
            "fe80::1",
            "fd00::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
        ];
        for bad in blocked {
            assert!(!ip_allowed(bad.parse().unwrap()), "must block {bad}");
        }
    }

    #[test]
    fn request_rejects_bad_input_without_io() {
        for url in [
            "",
            "   ",
            "ftp://example.com/",
            "notaurl",
            "http://127.0.0.1:9/x",
        ] {
            let d = request("GET", url, None, "", 1);
            assert_eq!(status_of(d.clone()), 0, "url: {url}");
            assert!(!error_of(d).is_empty());
        }
        assert_eq!(
            status_of(request("TRACE", "https://example.com/", None, "", 1)),
            0
        );
    }

    #[test]
    fn truncate_utf8_never_splits_characters() {
        // 1MB of multi-byte characters: the byte cap lands mid-character.
        let body = "你".repeat(600_000); // 1_800_000 bytes
        let cut = truncate_utf8(&body, MAX_BODY_OUT);
        assert!(cut.len() <= MAX_BODY_OUT);
        assert!(cut.is_char_boundary(cut.len()));
        assert!(std::panic::catch_unwind(|| truncate_utf8(&body, MAX_BODY_OUT)).is_ok());
        // Short bodies pass through untouched.
        assert_eq!(truncate_utf8("hello", MAX_BODY_OUT), "hello");
        assert_eq!(truncate_utf8("", 10), "");
    }

    #[test]
    fn header_validation() {
        assert!(valid_header("Authorization", "Bearer abc"));
        assert!(valid_header("Accept", "application/vnd.github+json"));
        assert!(!valid_header("X-Evil", "a\r\nX-Injected: 1"));
        assert!(!valid_header("X-Nul", "a\0b"));
        assert!(!valid_header("Host", "evil.com"));
        assert!(!valid_header("Content-Length", "10"));
        assert!(!valid_header("Connection", "close"));
        assert!(!valid_header("bad name", "v"));
        assert!(!valid_header("", "v"));
    }
}

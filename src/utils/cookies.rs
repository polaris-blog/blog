use axum::http::header::{HeaderMap, HeaderValue};

/// Extract a cookie value from request headers.
pub fn get_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(axum::http::header::COOKIE)?;
    let raw = raw.to_str().ok()?;
    for pair in raw.split(';') {
        let pair = pair.trim();
        if let Some((k, v)) = pair.split_once('=')
            && k.trim() == name
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

/// Build a `Set-Cookie` header value.
pub fn set_cookie(
    name: &str,
    value: &str,
    max_age_secs: i64,
    path: &str,
    http_only: bool,
    secure: bool,
) -> HeaderValue {
    let mut s = format!("{name}={value}; Path={path}; SameSite=Lax");
    // `0` means "expire now" and must still be emitted, otherwise the cookie
    // survives as a session cookie with an emptied value.
    if max_age_secs >= 0 {
        s.push_str(&format!("; Max-Age={max_age_secs}"));
    }
    if http_only {
        s.push_str("; HttpOnly");
    }
    if secure {
        s.push_str("; Secure");
    }
    HeaderValue::from_str(&s).expect("valid cookie header")
}

/// Random hex token of `bytes` random bytes (cryptographically secure).
pub fn random_token(bytes: usize) -> String {
    use argon2::password_hash::rand_core::{OsRng, RngCore};
    let mut buf = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Constant-time string comparison (avoids timing side channels on tokens).
pub fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

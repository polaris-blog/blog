//! Crypto & encoding helpers for sandboxed plugins — pure functions, always
//! available (no permission gate): nothing here touches the network, the
//! filesystem or process state.
//!
//! Primary use cases: webhook signing (`hmac_sha256_hex`), HTTP Basic auth
//! and data URIs (`base64_encode`/`base64_decode`), URL building
//! (`url_encode`), integrity checks (`sha256_hex`).

use rhai::Engine;
use sha2::{Digest, Sha256};

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// SHA-256 digest of a UTF-8 string, hex-encoded.
pub fn sha256_hex(data: &str) -> String {
    let mut h = Sha256::new();
    h.update(data.as_bytes());
    hex(&h.finalize())
}

/// HMAC-SHA256 (RFC 2104), hex-encoded — the standard webhook-signature
/// scheme (`X-Signature: sha256=<hex>`). The key is raw bytes (the Rhai
/// binding passes the UTF-8 bytes of the string argument).
pub fn hmac_sha256_hex(key: &[u8], data: &str) -> String {
    let mut key_material = [0u8; 64];
    if key.len() > 64 {
        let mut h = Sha256::new();
        h.update(key);
        key_material[..32].copy_from_slice(&h.finalize());
    } else {
        key_material[..key.len()].copy_from_slice(key);
    }

    let mut inner = Sha256::new();
    for b in &key_material {
        inner.update([b ^ 0x36]);
    }
    inner.update(data.as_bytes());
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    for b in &key_material {
        outer.update([b ^ 0x5c]);
    }
    outer.update(inner_hash);
    hex(&outer.finalize())
}

/// Base64 (RFC 4648, with padding) of a UTF-8 string.
pub fn base64_encode(data: &str) -> String {
    let bytes = data.as_bytes();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Base64 decode of a UTF-8 string; invalid input decodes to an empty string
/// (whitespace and padding are tolerated).
pub fn base64_decode(data: &str) -> String {
    let mut buf = Vec::with_capacity(data.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for ch in data.bytes() {
        let v = match ch {
            b'A'..=b'Z' => u32::from(ch - b'A'),
            b'a'..=b'z' => u32::from(ch - b'a') + 26,
            b'0'..=b'9' => u32::from(ch - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\r' | b'\n' | b' ' | b'\t' => continue,
            _ => return String::new(), // invalid alphabet — decode to empty
        };
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            buf.push(((acc >> bits) & 0xFF) as u8);
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Percent-encode a string for safe inclusion in a URL component: every byte
/// that is not an RFC 3986 unreserved character becomes `%XX`.
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Register the crypto/encoding helpers on a plugin engine.
pub fn register(engine: &mut Engine) {
    engine.register_fn("sha256_hex", sha256_hex);
    engine.register_fn("hmac_sha256_hex", |key: &str, data: &str| {
        hmac_sha256_hex(key.as_bytes(), data)
    });
    engine.register_fn("base64_encode", base64_encode);
    engine.register_fn("base64_decode", base64_decode);
    engine.register_fn("url_encode", url_encode);
}

/// Register `sign_hex(data)` — an HMAC bound to a **plugin-scoped** key
/// derived as `HMAC(instance_secret, "plugin-signing:<plugin>")`. Scripts can
/// sign and later verify their own tokens (challenge clearances, webhook
/// payloads) without ever seeing the instance secret or another plugin's key.
pub fn register_signing(engine: &mut Engine, plugin_name: &str, instance_secret: &str) {
    let key = hmac_sha256_hex(
        instance_secret.as_bytes(),
        &format!("plugin-signing:{plugin_name}"),
    );
    engine.register_fn("sign_hex", move |data: &str| -> String {
        hmac_sha256_hex(key.as_bytes(), data)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn hmac_sha256_rfc4231_vectors() {
        // RFC 4231 test case 1 and 2.
        assert_eq!(
            hmac_sha256_hex(&[0x0b; 20], "Hi There"),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            hmac_sha256_hex(b"Jefe", "what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // A key longer than the block size is hashed first (RFC 4231 case 6:
        // 131 bytes of 0xaa, cross-checked against Python's hmac module).
        assert_eq!(
            hmac_sha256_hex(
                &[0xaa; 131],
                "Test Using Larger Than Block-Size Key - Hash Key First"
            ),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
        // ASCII long key, cross-checked against Python's hmac module.
        assert_eq!(
            hmac_sha256_hex(
                b"a".repeat(131).as_slice(),
                "This is a test using a larger than block-size key and a larger than block-size data. The key needs to be hashed before being used."
            ),
            "a94a2fd391b00e0ddfc0056f15800819ce5f369994ee9b24e94c5f95b5bd9c56"
        );
    }

    #[test]
    fn base64_rfc4648_vectors() {
        assert_eq!(base64_encode(""), "");
        assert_eq!(base64_encode("f"), "Zg==");
        assert_eq!(base64_encode("fo"), "Zm8=");
        assert_eq!(base64_encode("foo"), "Zm9v");
        assert_eq!(base64_encode("foob"), "Zm9vYg==");
        assert_eq!(base64_encode("fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode("foobar"), "Zm9vYmFy");
        for src in [
            "",
            "f",
            "fo",
            "foo",
            "foob",
            "fooba",
            "foobar",
            "p@ss wоrd/+",
        ] {
            assert_eq!(base64_decode(&base64_encode(src)), src);
        }
        // Whitespace and padding are tolerated; invalid alphabet → empty.
        assert_eq!(base64_decode("Zm9v\nYmFy"), "foobar");
        assert_eq!(base64_decode("!!!"), "");
    }

    #[test]
    fn url_encode_rfc3986() {
        assert_eq!(url_encode("hello"), "hello");
        assert_eq!(url_encode("a b&c=d/e"), "a%20b%26c%3Dd%2Fe");
        assert_eq!(url_encode("-._~"), "-._~");
    }
}

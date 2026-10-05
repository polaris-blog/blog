/// FNV-1a 64-bit hash — used for lightweight ETags (not cryptographic).
pub fn fnv1a64(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Build an ETag header value (strong, quoted) for a response body.
pub fn etag(data: &[u8]) -> String {
    format!("\"{:016x}\"", fnv1a64(data))
}

#[cfg(test)]
mod tests {
    #[test]
    fn stable() {
        assert_eq!(super::fnv1a64(b"polaris"), super::fnv1a64(b"polaris"));
        assert_ne!(super::fnv1a64(b"polaris"), super::fnv1a64(b"polaris2"));
    }
}

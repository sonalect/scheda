//! FNV-1a 64-bit for collection directory names.
//!
//! Offset basis `0xcbf29ce484222325`, prime `0x100000001b3` (Fowler–Noll–Vo
//! FNV-1a). Output is sixteen lowercase hex digits of the digest of UTF-8
//! name bytes. Not a cryptographic hash; enough to keep a Vault's collection
//! directories distinct.

pub fn hex(bytes: &[u8]) -> String {
    format!("{:016x}", digest(bytes))
}

pub(crate) fn digest(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    #[test]
    fn matches_published_fnv1a_64_vectors() {
        assert_eq!(super::hex(b""), "cbf29ce484222325");
        assert_eq!(super::hex(b"a"), "af63dc4c8601ec8c");
        assert_eq!(super::hex(b"foobar"), "85944171f73967e8");
    }
}

//! FNV-1a 64-bit for ANN document keys (`search`), not collection paths.
//!
//! Offset basis `0xcbf29ce484222325`, prime `0x100000001b3` (Fowler–Noll–Vo
//! FNV-1a). Not a cryptographic hash.

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
        let hex = |bytes: &[u8]| format!("{:016x}", super::digest(bytes));
        assert_eq!(hex(b""), "cbf29ce484222325");
        assert_eq!(hex(b"a"), "af63dc4c8601ec8c");
        assert_eq!(hex(b"foobar"), "85944171f73967e8");
    }
}

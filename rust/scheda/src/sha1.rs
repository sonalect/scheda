//! SHA-1, for the trailer of a pack: the checksum git puts at the end of a
//! pack file over everything before it. The library's indexer verifies it,
//! and no object is named by it here, so this is an integrity checksum, not
//! a defence against an attacker.

/// An incremental SHA-1.
pub(crate) struct Sha1 {
    state: [u32; 5],
    block: [u8; 64],
    filled: usize,
    length: u64,
}

impl Sha1 {
    /// A hash of nothing yet.
    pub(crate) fn new() -> Sha1 {
        Sha1 {
            state: [
                0x6745_2301,
                0xEFCD_AB89,
                0x98BA_DCFE,
                0x1032_5476,
                0xC3D2_E1F0,
            ],
            block: [0; 64],
            filled: 0,
            length: 0,
        }
    }

    /// Adds `bytes` to the hash.
    pub(crate) fn update(&mut self, mut bytes: &[u8]) {
        self.length = self.length.wrapping_add(bytes.len() as u64);
        if self.filled > 0 {
            let take = (64 - self.filled).min(bytes.len());
            self.block[self.filled..self.filled + take].copy_from_slice(&bytes[..take]);
            self.filled += take;
            bytes = &bytes[take..];
            if self.filled < 64 {
                return;
            }
            let block = self.block;
            compress(&mut self.state, &block);
            self.filled = 0;
        }
        let (blocks, rest) = bytes.as_chunks::<64>();
        for block in blocks {
            compress(&mut self.state, block);
        }
        self.block[..rest.len()].copy_from_slice(rest);
        self.filled = rest.len();
    }

    /// The 20 bytes of the hash of everything added.
    pub(crate) fn finish(mut self) -> [u8; 20] {
        let bits = self.length.wrapping_mul(8);
        let pad = if self.filled < 56 { 56 } else { 120 } - self.filled;
        let mut tail = [0u8; 72];
        tail[0] = 0x80;
        tail[pad..pad + 8].copy_from_slice(&bits.to_be_bytes());
        // `update` would count the padding in `length`; the bits are taken.
        let length = self.length;
        self.update(&tail[..pad + 8]);
        self.length = length;
        let mut out = [0u8; 20];
        for (chunk, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.state) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

fn compress(state: &mut [u32; 5], block: &[u8; 64]) {
    let mut w = [0u32; 80];
    for (word, bytes) in w.iter_mut().zip(block.as_chunks::<4>().0) {
        *word = u32::from_be_bytes(*bytes);
    }
    for i in 16..80 {
        w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }
    let [mut a, mut b, mut c, mut d, mut e] = *state;
    for (i, word) in w.iter().enumerate() {
        let (f, k) = match i {
            0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
            20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
            40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
            _ => (b ^ c ^ d, 0xCA62_C1D6),
        };
        let next = a
            .rotate_left(5)
            .wrapping_add(f)
            .wrapping_add(e)
            .wrapping_add(k)
            .wrapping_add(*word);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = next;
    }
    for (slot, add) in state.iter_mut().zip([a, b, c, d, e]) {
        *slot = slot.wrapping_add(add);
    }
}

#[cfg(test)]
mod tests {
    use git2::{ObjectType, Oid};

    use super::Sha1;

    /// git hashes a blob as SHA-1 of `blob {len}\0` and the content, so the
    /// library gives a reference for any input.
    fn reference(content: &[u8]) -> [u8; 20] {
        let id = Oid::hash_object(ObjectType::Blob, content).unwrap();
        let mut out = [0u8; 20];
        out.copy_from_slice(id.as_bytes());
        out
    }

    fn hash_blob(content: &[u8], pieces: usize) -> [u8; 20] {
        let mut sha = Sha1::new();
        sha.update(format!("blob {}\0", content.len()).as_bytes());
        for piece in content.chunks(pieces.max(1)) {
            sha.update(piece);
        }
        sha.finish()
    }

    #[test]
    fn the_hash_is_the_one_git_computes_whatever_the_pieces() {
        for len in [0usize, 1, 55, 56, 57, 63, 64, 65, 119, 120, 128, 1000, 4096] {
            let content: Vec<u8> = (0..len).map(|i| (i * 7 + 3) as u8).collect();
            for pieces in [1usize, 7, 64, 1000, 5000] {
                assert_eq!(
                    hash_blob(&content, pieces),
                    reference(&content),
                    "{len} bytes in pieces of {pieces}"
                );
            }
        }
    }
}

//! Streaming MD5 (RFC 1321), SHA-1 and SHA-2 (FIPS 180-4).
//! All arithmetic and padding live here; the guest API only owns lifecycle.

#[derive(Clone)]
pub(crate) struct Digest {
    algorithm: u32,
    state: [u64; 8],
    pending: Vec<u8>,
    length: u128,
}
impl Digest {
    pub(crate) fn new(algorithm: u32) -> Option<Self> {
        let state = match algorithm {
            0x8003 => [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0, 0, 0, 0],
            0x8004 => [
                0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0, 0, 0, 0,
            ],
            0x800c => [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            0x800d => [
                0xcbbb9d5dc1059ed8,
                0x629a292a367cd507,
                0x9159015a3070dd17,
                0x152fecd8f70e5939,
                0x67332667ffc00b31,
                0x8eb44a8768581511,
                0xdb0c2e0d64f98fa7,
                0x47b5481dbefa4fa4,
            ],
            0x800e => [
                0x6a09e667f3bcc908,
                0xbb67ae8584caa73b,
                0x3c6ef372fe94f82b,
                0xa54ff53a5f1d36f1,
                0x510e527fade682d1,
                0x9b05688c2b3e6c1f,
                0x1f83d9abfb41bd6b,
                0x5be0cd19137e2179,
            ],
            _ => return None,
        };
        Some(Self {
            algorithm,
            state,
            pending: Vec::new(),
            length: 0,
        })
    }
    pub(super) fn algorithm(&self) -> u32 {
        self.algorithm
    }
    pub(super) fn block_size(&self) -> usize {
        if matches!(self.algorithm, 0x800d | 0x800e) {
            128
        } else {
            64
        }
    }
    pub(super) fn size(&self) -> usize {
        match self.algorithm {
            0x8003 => 16,
            0x8004 => 20,
            0x800c => 32,
            0x800d => 48,
            _ => 64,
        }
    }
    pub(crate) fn update(&mut self, mut bytes: &[u8]) {
        self.length = self.length.wrapping_add(bytes.len() as u128);
        let block = self.block_size();
        if !self.pending.is_empty() {
            let take = bytes.len().min(block - self.pending.len());
            self.pending.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.pending.len() == block {
                let input = std::mem::take(&mut self.pending);
                self.compress(&input);
            }
        }
        let mut chunks = bytes.chunks_exact(block);
        for chunk in &mut chunks {
            self.compress(chunk);
        }
        self.pending.extend_from_slice(chunks.remainder());
    }
    fn compress(&mut self, block: &[u8]) {
        match self.algorithm {
            0x8003 => md5(&mut self.state, block),
            0x8004 => sha1(&mut self.state, block),
            0x800c => sha256(&mut self.state, block),
            _ => sha512(&mut self.state, block),
        }
    }
    pub(crate) fn finish(&self) -> Vec<u8> {
        let mut value = self.clone();
        let block = self.block_size();
        let length_bytes = if block == 128 { 16 } else { 8 };
        let mut padding = vec![0x80];
        padding.resize(
            1 + (block - (self.pending.len() + 1 + length_bytes) % block) % block,
            0,
        );
        let bits = self.length.wrapping_mul(8);
        if block == 128 {
            padding.extend_from_slice(&bits.to_be_bytes());
        } else if self.algorithm == 0x8003 {
            padding.extend_from_slice(&(bits as u64).to_le_bytes());
        } else {
            padding.extend_from_slice(&(bits as u64).to_be_bytes());
        }
        value.update(&padding);
        let mut output = Vec::new();
        for word in value.state {
            if block == 128 {
                output.extend_from_slice(&word.to_be_bytes());
            } else if self.algorithm == 0x8003 {
                output.extend_from_slice(&(word as u32).to_le_bytes());
            } else {
                output.extend_from_slice(&(word as u32).to_be_bytes());
            }
        }
        output.truncate(self.size());
        output
    }
}
fn accumulate32(state: &mut [u64; 8], work: &[u32]) {
    for (dst, &word) in state.iter_mut().zip(work) {
        *dst = u64::from((*dst as u32).wrapping_add(word));
    }
}
fn md5(state: &mut [u64; 8], block: &[u8]) {
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];
    const S: [[u32; 4]; 4] = [
        [7, 12, 17, 22],
        [5, 9, 14, 20],
        [4, 11, 16, 23],
        [6, 10, 15, 21],
    ];
    let words: Vec<_> = block
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| u32::from_le_bytes(*b))
        .collect();
    let [mut a, mut b, mut c, mut d] = [
        state[0] as u32,
        state[1] as u32,
        state[2] as u32,
        state[3] as u32,
    ];
    for i in 0..64 {
        let (f, g) = match i / 16 {
            0 => ((b & c) | (!b & d), i),
            1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
            2 => (b ^ c ^ d, (3 * i + 5) % 16),
            _ => (c ^ (b | !d), (7 * i) % 16),
        };
        let next = b.wrapping_add(
            a.wrapping_add(f)
                .wrapping_add(K[i])
                .wrapping_add(words[g])
                .rotate_left(S[i / 16][i % 4]),
        );
        (a, b, c, d) = (d, next, b, c);
    }
    accumulate32(state, &[a, b, c, d]);
}
fn sha1(state: &mut [u64; 8], block: &[u8]) {
    let mut w = [0u32; 80];
    for (dst, src) in w.iter_mut().zip(block.as_chunks::<4>().0.iter()) {
        *dst = u32::from_be_bytes(*src);
    }
    for i in 16..80 {
        w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }
    let [mut a, mut b, mut c, mut d, mut e] = std::array::from_fn(|i| state[i] as u32);
    for (i, word) in w.into_iter().enumerate() {
        let (f, k) = match i / 20 {
            0 => ((b & c) | (!b & d), 0x5a827999),
            1 => (b ^ c ^ d, 0x6ed9eba1),
            2 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdc),
            _ => (b ^ c ^ d, 0xca62c1d6),
        };
        let next = a
            .rotate_left(5)
            .wrapping_add(f)
            .wrapping_add(e)
            .wrapping_add(k)
            .wrapping_add(word);
        (a, b, c, d, e) = (next, a, b.rotate_left(30), c, d);
    }
    accumulate32(state, &[a, b, c, d, e]);
}
const K256: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];
fn sha256(state: &mut [u64; 8], block: &[u8]) {
    let mut w = [0u32; 64];
    for (dst, src) in w.iter_mut().zip(block.as_chunks::<4>().0.iter()) {
        *dst = u32::from_be_bytes(*src);
    }
    for i in 16..64 {
        let x = w[i - 15];
        let y = w[i - 2];
        w[i] = w[i - 16]
            .wrapping_add(x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3))
            .wrapping_add(w[i - 7])
            .wrapping_add(y.rotate_right(17) ^ y.rotate_right(19) ^ (y >> 10));
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state.map(|v| v as u32);
    for (word, k) in w.into_iter().zip(K256) {
        let t = h
            .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
            .wrapping_add((e & f) ^ (!e & g))
            .wrapping_add(k)
            .wrapping_add(word);
        let u = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
            .wrapping_add((a & b) ^ (a & c) ^ (b & c));
        (a, b, c, d, e, f, g, h) = (t.wrapping_add(u), a, b, c, d.wrapping_add(t), e, f, g);
    }
    accumulate32(state, &[a, b, c, d, e, f, g, h]);
}
const K512: [u64; 80] = [
    0x428a2f98d728ae22,
    0x7137449123ef65cd,
    0xb5c0fbcfec4d3b2f,
    0xe9b5dba58189dbbc,
    0x3956c25bf348b538,
    0x59f111f1b605d019,
    0x923f82a4af194f9b,
    0xab1c5ed5da6d8118,
    0xd807aa98a3030242,
    0x12835b0145706fbe,
    0x243185be4ee4b28c,
    0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f,
    0x80deb1fe3b1696b1,
    0x9bdc06a725c71235,
    0xc19bf174cf692694,
    0xe49b69c19ef14ad2,
    0xefbe4786384f25e3,
    0x0fc19dc68b8cd5b5,
    0x240ca1cc77ac9c65,
    0x2de92c6f592b0275,
    0x4a7484aa6ea6e483,
    0x5cb0a9dcbd41fbd4,
    0x76f988da831153b5,
    0x983e5152ee66dfab,
    0xa831c66d2db43210,
    0xb00327c898fb213f,
    0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2,
    0xd5a79147930aa725,
    0x06ca6351e003826f,
    0x142929670a0e6e70,
    0x27b70a8546d22ffc,
    0x2e1b21385c26c926,
    0x4d2c6dfc5ac42aed,
    0x53380d139d95b3df,
    0x650a73548baf63de,
    0x766a0abb3c77b2a8,
    0x81c2c92e47edaee6,
    0x92722c851482353b,
    0xa2bfe8a14cf10364,
    0xa81a664bbc423001,
    0xc24b8b70d0f89791,
    0xc76c51a30654be30,
    0xd192e819d6ef5218,
    0xd69906245565a910,
    0xf40e35855771202a,
    0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8,
    0x1e376c085141ab53,
    0x2748774cdf8eeb99,
    0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63,
    0x4ed8aa4ae3418acb,
    0x5b9cca4f7763e373,
    0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc,
    0x78a5636f43172f60,
    0x84c87814a1f0ab72,
    0x8cc702081a6439ec,
    0x90befffa23631e28,
    0xa4506cebde82bde9,
    0xbef9a3f7b2c67915,
    0xc67178f2e372532b,
    0xca273eceea26619c,
    0xd186b8c721c0c207,
    0xeada7dd6cde0eb1e,
    0xf57d4f7fee6ed178,
    0x06f067aa72176fba,
    0x0a637dc5a2c898a6,
    0x113f9804bef90dae,
    0x1b710b35131c471b,
    0x28db77f523047d84,
    0x32caab7b40c72493,
    0x3c9ebe0a15c9bebc,
    0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6,
    0x597f299cfc657e2a,
    0x5fcb6fab3ad6faec,
    0x6c44198c4a475817,
];
fn sha512(state: &mut [u64; 8], block: &[u8]) {
    let mut w = [0u64; 80];
    for (dst, src) in w.iter_mut().zip(block.as_chunks::<8>().0.iter()) {
        *dst = u64::from_be_bytes(*src);
    }
    for i in 16..80 {
        let x = w[i - 15];
        let y = w[i - 2];
        w[i] = w[i - 16]
            .wrapping_add(x.rotate_right(1) ^ x.rotate_right(8) ^ (x >> 7))
            .wrapping_add(w[i - 7])
            .wrapping_add(y.rotate_right(19) ^ y.rotate_right(61) ^ (y >> 6));
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for (word, k) in w.into_iter().zip(K512) {
        let t = h
            .wrapping_add(e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41))
            .wrapping_add((e & f) ^ (!e & g))
            .wrapping_add(k)
            .wrapping_add(word);
        let u = (a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39))
            .wrapping_add((a & b) ^ (a & c) ^ (b & c));
        (a, b, c, d, e, f, g, h) = (t.wrapping_add(u), a, b, c, d.wrapping_add(t), e, f, g);
    }
    for (dst, word) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *dst = dst.wrapping_add(word);
    }
}

#[cfg(test)]
mod tests;

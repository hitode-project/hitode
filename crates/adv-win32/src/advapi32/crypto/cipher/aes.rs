//! Rijndael's 128-bit block transform and 128/192/256-bit schedules (FIPS 197).
#[derive(Clone)]
pub(in crate::advapi32::crypto) struct Aes {
    keys: Vec<[u8; 16]>,
}
fn multiply(mut a: u8, mut b: u8) -> u8 {
    let mut result = 0;
    for _ in 0..8 {
        if b & 1 != 0 {
            result ^= a;
        }
        a = (a << 1) ^ if a & 0x80 != 0 { 0x1b } else { 0 };
        b >>= 1;
    }
    result
}
fn substitute(a: u8) -> u8 {
    let mut inverse = 0;
    if a != 0 {
        inverse = 1;
        let mut power = a;
        let mut exponent = 254;
        while exponent != 0 {
            if exponent & 1 != 0 {
                inverse = multiply(inverse, power);
            }
            power = multiply(power, power);
            exponent >>= 1;
        }
    }
    inverse
        ^ inverse.rotate_left(1)
        ^ inverse.rotate_left(2)
        ^ inverse.rotate_left(3)
        ^ inverse.rotate_left(4)
        ^ 0x63
}
fn inverse_substitute(a: u8) -> u8 {
    // Invert the affine transform, then invert in GF(2^8).
    let b = a.rotate_left(1) ^ a.rotate_left(3) ^ a.rotate_left(6) ^ 0x05;
    if b == 0 {
        return 0;
    }
    let mut inverse = 1;
    let mut power = b;
    let mut exponent = 254;
    while exponent != 0 {
        if exponent & 1 != 0 {
            inverse = multiply(inverse, power);
        }
        power = multiply(power, power);
        exponent >>= 1;
    }
    inverse
}
impl Aes {
    pub(super) fn new(key: &[u8]) -> Option<Self> {
        let nk = key.len() / 4;
        if !matches!(key.len(), 16 | 24 | 32) {
            return None;
        }
        let rounds = nk + 6;
        let mut words: Vec<[u8; 4]> = key.as_chunks::<4>().0.to_vec();
        let mut rc = 1;
        for i in nk..4 * (rounds + 1) {
            let mut word = words[i - 1];
            if i % nk == 0 {
                word.rotate_left(1);
                word = word.map(substitute);
                word[0] ^= rc;
                rc = multiply(rc, 2);
            } else if nk > 6 && i % nk == 4 {
                word = word.map(substitute);
            }
            words.push(std::array::from_fn(|j| words[i - nk][j] ^ word[j]));
        }
        let keys = words
            .as_chunks::<4>()
            .0
            .iter()
            .map(|words| std::array::from_fn(|i| words[i / 4][i % 4]))
            .collect();
        Some(Self { keys })
    }
    pub(super) fn transform(&self, bytes: &mut [u8], decrypt: bool) {
        let mut state: [u8; 16] = bytes.try_into().unwrap();
        let rounds = self.keys.len() - 1;
        add_key(&mut state, &self.keys[if decrypt { rounds } else { 0 }]);
        for round in 1..=rounds {
            if decrypt {
                shift(&mut state, true);
                state = state.map(inverse_substitute);
                add_key(&mut state, &self.keys[rounds - round]);
                if round != rounds {
                    mix(&mut state, true);
                }
            } else {
                state = state.map(substitute);
                shift(&mut state, false);
                if round != rounds {
                    mix(&mut state, false);
                }
                add_key(&mut state, &self.keys[round]);
            }
        }
        bytes.copy_from_slice(&state);
    }
}
fn add_key(state: &mut [u8; 16], key: &[u8; 16]) {
    for (a, b) in state.iter_mut().zip(key) {
        *a ^= b;
    }
}
fn shift(state: &mut [u8; 16], inverse: bool) {
    let old = *state;
    for row in 0..4 {
        for column in 0..4 {
            state[4 * column + row] =
                old[4 * ((column + if inverse { 4 - row } else { row }) % 4) + row];
        }
    }
}
fn mix(state: &mut [u8; 16], inverse: bool) {
    let first = if inverse {
        [14, 11, 13, 9]
    } else {
        [2, 3, 1, 1]
    };
    for column in state.as_chunks_mut::<4>().0.iter_mut() {
        let old = *column;
        for (row, out) in column.iter_mut().enumerate() {
            *out = (0..4).fold(0, |sum, j| sum ^ multiply(old[j], first[(j + 4 - row) % 4]));
        }
    }
}

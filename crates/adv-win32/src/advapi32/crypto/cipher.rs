//! Software symmetric primitives. Standard block transforms are independent of
//! CryptoAPI's CBC chaining, padding and key/handle ownership.
mod aes;
mod des;
mod rc2;

#[derive(Clone)]
pub(super) enum Cipher {
    HmacOnly,
    Aes(aes::Aes),
    Des(des::Des),
    TripleDes(Box<[des::Des; 3]>),
    Rc2(rc2::Rc2),
    Rc4 {
        permutation: Box<[u8; 256]>,
        i: u8,
        j: u8,
    },
}
impl Cipher {
    pub(super) fn new(algorithm: u32, key: &[u8], effective: u32) -> Option<Self> {
        Some(match algorithm {
            0x660e..=0x6610 if key.len() == 16 + 8 * (algorithm - 0x660e) as usize => {
                Self::Aes(aes::Aes::new(key)?)
            }
            0x6601 if key.len() == 8 => Self::Des(des::Des::new(key)),
            0x6603 if key.len() == 24 => Self::TripleDes(Box::new([
                des::Des::new(&key[..8]),
                des::Des::new(&key[8..16]),
                des::Des::new(&key[16..]),
            ])),
            0x6609 if key.len() == 16 => Self::TripleDes(Box::new([
                des::Des::new(&key[..8]),
                des::Des::new(&key[8..]),
                des::Des::new(&key[..8]),
            ])),
            0x6602 => Self::Rc2(rc2::Rc2::new(key, effective)?),
            0x6801 if !key.is_empty() && key.len() <= 256 => {
                let mut permutation = Box::new(std::array::from_fn(|i| i as u8));
                let mut j = 0u8;
                for i in 0..256 {
                    j = j
                        .wrapping_add(permutation[i])
                        .wrapping_add(key[i % key.len()]);
                    permutation.swap(i, j as usize);
                }
                Self::Rc4 {
                    permutation,
                    i: 0,
                    j: 0,
                }
            }
            _ => return None,
        })
    }
    pub(super) fn block_size(&self) -> usize {
        match self {
            Self::Aes(_) => 16,
            Self::Rc4 { .. } | Self::HmacOnly => 1,
            _ => 8,
        }
    }
    pub(super) fn transform(&mut self, bytes: &mut [u8], decrypt: bool) {
        match self {
            Self::HmacOnly => unreachable!("HMAC-only keys cannot encrypt"),
            Self::Aes(cipher) => cipher.transform(bytes, decrypt),
            Self::Des(cipher) => cipher.transform(bytes, decrypt),
            Self::TripleDes(keys) => {
                let [a, b, c] = keys.as_ref();
                if decrypt {
                    c.transform(bytes, true);
                    b.transform(bytes, false);
                    a.transform(bytes, true);
                } else {
                    a.transform(bytes, false);
                    b.transform(bytes, true);
                    c.transform(bytes, false);
                }
            }
            Self::Rc2(cipher) => cipher.transform(bytes, decrypt),
            Self::Rc4 { permutation, i, j } => {
                for byte in bytes {
                    *i = i.wrapping_add(1);
                    *j = j.wrapping_add(permutation[*i as usize]);
                    permutation.swap(*i as usize, *j as usize);
                    *byte ^= permutation
                        [permutation[*i as usize].wrapping_add(permutation[*j as usize]) as usize];
                }
            }
        }
    }
}
#[cfg(test)]
mod tests;

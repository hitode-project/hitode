//! HMAC (RFC 2104), including CryptoAPI's selectable digest and pad strings.
use super::*;
#[derive(Clone)]
pub(super) struct Hmac {
    key: Vec<u8>,
    outer: digest::Digest,
}
impl Hmac {
    pub(super) fn new(
        key: Vec<u8>,
        algorithm: u32,
        inner: Option<Vec<u8>>,
        outer: Option<Vec<u8>>,
    ) -> Option<(Self, digest::Digest)> {
        let mut digest = digest::Digest::new(algorithm)?;
        let block = digest.block_size();
        let normalized = if key.len() > block {
            let mut h = digest.clone();
            h.update(&key);
            h.finish()
        } else {
            key.clone()
        };
        let mut inner = inner.unwrap_or_else(|| vec![0x36; block]);
        let mut outer = outer.unwrap_or_else(|| vec![0x5c; block]);
        for (dst, src) in inner.iter_mut().zip(&normalized) {
            *dst ^= src;
        }
        for (dst, src) in outer.iter_mut().zip(&normalized) {
            *dst ^= src;
        }
        digest.update(&inner);
        let mut outer_digest = digest::Digest::new(algorithm)?;
        outer_digest.update(&outer);
        Some((
            Self {
                key,
                outer: outer_digest,
            },
            digest,
        ))
    }
    pub(super) fn finish(&self, digest: &digest::Digest) -> Vec<u8> {
        let mut outer = self.outer.clone();
        outer.update(&digest.finish());
        outer.finish()
    }
    pub(super) fn key(&self) -> Vec<u8> {
        self.key.clone()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // spec: SYS-CRYPTO-4
    fn rfc_hmac_known_answers() {
        for (algorithm, key, message, expected) in [
            (
                0x8003,
                vec![0x0b; 16],
                b"Hi There".to_vec(),
                "9294727a3638bb1c13f48ef8158bfc9d",
            ),
            (
                0x8004,
                vec![0x0b; 20],
                b"Hi There".to_vec(),
                "b617318655057264e28bc0b6fb378c8ef146be00",
            ),
            (
                0x800c,
                vec![0x0b; 20],
                b"Hi There".to_vec(),
                "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
            ),
            (
                0x800d,
                vec![0x0b; 20],
                b"Hi There".to_vec(),
                "afd03944d84895626b0825f4ab46907f15f9dadbe4101ec682aa034c7cebc59cfaea9ea9076ede7f4af152e8b2fa9cb6",
            ),
            (
                0x800e,
                vec![0x0b; 20],
                b"Hi There".to_vec(),
                "87aa7cdea5ef619d4ff0b4241a1d6cb02379f4e2ce4ec2787ad0b30545e17cde daa833b7d6b8a702038b274eaea3f4e4be9d914eeb61f1702e696c203a126854",
            ),
            (
                0x800c,
                vec![0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First".to_vec(),
                "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54",
            ),
        ] {
            let (hmac, mut digest) = Hmac::new(key, algorithm, None, None).unwrap();
            for byte in message {
                digest.update(&[byte]);
            }
            let actual = hmac
                .finish(&digest)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            assert_eq!(actual, expected.replace(' ', ""), "{algorithm:x}");
        }
    }
}

use super::*;
fn bytes(text: &str) -> Vec<u8> {
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}
fn aes(mode: u32) -> Key {
    let mut key = construct(
        Arc::new(Provider {
            kind: 24,
            name: "Microsoft Enhanced RSA and AES Cryptographic Provider",
            machine: false,
        }),
        0x660e,
        bytes("2b7e151628aed2a6abf7158809cf4f3c"),
        128,
        1,
    )
    .unwrap();
    key.iv = bytes("000102030405060708090a0b0c0d0e0f");
    key.chain = key.iv.clone();
    key.mode = mode;
    key
}
#[test]
// spec: SYS-CRYPTO-3
fn nist_chaining_vectors_split_calls_and_final_reset() {
    // NIST SP 800-38A F.2.1 (CBC) and F.3.1 (CFB8).
    let plain = bytes(
        "6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c3710",
    );
    let expected = bytes(
        "7649abac8119b246cee98e9b12e9197d5086cb9b507219ee95db113a917678b273bed6b8e3c1743b7116e69e222295163ff1caa1681fac09120eca307586e1a7",
    );
    let mut key = aes(1);
    assert_eq!(
        apply(&mut key, plain.clone(), false, false).unwrap(),
        expected
    );
    let mut key = aes(1);
    let mut split = apply(&mut key, plain[..16].to_vec(), false, false).unwrap();
    split.extend(apply(&mut key, plain[16..].to_vec(), false, false).unwrap());
    assert_eq!(split, expected);
    let mut key = aes(1);
    assert_eq!(apply(&mut key, expected, false, true).unwrap(), plain);
    let expected = bytes("3b79424c9c0dd436bace9e0ed4586a4f");
    assert_eq!(
        apply(&mut aes(4), plain[..16].to_vec(), false, false).unwrap(),
        expected
    );
    assert_eq!(
        apply(&mut aes(4), expected, false, true).unwrap(),
        plain[..16]
    );
    let mut key = aes(1);
    let first = apply(&mut key, b"abc".to_vec(), true, false).unwrap();
    assert_eq!(first.len(), 16);
    assert_eq!(
        apply(&mut key, b"abc".to_vec(), true, false).unwrap(),
        first
    );
    assert_eq!(apply(&mut key, first.clone(), true, true).unwrap(), b"abc");
    assert_eq!(apply(&mut key, first, true, true).unwrap(), b"abc");
    assert_eq!(apply(&mut key, Vec::new(), true, false).unwrap().len(), 16);
}
#[test]
// spec: SYS-CRYPTO-4
fn microsoft_csp_non_sha2_expansion_and_des_parity() {
    // The independent expected values apply the six documented CryptDeriveKey steps.
    for (algorithm, hash, expected) in [
        (0x660e, 0x8003, "31b77b68d64862ca4b088e2ea98a126c"),
        (
            0x660f,
            0x8003,
            "31b77b68d64862ca4b088e2ea98a126ca80f884f2556f620",
        ),
        (
            0x6610,
            0x8003,
            "31b77b68d64862ca4b088e2ea98a126ca80f884f2556f620c7ed87be158d0eee",
        ),
        (
            0x6603,
            0x8004,
            "642aec57ea4ffe8f10c71acd73a732dcd5df7a08670d2f32",
        ),
        (0x6609, 0x8004, "642aec57ea4ffe8f10c71acd73a732dc"),
    ] {
        let mut digest = digest::Digest::new(hash).unwrap();
        digest.update(b"Hi There");
        let base = digest.finish();
        let mut material = expand(&base, hash, algorithm);
        material.truncate(bytes(expected).len());
        let key = construct(
            Arc::new(Provider {
                kind: 24,
                name: "Microsoft Enhanced RSA and AES Cryptographic Provider",
                machine: false,
            }),
            algorithm,
            material,
            128,
            1,
        )
        .unwrap();
        assert_eq!(key.material, bytes(expected));
    }
}

use super::Cipher;
fn bytes(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap())
        .collect()
}
#[test]
// spec: SYS-CRYPTO-3
fn nist_and_rfc_symmetric_known_answers() {
    // FIPS 197 Appendix C; DES canonical FIPS example; RFC 2268 section 5;
    // RFC 6229's 40-bit RC4 stream. Encryption and decryption use known answers.
    for (algorithm, key, bits, plain, ciphertext) in [
        (
            0x660e,
            "000102030405060708090a0b0c0d0e0f",
            128,
            "00112233445566778899aabbccddeeff",
            "69c4e0d86a7b0430d8cdb78070b4c55a",
        ),
        (
            0x660f,
            "000102030405060708090a0b0c0d0e0f1011121314151617",
            192,
            "00112233445566778899aabbccddeeff",
            "dda97ca4864cdfe06eaf70a0ec0d7191",
        ),
        (
            0x6610,
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            256,
            "00112233445566778899aabbccddeeff",
            "8ea2b7ca516745bfeafc49904b496089",
        ),
        (
            0x6601,
            "133457799bbcdff1",
            56,
            "0123456789abcdef",
            "85e813540f0ab405",
        ),
        (
            0x6602,
            "0000000000000000",
            63,
            "0000000000000000",
            "ebb773f993278eff",
        ),
        (
            0x6602,
            "ffffffffffffffff",
            64,
            "ffffffffffffffff",
            "278b27e42e2f0d49",
        ),
        (
            0x6602,
            "3000000000000000",
            64,
            "1000000000000001",
            "30649edf9be7d2c2",
        ),
        (0x6602, "88", 64, "0000000000000000", "61a8a244adacccf0"),
        (
            0x6602,
            "88bca90e90875a",
            64,
            "0000000000000000",
            "6ccf4308974c267f",
        ),
        (
            0x6602,
            "88bca90e90875a7f0f79c384627bafb2",
            64,
            "0000000000000000",
            "1a807d272bbe5db1",
        ),
        (
            0x6602,
            "88bca90e90875a7f0f79c384627bafb2",
            128,
            "0000000000000000",
            "2269552ab0f85ca6",
        ),
        (
            0x6801,
            "0102030405",
            40,
            "00000000000000000000000000000000",
            "b2396305f03dc027ccc3524a0a1118a8",
        ),
        (
            0x6603,
            "0123456789abcdef23456789abcdef01456789abcdef0123",
            168,
            "6bc1bee22e409f96",
            "714772f339841d34",
        ),
        (
            0x6609,
            "0123456789abcdeffedcba9876543210",
            112,
            "6bc1bee22e409f96",
            "ea43f9aa27f82f59",
        ),
    ] {
        let mut cipher = Cipher::new(algorithm, &bytes(key), bits).unwrap();
        let mut input = bytes(plain);
        cipher.transform(&mut input, false);
        assert_eq!(input, bytes(ciphertext), "encrypt {algorithm:x}/{bits}");
        let mut cipher = Cipher::new(algorithm, &bytes(key), bits).unwrap();
        let mut input = bytes(ciphertext);
        cipher.transform(&mut input, true);
        assert_eq!(input, bytes(plain), "decrypt {algorithm:x}/{bits}");
    }
}

use super::Digest;
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
// spec: PERS-CRYPTO-1
fn published_digest_vectors_and_streaming_padding_boundaries() {
    let vectors = [
        (0x8003, 0, "d41d8cd98f00b204e9800998ecf8427e"),
        (0x8003, 3, "900150983cd24fb0d6963f7d28e17f72"),
        (0x8003, 55, "ef1772b6dff9a122358552954ad0df65"),
        (0x8003, 56, "3b0c8ac703f828b04c6c197006d17218"),
        (0x8003, 63, "b06521f39153d618550606be297466d5"),
        (0x8003, 64, "014842d480b571495a4a0363793f7367"),
        (0x8003, 111, "089f243d1e831c5879aa375ee364a06e"),
        (0x8003, 112, "9146ef3527c7cfcc66dc615c3986e391"),
        (0x8003, 127, "020406e1d05cdc2aa287641f7ae2cc39"),
        (0x8003, 128, "e510683b3f5ffe4093d021808bc6ff70"),
        (0x8003, 1000000, "7707d6ae4e027c70eea2a935c2296f21"),
        (0x8004, 0, "da39a3ee5e6b4b0d3255bfef95601890afd80709"),
        (0x8004, 3, "a9993e364706816aba3e25717850c26c9cd0d89d"),
        (0x8004, 55, "c1c8bbdc22796e28c0e15163d20899b65621d65a"),
        (0x8004, 56, "c2db330f6083854c99d4b5bfb6e8f29f201be699"),
        (0x8004, 63, "03f09f5b158a7a8cdad920bddc29b81c18a551f5"),
        (0x8004, 64, "0098ba824b5c16427bd7a1122a5a442a25ec644d"),
        (0x8004, 111, "ac877859d427d9192054eea8feb3b8a403ef83a5"),
        (0x8004, 112, "689993727ba37386bb032495e9dbdfb4dd1ba744"),
        (0x8004, 127, "89d95fa32ed44a7c610b7ee38517ddf57e0bb975"),
        (0x8004, 128, "ad5b3fdbcb526778c2839d2f151ea753995e26a0"),
        (0x8004, 1000000, "34aa973cd4c4daa4f61eeb2bdbad27316534016f"),
        (
            0x800c,
            0,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            0x800c,
            3,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            0x800c,
            55,
            "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318",
        ),
        (
            0x800c,
            56,
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a",
        ),
        (
            0x800c,
            63,
            "7d3e74a05d7db15bce4ad9ec0658ea98e3f06eeecf16b4c6fff2da457ddc2f34",
        ),
        (
            0x800c,
            64,
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb",
        ),
        (
            0x800c,
            111,
            "6374f73208854473827f6f6a3f43b1f53eaa3b82c21c1a6d69a2110b2a79baad",
        ),
        (
            0x800c,
            112,
            "f54353008a2553262ecdc4a34749563ba0950e8b0fc8652780b0a614b99683c1",
        ),
        (
            0x800c,
            127,
            "c57e9278af78fa3cab38667bef4ce29d783787a2f731d4e12200270f0c32320a",
        ),
        (
            0x800c,
            128,
            "6836cf13bac400e9105071cd6af47084dfacad4e5e302c94bfed24e013afb73e",
        ),
        (
            0x800c,
            1000000,
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0",
        ),
        (
            0x800d,
            0,
            "38b060a751ac96384cd9327eb1b1e36a21fdb71114be07434c0cc7bf63f6e1da274edebfe76f65fbd51ad2f14898b95b",
        ),
        (
            0x800d,
            3,
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7",
        ),
        (
            0x800d,
            55,
            "5d91ac7e74e62b5c728904b40f10784d66b7af9cb6302123e48c92f0432ceb8d2a92c02de77dcb29ed75c4b42bde46f4",
        ),
        (
            0x800d,
            56,
            "8a8d9649ea04e993a6ca7135af7e3392cc5fca84f8531cac7aa3feed4eb98f55dcbe0f3284b61c6f35f98b02cc644b4c",
        ),
        (
            0x800d,
            63,
            "7e7f097e95b52bb8f53383450ecaf9868187c130981730c03b6d573adfc0b991e365244e5b4bfa082bfb43c517d37120",
        ),
        (
            0x800d,
            64,
            "2e404b9339da795776e510d96930b3be2904c500395b8cb7413334b82d4dec413b4b8113045a05bbbcff846f027423f6",
        ),
        (
            0x800d,
            111,
            "3c37955051cb5c3026f94d551d5b5e2ac38d572ae4e07172085fed81f8466b8f90dc23a8ffcdea0b8d8e58e8fdacc80a",
        ),
        (
            0x800d,
            112,
            "187d4e07cb306103c69967bf544d0dfbe9042577599c73c330abc0cb64c61236d5ed565ee19119d8c31779a38f791fcd",
        ),
        (
            0x800d,
            127,
            "9bd06b1763c2cf7aef40e795dc65bc96d59c41b537f3ad72ebdefd485476b5717c1aeb37c327fe9c1831b12b9efd08ae",
        ),
        (
            0x800d,
            128,
            "edb12730a366098b3b2beac75a3bef1b0969b15c48e2163c23d96994f8d1bef760c7e27f3c464d3829f56c0d53808b0b",
        ),
        (
            0x800d,
            1000000,
            "9d0e1809716474cb086e834e310a4a1ced149e9c00f248527972cec5704c2a5b07b8b3dc38ecc4ebae97ddd87f3d8985",
        ),
        (
            0x800e,
            0,
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e",
        ),
        (
            0x800e,
            3,
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
        ),
        (
            0x800e,
            55,
            "b0220c772cbf6c1822e2cb38a437d0e1d58772417a4bbb21c961364f8b6143e05aa6316dca8d1d7b19e16448419076395f6086cb55101fbd6d5497b148e1745f",
        ),
        (
            0x800e,
            56,
            "962b64aae357d2a4fee3ded8b539bdc9d325081822b0bfc55583133aab44f18bafe11d72a7ae16c79ce2ba620ae2242d5144809161945f1367f41b3972e26e04",
        ),
        (
            0x800e,
            63,
            "c1b0f5c6d3b03dfe4a2602e67242f54e344090b66e01100a469b129f583f016c7e27dddeaa438393dcc7ec54b0b57c9ba7af007f9b56db5f6fb677d972a31362",
        ),
        (
            0x800e,
            64,
            "01d35c10c6c38c2dcf48f7eebb3235fb5ad74a65ec4cd016e2354c637a8fb49b695ef3c1d6f7ae4cd74d78cc9c9bcac9d4f23a73019998a7f73038a5c9b2dbde",
        ),
        (
            0x800e,
            111,
            "fa9121c7b32b9e01733d034cfc78cbf67f926c7ed83e82200ef86818196921760b4beff48404df811b953828274461673c68d04e297b0eb7b2b4d60fc6b566a2",
        ),
        (
            0x800e,
            112,
            "c01d080efd492776a1c43bd23dd99d0a2e626d481e16782e75d54c2503b5dc32bd05f0f1ba33e568b88fd2d970929b719ecbb152f58f130a407c8830604b70ca",
        ),
        (
            0x800e,
            127,
            "828613968b501dc00a97e08c73b118aa8876c26b8aac93df128502ab360f91bab50a51e088769a5c1eff4782ace147dce3642554199876374291f5d921629502",
        ),
        (
            0x800e,
            128,
            "b73d1929aa615934e61a871596b3f3b33359f42b8175602e89f7e06e5f658a243667807ed300314b95cacdd579f3e33abdfbe351909519a846d465c59582f321",
        ),
        (
            0x800e,
            1000000,
            "e718483d0ce769644e2e42c7bc15b4638e1f98b13b2044285632a803afa973ebde0ff244877ea60a4cb0432ce577c31beb009c5c2c49aa2e4eadb217ad8cc09b",
        ),
    ];
    for (algorithm, length, expected) in vectors {
        let bytes = if length == 3 {
            b"abc".to_vec()
        } else {
            vec![b'a'; length]
        };
        for chunk in [1, 7, 64, 127, 1024] {
            let mut digest = Digest::new(algorithm).unwrap();
            for part in bytes.chunks(chunk) {
                digest.update(part);
            }
            assert_eq!(
                hex(&digest.finish()),
                expected,
                "{algorithm:x}, length={length}, chunk={chunk}"
            );
            assert_eq!(hex(&digest.finish()), expected); // Finalization of a copy preserves streaming state.
        }
    }
}

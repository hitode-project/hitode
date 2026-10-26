use super::*;

const STATE: u32 = DATA + 0x100;
const LCODES: u32 = DATA + 0x1000;
const DCODES: u32 = DATA + 0x2000;
const INPUT: u32 = DATA + 0x3000;
const OUTPUT: u32 = DATA + 0x4000;
const WINDOW: u32 = DATA + 0x10000;
const LENGTHS: [(usize, u32); 29] = [
    (3, 0),
    (4, 0),
    (5, 0),
    (6, 0),
    (7, 0),
    (8, 0),
    (9, 0),
    (10, 0),
    (11, 1),
    (13, 1),
    (15, 1),
    (17, 1),
    (19, 2),
    (23, 2),
    (27, 2),
    (31, 2),
    (35, 3),
    (43, 3),
    (51, 3),
    (59, 3),
    (67, 4),
    (83, 4),
    (99, 4),
    (115, 4),
    (131, 5),
    (163, 5),
    (195, 5),
    (227, 5),
    (258, 0),
];
const DISTANCES: [(usize, u32); 30] = [
    (1, 0),
    (2, 0),
    (3, 0),
    (4, 0),
    (5, 1),
    (7, 1),
    (9, 2),
    (13, 2),
    (17, 3),
    (25, 3),
    (33, 4),
    (49, 4),
    (65, 5),
    (97, 5),
    (129, 6),
    (193, 6),
    (257, 7),
    (385, 7),
    (513, 8),
    (769, 8),
    (1025, 9),
    (1537, 9),
    (2049, 10),
    (3073, 10),
    (4097, 11),
    (6145, 11),
    (8193, 12),
    (12289, 12),
    (16385, 13),
    (24577, 13),
];

fn lengths() -> (Vec<u16>, Vec<u16>) {
    let mut literals = vec![8; 288];
    literals[144..256].fill(9);
    literals[256..280].fill(7);
    let mut distances = vec![6; 32];
    distances[..8].fill(4);
    distances[8..16].fill(5);
    (literals, distances)
}
fn codes(lengths: &[u16]) -> Vec<(u32, u32)> {
    let mut count = [0; 16];
    for &len in lengths {
        count[len as usize] += 1;
    }
    let mut next = [0u32; 16];
    for n in 1..16 {
        next[n] = (next[n - 1] + count[n - 1]) << 1;
    }
    lengths
        .iter()
        .map(|&len| {
            let code = next[len as usize];
            next[len as usize] += 1;
            (code.reverse_bits() >> (32 - len), u32::from(len))
        })
        .collect()
}
#[derive(Clone, Copy)]
enum Token {
    Literal(u8),
    Match(usize, usize),
}
fn encoded(tokens: &[Token], initial: u32) -> (u32, Vec<u8>) {
    let (litlens, distlens) = lengths();
    let (literals, distances) = (codes(&litlens), codes(&distlens));
    let mut bits = Vec::new();
    let mut emit = |code: (u32, u32)| {
        for n in 0..code.1 {
            bits.push((code.0 >> n) as u8 & 1);
        }
    };
    for &token in tokens {
        match token {
            Token::Literal(byte) => emit(literals[byte as usize]),
            Token::Match(len, dist) => {
                let l = LENGTHS.iter().rposition(|&(base, _)| base <= len).unwrap();
                emit(literals[257 + l]);
                emit(((len - LENGTHS[l].0) as u32, LENGTHS[l].1));
                let d = DISTANCES
                    .iter()
                    .rposition(|&(base, _)| base <= dist)
                    .unwrap();
                emit(distances[d]);
                emit(((dist - DISTANCES[d].0) as u32, DISTANCES[d].1));
            }
        }
    }
    emit(literals[256]);
    bits.extend([0; 64]);
    let hold = bits[..initial as usize]
        .iter()
        .enumerate()
        .fold(0, |hold, (n, &bit)| hold | (u32::from(bit) << n));
    let bytes = bits[initial as usize..]
        .chunks(8)
        .map(|group| {
            group
                .iter()
                .enumerate()
                .fold(0, |byte, (n, &bit)| byte | (bit << n))
        })
        .collect();
    (hold, bytes)
}

// Use Wine itself to build both root and secondary tables. The independently
// authored canonical encoder above generates their literal/match bitstreams.
fn tables(reference: &mut Machine<Cpu>, pe: &Pe, secondary: bool) -> Vec<u8> {
    let table = reference.ext.get::<adv_nt::identity::Images>().0[&pe.image_base]
        .table()
        .unwrap();
    let entry = pe.image_base + table.named("inflate_table").unwrap().rva;
    prepare(reference, entry, 6, false);
    let mut data = vec![0x39; LEN as usize];
    let (literals, distances) = lengths();
    for (kind, lengths, address, root, field) in [
        (1, literals, LCODES, if secondary { 7 } else { 9 }, 88),
        (2, distances, DCODES, if secondary { 4 } else { 6 }, 92),
    ] {
        for (i, len) in lengths.iter().enumerate() {
            put(&mut data, DATA + 0x2d00 + 2 * i as u32, 2, u32::from(*len));
        }
        put(&mut data, DATA + 0x80, 4, address);
        put(&mut data, DATA + 0x84, 4, root);
        let result = call(
            reference,
            &data,
            &[
                kind,
                DATA + 0x2d00,
                lengths.len() as u32,
                DATA + 0x80,
                DATA + 0x84,
                DATA + 0x2a00,
            ],
        );
        assert_eq!(result.0.unwrap()[0], 0);
        data = result.1;
        let root = u32::from_le_bytes(data[0x84..0x88].try_into().unwrap());
        put(&mut data, STATE + field, 4, root);
    }
    put(&mut data, STATE + 80, 4, LCODES);
    put(&mut data, STATE + 84, 4, DCODES);
    data
}
fn compare(
    native: &mut Machine<Cpu>,
    reference: &mut Machine<Cpu>,
    data: &[u8],
    start: u32,
    accepted: bool,
    label: &str,
) {
    let before = native.sys.stats.leaf_abandons;
    let mut actual = call(native, data, &[DATA, start]);
    let mut expected = call(reference, data, &[DATA, start]);
    actual.0.as_mut().unwrap()[0] = 0; // void ABI
    expected.0.as_mut().unwrap()[0] = 0;
    assert_eq!(actual.0, expected.0, "{label}");
    assert!(
        actual.1 == expected.1,
        "{label}: first byte difference {:?}",
        actual.1.iter().zip(&expected.1).position(|(a, b)| a != b)
    );
    assert_eq!(
        native.sys.stats.leaf_abandons == before,
        accepted,
        "{label}: native acceptance"
    );
}

// spec: NAT-BODY-1, NAT-BODY-11, DEV-CODEC-2
#[test]
fn fast_batches_preserve_wine_state() {
    let Some(dist) = dist() else { return };
    for module in [
        "dbghelp.dll",
        "user32.dll",
        "wininet.dll",
        "windowscodecs.dll",
    ] {
        let (mut native, pe) = machine(&dist, module, true);
        let (mut reference, _) = machine(&dist, module, false);
        native.ext.insert(adv_nt::native::InflateFast::new(
            adv_device::codec_kernel::inflate_fast,
        ));
        for machine in [&mut native, &mut reference] {
            machine.cpu.map(WINDOW, 32768, Prot::RW).unwrap();
            let window: Vec<u8> = (0..32768).map(|i| (i as u8).wrapping_mul(37)).collect();
            machine.cpu.write(WINDOW, &window).unwrap();
        }
        let table = native.ext.get::<adv_nt::identity::Images>().0[&pe.image_base]
            .table()
            .unwrap();
        let entry = pe.image_base + table.named("inflate_fast").unwrap().rva;
        assert!(native.sys.redirects.contains_key(&entry));
        for secondary in [false, true] {
            let base = tables(&mut reference, &pe, secondary);
            for machine in [&mut native, &mut reference] {
                prepare(machine, entry, 2, false);
            }
            let bit_counts: &[u32] = if module == "windowscodecs.dll" {
                &[0, 1, 2, 3, 4, 5, 6, 7]
            } else {
                &[0]
            };
            for &initial in bit_counts {
                for (prefix, whave, next, distance) in [
                    (0, 0, 0, 0),
                    (1, 0, 0, 1),
                    (7, 0, 0, 7),
                    (300, 0, 0, 257),
                    (0, 8, 8, 7),
                    (0, 32768, 0, 32768),
                    (0, 32768, 1, 257),
                    (0, 32768, 32767, 258),
                    (7, 32768, 100, 32768),
                    (300, 32768, 32767, 301),
                ] {
                    let mut tokens: Vec<Token> = if distance == 0 {
                        (0..300)
                            .map(|i| Token::Literal((i as u8).wrapping_mul(19)))
                            .collect()
                    } else {
                        vec![Token::Match(258, distance); 35]
                    };
                    tokens.extend([
                        Token::Match(3, 1),
                        Token::Match(17, 2),
                        Token::Match(33, 3),
                        Token::Match(131, 4),
                        Token::Match(257, 5),
                    ]);
                    let (hold, bytes) = encoded(&tokens, initial);
                    for capacity in [258, 259, 511, 10240] {
                        for avail in [6, bytes.len() as u32] {
                            let mut data = base.clone();
                            let out = OUTPUT + prefix;
                            for (at, value) in [
                                (DATA, INPUT),
                                (DATA + 4, avail),
                                (DATA + 12, out),
                                (DATA + 16, capacity),
                                (DATA + 28, STATE),
                                (STATE + 4, 16200),
                                (STATE + 44, 32768),
                                (STATE + 48, whave),
                                (STATE + 52, next),
                                (STATE + 56, WINDOW),
                                (STATE + 60, hold),
                                (STATE + 64, initial),
                                (STATE + 7108, 1),
                            ] {
                                put(&mut data, at, 4, value);
                            }
                            data[(INPUT - DATA) as usize..(INPUT - DATA) as usize + bytes.len()]
                                .copy_from_slice(&bytes);
                            compare(
                                &mut native,
                                &mut reference,
                                &data,
                                capacity + prefix,
                                true,
                                &format!(
                                    "{module} secondary={secondary} bits={initial} prefix={prefix} window={whave}/{next} dist={distance} out={capacity} in={avail}"
                                ),
                            );
                        }
                    }
                }
            }
            // Exercise every length/distance extra-bit width at its upper edge.
            let tokens: Vec<Token> = LENGTHS
                .iter()
                .map(|&(base, extra)| Token::Match(base + (1 << extra) - 1, 1))
                .chain(
                    DISTANCES
                        .iter()
                        .map(|&(base, extra)| Token::Match(258, base + (1 << extra) - 1)),
                )
                .collect();
            let (hold, bytes) = encoded(&tokens, 7);
            let mut data = base.clone();
            for (at, value) in [
                (DATA, INPUT),
                (DATA + 4, bytes.len() as u32),
                (DATA + 12, OUTPUT),
                (DATA + 16, 16000),
                (DATA + 28, STATE),
                (STATE + 4, 16200),
                (STATE + 44, 32768),
                (STATE + 48, 32768),
                (STATE + 52, 17),
                (STATE + 56, WINDOW),
                (STATE + 60, hold),
                (STATE + 64, 7),
            ] {
                put(&mut data, at, 4, value);
            }
            data[0x3000..0x3000 + bytes.len()].copy_from_slice(&bytes);
            compare(
                &mut native,
                &mut reference,
                &data,
                16000,
                true,
                "all extra-bit widths",
            );
            // Invalid codes and unavailable history must reach Wine's own error
            // path with the original input, output and bit state intact.
            for invalid_code in [false, true] {
                let mut data = base.clone();
                let (hold, bytes) = encoded(&[Token::Literal(7), Token::Match(258, 32768)], 0);
                for (at, value) in [
                    (DATA, INPUT),
                    (DATA + 4, bytes.len() as u32),
                    (DATA + 12, OUTPUT),
                    (DATA + 16, 10240),
                    (DATA + 28, STATE),
                    (STATE + 4, 16200),
                    (STATE + 44, 32768),
                    (STATE + 48, 0),
                    (STATE + 52, 0),
                    (STATE + 56, WINDOW),
                    (STATE + 60, hold),
                    (STATE + 64, 0),
                    (STATE + 7108, 1),
                ] {
                    put(&mut data, at, 4, value);
                }
                data[0x3000..0x3000 + bytes.len()].copy_from_slice(&bytes);
                if invalid_code {
                    data[0x1000..0x1000 + 852 * 4]
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .for_each(|code| code.copy_from_slice(&[64, 1, 0, 0]));
                }
                compare(
                    &mut native,
                    &mut reference,
                    &data,
                    10240,
                    false,
                    "invalid stream",
                );
            }
        }
    }
}

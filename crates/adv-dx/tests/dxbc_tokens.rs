use adv_dx::shader::dxbc::{Body, Instruction, Program, Version, fields::*};

const FIXTURES: &[(&str, &[u8])] = &[
    ("vs_4_0", include_bytes!("fixtures/dxbc/vs_4_0.dxbc")),
    ("vs_4_1", include_bytes!("fixtures/dxbc/vs_4_1.dxbc")),
    ("vs_5_0", include_bytes!("fixtures/dxbc/vs_5_0.dxbc")),
    ("ps_4_0", include_bytes!("fixtures/dxbc/ps_4_0.dxbc")),
    ("ps_4_1", include_bytes!("fixtures/dxbc/ps_4_1.dxbc")),
    ("ps_5_0", include_bytes!("fixtures/dxbc/ps_5_0.dxbc")),
    ("gs_4_0", include_bytes!("fixtures/dxbc/gs_4_0.dxbc")),
    ("gs_4_1", include_bytes!("fixtures/dxbc/gs_4_1.dxbc")),
    ("gs_5_0", include_bytes!("fixtures/dxbc/gs_5_0.dxbc")),
    ("cs_4_0", include_bytes!("fixtures/dxbc/cs_4_0.dxbc")),
    ("cs_4_1", include_bytes!("fixtures/dxbc/cs_4_1.dxbc")),
    ("cs_5_0", include_bytes!("fixtures/dxbc/cs_5_0.dxbc")),
    ("hs_5_0", include_bytes!("fixtures/dxbc/hs_5_0.dxbc")),
    ("ds_5_0", include_bytes!("fixtures/dxbc/ds_5_0.dxbc")),
    (
        "vs_4_0_level_9_1",
        include_bytes!("fixtures/dxbc/vs_4_0_level_9_1.dxbc"),
    ),
    (
        "vs_4_0_level_9_3",
        include_bytes!("fixtures/dxbc/vs_4_0_level_9_3.dxbc"),
    ),
    (
        "ps_4_0_level_9_1",
        include_bytes!("fixtures/dxbc/ps_4_0_level_9_1.dxbc"),
    ),
    (
        "ps_4_0_level_9_3",
        include_bytes!("fixtures/dxbc/ps_4_0_level_9_3.dxbc"),
    ),
];

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

#[test]
fn fxc_token_streams_round_trip() {
    let mut custom = false;
    let mut extended = false;
    for &(profile, bytes) in FIXTURES {
        assert_eq!(&bytes[..4], b"DXBC");
        let offsets = (0..u32_at(bytes, 28)).map(|n| u32_at(bytes, 32 + n as usize * 4) as usize);
        let mut token_chunks = 0;
        let mut downlevel = false;
        for offset in offsets {
            let tag = &bytes[offset..offset + 4];
            downlevel |= tag == b"Aon9";
            if !matches!(tag, b"SHDR" | b"SHEX") {
                continue;
            }
            token_chunks += 1;
            let size = u32_at(bytes, offset + 4) as usize;
            let original = &bytes[offset + 8..offset + 8 + size];
            let tokens: Vec<_> = original.chunks(4).map(|word| u32_at(word, 0)).collect();
            let program =
                Program::decode(&tokens).unwrap_or_else(|err| panic!("{profile}: {err:?}"));
            let stage = match &profile[..2] {
                "ps" => D3D10_SB_PIXEL_SHADER,
                "vs" => D3D10_SB_VERTEX_SHADER,
                "gs" => D3D10_SB_GEOMETRY_SHADER,
                "hs" => D3D11_SB_HULL_SHADER,
                "ds" => D3D11_SB_DOMAIN_SHADER,
                "cs" => D3D11_SB_COMPUTE_SHADER,
                _ => panic!("{profile}"),
            };
            assert_eq!(
                program.version,
                Version {
                    stage,
                    major: profile[3..4].parse().unwrap(),
                    minor: profile[5..6].parse().unwrap(),
                    reserved: 0,
                },
                "{profile}"
            );
            for instruction in &program.instructions {
                match instruction {
                    Instruction::CustomData { .. } => custom = true,
                    Instruction::Operation { extensions, .. } => extended |= !extensions.is_empty(),
                }
            }
            assert!(
                program.instructions.iter().any(|instruction| matches!(
                    instruction,
                    Instruction::Operation {
                        body: Body::Declaration {
                            operand: Some(_),
                            ..
                        },
                        ..
                    }
                )),
                "{profile}"
            );
            let encoded: Vec<_> = program
                .encode()
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect();
            assert_eq!(encoded, original, "{profile}");
        }
        assert_eq!(token_chunks, 1, "{profile}");
        assert_eq!(downlevel, profile.contains("level_9"), "{profile}");
    }
    assert!(custom && extended);
}

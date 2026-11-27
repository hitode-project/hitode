use super::{checksum::checksum, operand::*, *};
use std::{fs, path::Path};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/shader-core");

fn fixture(name: &str) -> Vec<u8> {
    fs::read(Path::new(FIXTURES).join(format!("{name}.dxbc"))).unwrap()
}

fn words(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .copied()
        .map(u32::from_le_bytes)
        .collect()
}

fn register(kind: u32, components: u32, controls: u32, slot: Option<u32>) -> Operand<'static> {
    Operand {
        kind,
        components,
        controls,
        extensions: Vec::new(),
        indices: slot.into_iter().map(Index::Immediate32).collect(),
        literals: &[],
    }
}

fn temp(slot: u32, lane: u32) -> Operand<'static> {
    register(
        D3D10_SB_OPERAND_TYPE_TEMP,
        D3D10_SB_OPERAND_4_COMPONENT,
        encode_operand_4_component_selection_mode(D3D10_SB_OPERAND_4_COMPONENT_SELECT_1_MODE)
            | encode_operand_4_component_select_1(lane),
        Some(slot),
    )
}

fn destination(slot: u32, mask: u32) -> Operand<'static> {
    register(
        D3D10_SB_OPERAND_TYPE_TEMP,
        D3D10_SB_OPERAND_4_COMPONENT,
        encode_operand_4_component_mask(mask << D3D10_SB_OPERAND_4_COMPONENT_MASK_SHIFT),
        Some(slot),
    )
}

fn immediate(literal: &'static [u32; 1]) -> Operand<'static> {
    Operand {
        literals: literal,
        ..register(
            D3D10_SB_OPERAND_TYPE_IMMEDIATE32,
            D3D10_SB_OPERAND_1_COMPONENT,
            0,
            None,
        )
    }
}

fn op(opcode: u32, operands: Vec<Operand<'static>>) -> Instruction<'static> {
    operation(opcode, 0, Vec::new(), operands)
}

fn operation(
    opcode: u32,
    controls: u32,
    extensions: Vec<OpcodeExtension>,
    operands: Vec<Operand<'static>>,
) -> Instruction<'static> {
    Instruction::Operation {
        opcode,
        controls,
        length_word: false,
        extensions,
        body: Body::Operands(operands),
    }
}

fn declaration(
    opcode: u32,
    operand: Option<Operand<'static>>,
    words: &'static [u32],
) -> Instruction<'static> {
    Instruction::Operation {
        opcode,
        controls: 0,
        length_word: false,
        extensions: Vec::new(),
        body: Body::Declaration { operand, words },
    }
}

fn opcode(instruction: &Instruction<'_>) -> u32 {
    match instruction {
        Instruction::CustomData { .. } => D3D10_SB_OPCODE_CUSTOMDATA,
        Instruction::Operation { opcode, .. } => *opcode,
    }
}

fn store(address: Operand<'static>, value: Operand<'static>) -> Instruction<'static> {
    let uav = register(
        D3D11_SB_OPERAND_TYPE_UNORDERED_ACCESS_VIEW,
        2,
        0x10,
        Some(0),
    );
    op(
        D3D11_SB_OPCODE_STORE_STRUCTURED,
        vec![uav, address, immediate(&[0]), value],
    )
}

fn compute(name: &str, program: &mut Program<'_>) {
    let instructions = &mut program.instructions;
    instructions.retain(|instruction| declaration_has_operand(opcode(instruction)).is_some());
    for instruction in instructions.iter_mut() {
        if let Instruction::Operation {
            opcode,
            body: Body::Declaration { words, .. },
            ..
        } = instruction
        {
            match *opcode {
                D3D10_SB_OPCODE_DCL_TEMPS => *words = &[4],
                D3D11_SB_OPCODE_DCL_THREAD_GROUP => *words = &[1, 1, 1],
                _ => (),
            }
        }
    }
    let thread = || register(D3D11_SB_OPERAND_TYPE_INPUT_THREAD_ID, 2, 0xa, None);
    let srv = || register(D3D10_SB_OPERAND_TYPE_RESOURCE, 2, 0x6, Some(0));
    instructions.extend([
        op(
            D3D10_SB_OPCODE_IADD,
            vec![destination(0, 1), thread(), thread()],
        ),
        op(
            D3D10_SB_OPCODE_IADD,
            vec![destination(0, 2), temp(0, 0), immediate(&[1])],
        ),
        op(
            D3D11_SB_OPCODE_LD_STRUCTURED,
            vec![destination(1, 1), temp(0, 0), immediate(&[0]), srv()],
        ),
        op(
            D3D11_SB_OPCODE_LD_STRUCTURED,
            vec![destination(1, 2), temp(0, 1), immediate(&[0]), srv()],
        ),
    ]);
    match name {
        "encoded_alu" => {
            instructions.push(op(
                D3D10_SB_OPCODE_IMUL,
                vec![
                    register(D3D10_SB_OPERAND_TYPE_NULL, 0, 0, None),
                    destination(0, 1),
                    thread(),
                    immediate(&[7]),
                ],
            ));
            for operation in [
                D3D11_SB_OPCODE_UADDC,
                D3D11_SB_OPCODE_USUBB,
                D3D10_SB_OPCODE_UMUL,
            ] {
                instructions.extend([
                    op(
                        operation,
                        vec![destination(2, 1), destination(3, 1), temp(1, 0), temp(1, 1)],
                    ),
                    store(temp(0, 0), temp(2, 0)),
                    op(
                        D3D10_SB_OPCODE_IADD,
                        vec![destination(0, 1), temp(0, 0), immediate(&[1])],
                    ),
                    store(temp(0, 0), temp(3, 0)),
                    op(
                        D3D10_SB_OPCODE_IADD,
                        vec![destination(0, 1), temp(0, 0), immediate(&[1])],
                    ),
                ]);
            }
            instructions.extend([
                op(
                    D3D10_SB_OPCODE_UMAD,
                    vec![destination(2, 1), temp(1, 0), temp(1, 1), immediate(&[7])],
                ),
                store(temp(0, 0), temp(2, 0)),
            ]);
        }
        "encoded_flow" => {
            let conditional = |opcode, test| {
                operation(
                    opcode,
                    encode_instruction_test_boolean(D3D10_SB_INSTRUCTION_TEST_NONZERO),
                    Vec::new(),
                    vec![test],
                )
            };
            instructions.extend([
                op(
                    D3D10_SB_OPCODE_MOV,
                    vec![destination(2, 1), immediate(&[0])],
                ),
                op(D3D10_SB_OPCODE_LOOP, vec![]),
                op(
                    D3D10_SB_OPCODE_IADD,
                    vec![destination(2, 1), temp(2, 0), immediate(&[1])],
                ),
                op(
                    D3D10_SB_OPCODE_UGE,
                    vec![destination(2, 4), temp(2, 0), immediate(&[3])],
                ),
                conditional(D3D10_SB_OPCODE_BREAKC, temp(2, 2)),
                op(
                    D3D10_SB_OPCODE_IEQ,
                    vec![destination(2, 4), temp(2, 0), temp(1, 0)],
                ),
                conditional(D3D10_SB_OPCODE_CONTINUEC, temp(2, 2)),
                store(thread(), temp(2, 0)),
                op(D3D10_SB_OPCODE_ENDLOOP, vec![]),
                conditional(D3D10_SB_OPCODE_RETC, temp(1, 1)),
                store(thread(), immediate(&[9])),
            ]);
        }
        "encoded_call" | "encoded_callc" => {
            let label = || register(D3D10_SB_OPERAND_TYPE_LABEL, 0, 0, Some(0));
            let call = if name == "encoded_call" {
                op(D3D10_SB_OPCODE_CALL, vec![label()])
            } else {
                operation(
                    D3D10_SB_OPCODE_CALLC,
                    encode_instruction_test_boolean(D3D10_SB_INSTRUCTION_TEST_NONZERO),
                    Vec::new(),
                    vec![label(), temp(1, 0)],
                )
            };
            instructions.extend([
                op(
                    D3D10_SB_OPCODE_MOV,
                    vec![destination(2, 1), immediate(&[7])],
                ),
                call,
                store(thread(), temp(2, 0)),
                op(D3D10_SB_OPCODE_RET, vec![]),
                op(D3D10_SB_OPCODE_LABEL, vec![label()]),
                op(
                    D3D10_SB_OPCODE_MOV,
                    vec![destination(2, 1), immediate(&[11])],
                ),
            ]);
        }
        "encoded_shared_raw" => {
            let shared = |components, controls| {
                register(
                    D3D11_SB_OPERAND_TYPE_THREAD_GROUP_SHARED_MEMORY,
                    components,
                    controls,
                    Some(0),
                )
            };
            instructions.insert(
                4,
                declaration(
                    D3D11_SB_OPCODE_DCL_THREAD_GROUP_SHARED_MEMORY_RAW,
                    Some(shared(0, 0)),
                    &[4],
                ),
            );
            let dest = shared(D3D10_SB_OPERAND_4_COMPONENT, 0x10);
            let source = shared(D3D10_SB_OPERAND_4_COMPONENT, 6);
            instructions.extend([
                op(
                    D3D11_SB_OPCODE_STORE_RAW,
                    vec![dest, immediate(&[0]), temp(1, 0)],
                ),
                op(
                    D3D11_SB_OPCODE_LD_RAW,
                    vec![destination(2, 1), immediate(&[0]), source],
                ),
                store(thread(), temp(2, 0)),
            ]);
        }
        "encoded_dmov" => {
            let Instruction::Operation { controls, .. } = &mut instructions[0] else {
                unreachable!()
            };
            *controls |=
                encode_global_flags(D3D11_SB_GLOBAL_FLAG_ENABLE_DOUBLE_PRECISION_FLOAT_OPS);
            let double_one = Operand {
                literals: &[0, 0x3ff00000, 0, 0],
                ..register(D3D10_SB_OPERAND_TYPE_IMMEDIATE64, 2, 0, None)
            };
            instructions.extend([
                op(D3D11_SB_OPCODE_DMOV, vec![destination(2, 3), double_one]),
                store(temp(0, 0), temp(2, 0)),
                store(temp(0, 1), temp(2, 1)),
            ]);
        }
        "encoded_abort" => instructions.push(op(D3D11_SB_OPCODE_ABORT, vec![])),
        "encoded_nop" => instructions.push(op(D3D10_SB_OPCODE_NOP, vec![])),
        "encoded_debug_break" => instructions.push(op(D3D11_SB_OPCODE_DEBUG_BREAK, vec![])),
        "encoded_empty_opcode" => {
            instructions.push(operation(
                D3D10_SB_OPCODE_MOV,
                0,
                vec![OpcodeExtension {
                    kind: D3D10_SB_EXTENDED_OPCODE_EMPTY,
                    controls: 0,
                }],
                vec![destination(2, 1), temp(1, 0)],
            ));
            instructions.push(store(thread(), temp(2, 0)));
        }
        "encoded_empty_operand" | "encoded_min10" | "encoded_non_uniform" => {
            let mut source = temp(1, 0);
            let extension = match name {
                "encoded_empty_operand" => Extension {
                    kind: D3D10_SB_EXTENDED_OPERAND_EMPTY,
                    controls: 0,
                },
                "encoded_min10" => Extension {
                    kind: D3D10_SB_EXTENDED_OPERAND_MODIFIER,
                    controls: encode_operand_min_precision(
                        D3D11_SB_OPERAND_MIN_PRECISION_FLOAT_2_8,
                    ),
                },
                _ => Extension {
                    kind: D3D10_SB_EXTENDED_OPERAND_MODIFIER,
                    controls: encode_operand_non_uniform(1),
                },
            };
            source.extensions.push(extension);
            instructions.extend([
                op(D3D10_SB_OPCODE_MOV, vec![destination(2, 1), source]),
                store(thread(), temp(2, 0)),
            ]);
        }
        _ => unreachable!("{name}"),
    }
    instructions.push(op(D3D10_SB_OPCODE_RET, vec![]));
}

const ENCODED: &[(&str, &str)] = &[
    ("encoded_abort", "arithmetic"),
    ("encoded_alu", "arithmetic"),
    ("encoded_call", "arithmetic"),
    ("encoded_callc", "arithmetic"),
    ("encoded_debug_break", "arithmetic"),
    ("encoded_dmov", "arithmetic"),
    ("encoded_emit_cut", "gs_4_0"),
    ("encoded_emit_cut_stream", "gs_5_0"),
    ("encoded_empty_opcode", "arithmetic"),
    ("encoded_empty_operand", "arithmetic"),
    ("encoded_flow", "arithmetic"),
    ("encoded_hull_join", "hs_5_0"),
    ("encoded_min10", "arithmetic"),
    ("encoded_non_uniform", "arithmetic"),
    ("encoded_nop", "arithmetic"),
    ("encoded_shared_raw", "arithmetic"),
];

fn encoded_cases() -> impl Iterator<Item = (&'static str, Vec<u8>)> {
    ENCODED.iter().map(|&(name, source)| {
        let bytes = fixture(source);
        let original = Container::decode(&bytes).unwrap();
        let (tag, tokens) = original
            .chunks()
            .find(|(tag, _)| matches!(tag, b"SHDR" | b"SHEX"))
            .unwrap();
        let tokens = words(tokens);
        let mut program = Program::decode(&tokens).unwrap();
        let instructions = &mut program.instructions;
        match name {
            "encoded_emit_cut" | "encoded_emit_cut_stream" => {
                let cut = instructions.len() - 2;
                instructions.remove(cut - 1);
                let Instruction::Operation { opcode, .. } = &mut instructions[cut - 1] else {
                    unreachable!()
                };
                *opcode = if name == "encoded_emit_cut" {
                    D3D10_SB_OPCODE_EMITTHENCUT
                } else {
                    D3D11_SB_OPCODE_EMITTHENCUT_STREAM
                };
            }
            "encoded_hull_join" => {
                let fork = instructions
                    .iter()
                    .rposition(|inst| opcode(inst) == D3D11_SB_OPCODE_HS_FORK_PHASE)
                    .unwrap();
                instructions[fork] = op(D3D11_SB_OPCODE_HS_JOIN_PHASE, vec![]);
                instructions.insert(
                    fork + 1,
                    declaration(D3D11_SB_OPCODE_DCL_HS_JOIN_PHASE_INSTANCE_COUNT, None, &[1]),
                );
            }
            _ => compute(name, &mut program),
        }

        let code: Vec<_> = program
            .encode()
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        let chunks: Vec<_> = original
            .chunks()
            .map(|(kind, data)| (kind, if kind == tag { code.as_slice() } else { data }))
            .collect();
        (name, container(&chunks))
    })
}

fn container(chunks: &[([u8; 4], &[u8])]) -> Vec<u8> {
    let mut bytes = Vec::from(*b"DXBC");
    bytes.extend([0; 16]);
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(0u32.to_le_bytes());
    bytes.extend((chunks.len() as u32).to_le_bytes());
    bytes.resize(32 + 4 * chunks.len(), 0);
    for (i, (kind, data)) in chunks.iter().enumerate() {
        let offset = (bytes.len() as u32).to_le_bytes();
        bytes[32 + 4 * i..36 + 4 * i].copy_from_slice(&offset);
        bytes.extend(kind);
        bytes.extend((data.len() as u32).to_le_bytes());
        bytes.extend(*data);
    }
    let length = (bytes.len() as u32).to_le_bytes();
    bytes[24..28].copy_from_slice(&length);
    let digest = checksum(&bytes[20..]);
    bytes[4..20].copy_from_slice(&digest);
    bytes
}

#[cfg(feature = "gpu")]
pub(crate) fn geometry_routing_fixture() -> Vec<u8> {
    let bytes = fixture("gs_5_0");
    let original = Container::decode(&bytes).unwrap();
    let tokens = words(original.chunks().find(|(tag, _)| tag == b"SHEX").unwrap().1);
    let mut program = Program::decode(&tokens).unwrap();
    let output = |slot| {
        register(
            D3D10_SB_OPERAND_TYPE_OUTPUT,
            D3D10_SB_OPERAND_4_COMPONENT,
            encode_operand_4_component_mask(1 << D3D10_SB_OPERAND_4_COMPONENT_MASK_SHIFT),
            Some(slot),
        )
    };
    let first_move = program
        .instructions
        .iter()
        .position(|instruction| opcode(instruction) == D3D10_SB_OPCODE_MOV)
        .unwrap();
    program.instructions.splice(
        first_move..first_move,
        [
            declaration(D3D10_SB_OPCODE_DCL_OUTPUT_SIV, Some(output(2)), &[5]),
            declaration(D3D10_SB_OPCODE_DCL_OUTPUT_SIV, Some(output(3)), &[4]),
        ],
    );
    for index in (0..program.instructions.len()).rev() {
        if opcode(&program.instructions[index]) == D3D11_SB_OPCODE_EMIT_STREAM {
            program.instructions.splice(
                index..index,
                [
                    op(D3D10_SB_OPCODE_MOV, vec![output(2), immediate(&[1])]),
                    op(D3D10_SB_OPCODE_MOV, vec![output(3), immediate(&[1])]),
                ],
            );
        }
    }
    let code: Vec<_> = program
        .encode()
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    let mut strings = Vec::new();
    let mut signature = vec![4, 8];
    for (slot, (name, system, ty, mask)) in [
        ("SV_Position", 1, 3, 15),
        ("TEXCOORD", 0, 3, 3),
        ("SV_ViewportArrayIndex", 5, 1, 1),
        ("SV_RenderTargetArrayIndex", 4, 1, 1),
    ]
    .into_iter()
    .enumerate()
    {
        signature.extend([
            0,
            120 + strings.len() as u32,
            0,
            system,
            ty,
            slot as u32,
            mask,
        ]);
        strings.extend_from_slice(name.as_bytes());
        strings.push(0);
    }
    let mut signature: Vec<_> = signature.into_iter().flat_map(u32::to_le_bytes).collect();
    signature.extend(strings);
    let input = original.chunks().find(|(tag, _)| tag == b"ISGN").unwrap().1;
    container(&[(*b"ISGN", input), (*b"OSG5", &signature), (*b"SHEX", &code)])
}

#[cfg(feature = "gpu")]
pub(crate) fn pixel_uav_fixture() -> Vec<u8> {
    let program = Program {
        version: Version {
            stage: 0,
            major: 5,
            minor: 0,
            reserved: 0,
        },
        instructions: vec![
            declaration(
                D3D11_SB_OPCODE_DCL_UNORDERED_ACCESS_VIEW_STRUCTURED,
                Some(register(
                    D3D11_SB_OPERAND_TYPE_UNORDERED_ACCESS_VIEW,
                    0,
                    0,
                    Some(0),
                )),
                &[4],
            ),
            store(immediate(&[0]), immediate(&[17])),
            op(D3D10_SB_OPCODE_RET, Vec::new()),
        ],
    };
    let code: Vec<_> = program
        .encode()
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    container(&[(*b"SHEX", &code)])
}

#[cfg(feature = "gpu")]
pub(crate) fn pixel_atomic_fixture(
    interpolation: Option<u32>,
    dependent: bool,
    early: bool,
) -> Vec<u8> {
    const RED_STEP: [u32; 1] = [(64.0f32 / 255.0).to_bits()];
    let mask = |kind, slot, lanes: u32| {
        register(
            kind,
            D3D10_SB_OPERAND_4_COMPONENT,
            encode_operand_4_component_mask(lanes << D3D10_SB_OPERAND_4_COMPONENT_MASK_SHIFT),
            Some(slot),
        )
    };
    let output = |lanes| mask(D3D10_SB_OPERAND_TYPE_OUTPUT, 0, lanes);
    let uav = |components, controls| {
        register(
            D3D11_SB_OPERAND_TYPE_UNORDERED_ACCESS_VIEW,
            components,
            controls,
            Some(1),
        )
    };
    let mut instructions = vec![
        declaration(D3D10_SB_OPCODE_DCL_OUTPUT, Some(output(15)), &[]),
        declaration(
            D3D11_SB_OPCODE_DCL_UNORDERED_ACCESS_VIEW_STRUCTURED,
            Some(uav(0, 0)),
            &[4],
        ),
    ];
    if early {
        let mut flags = declaration(D3D10_SB_OPCODE_DCL_GLOBAL_FLAGS, None, &[]);
        if let Instruction::Operation { controls, .. } = &mut flags {
            *controls = encode_global_flags(D3D11_SB_GLOBAL_FLAG_FORCE_EARLY_DEPTH_STENCIL);
        }
        instructions.insert(0, flags);
    }
    if let Some(mode) = interpolation {
        let mut input = declaration(
            D3D10_SB_OPCODE_DCL_INPUT_PS,
            Some(mask(D3D10_SB_OPERAND_TYPE_INPUT, 1, 3)),
            &[],
        );
        if let Instruction::Operation { controls, .. } = &mut input {
            *controls = encode_input_interpolation_mode(mode);
        }
        instructions.insert(0, input);
        instructions.push(op(
            D3D11_SB_OPCODE_ATOMIC_IADD,
            vec![uav(2, 0x10), immediate(&[0]), immediate(&[1])],
        ));
        let input = register(
            D3D10_SB_OPERAND_TYPE_INPUT,
            D3D10_SB_OPERAND_4_COMPONENT,
            encode_operand_4_component_selection_mode(D3D10_SB_OPERAND_4_COMPONENT_SWIZZLE_MODE)
                | (0xe4 << 4),
            Some(1),
        );
        instructions.push(op(D3D10_SB_OPCODE_MOV, vec![output(3), input]));
    } else {
        instructions.extend([
            declaration(D3D10_SB_OPCODE_DCL_TEMPS, None, &[1]),
            op(
                D3D11_SB_OPCODE_IMM_ATOMIC_IADD,
                vec![
                    destination(0, 1),
                    uav(2, 0x10),
                    immediate(&[0]),
                    immediate(&[1]),
                ],
            ),
            op(
                D3D10_SB_OPCODE_MOV,
                vec![output(2), immediate(&[0x3f00_0000])],
            ),
        ]);
        if dependent {
            instructions.extend([
                op(D3D10_SB_OPCODE_UTOF, vec![destination(0, 1), temp(0, 0)]),
                // Successive invocations yield red values 64, 128, 192 and 255.
                op(
                    D3D10_SB_OPCODE_MAD,
                    vec![
                        output(1),
                        temp(0, 0),
                        immediate(&RED_STEP),
                        immediate(&RED_STEP),
                    ],
                ),
            ]);
        } else {
            instructions.push(op(
                D3D10_SB_OPCODE_MOV,
                vec![output(1), immediate(&[0x3f00_0000])],
            ));
        }
    }
    instructions.extend([
        op(D3D10_SB_OPCODE_MOV, vec![output(4), immediate(&[0])]),
        op(
            D3D10_SB_OPCODE_MOV,
            vec![output(8), immediate(&[0x3f80_0000])],
        ),
        op(D3D10_SB_OPCODE_RET, Vec::new()),
    ]);
    let code: Vec<_> = Program {
        version: Version {
            stage: 0,
            major: 5,
            minor: 0,
            reserved: 0,
        },
        instructions,
    }
    .encode()
    .into_iter()
    .flat_map(u32::to_le_bytes)
    .collect();
    let signature = |name: &[u8], slot, mask| {
        let mut bytes: Vec<_> = [1u32, 8, 32, 0, 0, 3, slot, mask]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        bytes.extend_from_slice(name);
        bytes
    };
    let input = signature(b"TEXCOORD\0", 1, 0x303);
    let output = signature(b"SV_Target\0", 0, 0xf);
    let mut chunks = vec![(*b"OSGN", output.as_slice()), (*b"SHEX", code.as_slice())];
    if interpolation.is_some() {
        chunks.push((*b"ISGN", &input));
    }
    container(&chunks)
}

#[test]
#[ignore = "exports inputs for the Windows opcode oracle"]
fn export_encoded_opcode_fixtures() {
    let dir = std::env::temp_dir().join("shader-core-opcodes");
    fs::create_dir_all(&dir).unwrap();
    for (name, bytes) in encoded_cases() {
        fs::write(dir.join(format!("{name}.dxbc")), bytes).unwrap();
    }
    println!("{}", dir.display());
}

use crate::shader::ir::{Opcode, opcode::Category};
use std::collections::BTreeMap;

fn operand_tokens(
    covered: &mut BTreeMap<(&'static str, u32), String>,
    name: &str,
    operand: &Operand<'_>,
) {
    let mut modifier = D3D10_SB_OPERAND_MODIFIER_NONE;
    let mut precision = D3D11_SB_OPERAND_MIN_PRECISION_DEFAULT;
    for extension in &operand.extensions {
        covered
            .entry(("operand extension", extension.kind))
            .or_insert_with(|| name.into());
        if extension.kind == D3D10_SB_EXTENDED_OPERAND_MODIFIER {
            modifier = decode_operand_modifier(extension.controls);
            precision = decode_operand_min_precision(extension.controls);
            if decode_operand_non_uniform(extension.controls) != 0 {
                covered
                    .entry(("operand non-uniform", 1))
                    .or_insert_with(|| name.into());
            }
        }
    }
    covered
        .entry(("modifier", modifier))
        .or_insert_with(|| name.into());
    covered
        .entry(("precision", precision))
        .or_insert_with(|| name.into());
    for index in &operand.indices {
        if let Index::Relative(relative)
        | Index::Immediate32Relative(_, relative)
        | Index::Immediate64Relative(_, relative) = index
        {
            operand_tokens(covered, name, relative);
        }
    }
}

fn accepts_opcode_extension(kind: u32) -> bool {
    let controls = match kind {
        D3D11_SB_EXTENDED_OPCODE_RESOURCE_DIM => {
            encode_extended_resource_dimension(D3D10_SB_RESOURCE_DIMENSION_TEXTURE2D)
        }
        D3D11_SB_EXTENDED_OPCODE_RESOURCE_RETURN_TYPE => (0..4).fold(0, |controls, lane| {
            controls | encode_extended_resource_return_type(D3D10_SB_RETURN_TYPE_FLOAT, lane)
        }),
        _ => 0,
    };
    accepts(operation(
        D3D10_SB_OPCODE_MOV,
        0,
        vec![OpcodeExtension { kind, controls }],
        vec![destination(0, 1), temp(0, 0)],
    ))
}

fn accepts_operand_extension(kind: u32, controls: u32) -> bool {
    let mut source = temp(0, 0);
    source.extensions.push(Extension { kind, controls });
    accepts(op(D3D10_SB_OPCODE_MOV, vec![destination(0, 1), source]))
}

fn accepts(instruction: Instruction<'_>) -> bool {
    lower::program(&Program {
        version: Version {
            stage: D3D11_SB_COMPUTE_SHADER,
            major: 5,
            minor: 0,
            reserved: 0,
        },
        instructions: vec![instruction],
    })
    .is_ok()
}

fn coverage_ledger() -> String {
    let mut paths: Vec<_> = fs::read_dir(FIXTURES)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "dxbc")
        })
        .collect();
    paths.sort();
    let mut covered = BTreeMap::new();
    for path in paths {
        let name = path.file_stem().unwrap().to_str().unwrap();
        if name == "diagnostic_pixel" {
            continue;
        }
        let bytes = fs::read(&path).unwrap();
        let container = Container::decode(&bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
        let (_, tokens) = container
            .chunks()
            .find(|(tag, _)| matches!(tag, b"SHDR" | b"SHEX"))
            .unwrap();
        let tokens = words(tokens);
        let program = Program::decode(&tokens).unwrap();
        parse(&bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
        for instruction in program.instructions {
            let token = opcode(&instruction);
            covered
                .entry(("opcode", token))
                .or_insert_with(|| name.into());
            match instruction {
                Instruction::CustomData { class, .. } => {
                    if class == D3D10_SB_CUSTOMDATA_DCL_IMMEDIATE_CONSTANT_BUFFER {
                        covered
                            .entry(("custom declaration", class))
                            .or_insert_with(|| name.into());
                    }
                }
                Instruction::Operation {
                    extensions, body, ..
                } => {
                    for extension in extensions {
                        covered
                            .entry(("opcode extension", extension.kind))
                            .or_insert_with(|| name.into());
                    }
                    match body {
                        Body::Operands(operands) => {
                            for operand in operands {
                                operand_tokens(&mut covered, name, &operand);
                            }
                        }
                        Body::Declaration {
                            operand: Some(operand),
                            ..
                        }
                        | Body::InterfaceCall {
                            interface: operand, ..
                        } => {
                            operand_tokens(&mut covered, name, &operand);
                        }
                        Body::Declaration { operand: None, .. } => {}
                    }
                }
            }
        }
    }

    let mut ledger = String::from("category\ttoken\tname\tfixture\tvalue_scope\n");
    let mut missing = Vec::new();
    let mut row = |category, key, token, name| {
        use std::fmt::Write;
        if let Some(fixture) = covered.get(&(key, token)) {
            let scope = match fixture.as_str() {
                "linkage" => "guest class linkage",
                "ds_5_0" | "hs_5_0" | "limited_hull" | "encoded_hull_join" => "guest tessellation",
                "gs_4_0"
                | "gs_4_1"
                | "gs_5_0"
                | "instanced_geometry"
                | "encoded_emit_cut"
                | "encoded_emit_cut_stream" => "guest geometry and stream output",
                "residency" => "tiled resources excluded",
                "doubles" | "encoded_dmov" => "double precision excluded",
                "encoded_abort" | "encoded_debug_break" => "diagnostic instruction",
                "derivative_queries"
                | "derivatives_4_0"
                | "multisample_queries"
                | "sample_modes"
                | "texture_ops" => "Windows value readback pending",
                _ => "shader core",
            };
            writeln!(ledger, "{category}\t{token}\t{name}\t{fixture}\t{scope}").unwrap();
        } else {
            missing.push(format!("{category} {token} {name}"));
        }
    };
    for token in 0..D3D10_SB_NUM_OPCODES {
        let opcode = Opcode::decode(token).unwrap();
        let category = match opcode.category() {
            Category::Reserved => continue,
            Category::Declaration => "declaration",
            Category::Operation | Category::Flow | Category::Custom => "opcode",
        };
        row(category, "opcode", token, opcode.mnemonic());
    }
    for (category, tokens) in [
        (
            "custom declaration",
            &[(
                D3D10_SB_CUSTOMDATA_DCL_IMMEDIATE_CONSTANT_BUFFER,
                "dcl_immediateConstantBuffer",
            )][..],
        ),
        (
            "opcode extension",
            &[
                (D3D10_SB_EXTENDED_OPCODE_EMPTY, "empty"),
                (D3D10_SB_EXTENDED_OPCODE_SAMPLE_CONTROLS, "sample controls"),
                (D3D11_SB_EXTENDED_OPCODE_RESOURCE_DIM, "resource dimension"),
                (
                    D3D11_SB_EXTENDED_OPCODE_RESOURCE_RETURN_TYPE,
                    "resource return type",
                ),
            ],
        ),
        (
            "operand extension",
            &[
                (D3D10_SB_EXTENDED_OPERAND_EMPTY, "empty"),
                (D3D10_SB_EXTENDED_OPERAND_MODIFIER, "modifier"),
            ],
        ),
        (
            "modifier",
            &[
                (D3D10_SB_OPERAND_MODIFIER_NONE, "none"),
                (D3D10_SB_OPERAND_MODIFIER_NEG, "negate"),
                (D3D10_SB_OPERAND_MODIFIER_ABS, "absolute"),
                (D3D10_SB_OPERAND_MODIFIER_ABSNEG, "absolute negate"),
            ],
        ),
        (
            "precision",
            &[
                (D3D11_SB_OPERAND_MIN_PRECISION_DEFAULT, "default"),
                (D3D11_SB_OPERAND_MIN_PRECISION_FLOAT_16, "float16"),
                (D3D11_SB_OPERAND_MIN_PRECISION_FLOAT_2_8, "float2.8"),
                (D3D11_SB_OPERAND_MIN_PRECISION_SINT_16, "int16"),
                (D3D11_SB_OPERAND_MIN_PRECISION_UINT_16, "uint16"),
            ],
        ),
        ("operand non-uniform", &[(1, "non-uniform")]),
    ] {
        for &(token, name) in tokens {
            row(category, category, token, name);
        }
    }
    for (category, mask, accepted) in [
        (
            "opcode extension",
            D3D10_SB_EXTENDED_OPCODE_TYPE_MASK,
            accepts_opcode_extension as fn(u32) -> bool,
        ),
        (
            "operand extension",
            D3D10_SB_EXTENDED_OPERAND_TYPE_MASK,
            |kind| accepts_operand_extension(kind, 0),
        ),
        (
            "modifier",
            D3D10_SB_OPERAND_MODIFIER_MASK >> D3D10_SB_OPERAND_MODIFIER_SHIFT,
            |modifier| {
                accepts_operand_extension(
                    D3D10_SB_EXTENDED_OPERAND_MODIFIER,
                    modifier << D3D10_SB_OPERAND_MODIFIER_SHIFT,
                )
            },
        ),
        (
            "precision",
            D3D11_SB_OPERAND_MIN_PRECISION_MASK >> D3D11_SB_OPERAND_MIN_PRECISION_SHIFT,
            |precision| {
                accepts_operand_extension(
                    D3D10_SB_EXTENDED_OPERAND_MODIFIER,
                    encode_operand_min_precision(precision),
                )
            },
        ),
    ] {
        for token in (0..=mask).filter(|&token| accepted(token)) {
            if !covered.contains_key(&(category, token)) {
                missing.push(format!("{category} {token}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "uncovered DXBC tokens:\n{}",
        missing.join("\n")
    );
    ledger
}

#[test]
// spec: GFX-SHADER-5
fn every_accepted_opcode_has_a_windows_fixture() {
    let actual = coverage_ledger();
    let expected = fs::read_to_string(Path::new(FIXTURES).join("coverage.tsv")).unwrap();
    assert_eq!(
        actual.lines().count(),
        expected.lines().count(),
        "coverage ledger row count"
    );
    for (actual, expected) in actual.lines().zip(expected.lines()) {
        assert_eq!(actual, expected, "coverage ledger row");
    }
    let creation = fs::read_to_string(Path::new(FIXTURES).join("creation.tsv")).unwrap();
    for (name, bytes) in encoded_cases() {
        assert_eq!(fixture(name), bytes, "{name}");
        let (_, results) = creation
            .lines()
            .filter_map(|row| row.split_once('\t'))
            .find(|&(fixture, _)| fixture == name)
            .expect(name);
        let results: Vec<_> = results.split('\t').collect();
        assert_eq!(results.len(), 3, "{name}");
        assert_eq!(
            results[2] == "00000000",
            Path::new(FIXTURES).join(format!("{name}.asm")).exists(),
            "{name}"
        );
    }
}

#[test]
#[ignore = "exports the coverage ledger for review"]
fn export_opcode_coverage_ledger() {
    print!("{}", coverage_ledger());
}

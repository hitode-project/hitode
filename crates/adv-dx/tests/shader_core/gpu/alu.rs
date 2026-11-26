use super::*;
use ir::{Components, Index, Instruction, Modifier, Operand, OperandValue, Precision, Statement};

#[derive(Clone, Copy)]
struct Case {
    source: [u32; 4],
    hardware: u32,
    warp: u32,
    mode: &'static str,
}

impl Case {
    fn agrees(self, bits: u32) -> bool {
        let result = f32::from_bits(bits);
        match self.mode {
            "bits" => bits == self.hardware,
            "either" => [self.hardware, self.warp].contains(&bits),
            "nan" => result.is_nan(),
            "half_nan" => bits & 0x7fff > 0x7c00,
            "ulp2" => {
                bits == self.warp
                    || (result.is_finite()
                        && f32::from_bits(self.warp).is_finite()
                        && (bits >> 31 == self.warp >> 31)
                        && bits.abs_diff(self.warp) <= 2)
            }
            "trig" => (result - f32::from_bits(self.warp)).abs() <= 0.000_007,
            mode => panic!("unknown oracle mode {mode}"),
        }
    }
}

fn register(register: Register, slot: Option<u32>, components: Components) -> Operand {
    Operand {
        value: OperandValue::Register {
            register,
            indices: slot.map_or_else(Vec::new, |slot| vec![Index::Direct(u64::from(slot))]),
        },
        components,
        modifier: Modifier::None,
        precision: Precision::Default,
        non_uniform: false,
    }
}

fn immediate(word: u32) -> Operand {
    Operand {
        value: OperandValue::Immediate([word, 0, 0, 0]),
        components: Components::Scalar,
        modifier: Modifier::None,
        precision: Precision::Default,
        non_uniform: false,
    }
}

fn instruction(opcode: Opcode, destinations: Vec<Operand>, sources: Vec<Operand>) -> Instruction {
    Instruction {
        opcode,
        destinations,
        sources,
        saturate: false,
        test_nonzero: false,
        precise: 0,
        controls: 0,
        resource: ir::ResourceOperands::default(),
        interface_function: None,
        flush_inputs: false,
        flush_outputs: false,
    }
}

fn shader(name: &str) -> ir::Module {
    use Components::{Mask, Select, Swizzle};
    use Opcode::*;
    let temp = |slot, components| register(Register::Temp, Some(slot), components);
    let thread = register(Register::InputThreadId, None, Select(0));
    let args = |indices: &[u8]| {
        indices
            .iter()
            .map(|&lane| temp(1, Select(lane)))
            .collect::<Vec<_>>()
    };
    let unary = |op| (op, args(&[0]), 0);
    let binary = |op| (op, args(&[0, 1]), 0);
    let (op, sources, output) = match name {
        "add" => binary(Add),
        "mul" => binary(Mul),
        "div" => binary(Div),
        "mad-a*b+1" => (
            Mad,
            vec![
                temp(1, Select(0)),
                temp(1, Select(1)),
                immediate(1.0f32.to_bits()),
            ],
            0,
        ),
        "min" => binary(Min),
        "max" => binary(Max),
        "eq" => binary(Eq),
        "ne" => binary(Ne),
        "lt" => binary(Lt),
        "ge" => binary(Ge),
        "dp2-(a,b)(b,a)" => (
            Dp2,
            vec![
                temp(1, Swizzle([0, 1, 0, 0])),
                temp(1, Swizzle([1, 0, 0, 0])),
            ],
            0,
        ),
        "mov-sat" | "neg-abs" | "abs" | "neg" | "mov" => unary(Mov),
        "rcp" => unary(Rcp),
        "rsq" => unary(Rsq),
        "sqrt" => unary(Sqrt),
        "exp" => unary(Exp),
        "log" => unary(Log),
        "sin" => unary(Sincos),
        "cos" => (Sincos, args(&[0]), 1),
        "frc" => unary(Frc),
        "round_ne" => unary(RoundNe),
        "round_ni" => unary(RoundNi),
        "round_pi" => unary(RoundPi),
        "round_z" => unary(RoundZ),
        "ftoi" => unary(Ftoi),
        "ftou" => unary(Ftou),
        "f32tof16" => unary(F32ToF16),
        "itof" => unary(Itof),
        "utof" => unary(Utof),
        "f16tof32" => unary(F16ToF32),
        "countbits" => unary(Countbits),
        "firstbit_hi" => unary(FirstbitHi),
        "firstbit_shi" => unary(FirstbitShi),
        "firstbit_lo" => unary(FirstbitLo),
        "bfrev" => unary(Bfrev),
        "ineg" => unary(Ineg),
        "not" => unary(Not),
        "ishl" => binary(Ishl),
        "ishr" => binary(Ishr),
        "ushr" => binary(Ushr),
        "imul" => (Imul, args(&[0, 1]), 1),
        "umul_hi" => binary(Umul),
        "udiv" => binary(Udiv),
        "urem" => (Udiv, args(&[0, 1]), 1),
        "imin" => binary(Imin),
        "umax" => binary(Umax),
        "ilt" => binary(Ilt),
        "ult" => binary(Ult),
        "iadd" => binary(Iadd),
        "ubfe-8-at-4" => (
            Ubfe,
            vec![immediate(8), immediate(4), temp(1, Select(0))],
            0,
        ),
        "ibfe-8-at-4" => (
            Ibfe,
            vec![immediate(8), immediate(4), temp(1, Select(0))],
            0,
        ),
        "bfi-8-at-4" => (
            Bfi,
            vec![
                immediate(8),
                immediate(4),
                temp(1, Select(1)),
                temp(1, Select(0)),
            ],
            0,
        ),
        "and" => binary(And),
        "or" => binary(Or),
        "xor" => binary(Xor),
        "ieq" => binary(Ieq),
        "ine" => binary(Ine),
        "ige" => binary(Ige),
        "uge" => binary(Uge),
        "imax" => binary(Imax),
        "umin" => binary(Umin),
        "imad" => (Imad, args(&[0, 1, 2]), 0),
        "umad" => (Umad, args(&[0, 1, 2]), 0),
        "movc" => (Movc, args(&[0, 1, 2]), 0),
        "swapc" => (Swapc, args(&[0, 1, 2]), 1),
        "uaddc" => (Uaddc, args(&[0, 1]), 1),
        "usubb" => (Usubb, args(&[0, 1]), 1),
        "msad" => (Msad, args(&[0, 1, 2]), 0),
        "dp3" => (
            Dp3,
            vec![
                temp(1, Swizzle([0, 1, 2, 0])),
                temp(1, Swizzle([0, 1, 2, 0])),
            ],
            0,
        ),
        "dp4" => (Dp4, vec![temp(1, Mask(15)), temp(1, Mask(15))], 0),
        other => panic!("no shader for {other}"),
    };
    let destinations = (0..op.types().0.len())
        .map(|i| {
            if i == output {
                temp(0, Mask(1))
            } else {
                register(Register::Null, None, Components::None)
            }
        })
        .collect();
    let mut operation = instruction(op, destinations, sources);
    operation.saturate = name == "mov-sat";
    if let Some(source) = operation.sources.first_mut() {
        source.modifier = match name {
            "neg-abs" => Modifier::AbsoluteNegate,
            "abs" => Modifier::Absolute,
            "neg" => Modifier::Negate,
            _ => Modifier::None,
        };
    }
    let mut module = module("arithmetic");
    module
        .body
        .retain(|stmt| matches!(stmt, Statement::Declare(_)));
    module.body.extend([
        Statement::Execute(instruction(
            Ishl,
            vec![temp(0, Mask(1))],
            vec![thread.clone(), immediate(2)],
        )),
        Statement::Execute(instruction(
            LdStructured,
            vec![temp(1, Mask(15))],
            vec![
                temp(0, Select(0)),
                immediate(0),
                register(Register::Resource, Some(0), Swizzle([0, 1, 2, 3])),
            ],
        )),
        Statement::Execute(operation),
        Statement::Execute(instruction(
            StoreStructured,
            vec![register(Register::UnorderedAccessView, Some(0), Mask(1))],
            vec![thread, immediate(0), temp(0, Select(0))],
        )),
        Statement::Execute(instruction(Ret, vec![], vec![])),
    ]);
    module
}

fn check(gpu: &mut Compute, name: &str, cases: Vec<Case>) {
    let module = shader(name);
    let count = cases.len().div_ceil(8) * 8;
    let mut data: Vec<_> = cases.iter().flat_map(|case| case.source).collect();
    data.resize(count * 4, 0);
    let scalar = interpret_compute(&module, &data, count, count as u32);
    let native = gpu.run(name, &module, &data, count, (count / 8) as u32);
    for (i, case) in cases.into_iter().enumerate() {
        for (engine, bits) in [("CPU", scalar[i]), ("GPU", native[i])] {
            assert!(
                case.agrees(bits),
                "{name} {engine} at {:08x?}: {bits:08x}, Windows hardware {:08x}, WARP {:08x} ({})",
                case.source,
                case.hardware,
                case.warp,
                case.mode
            );
        }
    }
}

#[test]
// spec: GFX-NUM-1
fn measured_opcode_vectors() {
    let fixture = include_str!("../../fixtures/shader-core/alu.tsv");
    let mut gpu = Compute::open();
    let rows: Vec<_> = fixture
        .lines()
        .map(|line| {
            let mut fields = line.split('\t');
            let name = fields.next().unwrap();
            let mode = fields.next().unwrap();
            let word = |field| u32::from_str_radix(field, 16).unwrap();
            let hardware = word(fields.next().unwrap());
            let warp = word(fields.next().unwrap());
            let mut source = [0; 4];
            for (dst, field) in source.iter_mut().zip(fields) {
                *dst = word(field);
            }
            (
                name,
                Case {
                    source,
                    hardware,
                    warp,
                    mode,
                },
            )
        })
        .collect();
    for group in rows.chunk_by(|a, b| a.0 == b.0) {
        check(
            &mut gpu,
            group[0].0,
            group.iter().map(|row| row.1).collect(),
        );
    }
}

#[test]
// spec: GFX-NUM-1
fn remaining_alu_and_source_modifiers() {
    let mut gpu = Compute::open();
    let f = f32::to_bits;
    for (name, source, hardware) in [
        ("abs", [f(-3.0), 0, 0, 0], f(3.0)),
        ("neg", [f(3.0), 0, 0, 0], f(-3.0)),
        ("mov", [1, 0, 0, 0], 1),
        ("umul_hi", [u32::MAX, u32::MAX, 0, 0], 0xffff_fffe),
        ("and", [0x1234, 0xf0f0, 0, 0], 0x1030),
        ("or", [0x1234, 0xf0f0, 0, 0], 0xf2f4),
        ("xor", [0x1234, 0xf0f0, 0, 0], 0xe2c4),
        ("ieq", [7, 7, 0, 0], u32::MAX),
        ("ine", [7, 7, 0, 0], 0),
        ("ige", [u32::MAX, 0, 0, 0], 0),
        ("uge", [u32::MAX, 0, 0, 0], u32::MAX),
        ("imax", [u32::MAX, 0, 0, 0], 0),
        ("umin", [u32::MAX, 0, 0, 0], 0),
        ("imad", [u32::MAX, 3, 5, 0], 2),
        ("umad", [u32::MAX, 3, 5, 0], 2),
        ("movc", [1, 9, 3, 0], 9),
        ("swapc", [1, 9, 3, 0], 9),
        ("uaddc", [u32::MAX, 1, 0, 0], 1),
        ("usubb", [0, 1, 0, 0], 1),
        ("msad", [0x0001_0203, 0xff00_0000, 4, 0], 10),
        ("dp3", [f(1.0), f(2.0), f(-3.0), f(99.0)], f(14.0)),
        ("dp4", [f(1.0), f(2.0), f(-3.0), f(4.0)], f(30.0)),
    ] {
        check(
            &mut gpu,
            name,
            vec![Case {
                source,
                hardware,
                warp: hardware,
                mode: "bits",
            }],
        );
    }
}

#[test]
// spec: GFX-NUM-1
fn generated_straight_line_and_structured_programs_match() {
    let mut gpu = Compute::open();
    let mut state = 0x6766_7835u32;
    let mut word = || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        state
    };
    let destination = register(Register::Temp, Some(0), Components::Mask(1));
    let accumulator = register(Register::Temp, Some(0), Components::Select(0));
    let count = register(Register::Temp, Some(1), Components::Select(2));
    let count_destination = register(Register::Temp, Some(1), Components::Mask(4));
    for n in 0..12 {
        let mut module = shader("mov");
        let terminal = module.body.split_off(module.body.len() - 2);
        let input: Vec<_> = (0..32).map(|_| word()).collect();
        let mut instructions = |count| -> ir::Block {
            (0..count)
                .map(|_| {
                    let opcodes = [
                        Opcode::Xor,
                        Opcode::Iadd,
                        Opcode::Ishl,
                        Opcode::Ishr,
                        Opcode::Imul,
                    ];
                    let opcode = opcodes[(word() >> 16) as usize % opcodes.len()];
                    let destinations = if opcode == Opcode::Imul {
                        vec![
                            register(Register::Null, None, Components::None),
                            destination.clone(),
                        ]
                    } else {
                        vec![destination.clone()]
                    };
                    Statement::Execute(instruction(
                        opcode,
                        destinations,
                        vec![accumulator.clone(), immediate(word())],
                    ))
                })
                .collect()
        };
        module.body.extend(instructions(7));
        if n % 2 == 1 {
            module.body.push(Statement::Execute(instruction(
                Opcode::And,
                vec![count_destination.clone()],
                vec![count.clone(), immediate(3)],
            )));
            let stop = instruction(Opcode::Breakc, vec![], vec![count.clone()]);
            let mut test = instruction(Opcode::If, vec![], vec![accumulator.clone()]);
            test.test_nonzero = true;
            let step = instruction(
                Opcode::Iadd,
                vec![count_destination.clone()],
                vec![count.clone(), immediate(u32::MAX)],
            );
            module.body.push(Statement::Loop(vec![
                Statement::Execute(stop),
                Statement::If {
                    test,
                    accept: instructions(3),
                    reject: Some(instructions(2)),
                },
                Statement::Execute(step),
            ]));
        }
        module.body.extend(terminal);
        let reference = interpret_compute(&module, &input, 8, 8);
        assert_eq!(
            gpu.run("generated program", &module, &input, 8, 1),
            reference,
            "program {n}"
        );
    }
}

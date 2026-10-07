use super::*;
use crate::{EXEC, interpreter::Step};
use iced_x86::code_asm::*;

const BASE: u32 = 0x10000;
const CODE: u32 = BASE + 0x4000;
type Asm = fn(&mut CodeAssembler, AsmRegisterSt) -> Result<(), IcedError>;

#[test]
fn wait_helper_checks_es_and_preserves_every_status_bit() {
    let mut asm = CodeAssembler::new(32).unwrap();
    asm.wait().unwrap();
    let bytes = asm.assemble(u64::from(CODE)).unwrap();
    let instruction = iced_x86::Decoder::with_ip(32, &bytes, u64::from(CODE), 0).decode();
    let op = NativeX87 {
        instruction,
        opcode: None,
        fast: classify(&instruction),
    };
    assert_eq!(op.fast, Some(Fast::Wait));
    let mut mem = memory(0, &bytes);
    for status in 0..=u16::MAX {
        let mut initial = CpuState {
            eip: CODE,
            ..CpuState::default()
        };
        initial.x87.status = status;
        initial.x87.control = status.rotate_left(3);
        initial.x87.ip = CODE - 16;
        initial.x87.dp = BASE;
        initial.x87.opcode = 0x123;
        initial.eflags = 0xad7;
        initial.refresh_mmx();
        let mut actual = initial.clone();
        let result = execute(&op, Fast::Wait, &mut actual, &mut mem);
        if status & 0x80 == 0 {
            assert_eq!(result, Some(X87_DONE));
            initial.eip = instruction.next_ip32();
            assert_eq!(actual, initial);
        } else {
            assert_eq!(result, None);
            assert_eq!(actual, initial);
            assert_eq!(
                super::super::execute(
                    &instruction,
                    super::super::Operation::X87,
                    None,
                    &mut actual,
                    &mut mem,
                    true
                ),
                Step::Trap {
                    pc: CODE,
                    vector: 16
                }
            );
            assert_eq!(actual, initial);
        }
    }
}

fn programs() -> Vec<Asm> {
    vec![
        |a, _| a.wait(),
        |a, _| a.fld(dword_ptr(esi)),
        |a, _| a.fld(qword_ptr(esi)),
        |a, _| a.fild(dword_ptr(esi)),
        |a, _| a.fild(word_ptr(esi)),
        |a, s| a.fld(s),
        |a, _| a.fst(dword_ptr(esi)),
        |a, _| a.fstp(qword_ptr(esi)),
        |a, _| a.fstp(dword_ptr(esi)),
        |a, _| a.fist(dword_ptr(esi)),
        |a, _| a.fistp(qword_ptr(esi)),
        |a, s| a.fst(s),
        |a, s| a.fstp(s),
        |a, _| a.fadd(dword_ptr(esi)),
        |a, _| a.fsub(qword_ptr(esi)),
        |a, _| a.fsubr(dword_ptr(esi)),
        |a, _| a.fmul(qword_ptr(esi)),
        |a, _| a.fdiv(dword_ptr(esi)),
        |a, _| a.fdivr(qword_ptr(esi)),
        |a, _| a.fiadd(dword_ptr(esi)),
        |a, _| a.fimul(word_ptr(esi)),
        |a, _| a.fidivr(dword_ptr(esi)),
        |a, s| a.fadd_2(st0, s),
        |a, s| a.fsub_2(s, st0),
        |a, s| a.fmul_2(st0, s),
        |a, s| a.fdiv_2(s, st0),
        |a, s| a.faddp(s, st0),
        |a, s| a.fsubrp(s, st0),
        |a, s| a.fmulp(s, st0),
        |a, s| a.fdivp(s, st0),
        |a, s| a.fdivrp(s, st0),
        |a, _| a.fcom(dword_ptr(esi)),
        |a, _| a.fcomp(qword_ptr(esi)),
        |a, s| a.fucom(st0, s),
        |a, _| a.fcompp(),
        |a, s| a.fxch(st0, s),
        |a, _| a.fchs(),
        |a, _| a.fabs(),
        |a, _| a.fldz(),
        |a, _| a.fld1(),
        |a, _| a.fldcw(word_ptr(esi)),
        |a, _| a.fnstcw(word_ptr(esi)),
        |a, _| a.fnstsw(ax),
        |a, _| a.fnstsw(word_ptr(esi)),
    ]
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.next() as usize % items.len()]
    }
}

fn value(rng: &mut Rng) -> f64 {
    let special = [
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.5,
        3.0,
        1.0 / 3.0,
        255.0,
        0.1,
        1e-30,
        1e30,
        1e300,
        1e-300,
        2.5,
        -2.5,
        3.5,
        2_147_483_647.5,
        -2_147_483_648.0,
        4_294_967_296.0,
        1e19,
        f64::MIN_POSITIVE,
    ];
    match rng.next() % 4 {
        0 => rng.pick(&special),
        1 => f64::from(f32::from_bits(rng.next() as u32)),
        2 => (rng.next() as i32 % 100_000) as f64 / 7.0,
        _ => f64::from_bits(rng.next()),
    }
}

fn register(rng: &mut Rng) -> Ext {
    if rng.next().is_multiple_of(16) {
        return Ext::from_parts(
            rng.pick(&[0, 1, 1 << 63, (1 << 63) | 0x800, u64::MAX]),
            rng.pick(&[0, 0x3bff, 0x43ff, 0x7ffe, 0x7fff, 0x8000, 0xffff]),
        );
    }
    if rng.next().is_multiple_of(8) {
        // Not an exact double: a full extended significand.
        return Ext::from_parts(rng.next() | 1 << 63, (rng.next() as u16 & 0x8000) | 0x3fff);
    }
    let v = value(rng);
    if v.is_finite() && (v == 0.0 || v.is_normal()) {
        extended(v)
    } else {
        Ext::from_parts(1 << 63, 0x3fff)
    }
}

fn memory(data: u64, bytes: &[u8]) -> Memory {
    let mut memory = Memory::new(BASE, 0x5000).unwrap();
    memory.protect(CODE, 0x1000, READ | WRITE).unwrap();
    memory.write(CODE, bytes).unwrap();
    memory.protect(CODE, 0x1000, READ | EXEC).unwrap();
    memory.protect(BASE, 0x3000, READ | WRITE).unwrap();
    for page in 0..3 {
        let at = BASE + page * 0x1000;
        memory.write(at + 0x10, &data.to_le_bytes()).unwrap();
        memory.write(at + 0xff8, &data.to_le_bytes()).unwrap();
    }
    // Page 1 is read-only, page 2 holds code, page 3 is inaccessible.
    memory.protect(BASE + 0x1000, 0x1000, READ).unwrap();
    memory
        .protect(BASE + 0x2000, 0x1000, READ | WRITE | EXEC)
        .unwrap();
    memory
}

#[test]
fn fast_x87_matches_the_transactional_path_bit_for_bit() {
    differential(false);
}

#[test]
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn compiled_x87_matches_the_transactional_path_bit_for_bit() {
    differential(true);
}

#[test]
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn compiled_x87_without_fma_matches_the_transactional_path() {
    crate::compile::x87::testing::FORCE_DEKKER.set(true);
    differential(true);
    crate::compile::x87::testing::FORCE_DEKKER.set(false);
}

fn differential(compiled: bool) {
    use crate::compile::x87::testing;
    let mut cache = std::collections::BTreeMap::new();
    let mut fallbacks = 0;
    let mut faults = 0;
    let programs = programs();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut taken = vec![0u32; programs.len()];
    let (mut rounded_up, mut inexact) = (0, 0);
    let mut pc64_hits = 0;
    for round in 0..300_000 {
        let index = round % programs.len();
        let mut asm = CodeAssembler::new(32).unwrap();
        let st = rng.pick(&[st0, st1, st2, st5, st7]);
        programs[index](&mut asm, st).unwrap();
        let bytes = asm.assemble(u64::from(CODE)).unwrap();
        let instruction = iced_x86::Decoder::with_ip(32, &bytes, u64::from(CODE), 0).decode();
        let opcode = bytes
            .windows(2)
            .find(|b| (0xd8..=0xdf).contains(&b[0]))
            .map(|b| ((b[0] as u16 & 7) << 8) | b[1] as u16);
        let native = NativeX87 {
            instruction,
            opcode,
            fast: classify(&instruction),
        };
        let fast = native
            .fast
            .unwrap_or_else(|| panic!("{:?} has no fast path", instruction.mnemonic()));

        let mut state = CpuState {
            eip: CODE,
            fuel: 1,
            ..CpuState::default()
        };
        state.fault_addr = rng.next() as u32;
        state.xmm = std::array::from_fn(|_| std::array::from_fn(|_| rng.next() as u8));
        state.mxcsr = rng.pick(&[0x1f80, 0x3f80, 0x5f80, 0x7f80, 0xdfc0]);
        state.x87.control = rng.pick(&[
            0x027f, 0x027f, 0x027f, 0x027f, 0x037f, 0x0e7f, 0x0c7f, 0x007f, 0x007f, 0x007f, 0x025f,
            0x021f, 0x047f, 0x087f, 0x067f, 0x0a7f, 0x077f, 0x0b7f, 0x0f7f,
        ]);
        state.x87.status = rng.pick(&[0, 0x20, 0x3800, 0x4100, 0x0001, 0x80, 0x8081]);
        state.x87.set_top(rng.next() as usize);
        state.x87.tag = rng.next() as u16;
        for slot in 0..8 {
            state.x87.regs[slot] = register(&mut rng);
            if state.x87.physical_tag(slot) != 3 {
                let tag = state.x87.regs[slot].tag();
                state.x87.set_physical_tag(slot, tag);
            }
        }
        state.x87.ip = rng.next() as u32;
        state.x87.dp = rng.next() as u32;
        state.x87.opcode = rng.next() as u16 & 0x7ff;
        state.x87.cs = rng.next() as u16;
        state.x87.ds = rng.next() as u16;
        state.mmx = state.x87.regs.map(Ext::significand);
        state.regs[0] = rng.next() as u32;
        state.regs[6] = BASE.wrapping_add(rng.pick(&[
            0x10,
            0x10,
            0x10,
            0xffc,
            0x1010,
            0x2010,
            0x3010,
            0xff8,
            u32::MAX - BASE - 3,
        ]));
        let mut data = match rng.next() % 3 {
            0 => u64::from((value(&mut rng) as f32).to_bits()) | rng.next() << 32,
            1 => value(&mut rng).to_bits(),
            _ => rng.next(),
        };
        if fast == Fast::LoadControl && !rng.next().is_multiple_of(4) {
            let word = rng.pick(&[0x027fu16, 0x0e7f, 0x037f, 0x0c7f]);
            data = (data & !0xffff) | u64::from(word);
        }

        let mut expected = state.clone();
        let mut reference = memory(data, &bytes);
        let mut fast_memory = memory(data, &bytes);
        if compiled {
            // Compilation observes the code page, which advances the epoch
            // once. Observe it in both memories up front so the comparison
            // sees only the instruction's own effects, cached code or not.
            reference.watch_code(CODE, bytes.len());
            fast_memory.watch_code(CODE, bytes.len());
        }
        let epoch = reference.epoch;
        let step = super::super::execute(
            &instruction,
            super::super::Operation::X87,
            opcode,
            &mut expected,
            &mut reference,
            true,
        );
        let mut actual = state.clone();
        let before = testing::calls();
        let result = if compiled {
            let code = cache.entry(bytes.clone()).or_insert_with(|| {
                crate::compile::compile(
                    &crate::Region::new(CODE, bytes.len()),
                    &mut fast_memory,
                    false,
                )
                .unwrap_or_else(|error| panic!("compile {:?}: {error}", instruction.mnemonic()))
            });
            testing::run(code, &mut actual, &mut fast_memory)
        } else {
            let Some(result) = execute(&native, fast, &mut actual, &mut fast_memory) else {
                assert_eq!(actual, state, "helper rejection must not mutate state");
                continue;
            };
            result
        };
        let helper = compiled && testing::calls() != before;
        if helper {
            fallbacks += 1;
        } else {
            taken[index] += 1;
            pc64_hits += u32::from(
                state.x87.control & 0x0f3f == 0x033f && matches!(fast, Fast::Arith { .. }),
            );
        }
        // Inexact results and magnitude round-ups are the delicate cases.
        rounded_up += u32::from(actual.x87.status & !state.x87.status & C1 != 0);
        inexact += u32::from(actual.x87.status & !state.x87.status & PE != 0);
        let context = format!(
            "{:?} {:?} {:?} round {round} data {data:#x} state {:?}",
            instruction.mnemonic(),
            instruction.op0_kind(),
            instruction.op0_register(),
            state.x87
        );
        if compiled {
            let expected_exit = match step {
                Step::Executed if reference.epoch != epoch || expected.x87.status & 0x80 != 0 => {
                    crate::Exit::Fallback
                }
                Step::Executed => crate::Exit::Dispatch,
                Step::ReadFault { address, .. } => {
                    expected.fault_addr = address;
                    faults += 1;
                    crate::Exit::ReadFault
                }
                Step::WriteFault { address, .. } => {
                    expected.fault_addr = address;
                    faults += 1;
                    crate::Exit::WriteFault
                }
                Step::Trap { .. } => crate::Exit::Fallback,
                _ => panic!("unexpected {step:?}: {context}"),
            };
            assert_eq!(result, expected_exit as u32, "{context}: {step:?}");
            assert_eq!(
                actual.fuel, 0,
                "one compiled x87 block must charge exactly one unit"
            );
            expected.fuel = actual.fuel;
        } else {
            assert!(matches!(step, Step::Executed), "{context}: {step:?}");
            assert_eq!(result == X87_EXIT, reference.epoch != epoch, "{context}");
        }
        assert_eq!(actual, expected, "{context}");
        assert_eq!(actual.x87, expected.x87, "{context}");
        assert_eq!(actual.mmx, expected.mmx, "{context}");
        assert_eq!(actual.regs, expected.regs, "{context}");
        assert_eq!(actual.eflags, expected.eflags, "{context}");
        assert_eq!(actual.eip, expected.eip, "{context}");
        for page in 0..3 {
            let at = BASE + page * 0x1000;
            assert_eq!(
                fast_memory.read_data(at, 0x1000).unwrap(),
                reference.read_data(at, 0x1000).unwrap(),
                "{context}"
            );
        }
        assert_eq!(fast_memory.epoch, reference.epoch, "{context}");
    }
    eprintln!(
        "compiled={compiled} native_hits={} helper_fallbacks={fallbacks} faults={faults} PE={inexact} C1={rounded_up}",
        taken.iter().sum::<u32>()
    );
    if compiled {
        assert!(fallbacks > 10_000 && faults > 1000, "{fallbacks} {faults}");
    }
    assert!(rounded_up > 500 && inexact > 1000, "{rounded_up} {inexact}");
    assert!(pc64_hits > 100, "PC=64 arithmetic hits: {pc64_hits}");
    for (index, count) in taken.iter().enumerate() {
        assert!(
            *count > 100,
            "program {index} took the fast path {count} times"
        );
    }
}

#[test]
fn dekker_product_error_matches_fused_multiply_add() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for _ in 0..100_000 {
        let mut operand = || {
            let exponent = (rng.next() % (2 * RANGE as u64 + 1)) as i64 - i64::from(RANGE);
            let bits = (rng.next() & ((1 << 52) - 1)) | ((exponent + 1023) as u64) << 52;
            let value = f64::from_bits(bits);
            if rng.next() & 1 == 0 { value } else { -value }
        };
        let (a, b) = (operand(), operand());
        let p = a * b;
        assert_eq!(dekker_error(a, b, p), a.mul_add(b, -p), "{a:e} {b:e}");
    }
}

#[test]
fn pc64_probe_separates_exactness_range_and_operand_boundaries() {
    use crate::cliffs::X87Pc64::*;
    let mut asm = CodeAssembler::new(32).unwrap();
    asm.fadd_2(st0, st1).unwrap();
    let bytes = asm.assemble(u64::from(CODE)).unwrap();
    let i = iced_x86::Decoder::with_ip(32, &bytes, u64::from(CODE), 0).decode();
    let fast = classify(&i).unwrap();
    let mem = memory(0, &bytes);
    let one = extended(1.0);
    for (a, b, category) in [
        (one, extended(2f64.powi(-52)), Exact),
        (one, extended(2f64.powi(-63)), Inexact),
        (one, Ext::from_parts((1 << 63) | 1, 0x3fff), Operand),
        (extended(2f64.powi(400)), extended(2f64.powi(400)), Exact),
        (one, extended(2f64.powi(401)), Range),
        (one, extended(f64::MIN_POSITIVE), Range),
        (one, Ext::from_parts(1 << 63, 0x3fff - 1074), Operand),
        (one, Ext::from_parts(1, 0), Operand),
        (one, Ext::from_parts(1 << 63, 0x7fff), Operand),
        (one, Ext::from_parts(3 << 62, 0x7fff), Operand),
        (extended(-0.0), extended(-0.0), Zero),
        (one, extended(-1.0), Zero),
    ] {
        let mut state = CpuState::default();
        state.x87.control = 0x037f;
        for (logical, value) in [a, b].into_iter().enumerate() {
            let slot = state.x87.physical_index(logical);
            state.x87.regs[slot] = value;
            state.x87.set_physical_tag(slot, value.tag());
        }
        assert_eq!(pc64_opportunity(&i, fast, &state, &mem), category);
    }

    // A normal rounded product alone does not protect its FMA residual: the
    // true error here is -2^-1126 and rounds to zero even with gradual underflow.
    let a = (1.0 + 2f64.powi(-52)) * 2f64.powi(-511);
    let b = (1.0 - 2f64.powi(-52)) * 2f64.powi(-511);
    let p = a * b;
    assert!(p.is_normal());
    assert_eq!(a.mul_add(b, -p), 0.0);
    assert!(!bounded(a, RANGE) && !bounded(b, RANGE));
}

#[test]
fn pc64_probe_candidates_match_extended_interpreter_in_300000_cases() {
    use crate::cliffs::X87Pc64;
    let programs: [Asm; 8] = [
        |a, _| a.fadd_2(st0, st1),
        |a, _| a.fsub_2(st0, st1),
        |a, _| a.fsubr_2(st0, st1),
        |a, _| a.fmul_2(st0, st1),
        |a, _| a.faddp(st1, st0),
        |a, _| a.fsubp(st1, st0),
        |a, _| a.fsubrp(st1, st0),
        |a, _| a.fmulp(st1, st0),
    ];
    let operations: Vec<_> = programs
        .iter()
        .map(|program| {
            let mut asm = CodeAssembler::new(32).unwrap();
            program(&mut asm, st1).unwrap();
            let bytes = asm.assemble(u64::from(CODE)).unwrap();
            let instruction = iced_x86::Decoder::with_ip(32, &bytes, u64::from(CODE), 0).decode();
            NativeX87 {
                instruction,
                opcode: Some(((u16::from(bytes[0]) & 7) << 8) | u16::from(bytes[1])),
                fast: classify(&instruction),
            }
        })
        .collect();
    let mut rng = Rng(0x5fe1_933a_129e_a843);
    let mut mem = memory(0, &[]);
    let mut hits = [0; 8];
    let mut categories = std::collections::HashMap::new();
    for round in 0..300_000 {
        let index = round % operations.len();
        let native = &operations[index];
        let fast = native.fast.unwrap();
        let mut state = CpuState {
            eip: CODE,
            eflags: rng.next() as u32,
            ..CpuState::default()
        };
        state.x87.control = 0x037f;
        state.x87.status = rng.next() as u16 & 0x7f7f;
        state.x87.set_top(rng.next() as usize);
        for logical in 0..2 {
            let value = register(&mut rng);
            let slot = state.x87.physical_index(logical);
            state.x87.regs[slot] = value;
            state.x87.set_physical_tag(slot, value.tag());
        }
        state.refresh_mmx();
        let category = pc64_opportunity(&native.instruction, fast, &state, &mem);
        *categories.entry(category).or_insert(0usize) += 1;
        if !matches!(category, X87Pc64::Exact | X87Pc64::Zero) {
            continue;
        }
        hits[index] += 1;
        let mut expected = state.clone();
        assert_eq!(
            super::super::execute(
                &native.instruction,
                super::super::Operation::X87,
                native.opcode,
                &mut expected,
                &mut mem,
                true
            ),
            Step::Executed
        );
        let mut actual = state.clone();
        assert_eq!(execute(native, fast, &mut actual, &mut mem), Some(X87_DONE));
        assert_eq!(actual, expected, "round {round}, {category:?}");
        assert_eq!(actual.x87.status & PE, state.x87.status & PE);
        assert_eq!(actual.x87.status & C1, 0);
    }
    assert!(hits.iter().all(|&n| n > 100));
    for category in [
        X87Pc64::Exact,
        X87Pc64::Operand,
        X87Pc64::Range,
        X87Pc64::Inexact,
    ] {
        assert!(categories[&category] > 1000, "{categories:?}");
    }
    eprintln!("PC64 opportunity categories: {categories:?}; exact hits by form: {hits:?}");
}

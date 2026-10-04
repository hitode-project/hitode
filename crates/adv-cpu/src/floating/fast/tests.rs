use super::*;
use crate::{EXEC, interpreter::Step};
use iced_x86::code_asm::*;

const BASE: u32 = 0x10000;
type Asm = fn(&mut CodeAssembler, AsmRegisterSt) -> Result<(), IcedError>;

fn programs() -> Vec<Asm> {
    vec![
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

fn memory(data: u64) -> Memory {
    let mut memory = Memory::new(BASE, 0x4000).unwrap();
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
    let programs = programs();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut taken = vec![0u32; programs.len()];
    let (mut rounded_up, mut inexact) = (0, 0);
    for round in 0..150_000 {
        let index = round % programs.len();
        let mut asm = CodeAssembler::new(32).unwrap();
        let st = rng.pick(&[st0, st1, st2, st5, st7]);
        programs[index](&mut asm, st).unwrap();
        let bytes = asm.assemble(0x1000).unwrap();
        let instruction = iced_x86::Decoder::with_ip(32, &bytes, 0x1000, 0).decode();
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
            eip: 0x1000,
            ..CpuState::default()
        };
        state.x87.control = rng.pick(&[
            0x027f, 0x027f, 0x037f, 0x0e7f, 0x0c7f, 0x007f, 0x025f, 0x021f,
        ]);
        state.x87.status = rng.pick(&[0, 0x20, 0x3800, 0x4100, 0x0001]);
        state.x87.set_top(rng.next() as usize);
        state.x87.tag = rng.next() as u16;
        for slot in 0..8 {
            state.x87.regs[slot] = register(&mut rng);
            if state.x87.physical_tag(slot) != 3 {
                let tag = state.x87.regs[slot].tag();
                state.x87.set_physical_tag(slot, tag);
            }
        }
        state.mmx = state.x87.regs.map(Ext::significand);
        state.regs[0] = rng.next() as u32;
        state.regs[6] = BASE + rng.pick(&[0x10, 0xffc, 0x1010, 0x2010, 0x3010, 0xff8]);
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
        let mut reference = memory(data);
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
        let mut fast_memory = memory(data);
        let Some(result) = execute(&native, fast, &mut actual, &mut fast_memory) else {
            continue;
        };
        taken[index] += 1;
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
        assert!(matches!(step, Step::Executed), "{context}: {step:?}");
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
        assert_eq!(result == X87_EXIT, reference.epoch != epoch, "{context}");
    }
    assert!(rounded_up > 500 && inexact > 1000, "{rounded_up} {inexact}");
    for (index, count) in taken.iter().enumerate() {
        assert!(
            *count > 100,
            "program {index} took the fast path {count} times"
        );
    }
}

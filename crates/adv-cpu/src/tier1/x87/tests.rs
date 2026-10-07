use super::*;
use crate::{EXEC, READ, WRITE, X87Register as Ext, interpreter::Interpreter, interpreter::Step};
use iced_x86::{Decoder, code_asm::*};

const CODE: u32 = 0x10000;
const DATA: u32 = 0x20000;
type Asm = fn(&mut CodeAssembler) -> Result<(), IcedError>;

fn programs() -> Vec<Asm> {
    vec![
        |a| a.fld(dword_ptr(esi)),
        |a| a.fld(qword_ptr(esi)),
        |a| a.fld(st2),
        |a| a.fst(dword_ptr(esi)),
        |a| a.fst(qword_ptr(esi)),
        |a| a.fstp(dword_ptr(esi)),
        |a| a.fstp(qword_ptr(esi)),
        |a| a.fstp(st2),
        |a| a.fadd(dword_ptr(esi)),
        |a| a.fadd(qword_ptr(esi)),
        |a| a.fadd_2(st0, st2),
        |a| a.fadd_2(st2, st0),
        |a| a.fsub(dword_ptr(esi)),
        |a| a.fsub(qword_ptr(esi)),
        |a| a.fsub_2(st0, st2),
        |a| a.fsub_2(st2, st0),
        |a| a.fsubr(dword_ptr(esi)),
        |a| a.fsubr(qword_ptr(esi)),
        |a| a.fsubr_2(st0, st2),
        |a| a.fsubr_2(st2, st0),
        |a| a.fmul(dword_ptr(esi)),
        |a| a.fmul(qword_ptr(esi)),
        |a| a.fmul_2(st0, st2),
        |a| a.fmul_2(st2, st0),
        |a| a.fxch(st0, st2),
    ]
}

fn fixture(build: Asm) -> (Operations, Memory, CpuState) {
    let mut asm = CodeAssembler::new(32).unwrap();
    build(&mut asm).unwrap();
    let bytes = asm.assemble(u64::from(CODE)).unwrap();
    let instruction = Decoder::with_ip(32, &bytes, u64::from(CODE), 0).decode();
    assert!(super::super::forms::supported(&instruction));
    let operations = Operations::new(
        &BTreeMap::from([(CODE, instruction)]),
        &[(CODE, bytes.clone())],
    );
    let mut memory = Memory::new(CODE, 0x20000).unwrap();
    memory.protect(CODE, 0x20000, READ | WRITE).unwrap();
    memory.write(CODE, &bytes).unwrap();
    memory.protect(CODE, 4096, READ | EXEC).unwrap();
    let mut state = CpuState {
        eip: CODE,
        eflags: 0xcd7,
        regs: [3, 5, 7, 11, DATA + 128, 13, DATA, 17],
        xmm: [[0xa5; 16]; 8],
        ..CpuState::default()
    };
    for exponent in 0..3 {
        state.x87.push(Ext::from_parts(1 << 63, 0x3fff + exponent));
    }
    state.x87.status |= 0x4700;
    state.refresh_mmx();
    let bytes = if instruction.memory_size().size() == 4 {
        2f32.to_le_bytes().to_vec()
    } else {
        2f64.to_le_bytes().to_vec()
    };
    memory.write(DATA, &bytes).unwrap();
    (operations, memory, state)
}

#[test]
fn all_admitted_x87_forms_use_the_existing_exact_helper() {
    for build in programs() {
        let (operations, mut memory, initial) = fixture(build);
        let op = &operations.0[0];
        assert_eq!(operations.get(CODE), Some(std::ptr::from_ref(op) as u64));
        assert_eq!(operations.get(CODE + 1), None);
        for control in [0x7f, 0x27f, 0x37f] {
            for top in 0..8 {
                let mut initial = initial.clone();
                initial.x87.control = control;
                let old = initial.x87.clone();
                initial.x87.set_top(top);
                initial.x87.tag = 0xffff;
                for logical in 0..3 {
                    let p = initial.x87.physical_index(logical);
                    initial.x87.regs[p] = old.regs[old.physical_index(logical)];
                    initial.x87.set_physical_tag(p, 0);
                }
                initial.refresh_mmx();
                let bytes = memory.read_data(DATA, 8).unwrap().into_owned();
                let mut expected = initial.clone();
                assert_eq!(
                    Interpreter::new().step(&mut expected, &mut memory),
                    Step::Executed
                );
                let stored = memory.read_data(DATA, 8).unwrap().into_owned();
                memory.write(DATA, &bytes).unwrap();
                let mut actual = initial;
                assert_eq!(
                    execute(op, &mut actual, &mut memory),
                    0,
                    "{:?}",
                    op.instruction.code()
                );
                assert_eq!(actual, expected, "{:?}", op.instruction.code());
                assert_eq!(memory.read_data(DATA, 8).unwrap().as_ref(), stored);
                memory.write(DATA, &bytes).unwrap();
            }
        }
    }
}

#[test]
fn refused_x87_guards_leave_the_complete_state_and_memory_unchanged() {
    for case in 0..12 {
        let (ops, mut memory, mut state) = fixture(|a| a.fadd(qword_ptr(esi)));
        match case {
            0 => {
                state.x87.control = 0x37f;
                memory.write(DATA, &2f64.powi(-60).to_le_bytes()).unwrap();
            }
            1..=3 => state.x87.control = 0x27f | (case << 10),
            4 => state.x87.control = 0x27e,
            5 => state.x87.status |= 0x8081,
            6 => state.x87.tag = 0xffff,
            7 => state.mmx_mode = 1,
            8 => memory.write(DATA, &1u64.to_le_bytes()).unwrap(),
            9 => memory.write(DATA, &f64::INFINITY.to_le_bytes()).unwrap(),
            10 => memory
                .write(DATA, &0x7ff0_0000_0000_0001u64.to_le_bytes())
                .unwrap(),
            _ => state.x87.regs[state.x87.top()] = Ext::from_parts((1 << 63) | 1, 0x3fff),
        }
        let before = state.clone();
        let bytes = memory.read_data(DATA, 8).unwrap().into_owned();
        let epoch = memory.epoch;
        assert_eq!(
            execute(&ops.0[0], &mut state, &mut memory),
            2,
            "case {case}"
        );
        assert_eq!(state, before, "case {case}");
        assert_eq!(memory.read_data(DATA, 8).unwrap().as_ref(), bytes);
        assert_eq!(memory.epoch, epoch);
    }
}

#[test]
fn x87_cross_page_refusal_and_executable_store_follow_checked_exit_protocol() {
    for build in [programs()[0], programs()[1], programs()[5], programs()[6]] {
        let (ops, mut memory, mut state) = fixture(build);
        state.regs[6] = DATA + 4094;
        memory.write(DATA + 4094, &2f64.to_le_bytes()).unwrap();
        memory.protect(DATA + 4096, 4096, 0).unwrap();
        let before = state.clone();
        let prefix = memory.read_data(DATA + 4094, 2).unwrap().into_owned();
        assert_eq!(execute(&ops.0[0], &mut state, &mut memory), 2);
        assert_eq!(state, before);
        assert_eq!(memory.read_data(DATA + 4094, 2).unwrap().as_ref(), prefix);
    }
    let (ops, mut memory, mut state) = fixture(|a| a.fstp(qword_ptr(esi)));
    memory.protect(DATA, 4096, READ | WRITE | EXEC).unwrap();
    memory.watch_code(DATA, 8);
    let top = state.x87.top();
    assert_eq!(execute(&ops.0[0], &mut state, &mut memory), 1);
    assert_eq!(state.x87.top(), (top + 1) & 7);
}

#[test]
fn x87_forms_preserve_prefix_and_address_guards() {
    let (ops, _, _) = fixture(|a| a.fld(qword_ptr(esi)));
    for guard in 0..5 {
        let mut instruction = ops.0[0].instruction;
        match guard {
            0 => instruction.set_has_lock_prefix(true),
            1 => instruction.set_has_rep_prefix(true),
            2 => instruction.set_has_repne_prefix(true),
            3 => instruction.set_segment_prefix(iced_x86::Register::GS),
            _ => instruction.set_memory_base(iced_x86::Register::SI),
        }
        assert!(!super::super::forms::supported(&instruction));
    }
    let mut asm = CodeAssembler::new(32).unwrap();
    asm.fld(tbyte_ptr(esi)).unwrap();
    let bytes = asm.assemble(u64::from(CODE)).unwrap();
    assert!(!super::super::forms::supported(
        &Decoder::new(32, &bytes, 0).decode()
    ));
}

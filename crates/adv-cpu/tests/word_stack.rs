mod support;
use adv_cpu::{CpuState, EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use support::Executor;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const DATA: u32 = 0x20000;
const STACK: u32 = 0x2f800;
const VALUES: [u32; 5] = [0, 0x7fff, 0x8000_80f0, 0x1234_5678, 0xffff_ffff];

fn machine(bytes: &[u8], jit: bool) -> Executor {
    let mut m = Executor::new(CODE, 0x20000).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    m.write(CODE, bytes).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    if jit {
        m.add_region(Region::new(CODE, bytes.len())).unwrap();
    }
    m
}

fn reset(m: &mut Executor, value: u32) {
    m.state = CpuState {
        regs: [value, !value, value.rotate_left(8), 3, STACK, DATA, DATA, 7],
        eflags: 0x8d7,
        eip: CODE,
        ..CpuState::default()
    };
    m.write(STACK - 16, &[0x5a; 16]).unwrap();
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    m.write(DATA, &value.rotate_right(4).to_le_bytes()).unwrap();
}

#[test]
fn word_pushes_and_pops_run_natively_and_match_unicorn() {
    type Op = fn(&mut CodeAssembler) -> Result<(), IcedError>;
    let ops: &[Op] = &[
        |a| {
            a.push(cx)?;
            a.pop(ax)
        },
        |a| {
            a.push(word_ptr(esi))?;
            a.pop(dx)
        },
        // push imm16, then pushw imm8 sign-extended to a word.
        |a| {
            a.db(&[0x66, 0x68, 0x34, 0x12])?;
            a.db(&[0x66, 0x6a, 0x80])?;
            a.pop(ebx)
        },
        // PUSH SP stores the value from before the decrement.
        |a| {
            a.push(sp)?;
            a.pop(bx)
        },
        // Word and doubleword stack slots overlap.
        |a| {
            a.push(ax)?;
            a.push(cx)?;
            a.pop(edx)?;
            a.push(eax)?;
            a.pop(cx)?;
            a.pop(bx)
        },
        |a| {
            a.push(dx)?;
            a.add(word_ptr(esp), 0x7fff)?;
            a.pop(si)
        },
    ];
    for (index, op) in ops.iter().enumerate() {
        let mut asm = CodeAssembler::new(32).unwrap();
        op(&mut asm).unwrap();
        // `rep ret`, the branch-predictor idiom for a plain return.
        asm.db(&[0xf3, 0xc3]).unwrap();
        let bytes = asm.assemble(u64::from(CODE)).unwrap();
        let mut jit = machine(&bytes, true);
        let mut reference = machine(&bytes, false);
        for value in VALUES {
            reset(&mut jit, value);
            reset(&mut reference, value);
            assert_eq!(
                jit.run(STOP, 100).unwrap(),
                RunOutcome::ReachedStop,
                "op {index} value {value:#x}"
            );
            assert_eq!(
                reference.run_unicorn(STOP, 100).unwrap(),
                RunOutcome::ReachedStop
            );
            let context = format!("op {index} value {value:#x}");
            assert_eq!(jit.state.regs, reference.state.regs, "{context}");
            assert_eq!(jit.state.eflags, reference.state.eflags, "{context}");
            assert_eq!(
                jit.memory().read(STACK - 16, 20).unwrap(),
                reference.memory().read(STACK - 16, 20).unwrap(),
                "{context}"
            );
        }
        assert_eq!(jit.stats.interpreter_entries, 0, "op {index}");
    }
}

mod support;
use adv_cpu::{CpuState, EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use support::Executor;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const DATA: u32 = 0x20000;
const STACK: u32 = 0x2f800;
const VALUES: [u32; 8] = [0, 1, 0x0f, 0x7f, 0x80, 0xff, 0x7fff_80f0, 0xffff_ffff];

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

fn reset(m: &mut Executor, left: u32, right: u32, flags: u32) {
    m.state = CpuState {
        regs: [left, right, right, left, STACK, DATA, DATA, right],
        eflags: flags,
        eip: CODE,
        ..CpuState::default()
    };
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    m.write(DATA, &left.to_le_bytes()).unwrap();
    m.write(DATA + 4, &right.to_le_bytes()).unwrap();
}

#[test]
fn narrow_integer_operations_run_natively_and_match_unicorn() {
    type Op = fn(&mut CodeAssembler) -> Result<(), IcedError>;
    let ops: &[Op] = &[
        |a| a.or(ah, 0x0c),
        |a| a.add(al, cl),
        |a| a.add(ah, ch),
        |a| a.sub(bl, dh),
        |a| a.and(ch, al),
        |a| a.xor(byte_ptr(esi), dl),
        |a| a.or(byte_ptr(esi + 1), 0x81),
        |a| a.sub(byte_ptr(esi), al),
        |a| a.add(ax, cx),
        |a| a.sub(word_ptr(esi), 0x7fff),
        |a| a.and(dx, -2i32),
        |a| a.xor(word_ptr(esi + 2), bx),
        |a| a.inc(al),
        |a| a.dec(bh),
        |a| a.inc(word_ptr(esi)),
        |a| a.dec(cx),
        |a| a.cmp(ah, 0x80),
        |a| a.movzx(bx, byte_ptr(esi)),
        |a| a.movsx(dx, byte_ptr(esi + 4)),
    ];
    for (index, op) in ops.iter().enumerate() {
        let mut asm = CodeAssembler::new(32).unwrap();
        op(&mut asm).unwrap();
        asm.ret().unwrap();
        let bytes = asm.assemble(u64::from(CODE)).unwrap();
        let mut jit = machine(&bytes, true);
        let mut reference = machine(&bytes, false);
        for &left in &VALUES {
            for &right in &VALUES {
                for flags in [0x2, 0x8d7] {
                    reset(&mut jit, left, right, flags);
                    reset(&mut reference, left, right, flags);
                    assert_eq!(jit.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                    assert_eq!(
                        reference.run_unicorn(STOP, 100).unwrap(),
                        RunOutcome::ReachedStop
                    );
                    let context = format!("op {index} left {left:#x} right {right:#x}");
                    assert_eq!(jit.state.regs, reference.state.regs, "{context}");
                    // AF is architecturally undefined after logical operations.
                    let mask = if matches!(index, 0 | 4 | 5 | 6 | 10 | 11) {
                        !0x10
                    } else {
                        !0
                    };
                    assert_eq!(
                        jit.state.eflags & mask,
                        reference.state.eflags & mask,
                        "{context}"
                    );
                    assert_eq!(
                        jit.memory().read(DATA, 8).unwrap(),
                        reference.memory().read(DATA, 8).unwrap(),
                        "{context}"
                    );
                }
            }
        }
        support::x64_shape!(assert_eq!(jit.stats.interpreter_entries, 0, "op {index}"));
    }
}

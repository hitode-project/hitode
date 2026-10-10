mod support;
use adv_cpu::{CompileStrategy, EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use support::Executor;

const CODE: u32 = 0x10000;
// Callees sit on the page below the caller, as in a blitter's blend table.
const ADD: u32 = CODE + 0xf40;
const XOR: u32 = CODE + 0xf60;
const OTHER: u32 = CODE + 0xf80;
const ROOT: u32 = CODE + 0x1010;
const STOP: u32 = 0x18000;
const TABLE: u32 = 0x20000;
const STACK: u32 = 0x2f800;

fn assemble(at: u32, build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(u64::from(at)).unwrap()
}

fn leaf(at: u32, subtract: bool) -> Vec<u8> {
    assemble(at, |a| {
        a.paddw(mm0, mm1).unwrap();
        if subtract {
            a.sub(eax, ebx).unwrap();
        } else {
            a.add(eax, ebx).unwrap();
        }
        a.ret().unwrap();
    })
}

fn load(m: &mut Executor, add: &[u8]) {
    m.write(ADD, add).unwrap();
    m.write(
        XOR,
        &assemble(XOR, |a| {
            a.xor(eax, 0x55).unwrap();
            a.ret_1(4).unwrap();
        }),
    )
    .unwrap();
    m.write(
        OTHER,
        &assemble(OTHER, |a| {
            a.inc(edi).unwrap();
            a.ret().unwrap();
        }),
    )
    .unwrap();
    m.write(
        ROOT,
        &assemble(ROOT, |a| {
            a.mov(ecx, 64).unwrap();
            let mut top = a.create_label();
            a.set_label(&mut top).unwrap();
            a.mov(edx, ecx).unwrap();
            a.and(edx, 3).unwrap();
            a.push(ecx).unwrap();
            a.call(dword_ptr(edx * 4 + TABLE as i32)).unwrap();
            a.cmp(edx, 1).unwrap();
            let mut popped = a.create_label();
            a.je(popped).unwrap();
            a.pop(esi).unwrap();
            a.set_label(&mut popped).unwrap();
            a.dec(ecx).unwrap();
            a.jnz(top).unwrap();
            a.emms().unwrap();
            a.ret().unwrap();
        }),
    )
    .unwrap();
    for (index, target) in [ADD, XOR, OTHER, ADD].into_iter().enumerate() {
        m.write(TABLE + index as u32 * 4, &target.to_le_bytes())
            .unwrap();
    }
}

fn machine(add: &[u8]) -> Executor {
    let mut m = Executor::new(CODE, 0x20000).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    load(&mut m, add);
    m.protect(CODE, 0x2000, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    m
}

fn start(m: &mut Executor) {
    m.state.eip = ROOT;
    m.state.eflags = 0x202;
    m.state.regs = [7, 0, 0, 0x0101_0101, STACK, 0, 0, 0];
    m.state.mmx = [0, 0x0001_0002_0003_0004, 0, 0, 0, 0, 0, 0];
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
}

fn unit(start: u32, entry: u32, len: usize, callees: &[u32]) -> Region {
    let mut region = Region::new(start, len);
    region.reachable = true;
    region.entries.push(entry);
    region.compile_strategy = CompileStrategy::Speed;
    region.callees = callees.to_vec();
    region
}

#[test]
fn observed_callees_run_inside_the_caller_and_follow_their_own_code_pages() {
    let mut transfers = Vec::new();
    for callees in [&[][..], &[ADD, XOR]] {
        let mut jit = machine(&leaf(ADD, false));
        let mut reference = machine(&leaf(ADD, false));
        jit.add_region(unit(ROOT, ROOT, 64, callees)).unwrap();
        // Rust entries count region transfers only while linking is off.
        jit.set_region_linking(false);
        for pc in [ADD, XOR, OTHER] {
            jit.add_region(unit(pc, pc, 16, &[])).unwrap();
        }
        for subtract in [false, true] {
            if subtract {
                // A callee change on another page must reach the caller's copy.
                for m in [&mut jit, &mut reference] {
                    m.protect(CODE, 4096, READ | WRITE).unwrap();
                    m.write(ADD, &leaf(ADD, true)).unwrap();
                    m.protect(CODE, 4096, READ | EXEC).unwrap();
                }
            }
            start(&mut jit);
            start(&mut reference);
            let before = jit.stats.native_entries;
            assert_eq!(jit.run(STOP, 10_000).unwrap(), RunOutcome::ReachedStop);
            assert_eq!(
                reference.run_unicorn(STOP, 10_000).unwrap(),
                RunOutcome::ReachedStop
            );
            assert_eq!(jit.state.regs, reference.state.regs, "{callees:x?}");
            assert_eq!(jit.state.mmx, reference.state.mmx);
            assert_eq!(jit.state.eflags & 0x8d5, reference.state.eflags & 0x8d5);
            if subtract {
                assert!(jit.stats.invalidations > 0);
            } else {
                assert_eq!(jit.stats.interpreter_entries, 0);
                transfers.push(jit.stats.native_entries - before);
            }
        }
    }
    // 64 calls and returns transfer between units; only the 16 calls to the
    // target that was never observed still do with callees.
    assert!(transfers[0] >= 128, "{transfers:?}");
    assert!(transfers[1] <= 40, "{transfers:?}");
}

#[test]
fn far_callees_are_dropped_without_changing_the_unit() {
    let mut jit = machine(&leaf(ADD, false));
    let mut reference = machine(&leaf(ADD, false));
    // Above the unit, and far below it: neither shares its window.
    jit.add_region(unit(ROOT, ROOT, 64, &[ROOT + 0x100, ROOT - 0x8000]))
        .unwrap();
    start(&mut jit);
    start(&mut reference);
    assert_eq!(jit.run(STOP, 10_000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(
        reference.run_unicorn(STOP, 10_000).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(jit.state.regs, reference.state.regs);
    assert_eq!(jit.state.mmx, reference.state.mmx);
}

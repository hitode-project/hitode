use adv_jit::{CompileStrategy, EXEC, Machine, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;

const CODE: u32 = 0x10000;
const ROOT: u32 = CODE + 0x31;
const HELPER: u32 = CODE + 0x401;
const STOP: u32 = 0x18000;
const SOURCE: u32 = 0x20001;
const WORK: u32 = 0x22003;
const OUTPUT: u32 = 0x24001;
const DOUBLE: u32 = 0x26000;
const STACK: u32 = 0x2f800;

fn assemble(at: u32, build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(u64::from(at)).unwrap()
}

fn machine() -> Machine {
    let mut m = Machine::new(CODE, 0x40000).unwrap();
    m.protect(CODE, 0x40000, READ | WRITE).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    m
}

fn start(m: &mut Machine, pc: u32) {
    m.state.eip = pc;
    m.state.eflags = 0x8d7;
    m.state.regs[4] = STACK;
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
}

fn region(start: u32, entry: u32, strategy: CompileStrategy) -> Region {
    let mut r = Region::new(start, 512);
    r.reachable = true;
    r.entries.push(entry);
    r.compile_strategy = strategy;
    r
}

#[test]
fn both_strategies_match_unicorn_across_mmx_repeat_fpu_and_region_calls() {
    assert_eq!(
        Region::new(CODE, 1).compile_strategy,
        CompileStrategy::Speed
    );
    let code = assemble(ROOT, |a| {
        a.mov(eax, 0x2020_2020).unwrap();
        a.mov(edi, WORK).unwrap();
        a.mov(ecx, 128).unwrap();
        a.rep().stosd().unwrap();
        a.mov(esi, SOURCE).unwrap();
        a.mov(edi, WORK).unwrap();
        a.mov(ecx, 64).unwrap();
        let mut blend = a.create_label();
        a.set_label(&mut blend).unwrap();
        a.movq(mm0, qword_ptr(esi)).unwrap();
        a.pavgb(mm0, qword_ptr(edi)).unwrap();
        a.movq(qword_ptr(edi), mm0).unwrap();
        a.add(esi, 8).unwrap();
        a.add(edi, 8).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(blend).unwrap();
        a.emms().unwrap();
        // Unsupported x87 instructions force a native -> Unicorn -> native handoff.
        a.fld1().unwrap();
        a.fstp(qword_ptr(DOUBLE as i32)).unwrap();
        a.mov(esi, WORK).unwrap();
        a.mov(edi, OUTPUT).unwrap();
        a.mov(ecx, 128).unwrap();
        a.rep().movsd().unwrap();
        a.xor(eax, eax).unwrap();
        a.mov(esi, OUTPUT).unwrap();
        a.mov(ecx, 128).unwrap();
        let mut sum = a.create_label();
        a.set_label(&mut sum).unwrap();
        a.add(eax, dword_ptr(esi)).unwrap();
        a.add(esi, 4).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(sum).unwrap();
        a.call(u64::from(HELPER)).unwrap();
        a.ret().unwrap();
    });
    let helper = assemble(HELPER, |a| {
        a.imul_3(eax, eax, -7).unwrap();
        a.shl(eax, 1).unwrap();
        a.test(al, 0x81).unwrap();
        a.ret().unwrap();
    });
    let source: Vec<_> = (0..512).map(|i| (i * 37 + i / 13) as u8).collect();
    let expected: Vec<_> = source
        .iter()
        .map(|&b| ((u16::from(b) + 0x21) >> 1) as u8)
        .collect();
    let mut reference = machine();
    reference.write(ROOT, &code).unwrap();
    reference.write(HELPER, &helper).unwrap();
    reference.write(SOURCE, &source).unwrap();
    start(&mut reference, ROOT);
    assert_eq!(
        reference.run_unicorn(STOP, 10_000).unwrap(),
        RunOutcome::ReachedStop
    );
    for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
        let mut jit = machine();
        jit.write(ROOT, &code).unwrap();
        jit.write(HELPER, &helper).unwrap();
        jit.write(SOURCE, &source).unwrap();
        jit.add_region(region(CODE, ROOT, strategy)).unwrap();
        jit.add_region(region(CODE + 0x400, HELPER, strategy))
            .unwrap();
        start(&mut jit, ROOT);
        assert_eq!(jit.run(STOP, 10_000).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(jit.state.regs, reference.state.regs, "{strategy:?}");
        assert_eq!(jit.state.eflags & 0x8d5, reference.state.eflags & 0x8d5);
        assert_eq!(jit.state.mmx, reference.state.mmx);
        assert_eq!(jit.memory().read(OUTPUT, 512).unwrap(), expected);
        assert_eq!(jit.memory().read(WORK, 512).unwrap(), expected);
        assert_eq!(jit.memory().read(DOUBLE, 8).unwrap(), 1.0f64.to_le_bytes());
        assert!(jit.stats.native_entries > 0);
        assert!(jit.stats.fallback_entries > 0);
        assert_eq!(jit.stats.bulk_bytes, 1024);
    }
}

#[test]
fn automatic_strategies_expand_indirect_roots_and_recompile_modified_code() {
    let root = CODE + 0x41;
    let first_pc = CODE + 0x121;
    let second_pc = CODE + 0x171;
    let code = assemble(root, |a| {
        a.mov(edi, dword_ptr(DOUBLE as i32)).unwrap();
        a.mov(ecx, 80).unwrap();
        a.xor(eax, eax).unwrap();
        a.xor(ebx, ebx).unwrap();
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.call(edi).unwrap();
        a.inc(eax).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
        a.ret().unwrap();
    });
    let helper = |at, amount| {
        assemble(at, |a| {
            a.add(ebx, amount).unwrap();
            a.ret().unwrap();
        })
    };
    for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
        let mut jit = machine();
        let mut reference = machine();
        for m in [&mut jit, &mut reference] {
            m.write(CODE, &[0xcc; 512]).unwrap();
            m.write(root, &code).unwrap();
            m.write(first_pc, &helper(first_pc, 2)).unwrap();
            m.write(second_pc, &helper(second_pc, 5)).unwrap();
        }
        jit.set_auto_compile_strategy(strategy);
        jit.enable_auto_jit(true);
        for (pc, expected) in [(first_pc, 160), (second_pc, 400), (second_pc, 720)] {
            if expected == 720 {
                for m in [&mut jit, &mut reference] {
                    m.write(second_pc, &helper(second_pc, 9)).unwrap();
                }
            }
            for m in [&mut jit, &mut reference] {
                m.write(DOUBLE, &pc.to_le_bytes()).unwrap();
                start(m, root);
            }
            assert_eq!(jit.run(STOP, 10_000).unwrap(), RunOutcome::ReachedStop);
            assert_eq!(
                reference.run_unicorn(STOP, 10_000).unwrap(),
                RunOutcome::ReachedStop
            );
            assert_eq!(jit.state.regs, reference.state.regs, "{strategy:?}");
            assert_eq!(jit.state.regs[3], expected);
            assert_eq!(jit.state.eflags & 0x8d5, reference.state.eflags & 0x8d5);
        }
        assert_eq!(jit.stats.compilations, 4);
        assert_eq!(jit.stats.invalidations, 1);
        assert!(jit.stats.native_entries > 0);
    }
}

#[test]
fn fast_repeat_preserves_partial_fault_and_resumes_after_page_repair() {
    const SRC: u32 = 0x30000;
    const DST: u32 = 0x40000;
    let code = assemble(ROOT, |a| {
        a.rep().movsd().unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine();
    let mut reference = machine();
    let source: Vec<_> = (0..6144).map(|i| (i * 29 + i / 11) as u8).collect();
    for m in [&mut jit, &mut reference] {
        m.write(ROOT, &code).unwrap();
        m.write(SRC, &source).unwrap();
        m.protect(DST + 4096, 4096, READ).unwrap();
        start(m, ROOT);
        m.state.regs[1] = 1536;
        m.state.regs[6] = SRC;
        m.state.regs[7] = DST;
    }
    jit.add_region(region(CODE, ROOT, CompileStrategy::Fast))
        .unwrap();
    let expected = RunOutcome::WriteFault {
        pc: ROOT,
        address: DST + 4096,
    };
    assert_eq!(jit.run(STOP, 10_000).unwrap(), expected);
    assert_eq!(reference.run_unicorn(STOP, 10_000).unwrap(), expected);
    assert_eq!(jit.state.regs, reference.state.regs);
    assert_eq!(jit.state.regs[1], 512);
    assert_eq!(jit.memory().read(DST, 4096).unwrap(), &source[..4096]);
    assert_eq!(jit.stats.bulk_bytes, 0);
    for m in [&mut jit, &mut reference] {
        m.protect(DST + 4096, 4096, READ | WRITE).unwrap();
    }
    assert_eq!(jit.run(STOP, 10_000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(
        reference.run_unicorn(STOP, 10_000).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(jit.state.regs, reference.state.regs);
    assert_eq!(jit.state.eflags & 0x8d5, reference.state.eflags & 0x8d5);
    assert_eq!(jit.memory().read(DST, 6144).unwrap(), source);
    // A partially executed fallback stays in Unicorn until it completes.
    assert_eq!(jit.stats.bulk_bytes, 0);
    start(&mut jit, ROOT);
    jit.state.regs[1] = 1536;
    jit.state.regs[6] = SRC;
    jit.state.regs[7] = DST;
    assert_eq!(jit.run(STOP, 10_000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(jit.memory().read(DST, 6144).unwrap(), source);
    assert_eq!(jit.stats.bulk_bytes, 6144);
}

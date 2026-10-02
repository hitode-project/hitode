mod support;
use adv_jit::{CpuState, EXEC, ExecutionContext, READ, Region, RunOutcome, WRITE, X87Register};
use iced_x86::code_asm::*;
use support::Machine;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const DATA: u32 = 0x20000;
const STACK: u32 = 0x2f800;

fn code(build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(u64::from(CODE)).unwrap()
}
fn machine(bytes: &[u8], native: bool) -> Machine {
    let mut m = Machine::new(CODE, 0x20000).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    m.write(CODE, bytes).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    if native {
        m.add_region(Region::new(CODE, bytes.len())).unwrap();
    }
    m
}
fn start(m: &mut Machine) {
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    m.state.eip = CODE;
    m.state.regs[4] = STACK;
}
fn raw_values() -> [X87Register; 8] {
    [
        (0, 0),                          // positive zero
        (0, 0x8000),                     // negative zero
        (1, 0),                          // smallest raw extended denormal
        (0x8000_0000_0000_0001, 0x3fff), // precision beyond binary64
        (0x8000_0000_0000_0000, 0x7fff), // infinity
        (0xc000_1234_5678_9abc, 0x7fff), // quiet NaN payload
        (0x8000_0000_0000_0042, 0xffff), // negative signaling NaN payload
        (0xffff_ffff_ffff_ffff, 0x7ffe), // largest finite extended number
    ]
    .map(|(significand, exponent)| X87Register::from_parts(significand, exponent))
}

#[test]
fn full_raw_context_is_cloneable_portable_and_independent_of_the_executor() {
    // The context is plain architectural data and may be moved to a different
    // execution thread or copied to another machine without an engine handle.
    fn require_send_sync<T: Send + Sync>() {}
    require_send_sync::<ExecutionContext>();
    let bytes = code(|a| {
        a.nop().unwrap();
        a.ret().unwrap();
    });
    let mut source = machine(&bytes, false);
    let mut receiver = machine(&bytes, false);
    receiver.write(STACK, &STOP.to_le_bytes()).unwrap();
    let mut contexts = Vec::new();
    for thread in 0..2 {
        source.state = CpuState::default();
        source.state.x87.regs = raw_values();
        source.state.x87.regs.rotate_left(thread * 3);
        source.state.x87.control = if thread == 0 { 0x027f } else { 0x0e7f };
        source.state.x87.status = 0x4500;
        source.state.x87.set_top(thread * 3 + 2);
        // Deliberately retain explicit tags rather than derive them from values.
        source.state.x87.tag = if thread == 0 { 0x1be4 } else { 0xb1e4 };
        source.state.x87.opcode = 0x05ab;
        source.state.x87.ip = 0x1234_5678 + thread as u32;
        source.state.x87.dp = 0x8765_4321 + thread as u32;
        source.state.x87.cs = 0x23;
        source.state.x87.ds = 0x2b;
        source.state.mxcsr = if thread == 0 { 0x1f80 } else { 0xffc0 };
        source.state.fs_base = DATA + thread as u32 * 128;
        source.state.xmm = std::array::from_fn(|slot| [slot as u8 + thread as u8 * 31; 16]);
        start(&mut source);
        contexts.push(source.save_context().unwrap());
    }
    for index in [1, 0, 1, 0] {
        let snapshot = contexts[index].clone();
        receiver.restore_context(&snapshot).unwrap();
        assert_eq!(&receiver.state, snapshot.cpu());
        // A real transition through Unicorn must preserve raw bits and untouched
        // tags too; the expected value is the explicitly seeded context.
        assert_eq!(
            receiver.run_unicorn(STOP, 100).unwrap(),
            RunOutcome::ReachedStop
        );
        assert_eq!(receiver.state.x87, snapshot.cpu().x87);
        assert_eq!(receiver.state.mxcsr, snapshot.cpu().mxcsr);
        assert_eq!(receiver.state.xmm, snapshot.cpu().xmm);
        assert_eq!(receiver.state.fs_base, snapshot.cpu().fs_base);
    }
    let mut edited = contexts[0].clone();
    edited.cpu_mut().x87.regs[7] = X87Register::from_parts(0xdead_beef_dead_beef, 0xffff);
    edited.cpu_mut().x87.set_top(7);
    receiver.restore_context(&edited).unwrap();
    assert_eq!(receiver.state.mmx[7], 0xdead_beef_dead_beef);
    assert_ne!(edited.cpu().x87, contexts[0].cpu().x87);
}

#[test]
fn raw_extended_guest_load_store_and_context_switch_keep_payload_and_low_bits() {
    let bytes = code(|a| {
        a.fld(tbyte_ptr(DATA as i32)).unwrap();
        a.fstp(tbyte_ptr(DATA as i32 + 32)).unwrap();
        a.ret().unwrap();
    });
    for native in [false, true] {
        let mut m = machine(&bytes, native);
        for value in [raw_values()[3], raw_values()[5], raw_values()[7]] {
            m.write(DATA, &value.0).unwrap();
            start(&mut m);
            // Stop immediately after FLD, then round-trip the complete thread.
            assert_eq!(m.run_unicorn(STOP, 1).unwrap(), RunOutcome::Budget);
            let context = m.save_context().unwrap();
            let top = m.state.x87.top();
            assert_eq!(m.state.x87.regs[top], value);
            m.state = CpuState::default();
            m.restore_context(&context).unwrap();
            let outcome = if native {
                m.run(STOP, 100)
            } else {
                m.run_unicorn(STOP, 100)
            };
            assert_eq!(outcome.unwrap(), RunOutcome::ReachedStop);
            assert_eq!(m.memory().read(DATA + 32, 10).unwrap(), value.0);
            assert_eq!(m.state.x87.tag, 0xffff);
        }
    }
}

#[test]
fn context_rounding_controls_drive_x87_and_sse_conversions_independently() {
    let bytes = code(|a| {
        a.fistp(dword_ptr(DATA as i32)).unwrap();
        a.cvtsd2si(eax, xmm0).unwrap();
        a.stmxcsr(dword_ptr(DATA as i32 + 8)).unwrap();
        a.fnstcw(word_ptr(DATA as i32 + 12)).unwrap();
        a.ret().unwrap();
    });
    for native in [false, true] {
        let mut m = machine(&bytes, native);
        let mut contexts = Vec::new();
        for mode in 0..4u16 {
            m.state = CpuState::default();
            m.state.x87.control = 0x027f | (mode << 10);
            m.state
                .x87
                .push(X87Register::from_parts(0xc000_0000_0000_0000, 0x3fff));
            m.state.mxcsr = 0x1f80 | (u32::from(3 - mode) << 13);
            m.state.xmm[0][..8].copy_from_slice(&1.5f64.to_le_bytes());
            start(&mut m);
            contexts.push(m.save_context().unwrap());
        }
        for index in [3, 1, 0, 2, 1] {
            m.restore_context(&contexts[index]).unwrap();
            let outcome = if native {
                m.run(STOP, 100)
            } else {
                m.run_unicorn(STOP, 100)
            };
            assert_eq!(outcome.unwrap(), RunOutcome::ReachedStop);
            let rounded = [2i32, 1, 2, 1];
            assert_eq!(
                m.memory().read(DATA, 4).unwrap(),
                rounded[index].to_le_bytes()
            );
            assert_eq!(m.state.regs[0], rounded[3 - index] as u32);
            assert_eq!(m.state.x87.control, contexts[index].cpu().x87.control);
            // Conversions may set sticky precision flags, but rounding/DAZ/FTZ remain.
            assert_eq!(m.state.mxcsr & !0x3f, contexts[index].cpu().mxcsr);
            assert_eq!(
                m.memory().read(DATA + 12, 2).unwrap(),
                m.state.x87.control.to_le_bytes()
            );
        }
    }
}

#[test]
fn native_mmx_writes_only_destination_exponents_and_supersedes_prior_emms() {
    let bytes = code(|a| {
        a.movd(mm0, eax).unwrap();
        a.movd(mm2, edx).unwrap();
        a.emms().unwrap();
        a.movd(mm5, ebx).unwrap();
        a.ret().unwrap();
    });
    let mut m = machine(&bytes, true);
    m.state.x87.regs = raw_values();
    m.state.x87.set_top(3);
    let before = m.state.x87.regs;
    m.state.regs[0] = 0x1234_5678;
    m.state.regs[2] = 0x89ab_cdef;
    m.state.regs[3] = 0xfedc_ba98;
    start(&mut m);
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    let snapshot = m.save_context().unwrap();
    assert_eq!(m.state.x87.top(), 0);
    assert_eq!(m.state.x87.tag, 0);
    for (slot, previous) in before.into_iter().enumerate() {
        let bits = match slot {
            0 => Some(0x1234_5678),
            2 => Some(0x89ab_cdef),
            5 => Some(0xfedc_ba98),
            _ => None,
        };
        let expected = bits.map_or(previous, |v| X87Register::from_parts(v, 0xffff));
        assert_eq!(m.state.x87.regs[slot], expected, "physical register {slot}");
    }
    m.state = CpuState::default();
    m.restore_context(&snapshot).unwrap();
    assert_eq!(m.state.mmx[2], 0x89ab_cdef);
    assert_eq!(m.state.mmx_mode, 0);
    assert_eq!(m.stats.interpreter_entries, 0);
}

#[test]
fn native_mmx_store_then_emms_retains_raw_alias_bits_but_empties_the_stack() {
    let bytes = code(|a| {
        a.movq(qword_ptr(DATA as i32), mm4).unwrap();
        a.emms().unwrap();
        a.ret().unwrap();
    });
    let mut m = machine(&bytes, true);
    m.state.x87.regs = raw_values();
    m.state.x87.set_top(6);
    let before = m.state.x87.regs;
    start(&mut m);
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    m.save_context().unwrap();
    assert_eq!(
        m.memory().read(DATA, 8).unwrap(),
        before[4].significand().to_le_bytes()
    );
    assert_eq!(m.state.x87.regs, before);
    assert_eq!(m.state.x87.tag, 0xffff);
    assert_eq!(m.state.x87.top(), 0);
}

#[test]
fn native_mmx_write_masks_survive_region_links_and_context_boundaries() {
    const HELPER: u32 = CODE + 0x1000;
    let bytes = code(|a| {
        a.movd(mm1, eax).unwrap();
        a.call(u64::from(HELPER)).unwrap();
        a.movd(mm6, ebx).unwrap();
        a.ret().unwrap();
    });
    let mut assembler = CodeAssembler::new(32).unwrap();
    assembler.movd(mm3, edx).unwrap();
    assembler.emms().unwrap();
    assembler.ret().unwrap();
    let helper = assembler.assemble(u64::from(HELPER)).unwrap();
    for links in [false, true] {
        let mut m = machine(&bytes, true);
        m.write(HELPER, &helper).unwrap();
        m.protect(HELPER, 4096, READ | EXEC).unwrap();
        m.add_region(Region::new(HELPER, helper.len())).unwrap();
        m.set_region_linking(links);
        m.state.x87.regs = raw_values();
        m.state.x87.set_top(4);
        let before = m.state.x87.regs;
        m.state.regs[0] = 0x0123_4567;
        m.state.regs[2] = 0x89ab_cdef;
        m.state.regs[3] = 0x7654_3210;
        start(&mut m);
        assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        m.save_context().unwrap();
        assert_eq!(m.state.x87.tag, 0);
        assert_eq!(m.state.x87.top(), 0);
        for (slot, previous) in before.into_iter().enumerate() {
            let written = match slot {
                1 => Some(0x0123_4567),
                3 => Some(0x89ab_cdef),
                6 => Some(0x7654_3210),
                _ => None,
            };
            assert_eq!(
                m.state.x87.regs[slot],
                written.map_or(previous, |bits| X87Register::from_parts(bits, 0xffff)),
            );
        }
        assert_eq!(m.stats.interpreter_entries, 0);
    }
}

#[test]
fn emms_alone_preserves_top_and_all_physical_payloads() {
    let bytes = code(|a| {
        a.emms().unwrap();
        a.ret().unwrap();
    });
    let mut m = machine(&bytes, true);
    m.state.x87.regs = raw_values();
    m.state.x87.tag = 0;
    m.state.x87.set_top(5);
    start(&mut m);
    let before = m.state.x87.clone();
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    m.save_context().unwrap();
    assert_eq!(m.state.x87.regs, before.regs);
    assert_eq!(m.state.x87.status, before.status);
    assert_eq!(m.state.x87.tag, 0xffff);
}

#[test]
fn mmx_fault_prefix_keeps_each_conditional_paths_written_slots_and_emms_order() {
    use adv_jit::CompileStrategy;
    const BAD: u32 = DATA + 4096;
    let bytes = code(|a| {
        let mut other = a.create_label();
        a.movd(mm0, eax).unwrap();
        a.test(ecx, ecx).unwrap();
        a.jnz(other).unwrap();
        a.movd(mm2, edx).unwrap();
        a.emms().unwrap();
        a.movq(mm3, qword_ptr(BAD as i32)).unwrap();
        a.ret().unwrap();
        a.set_label(&mut other).unwrap();
        a.movd(mm4, ebx).unwrap();
        a.movq(qword_ptr(BAD as i32), mm5).unwrap();
        a.ret().unwrap();
    });
    for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
        for branch in [0, 1] {
            let mut m = machine(&bytes, false);
            let mut region = Region::new(CODE, bytes.len());
            region.compile_strategy = strategy;
            m.add_region(region).unwrap();
            m.state.x87.regs = raw_values();
            m.state.x87.set_top(5);
            let before = m.state.x87.regs;
            m.state.regs[0] = 0x1234_5678;
            m.state.regs[1] = branch;
            m.state.regs[2] = 0x2345_6789;
            m.state.regs[3] = 0x3456_789a;
            m.write(BAD, &0xfedc_ba98_7654_3210u64.to_le_bytes())
                .unwrap();
            m.protect(BAD, 4096, 0).unwrap();
            start(&mut m);
            let outcome = m.run(STOP, 100).unwrap();
            if branch == 0 {
                assert!(matches!(
                    outcome,
                    RunOutcome::ReadFault { address: BAD, .. }
                ));
            } else {
                assert!(matches!(
                    outcome,
                    RunOutcome::WriteFault { address: BAD, .. }
                ));
            }
            let context = m.save_context().unwrap();
            assert_eq!(m.state.x87.top(), 0);
            assert_eq!(m.state.x87.tag, if branch == 0 { 0xffff } else { 0 });
            for (slot, previous) in before.into_iter().enumerate() {
                let written = match (branch, slot) {
                    (_, 0) => Some(0x1234_5678),
                    (0, 2) => Some(0x2345_6789),
                    (1, 4) => Some(0x3456_789a),
                    _ => None,
                };
                assert_eq!(
                    m.state.x87.regs[slot],
                    written.map_or(previous, |bits| X87Register::from_parts(bits, 0xffff)),
                    "branch {branch}, physical slot {slot}",
                );
            }
            // The denied memory instruction has not yet entered MMX mode or
            // written its destination. Repair and resume the saved thread.
            m.restore_context(&context).unwrap();
            m.protect(BAD, 4096, READ | WRITE).unwrap();
            // Resume this guard's instruction as a native entry; the transition
            // engine cannot retain MMX's full all-valid tag representation.
            let mut resumed = Region::new(CODE, bytes.len());
            resumed.compile_strategy = strategy;
            resumed.entries.push(context.cpu().eip);
            m.add_region(resumed).unwrap();
            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
            m.save_context().unwrap();
            assert_eq!(m.state.x87.tag, 0);
            if branch == 0 {
                assert_eq!(
                    m.state.x87.regs[3],
                    X87Register::from_parts(0xfedc_ba98_7654_3210, 0xffff)
                );
            } else {
                assert_eq!(
                    m.memory().read(BAD, 8).unwrap(),
                    before[5].significand().to_le_bytes()
                );
                assert_eq!(m.state.x87.regs[5], before[5]);
            }
        }
    }
}

mod support;
use adv_cpu::{CompileStrategy, EXEC, READ, Region, RunOutcome, WRITE};
use support::Executor;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const SRC: u32 = 0x20000;
const DST: u32 = 0x40000;
const STACK: u32 = 0x9ff800;
const VALUE: u32 = 0xa1b2_c3d4;

#[derive(Clone, Copy, Debug)]
struct Form {
    cmps: bool,
    equal: bool,
    width: u32,
}
fn forms() -> impl Iterator<Item = Form> {
    [true, false].into_iter().flat_map(|cmps| {
        [true, false].into_iter().flat_map(move |equal| {
            [1, 2, 4]
                .into_iter()
                .map(move |width| Form { cmps, equal, width })
        })
    })
}
fn instruction(form: Form) -> Vec<u8> {
    let mut bytes = vec![if form.equal { 0xf3 } else { 0xf2 }];
    if form.width == 2 {
        bytes.push(0x66);
    }
    bytes.push(match (form.cmps, form.width == 1) {
        (true, true) => 0xa6,
        (true, false) => 0xa7,
        (false, true) => 0xae,
        (false, false) => 0xaf,
    });
    bytes
}
fn machine(form: Form, jit: bool) -> Executor {
    program(instruction(form), jit)
}
fn program(mut bytes: Vec<u8>, jit: bool) -> Executor {
    let mut m = Executor::new(CODE, 0x9f0000).unwrap();
    m.protect(CODE, 0x9f0000, READ | WRITE).unwrap();
    bytes.push(0xc3);
    m.write(CODE, &bytes).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    if jit {
        m.add_region(Region::new(CODE, bytes.len())).unwrap();
    }
    m
}
fn start(m: &mut Executor, count: u32, source: u32, destination: u32, backwards: bool) {
    // This fixture starts a new guest instruction, rather than resuming a chunk.
    m.state.rep = None;
    m.state.eip = CODE;
    m.state.regs = [
        VALUE,
        count,
        0x1234,
        0x2345,
        STACK,
        0x3456,
        source,
        destination,
    ];
    m.state.eflags = 0x8d7 | if backwards { 0x400 } else { 0 };
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
}
fn put(m: &mut Executor, address: u32, width: u32, value: u32) {
    m.write(address, &value.to_le_bytes()[..width as usize])
        .unwrap();
}
fn fill(
    m: &mut Executor,
    form: Form,
    count: u32,
    source: u32,
    destination: u32,
    backwards: bool,
    stop: Option<u32>,
) {
    for i in 0..count {
        let offset = i * form.width;
        let advance = |address: u32| {
            if backwards {
                address - offset
            } else {
                address + offset
            }
        };
        if form.cmps {
            put(m, advance(source), form.width, VALUE);
        }
        let equal = form.equal != (stop == Some(i));
        put(
            m,
            advance(destination),
            form.width,
            if equal { VALUE } else { VALUE ^ 0x80 },
        );
    }
}
fn finish(jit: &mut Executor, uc: &mut Executor) {
    assert_eq!(jit.run(STOP, 10000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(
        uc.run_unicorn(STOP, 4_000_000).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(jit.state.regs, uc.state.regs);
    assert_eq!(jit.state.eflags, uc.state.eflags);
    assert_eq!(jit.state.eip, uc.state.eip);
}

#[test]
fn all_repeat_conditions_widths_directions_and_alignment_match_unicorn() {
    for form in forms() {
        for backwards in [false, true] {
            for unaligned in [0, 3] {
                for count in [0, 1, 7, 1025] {
                    for stop in [None, (count > 0).then_some(count / 2)] {
                        let (source, destination) =
                            (SRC + 0x8000 + unaligned, DST + 0x8000 + unaligned);
                        let mut jit = machine(form, true);
                        let mut uc = machine(form, false);
                        for m in [&mut jit, &mut uc] {
                            start(m, count, source, destination, backwards);
                            fill(m, form, count, source, destination, backwards, stop);
                        }
                        let original_source = jit.memory().read(SRC, 0x10000).unwrap().to_vec();
                        let original_destination =
                            jit.memory().read(DST, 0x10000).unwrap().to_vec();
                        finish(&mut jit, &mut uc);
                        assert_eq!(jit.memory().read(SRC, 0x10000).unwrap(), original_source);
                        assert_eq!(
                            jit.memory().read(DST, 0x10000).unwrap(),
                            original_destination
                        );
                        support::x64_shape!(assert_eq!(
                            jit.stats.interpreter_entries, 0,
                            "{form:?}, DF={backwards}"
                        ));
                        let compared = stop.map_or(count, |i| i + 1);
                        support::x64_shape!(assert_eq!(
                            jit.stats.bulk_bytes,
                            u64::from(compared * form.width)
                        ));
                    }
                }
            }
        }
    }
}

#[test]
fn final_subtraction_flags_cover_carry_borrow_parity_sign_and_overflow() {
    for form in forms() {
        for (left, right) in [
            (0, 0),
            (0, 1),
            (1, 0),
            (0x80, 1),
            (0x7f, 0xff),
            (0x8000, 1),
            (0x7fff, 0xffff),
            (0x8000_0000, 1),
            (0x7fff_ffff, 0xffff_ffff),
            (0x10, 1),
            (0xff, 1),
        ] {
            let mut jit = machine(form, true);
            let mut uc = machine(form, false);
            for m in [&mut jit, &mut uc] {
                start(m, 1, SRC, DST, false);
                m.state.regs[0] = left;
                put(m, SRC, form.width, left);
                put(m, DST, form.width, right);
            }
            finish(&mut jit, &mut uc);
            support::x64_shape!(assert_eq!(jit.stats.interpreter_entries, 0));
        }
    }
}

#[test]
fn zero_count_has_no_memory_access_and_preserves_all_flags() {
    for form in forms() {
        for backwards in [false, true] {
            let mut jit = machine(form, true);
            let mut uc = machine(form, false);
            for m in [&mut jit, &mut uc] {
                start(m, 0, 0xffff_ffff, 0xffff_fffe, backwards);
            }
            finish(&mut jit, &mut uc);
            assert_eq!(jit.state.eflags, 0x8d7 | if backwards { 0x400 } else { 0 });
            support::x64_shape!(assert_eq!(jit.stats.bulk_bytes, 0));
            support::x64_shape!(assert_eq!(jit.stats.interpreter_entries, 0));
        }
    }
}

#[test]
fn stopping_element_before_a_denied_page_does_not_access_it() {
    for form in forms() {
        for backwards in [false, true] {
            for deny_source in [false, true] {
                if deny_source && !form.cmps {
                    continue;
                }
                let source = if backwards {
                    SRC
                } else {
                    SRC + 4096 - form.width
                };
                let destination = if backwards {
                    DST
                } else {
                    DST + 4096 - form.width
                };
                let denied = if deny_source { SRC } else { DST };
                let denied = if backwards {
                    denied - 4096
                } else {
                    denied + 4096
                };
                let mut jit = machine(form, true);
                let mut uc = machine(form, false);
                for m in [&mut jit, &mut uc] {
                    start(m, 100, source, destination, backwards);
                    fill(m, form, 1, source, destination, backwards, Some(0));
                    m.protect(denied, 4096, 0).unwrap();
                }
                finish(&mut jit, &mut uc);
                assert_eq!(jit.state.regs[1], 99);
                support::x64_shape!(assert_eq!(jit.stats.interpreter_entries, 0));
                support::x64_shape!(assert_eq!(jit.stats.bulk_bytes, u64::from(form.width)));
            }
        }
    }
}

#[test]
fn page_fault_restores_input_flags_and_preserves_completed_progress() {
    for form in forms() {
        for backwards in [false, true] {
            for deny_source in [false, true] {
                if deny_source && !form.cmps {
                    continue;
                }
                let source = if backwards {
                    SRC + 2 * form.width
                } else {
                    SRC + 4096 - 3 * form.width
                };
                let destination = if backwards {
                    DST + 2 * form.width
                } else {
                    DST + 4096 - 3 * form.width
                };
                let denied = if deny_source { SRC } else { DST };
                let denied = if backwards {
                    denied - 4096
                } else {
                    denied + 4096
                };
                let mut jit = machine(form, true);
                let mut uc = machine(form, false);
                for m in [&mut jit, &mut uc] {
                    start(m, 10, source, destination, backwards);
                    fill(m, form, 10, source, destination, backwards, None);
                    m.protect(denied, 4096, 0).unwrap();
                }
                let input_flags = jit.state.eflags;
                // The oracle is useful for completed index/count progress, but
                // Unicorn 2.1.5 does not implement Intel's input-EFLAGS restore
                // on REP comparison faults. Assert that rule independently.
                let expected = uc.run_unicorn(STOP, 1000).unwrap();
                assert!(matches!(expected, RunOutcome::ReadFault { .. }));
                assert_eq!(jit.run(STOP, 1000).unwrap(), expected);
                assert_eq!(jit.state.regs, uc.state.regs);
                assert_eq!(jit.state.eflags, input_flags);
                assert_eq!(jit.state.eip, CODE);
                assert_eq!(jit.state.regs[1], 7);
                support::x64_shape!(assert_eq!(jit.stats.bulk_bytes, 0));
                support::x64_shape!(assert!(jit.stats.interpreter_entries > 0));
                for m in [&mut jit, &mut uc] {
                    m.protect(denied, 4096, READ).unwrap();
                }
                finish(&mut jit, &mut uc);
                assert_eq!(jit.state.regs[1], 0);
            }
        }
    }
}

#[test]
fn unaligned_denied_page_faults_before_the_first_comparison_and_restarts() {
    for form in forms().filter(|f| f.width > 1) {
        let mut jit = machine(form, true);
        let mut uc = machine(form, false);
        for m in [&mut jit, &mut uc] {
            start(m, 4, SRC + 4095, DST + 4095, false);
            fill(m, form, 4, SRC + 4095, DST + 4095, false, None);
            m.protect(DST + 4096, 4096, 0).unwrap();
        }
        let before = jit.state.clone();
        assert_eq!(
            jit.run(STOP, 1000).unwrap(),
            RunOutcome::ReadFault {
                pc: CODE,
                address: DST + 4096
            }
        );
        assert_eq!(jit.state.regs, before.regs);
        assert_eq!(jit.state.eflags, before.eflags);
        assert_eq!(jit.state.eip, CODE);
        // Unicorn can permit the straddling first read and fault at a later
        // element, so its fault ordering/address is not the specification here.
        support::x64_shape!(assert_eq!(jit.stats.bulk_bytes, 0));
        support::x64_shape!(assert!(jit.stats.interpreter_entries > 0));
        for m in [&mut jit, &mut uc] {
            m.protect(DST + 4096, 4096, READ).unwrap();
        }
        finish(&mut jit, &mut uc);
    }
}

#[test]
fn huge_counts_can_stop_early_and_long_unknown_spans_preserve_element_budget() {
    for form in forms() {
        let mut jit = machine(form, true);
        let mut uc = machine(form, false);
        for m in [&mut jit, &mut uc] {
            start(m, u32::MAX, SRC, DST, false);
            fill(m, form, 1, SRC, DST, false, Some(0));
        }
        finish(&mut jit, &mut uc);
        assert_eq!(jit.state.regs[1], u32::MAX - 1);
        support::x64_shape!(assert_eq!(jit.stats.interpreter_entries, 0));
        support::x64_shape!(assert_eq!(jit.stats.bulk_bytes, u64::from(form.width)));
    }
    let form = Form {
        cmps: false,
        equal: true,
        width: 1,
    };
    let mut jit = machine(form, true);
    let mut uc = machine(form, false);
    for m in [&mut jit, &mut uc] {
        m.write(0x100000, &vec![VALUE as u8; 2 << 20]).unwrap();
        start(m, u32::MAX, 0xffff_ffff, 0x100000, false);
    }
    // As with other guarded REP helpers, one native attempt consumes one unit,
    // then one interpreted element consumes the remaining unit.
    assert_eq!(jit.run(STOP, 2).unwrap(), RunOutcome::Budget);
    assert_eq!(uc.run_unicorn(STOP, 1).unwrap(), RunOutcome::Budget);
    assert_eq!(jit.state.regs, uc.state.regs);
    assert_eq!(jit.state.eflags, uc.state.eflags);
    assert_eq!(jit.state.eip, CODE);
    support::x64_shape!(assert_eq!(jit.stats.bulk_bytes, 0));
    support::x64_shape!(assert_eq!(jit.stats.bulk_calls, 0));
}

#[test]
fn invalid_first_element_and_overflowing_addresses_keep_precise_faults() {
    for form in forms() {
        for bad_source in [false, true] {
            if bad_source && !form.cmps {
                continue;
            }
            let mut jit = machine(form, true);
            let mut uc = machine(form, false);
            for m in [&mut jit, &mut uc] {
                start(
                    m,
                    9,
                    if bad_source { 0xffff_fffe } else { SRC },
                    if bad_source { DST } else { 0xffff_fffe },
                    false,
                );
            }
            let actual = jit.run(STOP, 1000).unwrap();
            let expected = uc.run_unicorn(STOP, 1000).unwrap();
            assert_eq!(actual, expected);
            assert!(matches!(actual, RunOutcome::ReadFault { .. }));
            assert_eq!(jit.state.regs, uc.state.regs);
            assert_eq!(jit.state.eflags, uc.state.eflags);
            support::x64_shape!(assert_eq!(jit.stats.bulk_bytes, 0));
            assert_eq!(jit.state.regs[1], 9);
        }
    }
}

#[test]
fn crt_strlen_with_unbounded_count_stops_natively_at_the_terminator() {
    // REPNE SCASB; NOT ECX; DEC ECX; MOV EAX,ECX; RET.
    let mut bytes = instruction(Form {
        cmps: false,
        equal: false,
        width: 1,
    });
    bytes.extend_from_slice(&[0xf7, 0xd1, 0x49, 0x89, 0xc8]);
    for length in [0, 1, 31, 4097, 8193] {
        let mut jit = program(bytes.clone(), true);
        let mut uc = program(bytes.clone(), false);
        for m in [&mut jit, &mut uc] {
            start(m, u32::MAX, 0xffff_ffff, DST + 3, false);
            m.state.regs[0] = 0;
            m.write(DST + 3, &vec![b'a'; length]).unwrap();
            m.write(DST + 3 + length as u32, &[0]).unwrap();
        }
        finish(&mut jit, &mut uc);
        assert_eq!(jit.state.regs[0], length as u32);
        support::x64_shape!(assert_eq!(jit.stats.bulk_bytes, length as u64 + 1));
        support::x64_shape!(assert_eq!(jit.stats.interpreter_entries, 0));
    }
}

#[test]
fn crt_memcmp_branches_use_last_comparison_flags_under_both_compile_strategies() {
    // REPE CMPSB; JE equal; MOV EAX,1; JAE return; MOV EAX,-1; RET;
    // equal: MOV EAX,0; RET. All instruction bytes are this synthetic CRT pattern.
    let bytes = vec![
        0xf3, 0xa6, 0x74, 0x0d, 0xb8, 1, 0, 0, 0, 0x73, 5, 0xb8, 0xff, 0xff, 0xff, 0xff, 0xc3,
        0xb8, 0, 0, 0, 0, 0xc3,
    ];
    for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
        for (left, right, result) in [(b'b', b'c', u32::MAX), (b'c', b'b', 1), (b'c', b'c', 0)] {
            let mut jit = program(bytes.clone(), false);
            let mut region = Region::new(CODE, bytes.len() + 1);
            region.compile_strategy = strategy;
            jit.add_region(region).unwrap();
            let mut uc = program(bytes.clone(), false);
            for m in [&mut jit, &mut uc] {
                start(m, 32, SRC, DST, false);
                m.write(SRC, &[b'a'; 32]).unwrap();
                m.write(DST, &[b'a'; 32]).unwrap();
                m.write(SRC + 12, &[left]).unwrap();
                m.write(DST + 12, &[right]).unwrap();
            }
            finish(&mut jit, &mut uc);
            assert_eq!(jit.state.regs[0], result);
            support::x64_shape!(assert_eq!(jit.stats.interpreter_entries, 0));
            support::x64_shape!(assert_eq!(
                jit.stats.bulk_bytes,
                if left == right { 32 } else { 13 }
            ));
        }
    }
}

#[test]
fn finite_large_comparisons_commit_bounded_chunks_and_restore_independent_contexts() {
    let form = Form {
        cmps: true,
        equal: true,
        width: 4,
    };
    let mut m = machine(form, true);
    m.write(0x100000, &vec![0x52; 3 << 20]).unwrap();
    m.write(0x500000, &vec![0x52; 3 << 20]).unwrap();
    start(&mut m, (3 << 20) / 4, 0x100000, 0x500000, false);
    assert_eq!(m.run(STOP, 1).unwrap(), RunOutcome::Budget);
    assert_eq!(m.state.eip, CODE);
    assert_eq!(m.state.regs[1], (2 << 20) / 4);
    assert_eq!(m.state.eflags & 0x8d5, 0x44);
    let a = m.save_context().unwrap();
    start(&mut m, (2 << 20) / 4, 0x2ffffc, 0x6ffffc, true);
    assert_eq!(m.run(STOP, 1).unwrap(), RunOutcome::Budget);
    assert_eq!(m.state.regs[1], (1 << 20) / 4);
    let b = m.save_context().unwrap();
    for (context, backwards) in [(&a, false), (&b, true)] {
        m.restore_context(context).unwrap();
        assert_eq!(m.run(STOP, 10).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[1], 0);
        assert_eq!(m.state.eflags, 0x46 | if backwards { 0x400 } else { 0 });
    }
    support::x64_shape!(assert_eq!(m.stats.bulk_bytes, 5 << 20));
    support::x64_shape!(assert_eq!(m.stats.interpreter_entries, 0));
}

#[test]
fn native_comparison_fault_after_a_slice_restores_instruction_input_flags() {
    let form = Form {
        cmps: false,
        equal: true,
        width: 1,
    };
    let mut jit = machine(form, true);
    jit.write(0x100000, &vec![VALUE as u8; 2 << 20]).unwrap();
    start(&mut jit, 2 << 20, SRC, 0x100000, false);
    let original_flags = jit.state.eflags;
    assert_eq!(jit.run(STOP, 1).unwrap(), RunOutcome::Budget);
    assert_eq!(jit.state.eflags, 0x46);
    assert!(jit.state.rep.is_some());
    let regs = jit.state.regs;
    let context = jit.save_context().unwrap();
    // Another instruction on this machine cannot replace the saved origin flags.
    start(&mut jit, 0, SRC, DST, false);
    assert_eq!(jit.run(STOP, 10).unwrap(), RunOutcome::ReachedStop);
    jit.restore_context(&context).unwrap();
    jit.protect(0x200000, 4096, 0).unwrap();
    assert_eq!(
        jit.run(STOP, 1000).unwrap(),
        RunOutcome::ReadFault {
            pc: CODE,
            address: 0x200000
        }
    );
    assert_eq!(jit.state.regs, regs);
    assert_eq!(jit.state.eflags, original_flags);
    assert_eq!(jit.state.eip, CODE);
}

#[test]
fn comparison_can_resume_from_native_through_interpreter_back_to_native() {
    let form = Form {
        cmps: false,
        equal: true,
        width: 1,
    };
    let mut jit = machine(form, true);
    jit.write(0x100000, &vec![VALUE as u8; 2 << 20]).unwrap();
    start(&mut jit, 2 << 20, SRC, 0x100000, false);
    assert_eq!(jit.run(STOP, 1).unwrap(), RunOutcome::Budget);
    support::x64_shape!(assert_eq!(jit.stats.bulk_bytes, 1 << 20));
    // The guarded scan declines the inaccessible range without committing;
    // the interpreter then commits exactly the next 256 readable elements.
    jit.protect(0x201000, 4096, 0).unwrap();
    assert_eq!(jit.run(STOP, 257).unwrap(), RunOutcome::Budget);
    assert_eq!(jit.state.regs[1], (1 << 20) - 256);
    assert_eq!(jit.state.regs[7], 0x200100);
    support::x64_shape!(assert_eq!(jit.stats.interpreter_instructions, 256));
    support::x64_shape!(assert_eq!(jit.stats.bulk_bytes, 1 << 20));
    assert!(jit.state.rep.is_some());
    // Changing an already compared byte would stop a replay from the old PC.
    jit.write(0x100000, &[VALUE as u8 ^ 1]).unwrap();
    jit.protect(0x201000, 4096, READ | WRITE).unwrap();
    assert_eq!(jit.run(STOP, 10).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(jit.state.regs[1], 0);
    assert_eq!(jit.state.regs[7], 0x300000);
    assert_eq!(jit.state.eflags, 0x46);
    assert!(jit.state.rep.is_none());
    support::x64_shape!(assert_eq!(jit.stats.interpreter_instructions, 256));
    support::x64_shape!(assert_eq!(jit.stats.bulk_bytes, (2 << 20) - 256));
}

#[test]
fn sixteen_bit_addresses_and_segment_overrides_use_the_interpreter() {
    for mut bytes in [vec![0x67, 0xf3, 0xa6], vec![0x64, 0xf3, 0xa6]] {
        // Zero count avoids a segment/address dependency while still checking that
        // these instruction forms have not been admitted to the bulk helper.
        let mut jit = program(std::mem::take(&mut bytes), true);
        start(&mut jit, 0, 0xffff_ffff, 0xffff_ffff, false);
        assert_eq!(jit.run(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
        support::x64_shape!(assert!(jit.stats.interpreter_entries > 0));
        support::x64_shape!(assert_eq!(jit.stats.bulk_bytes, 0));
    }
}

#[test]
fn comparing_executable_memory_observes_host_smc_updates() {
    let form = Form {
        cmps: true,
        equal: true,
        width: 1,
    };
    let mut m = machine(form, true);
    m.write(SRC, &[0xf3, 0xa6, 0xc3]).unwrap();
    start(&mut m, 3, SRC, CODE, false);
    assert_eq!(m.run(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[1], 0);
    let compilations = m.stats.compilations;
    // Replace REPE with REPNE through the ordinary host write/invalidation path.
    m.write(CODE, &[0xf2]).unwrap();
    start(&mut m, 3, SRC, CODE, false);
    assert_eq!(m.run(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[1], 1);
    support::x64_shape!(assert_eq!(m.stats.invalidations, 1));
    support::x64_shape!(assert!(m.stats.compilations > compilations));
    support::x64_shape!(assert_eq!(m.stats.interpreter_entries, 0));
}

mod sparse_memory {
    use super::*;
    use adv_cpu::JitCpu;
    use adv_cpu::backend::{Backend, Exit, Guest, GuestExt, Prot, Reg, TrapAction, TrapHandler};
    use std::time::{Duration, Instant};

    struct NoTrap;
    impl TrapHandler for NoTrap {
        fn on_trap(&mut self, _: &mut dyn Guest, _: u8) -> TrapAction {
            panic!("unexpected trap")
        }
    }
    fn cpu(form: Form, jit: bool) -> JitCpu {
        let mut cpu = JitCpu::new(STOP, jit).unwrap();
        cpu.map(CODE, 4096, Prot::RX).unwrap();
        cpu.map(STOP, 4096, Prot::RX).unwrap();
        cpu.map(STACK & !4095, 4096, Prot::RW).unwrap();
        let mut bytes = instruction(form);
        bytes.push(0xc3);
        cpu.write(CODE, &bytes).unwrap();
        if jit {
            cpu.executor_mut()
                .add_region(Region::new(CODE, bytes.len()))
                .unwrap();
        }
        cpu.write_u32(STACK, STOP).unwrap();
        cpu.set_reg(Reg::Esp, STACK);
        cpu.set_reg(Reg::Eip, CODE);
        cpu.set_reg(Reg::Eax, VALUE);
        cpu.set_reg(Reg::Eflags, 0x8d7);
        cpu
    }

    #[test]
    fn unmapped_later_page_early_stop_and_fault_repair_match_interpreter() {
        for form in forms() {
            for stop in [true, false] {
                let mut jit = cpu(form, true);
                let mut uc = cpu(form, false);
                for c in [&mut jit, &mut uc] {
                    c.map(SRC, 4096, Prot::RW).unwrap();
                    c.map(DST, 4096, Prot::RW).unwrap();
                    let source = SRC + 4096 - form.width;
                    let destination = DST + 4096 - form.width;
                    c.set_reg(Reg::Ecx, 2);
                    c.set_reg(Reg::Esi, source);
                    c.set_reg(Reg::Edi, destination);
                    c.write(source, &VALUE.to_le_bytes()[..form.width as usize])
                        .unwrap();
                    let equal = form.equal != stop;
                    c.write(
                        destination,
                        &(if equal { VALUE } else { VALUE ^ 0x80 }).to_le_bytes()
                            [..form.width as usize],
                    )
                    .unwrap();
                }
                let actual = jit.run(&mut NoTrap);
                let expected = uc.run(&mut NoTrap);
                assert_eq!(actual, expected, "{form:?}, stop={stop}");
                assert_eq!(jit.reg(Reg::Ecx), uc.reg(Reg::Ecx));
                assert_eq!(jit.reg(Reg::Esi), uc.reg(Reg::Esi));
                assert_eq!(jit.reg(Reg::Edi), uc.reg(Reg::Edi));
                assert_eq!(jit.reg(Reg::Eflags), uc.reg(Reg::Eflags));
                if stop {
                    assert_eq!(actual, Exit::Parked);
                    support::x64_shape!(assert_eq!(jit.stats().interpreter_entries, 0));
                } else {
                    assert!(matches!(actual, Exit::Fault(_)));
                    assert_eq!(jit.reg(Reg::Eip), CODE);
                    support::x64_shape!(assert_eq!(jit.stats().bulk_bytes, 0));
                    for c in [&mut jit, &mut uc] {
                        c.map(SRC + 4096, 4096, Prot::RW).unwrap();
                        c.map(DST + 4096, 4096, Prot::RW).unwrap();
                        c.write(SRC + 4096, &VALUE.to_le_bytes()[..form.width as usize])
                            .unwrap();
                        c.write(
                            DST + 4096,
                            &(if form.equal { VALUE } else { VALUE ^ 0x80 }).to_le_bytes()
                                [..form.width as usize],
                        )
                        .unwrap();
                        assert_eq!(c.run(&mut NoTrap), Exit::Parked);
                    }
                    assert_eq!(jit.reg(Reg::Ecx), uc.reg(Reg::Ecx));
                    assert_eq!(jit.reg(Reg::Eflags), uc.reg(Reg::Eflags));
                }
            }
        }
    }

    #[test]
    fn wall_deadline_preserves_restartable_comparison_progress() {
        const BUFFER: u32 = 0x6100_0000;
        const SIZE: u32 = 16 << 20;
        let form = Form {
            cmps: false,
            equal: true,
            width: 1,
        };
        let mut cpu = cpu(form, true);
        cpu.map(BUFFER, SIZE, Prot::RW).unwrap();
        cpu.set_reg(Reg::Eax, 0);
        cpu.set_reg(Reg::Ecx, 1);
        cpu.set_reg(Reg::Edi, BUFFER);
        // Warm compilation so the deadline exercises validation/scanning.
        assert_eq!(cpu.run(&mut NoTrap), Exit::Parked);
        cpu.set_reg(Reg::Esp, STACK);
        cpu.set_reg(Reg::Eip, CODE);
        cpu.set_reg(Reg::Ecx, SIZE);
        cpu.set_reg(Reg::Edi, BUFFER);
        cpu.set_reg(Reg::Eflags, 0x8d7);
        let budget = Duration::from_millis(1);
        cpu.set_execution_budget(Some(budget));
        let at = Instant::now();
        assert_eq!(
            cpu.run(&mut NoTrap),
            Exit::BudgetExceeded { pc: CODE, budget }
        );
        assert!(at.elapsed() < Duration::from_secs(1));
        assert!(cpu.reg(Reg::Ecx) > 0);
        assert_eq!(cpu.reg(Reg::Ecx) + cpu.reg(Reg::Edi) - BUFFER, SIZE);
        assert_eq!(
            cpu.reg(Reg::Eflags),
            if cpu.reg(Reg::Ecx) == SIZE {
                0x8d7
            } else {
                0x46
            }
        );
        cpu.set_execution_budget(None);
        assert_eq!(cpu.run(&mut NoTrap), Exit::Parked);
        assert_eq!(cpu.reg(Reg::Ecx), 0);
        assert_eq!(cpu.reg(Reg::Edi), BUFFER + SIZE);
        assert_eq!(cpu.reg(Reg::Eflags), 0x46);
        support::x64_shape!(assert_eq!(cpu.stats().interpreter_entries, 0));
    }
}

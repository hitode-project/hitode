mod support;
use adv_cpu::{
    CompileStrategy, CpuState, EXEC, ExecutionTrap, Memory, READ, Region, RunOutcome, WRITE,
};
use iced_x86::code_asm::*;
use support::Executor;

const CODE: u32 = 0x10000;
const BODY: u32 = 0x11000;
const LEAF: u32 = 0x12000;
const OTHER: u32 = 0x13000;
const STOP: u32 = 0x18000;
const STACK: u32 = 0x2f800;
const ALT_STACK: u32 = 0x2d800;

fn code(at: u32, f: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    f(&mut a);
    a.assemble(u64::from(at)).unwrap()
}
fn machine(shadow: bool, parts: &[(u32, Vec<u8>)]) -> Executor {
    let mut m = Executor::new(CODE, 0x20000).unwrap();
    m.set_shadow_returns(shadow).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    for (at, bytes) in parts {
        m.write(*at, bytes).unwrap();
        m.protect(*at, 4096, READ | EXEC).unwrap();
        let mut region = Region::new(*at, bytes.len());
        region.reachable = true;
        region.compile_strategy = CompileStrategy::Fast;
        m.add_region(region).unwrap();
    }
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    m.state.eip = CODE;
    m.state.eflags = 0x202;
    m.state.regs[4] = STACK;
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    m
}
fn root() -> Vec<u8> {
    code(CODE, |a| {
        a.call(u64::from(BODY)).unwrap();
        a.inc(eax).unwrap();
        a.ret().unwrap();
    })
}
fn assert_oracle(parts: &[(u32, Vec<u8>)], configure: impl Fn(&mut Executor)) -> Executor {
    let mut oracle = machine(false, parts);
    configure(&mut oracle);
    assert_eq!(
        oracle.run_unicorn(STOP, 10_000).unwrap(),
        RunOutcome::ReachedStop
    );
    let mut m = machine(true, parts);
    configure(&mut m);
    assert_eq!(m.run(STOP, 10_000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs, oracle.state.regs);
    assert_eq!(m.state.eflags, oracle.state.eflags);
    assert_eq!(m.state.xmm, oracle.state.xmm);
    assert_eq!(m.state.x87, oracle.state.x87);
    assert_eq!(m.stats.interpreter_entries, 0);
    m
}

#[test]
fn local_return_hint_does_not_push_or_consume_the_outer_cross_region_frame() {
    let mut body = code(BODY, |a| {
        a.call(u64::from(BODY + 32)).unwrap();
        a.ret().unwrap();
    });
    body.resize(32, 0xcc);
    body.extend(code(BODY + 32, |a| {
        a.inc(eax).unwrap();
        a.ret().unwrap();
    }));
    let m = assert_oracle(&[(CODE, root()), (BODY, body)], |_| {});
    assert_eq!(m.stats.shadow_matches, 1);
    assert_eq!(m.stats.shadow_hits, 1);
    assert_eq!(m.stats.shadow_misses, 1); // Root's final RET has no native caller.
}

#[test]
fn mixed_local_and_cross_region_calls_keep_every_frame_balanced() {
    let mut body = code(BODY, |a| {
        a.call(u64::from(BODY + 32)).unwrap();
        a.ret().unwrap();
    });
    body.resize(32, 0xcc);
    body.extend(code(BODY + 32, |a| {
        a.call(u64::from(LEAF)).unwrap();
        a.inc(eax).unwrap();
        a.ret().unwrap();
    }));
    let leaf = code(LEAF, |a| {
        a.inc(eax).unwrap();
        a.ret().unwrap();
    });
    let m = assert_oracle(&[(CODE, root()), (BODY, body), (LEAF, leaf)], |_| {});
    assert_eq!(m.stats.shadow_matches, 3);
    assert_eq!(m.stats.shadow_hits, 2); // Local transfer stays inside the body.
    assert_eq!(m.stats.shadow_misses, 1);
}

#[test]
fn changed_guest_return_address_uses_the_actual_target() {
    let body = code(BODY, |a| {
        a.mov(dword_ptr(esp), OTHER).unwrap();
        a.ret().unwrap();
    });
    let other = code(OTHER, |a| {
        a.add(eax, 7).unwrap();
        a.ret().unwrap();
    });
    let m = assert_oracle(&[(CODE, root()), (BODY, body), (OTHER, other)], |_| {});
    assert_eq!(m.state.regs[0], 7); // The original caller continuation is skipped.
    assert_eq!(m.stats.shadow_hits, 0);
    assert!(m.stats.shadow_misses >= 1);
}

#[test]
fn equal_return_pc_on_a_different_guest_stack_cannot_hit_the_hint() {
    let body = code(BODY, |a| {
        a.mov(esp, ALT_STACK).unwrap();
        a.ret().unwrap();
    });
    let m = assert_oracle(&[(CODE, root()), (BODY, body)], |m| {
        m.write(ALT_STACK, &(CODE + 5).to_le_bytes()).unwrap();
        m.write(ALT_STACK + 4, &STOP.to_le_bytes()).unwrap();
    });
    assert_eq!(m.state.regs[4], ALT_STACK + 8);
    assert_eq!(m.stats.shadow_hits, 0);
    assert_eq!(m.stats.shadow_matches, 0);
}

#[test]
fn nonlocal_jump_skipping_a_frame_preserves_guest_semantics() {
    let body = code(BODY, |a| {
        a.mov(esp, STACK).unwrap();
        a.jmp(u64::from(CODE + 5)).unwrap();
    });
    let m = assert_oracle(&[(CODE, root()), (BODY, body)], |_| {});
    assert_eq!(m.state.regs[0], 1);
    assert_eq!(m.state.regs[4], STACK + 4);
    assert_eq!(m.stats.shadow_hits, 0);
}

#[test]
fn ret_imm_validates_the_original_sp_before_cleaning_arguments() {
    let main = code(CODE, |a| {
        a.push(7).unwrap();
        a.call(u64::from(BODY)).unwrap();
        a.inc(eax).unwrap();
        a.ret().unwrap();
    });
    let body = code(BODY, |a| {
        a.ret_1(4).unwrap();
    });
    let m = assert_oracle(&[(CODE, main), (BODY, body)], |_| {});
    assert_eq!(m.state.regs[4], STACK + 4);
    assert_eq!(m.stats.shadow_hits, 1);
}

#[test]
fn a_valid_hint_never_bypasses_the_real_guest_stack_read_permission() {
    let body = code(BODY, |a| {
        a.inc(eax).unwrap();
        a.ret().unwrap();
    });
    let mut m = machine(true, &[(CODE, root()), (BODY, body)]);
    m.protect(STACK & !4095, 4096, WRITE).unwrap();
    assert_eq!(
        m.run(STOP, 100).unwrap(),
        RunOutcome::ReadFault {
            pc: BODY + 1,
            address: STACK - 4
        }
    );
    assert_eq!(m.state.regs[4], STACK - 4);
    assert_eq!(m.state.regs[0], 1);
    assert_eq!(m.stats.shadow_matches, 0);
    assert_eq!(m.stats.shadow_hits, 0);
    m.protect(STACK & !4095, 4096, READ | WRITE).unwrap();
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(
        m.state.regs[0], 2,
        "the completed callee prefix was replayed"
    );
    assert_eq!(m.stats.shadow_hits, 0, "a fault retained a native hint");
}

#[test]
fn faulting_call_does_not_commit_sp_or_publish_a_hint() {
    let mut m = machine(true, &[(CODE, root()), (BODY, vec![0xc3])]);
    m.protect(STACK & !4095, 4096, READ).unwrap();
    assert_eq!(
        m.run(STOP, 100).unwrap(),
        RunOutcome::WriteFault {
            pc: CODE,
            address: STACK - 4
        }
    );
    assert_eq!(m.state.regs[4], STACK);
    assert_eq!(
        m.stats.shadow_matches + m.stats.shadow_hits + m.stats.shadow_overflows,
        0
    );
    m.protect(STACK & !4095, 4096, READ | WRITE).unwrap();
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.stats.shadow_hits, 1);
}

#[test]
fn recursion_beyond_capacity_falls_back_without_dropping_guest_frames() {
    let main = code(CODE, |a| {
        a.mov(ecx, 80).unwrap();
        a.call(u64::from(BODY)).unwrap();
        a.ret().unwrap();
    });
    let body = code(BODY, |a| {
        let mut end = a.create_label();
        a.dec(ecx).unwrap();
        a.jz(end).unwrap();
        a.call(u64::from(LEAF)).unwrap();
        a.set_label(&mut end).unwrap();
        a.ret().unwrap();
    });
    let leaf = code(LEAF, |a| {
        a.inc(eax).unwrap();
        a.call(u64::from(BODY)).unwrap();
        a.ret().unwrap();
    });
    let m = assert_oracle(&[(CODE, main), (BODY, body), (LEAF, leaf)], |_| {});
    assert_eq!(m.state.regs[0], 79);
    assert_eq!(m.state.regs[4], STACK + 4);
    assert!(m.stats.shadow_hits > 0 && m.stats.shadow_misses > 0);
    assert_eq!(m.stats.shadow_overflows, 2);
}

#[test]
fn budget_and_context_switches_never_restore_prediction_metadata() {
    let body = code(BODY, |a| {
        a.inc(eax).unwrap();
        a.ret().unwrap();
    });
    let mut m = machine(true, &[(CODE, root()), (BODY, body), (OTHER, vec![0xc3])]);
    assert_eq!(m.run(STOP, 1).unwrap(), RunOutcome::Budget);
    assert_eq!(m.state.eip, BODY);
    let suspended = m.save_context().unwrap();
    m.state.eip = OTHER;
    m.state.regs[4] = ALT_STACK;
    m.write(ALT_STACK, &STOP.to_le_bytes()).unwrap();
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    m.restore_context(&suspended).unwrap();
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[0], 2);
    assert_eq!(m.stats.shadow_hits, 0);
}

#[test]
fn smc_and_native_retirement_cannot_reuse_a_saved_caller_pointer() {
    for evict in [false, true] {
        let mut m = machine(true, &[(CODE, root()), (BODY, vec![0xc3])]);
        assert_eq!(m.run(STOP, 1).unwrap(), RunOutcome::Budget);
        if evict {
            m.set_code_cache_limits(1, 32 * 1024 * 1024);
            // Re-publication evicts the now older caller's real code owner.
            m.add_region(Region::new(BODY, 1)).unwrap();
            assert!(m.stats.cache_evictions > 0);
        } else {
            // Replace INC EAX at the old continuation with INC EBX.
            m.write(CODE + 5, &[0x43]).unwrap();
        }
        assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], u32::from(evict));
        assert_eq!(m.state.regs[3], u32::from(!evict));
        assert_eq!(m.stats.shadow_hits, 0);
    }
}

#[test]
fn a_host_trap_that_resumes_natively_still_clears_the_prediction_stack() {
    struct Handler;
    impl ExecutionTrap for Handler {
        fn call(
            &mut self,
            state: &mut CpuState,
            _: &mut Memory,
            vector: u32,
        ) -> Result<bool, String> {
            assert_eq!(vector, 0x2e);
            state.regs[0] += 7;
            Ok(false)
        }
    }
    let body = code(BODY, |a| {
        a.inc(eax).unwrap();
        a.int(0x2e).unwrap();
        a.ret().unwrap();
    });
    let mut m = machine(true, &[(CODE, root()), (BODY, body)]);
    assert_eq!(
        m.run_with_traps(STOP, 100, &mut Handler, false).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(m.state.regs[0], 9);
    assert_eq!(m.stats.trap_calls, 1);
    assert_eq!(m.stats.shadow_matches, 0);
    assert_eq!(m.stats.shadow_hits, 0);
}

#[test]
fn codegen_switch_is_immutable_after_native_compilation() {
    let mut m = machine(true, &[(CODE, root()), (BODY, vec![0xc3])]);
    assert!(m.set_shadow_returns(false).is_err());
    m.set_shadow_returns(true).unwrap();
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.stats.shadow_hits, 1);
}

#[test]
fn an_unsupported_continuation_never_becomes_a_native_hint_or_an_extra_budget_charge() {
    let main = code(CODE, |a| {
        a.call(u64::from(BODY)).unwrap();
        a.fld1().unwrap();
        a.fstp(qword_ptr(0x23000)).unwrap();
        a.inc(eax).unwrap();
        a.ret().unwrap();
    });
    let mut plain = machine(false, &[(CODE, main.clone()), (BODY, vec![0xc3])]);
    let mut predicted = machine(true, &[(CODE, main), (BODY, vec![0xc3])]);
    assert_eq!(plain.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(predicted.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(predicted.state, plain.state);
    assert_eq!(predicted.stats.native_blocks, plain.stats.native_blocks);
    assert_eq!(
        predicted.stats.interpreter_instructions,
        plain.stats.interpreter_instructions
    );
    assert_eq!(predicted.stats.shadow_hits, 0);
    assert_eq!(
        predicted.memory().read(0x23000, 8).unwrap(),
        1.0f64.to_le_bytes()
    );
}

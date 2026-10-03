mod support;
use adv_cpu::*;
use iced_x86::{BlockEncoderOptions, code_asm::*};
use support::Machine;

const CODE: u32 = 0x10000;
const API: u32 = 0x11000;
const STOP: u32 = 0x18000;
const PAD: u32 = 0x18010;
const STACK: u32 = 0x2f000;
const DATA: u32 = 0x20000;

struct Services {
    log: Vec<String>,
}
impl HostServices for Services {
    fn call(&mut self, call: ApiCall<'_>, _: &mut ApiMemory<'_>) -> Result<ApiOutcome, String> {
        self.log.push(format!("call:{}", call.spec.id));
        Ok(match call.spec.id {
            1 => ApiOutcome::Return(call.args[0].wrapping_add(7)),
            2 => ApiOutcome::Callback {
                target: call.args[0],
                args: vec![call.args[1]],
                convention: Convention::Stdcall(1),
                continuation: 20,
            },
            3 => ApiOutcome::Wait {
                wait: Wait::Event(42),
                continuation: 30,
            },
            _ => return Err("unknown API".into()),
        })
    }
    fn resume(
        &mut self,
        token: u64,
        reason: Resume,
        _: &mut ApiMemory<'_>,
    ) -> Result<ApiOutcome, String> {
        self.log.push(format!("resume:{token}:{reason:?}"));
        Ok(match (token, reason) {
            (20, Resume::Returned(value)) => ApiOutcome::Return(value + 100),
            (30, Resume::Signaled(42)) => ApiOutcome::Return(5),
            _ => return Err("wrong continuation".into()),
        })
    }
}

fn machine(code: &[u8]) -> Machine {
    let mut m = Machine::new(CODE, 0x20000).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    m.write(CODE, code).unwrap();
    for (n, count) in [(1, 1), (2, 2), (3, 0)] {
        m.register_api(ApiSpec::new(
            n,
            "test.dll",
            "test",
            API + (n - 1) * 16,
            Convention::Stdcall(count),
        ))
        .unwrap();
    }
    m.protect(CODE, 0x2000, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    m.set_callback_return_pad(PAD).unwrap();
    m.set_region(Region::new(CODE, 0x1030)).unwrap();
    m.state.eip = CODE;
    m.state.regs[4] = STACK;
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    m
}

#[test]
fn api_loop_stays_in_one_native_invocation_and_matches_unicorn() {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    a.mov(ecx, 1000).unwrap();
    a.xor(ebx, ebx).unwrap();
    a.set_label(&mut top).unwrap();
    a.push(ecx).unwrap();
    a.call(u64::from(API)).unwrap();
    a.add(ebx, eax).unwrap();
    a.dec(ecx).unwrap();
    a.jnz(top).unwrap();
    a.ret().unwrap();
    let code = a.assemble(u64::from(CODE)).unwrap();
    let mut jit = machine(&code);
    let mut uc = machine(&code);
    let mut js = Services { log: Vec::new() };
    let mut us = Services { log: Vec::new() };
    assert_eq!(
        jit.run_with_services(STOP, 100_000, &mut js).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(
        uc.run_unicorn_with_services(STOP, 100_000, &mut us)
            .unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(jit.state.regs, uc.state.regs);
    assert_eq!(js.log, us.log);
    assert_eq!(jit.stats.native_entries, 1);
    assert_eq!(jit.stats.interpreter_entries, 0);
    assert_eq!(jit.stats.host_calls, 1000);
    assert_eq!(jit.state.regs[3], 507500);
}

#[test]
fn nested_callbacks_can_wait_then_resume_in_the_right_order() {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut callback = a.create_label();
    let mut inner = a.create_label();
    // Callback addresses are supplied through data so the fixture has real indirect calls.
    a.push(10).unwrap();
    a.push(dword_ptr(DATA as i32)).unwrap();
    a.call(u64::from(API + 16)).unwrap();
    a.ret().unwrap();
    a.set_label(&mut callback).unwrap();
    a.push(dword_ptr(esp + 4)).unwrap();
    a.push(dword_ptr(DATA as i32 + 4)).unwrap();
    a.call(u64::from(API + 16)).unwrap();
    a.add(eax, 1).unwrap();
    a.ret_1(4).unwrap();
    a.set_label(&mut inner).unwrap();
    a.call(u64::from(API + 32)).unwrap();
    a.add(eax, dword_ptr(esp + 4)).unwrap();
    a.push(eax).unwrap();
    a.call(u64::from(API)).unwrap();
    a.ret_1(4).unwrap();
    let result = a
        .assemble_options(
            u64::from(CODE),
            BlockEncoderOptions::RETURN_NEW_INSTRUCTION_OFFSETS,
        )
        .unwrap();
    let callback_pc = result.label_ip(&callback).unwrap() as u32;
    let inner_pc = result.label_ip(&inner).unwrap() as u32;
    let mut results = Vec::new();
    for (unicorn, budget) in [(false, 10000), (true, 10000), (false, 1), (true, 1)] {
        let mut m = machine(&result.inner.code_buffer);
        m.write(DATA, &callback_pc.to_le_bytes()).unwrap();
        m.write(DATA + 4, &inner_pc.to_le_bytes()).unwrap();
        let mut region = Region::new(CODE, 0x1030);
        region.entries = vec![callback_pc, inner_pc];
        m.set_region(region).unwrap();
        let mut services = Services { log: Vec::new() };
        let run = |m: &mut Machine, s: &mut Services| {
            for _ in 0..10000 {
                let result = if unicorn {
                    m.run_unicorn_with_services(STOP, budget, s)
                } else {
                    m.run_with_services(STOP, budget, s)
                };
                if result != Ok(RunOutcome::Budget) {
                    return result;
                }
            }
            panic!("callback scheduler made no progress");
        };
        assert_eq!(
            run(&mut m, &mut services).unwrap(),
            RunOutcome::Waiting(Wait::Event(42))
        );
        let paused = m.state.clone();
        let count = services.log.len();
        assert!(!m.wake(Resume::Signaled(7)));
        assert_eq!(
            run(&mut m, &mut services).unwrap(),
            RunOutcome::Waiting(Wait::Event(42))
        );
        assert_eq!(services.log.len(), count);
        assert_eq!(m.state.regs, paused.regs);
        assert!(m.wake(Resume::Signaled(42)));
        assert!(!m.wake(Resume::Signaled(42)));
        assert_eq!(run(&mut m, &mut services).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 223);
        assert_eq!(m.state.regs[4], STACK + 4);
        if !unicorn {
            assert_eq!(m.stats.interpreter_entries, 0);
        }
        results.push(services.log);
    }
    assert!(results.iter().all(|log| *log == results[0]));
}

#[test]
fn relocated_api_trap_in_an_inline_hook_trampoline_keeps_its_identity() {
    let mut a = CodeAssembler::new(32).unwrap();
    a.push(9).unwrap();
    a.call(u64::from(API)).unwrap();
    a.ret().unwrap();
    let code = a.assemble(u64::from(CODE)).unwrap();
    let mut m = machine(&code);
    let mut services = Services { log: Vec::new() };
    assert_eq!(
        m.run_with_services(STOP, 100, &mut services).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(m.state.regs[0], 16);
    let hook = CODE + 0x200;
    let trampoline = CODE + 0x300;
    // Copy the complete API thunk, including INT, to a different guest address.
    let original = m.memory().read(API, 16).unwrap().to_vec();
    m.write(trampoline, &original).unwrap();
    let mut patch = CodeAssembler::new(32).unwrap();
    patch.jmp(u64::from(hook)).unwrap();
    m.write(API, &patch.assemble(u64::from(API)).unwrap())
        .unwrap();
    let mut detour = CodeAssembler::new(32).unwrap();
    detour.push(dword_ptr(esp + 4)).unwrap();
    detour.call(u64::from(trampoline)).unwrap();
    detour.add(eax, 100).unwrap();
    detour.ret_1(4).unwrap();
    m.write(hook, &detour.assemble(u64::from(hook)).unwrap())
        .unwrap();
    m.state.eip = CODE;
    m.state.regs[4] = STACK;
    assert_eq!(
        m.run_with_services(STOP, 1000, &mut services).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(m.state.regs[0], 116);
    assert_eq!(m.stats.host_calls, 2);
    assert!(m.stats.invalidations > 0);
    assert_eq!(m.stats.interpreter_entries, 0);
}

#[test]
fn cdecl_callback_and_deadline_resume_invalidate_warm_fallback_code() {
    const COLD: u32 = 0x15000;
    struct Continuations {
        log: Vec<Resume>,
    }
    impl HostServices for Continuations {
        fn call(&mut self, c: ApiCall<'_>, _: &mut ApiMemory<'_>) -> Result<ApiOutcome, String> {
            Ok(ApiOutcome::Callback {
                target: c.args[0],
                args: vec![c.args[1]],
                convention: Convention::Cdecl(1),
                continuation: 1,
            })
        }
        fn resume(
            &mut self,
            token: u64,
            why: Resume,
            m: &mut ApiMemory<'_>,
        ) -> Result<ApiOutcome, String> {
            self.log.push(why);
            Ok(match (token, why) {
                (1, Resume::Returned(16)) => ApiOutcome::Wait {
                    wait: Wait::Until(100),
                    continuation: 2,
                },
                (2, Resume::Deadline(100)) => {
                    m.put_u32(COLD + 1, 77)?;
                    ApiOutcome::Return(16)
                }
                _ => return Err("unexpected continuation".into()),
            })
        }
    }
    let mut a = CodeAssembler::new(32).unwrap();
    let mut cb = a.create_label();
    a.push(9).unwrap();
    a.push(dword_ptr(DATA as i32)).unwrap();
    a.call(u64::from(API + 48)).unwrap();
    a.add(esp, 8).unwrap();
    a.mov(ebx, eax).unwrap();
    a.call(u64::from(COLD)).unwrap();
    a.ret().unwrap();
    a.set_label(&mut cb).unwrap();
    a.mov(eax, dword_ptr(esp + 4)).unwrap();
    a.add(eax, 7).unwrap();
    a.ret().unwrap();
    let code = a
        .assemble_options(
            u64::from(CODE),
            BlockEncoderOptions::RETURN_NEW_INSTRUCTION_OFFSETS,
        )
        .unwrap();
    for unicorn in [false, true] {
        let mut m = machine(&code.inner.code_buffer);
        m.register_api(ApiSpec::new(
            4,
            "test",
            "cdecl",
            API + 48,
            Convention::Cdecl(2),
        ))
        .unwrap();
        m.write(DATA, &(code.label_ip(&cb).unwrap() as u32).to_le_bytes())
            .unwrap();
        let mut region = Region::new(CODE, 0x1040);
        region.entries.push(code.label_ip(&cb).unwrap() as u32);
        m.set_region(region).unwrap();
        m.write(COLD, &[0xb8, 1, 0, 0, 0, 0xc3]).unwrap();
        m.protect(COLD, 4096, READ | WRITE | EXEC).unwrap();
        m.state.eip = COLD;
        assert_eq!(m.run_unicorn(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 1);
        m.state.eip = CODE;
        m.state.regs[4] = STACK;
        let mut s = Continuations { log: Vec::new() };
        let run = |m: &mut Machine, s: &mut Continuations| {
            if unicorn {
                m.run_unicorn_with_services(STOP, 1000, s)
            } else {
                m.run_with_services(STOP, 1000, s)
            }
        };
        assert_eq!(
            run(&mut m, &mut s).unwrap(),
            RunOutcome::Waiting(Wait::Until(100))
        );
        assert!(!m.wake(Resume::Deadline(99)));
        assert!(!m.wake(Resume::Signaled(100)));
        assert!(m.wake(Resume::Deadline(100)));
        assert_eq!(run(&mut m, &mut s).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 77);
        assert_eq!(m.state.regs[3], 16);
        assert_eq!(m.state.regs[4], STACK + 4);
        assert_eq!(s.log, vec![Resume::Returned(16), Resume::Deadline(100)]);
    }
}

#[test]
fn handler_code_write_exits_before_executing_stale_native_code() {
    struct Patch;
    impl HostServices for Patch {
        fn call(&mut self, c: ApiCall<'_>, m: &mut ApiMemory<'_>) -> Result<ApiOutcome, String> {
            m.put_u32(c.args[0], 99)?;
            Ok(ApiOutcome::Return(7))
        }
    }
    let mut a = CodeAssembler::new(32).unwrap();
    let mut changed = a.create_label();
    a.push(dword_ptr(DATA as i32)).unwrap();
    a.call(u64::from(API)).unwrap();
    a.set_label(&mut changed).unwrap();
    a.mov(eax, 1).unwrap();
    a.ret().unwrap();
    let code = a
        .assemble_options(
            u64::from(CODE),
            BlockEncoderOptions::RETURN_NEW_INSTRUCTION_OFFSETS,
        )
        .unwrap();
    for unicorn in [false, true] {
        let mut m = machine(&code.inner.code_buffer);
        m.protect(CODE, 4096, READ | WRITE | EXEC).unwrap();
        m.write(
            DATA,
            &(code.label_ip(&changed).unwrap() as u32 + 1).to_le_bytes(),
        )
        .unwrap();
        let result = if unicorn {
            m.run_unicorn_with_services(STOP, 1000, &mut Patch)
        } else {
            m.run_with_services(STOP, 1000, &mut Patch)
        };
        assert_eq!(result.unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 99);
        assert_eq!(m.stats.host_calls, 1);
        if !unicorn {
            assert_eq!(m.stats.interpreter_entries, 0);
            assert!(m.stats.invalidations >= 1);
        }
    }
}

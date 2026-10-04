//! Authored guest callbacks on the sole Machine execution owner. No PE loader,
//! native callback worker, audio device, game or host thread is started here.
//! The tiny thread-entry fixture deliberately omits unrelated loader callbacks.
use adv_abi::Registry;
use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::layout::{PAGE, PARK_ADDR};
use adv_machine::{Clock, Conv, Machine, Object, ObjectLimits, Objects, Outcome, RunResult, Wait};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const CALLBACK: u32 = CODE + 0x4000;
const CREATOR: u32 = CODE + 0x8000;
const DATA: u32 = 0x410000;
const ID: u32 = DATA;
const OUT: u32 = DATA + 0x100;
const COUNT: u32 = DATA + 0x200;
const ENDED: u32 = DATA + 0x204;
const AFTER_KILL: u32 = DATA + 0x208;
const CALLBACK_OBSERVED: u32 = DATA + 0x20c;

enum Arg {
    Value(u32),
    Stored(u32),
}
use Arg::{Stored, Value};

struct Test {
    machine: Machine<UnicornCpu>,
    asm: Asm,
}

impl Test {
    fn new(limits: ObjectLimits) -> Self {
        let mut registry = Registry::default();
        adv_win32::winmm::register(&mut registry);
        registry.add(
            "<win32>",
            "thread_start",
            Conv::Stdcall { argc: 2 },
            |env| {
                Outcome::call(env.arg(0), vec![env.arg(1)], |_, returned| {
                    Outcome::ExitThread(returned.value())
                })
            },
        );
        registry.add("test.dll", "finish", Conv::Stdcall { argc: 0 }, |_| {
            Outcome::ExitProcess(0)
        });
        registry.add("test.dll", "exit_thread", Conv::Stdcall { argc: 0 }, |_| {
            Outcome::ExitThread(0)
        });
        registry.add("test.dll", "sleep", Conv::Stdcall { argc: 1 }, |env| {
            Outcome::block_then_return(
                Wait::Until(env.sys.clock.guest_now_us() + u64::from(env.arg(0)) * 1000),
                0,
            )
        });
        registry.add("test.dll", "objects", Conv::Stdcall { argc: 0 }, |env| {
            Outcome::Return(env.sys.objects.object_count() as u32)
        });
        registry.add("test.dll", "wait", Conv::Stdcall { argc: 1 }, |env| {
            Outcome::block_then_return(
                Wait::Object {
                    handle: env.arg(0),
                    deadline: None,
                },
                0,
            )
        });
        registry.add("test.dll", "signal", Conv::Stdcall { argc: 1 }, |env| {
            env.sys.set_event(env.arg(0));
            Outcome::Return(0)
        });
        registry.add("test.dll", "event", Conv::Stdcall { argc: 0 }, |env| {
            Outcome::Return(env.sys.objects.insert(Object::Event {
                manual_reset: true,
                signaled: false,
            }))
        });
        registry.add("test.dll", "close", Conv::Stdcall { argc: 1 }, |env| {
            Outcome::Return(u32::from(env.sys.objects.close(env.arg(0)).is_some()))
        });
        let mut machine = Machine::new(
            UnicornCpu::new(PARK_ADDR).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        machine.sys.objects = Objects::with_limits(limits);
        machine.cpu.map(CODE, PAGE * 16, Prot::RX).unwrap();
        machine.cpu.map(DATA, PAGE, Prot::RW).unwrap();
        machine
            .spawn("timer test main", CODE, &[], 0x10000)
            .unwrap();
        Self {
            machine,
            asm: Asm::new(CODE),
        }
    }

    fn emit_call(&mut self, asm: &mut Asm, dll: &str, name: &str, args: &[Arg], out: u32) {
        for arg in args.iter().rev() {
            match arg {
                Value(v) => {
                    asm.push_imm(*v);
                }
                Stored(addr) => {
                    asm.mov_eax_mem(*addr).push_eax();
                }
            }
        }
        let thunk = self.machine.resolve_import(dll, name).unwrap();
        asm.call(thunk).mov_mem_eax(out);
    }

    fn call(&mut self, dll: &str, name: &str, args: &[Arg], out: u32) {
        // Move the builder, rather than giving a host handler test-only access
        // to private timer state or bypassing the actual guest call ABI.
        let mut asm = std::mem::replace(&mut self.asm, Asm::new(CODE));
        self.emit_call(&mut asm, dll, name, args, out);
        self.asm = asm;
    }

    fn set(&mut self, delay: u32, target: u32, flags: u32, out: u32) {
        self.call(
            "winmm.dll",
            "timeSetEvent",
            &[
                Value(delay),
                Value(0),
                Value(target),
                Value(0x12345678),
                Value(flags),
            ],
            out,
        );
    }
    fn kill(&mut self, id: u32, out: u32) {
        self.call("winmm.dll", "timeKillEvent", &[Stored(id)], out);
    }
    fn sleep(&mut self, ms: u32) {
        self.call("test.dll", "sleep", &[Value(ms)], OUT + 0xfc);
    }
    fn snapshot(&mut self, out: u32) {
        self.call("test.dll", "objects", &[], out);
    }
    fn event(&mut self) -> u32 {
        let handle = self.machine.sys.objects.insert(Object::Event {
            manual_reset: true,
            signaled: false,
        });
        assert_ne!(handle, 0);
        handle
    }
    fn code(&mut self, asm: Asm, address: u32) {
        self.machine.cpu.write(address, &asm.bytes).unwrap();
    }
    fn run(&mut self) {
        self.call("test.dll", "finish", &[], OUT + 0xfc);
        self.machine.cpu.write(CODE, &self.asm.bytes).unwrap();
        assert!(matches!(
            self.machine.run_virtual(1_000_000),
            RunResult::Exited(0)
        ));
    }
    fn word(&self, address: u32) -> u32 {
        self.machine.cpu.read_u32(address).unwrap()
    }
}

fn increment(asm: &mut Asm, address: u32) {
    asm.mov_eax_mem(address).add_eax_imm(1).mov_mem_eax(address);
}

#[test]
fn object_alias_and_namespace_exhaustion_leave_no_timer_events_or_worker() {
    let limits = (1..=4)
        .map(|limit| ObjectLimits {
            objects: limit,
            ..ObjectLimits::default()
        })
        .chain((1..=4).map(|limit| ObjectLimits {
            aliases: limit,
            ..ObjectLimits::default()
        }))
        .chain([ObjectLimits {
            namespaces: 1,
            ..ObjectLimits::default()
        }]);
    for limits in limits {
        let mut test = Test::new(limits);
        test.set(10, CALLBACK, 0, ID);
        test.set(10, CALLBACK, 0, ID + 4);
        test.run();
        assert_eq!(test.word(ID), 0, "{limits:?}");
        assert_eq!(test.word(ID + 4), 0, "{limits:?}");
        assert_eq!(test.machine.sys.objects.object_count(), 1, "{limits:?}");
        assert_eq!(test.machine.sys.contexts.len(), 1, "{limits:?}");
    }
}

#[test]
fn full_win32_startup_budget_is_unchanged_by_failed_timer_creation() {
    // Full startup retains its loader mutex as well as the main thread. Compare
    // before/after API counts, rather than pretending the mutex is a timer leak.
    for limit in 2..=5 {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        registry.add("test.dll", "objects", Conv::Stdcall { argc: 0 }, |env| {
            Outcome::Return(env.sys.objects.object_count() as u32)
        });
        let mut machine = Machine::new(
            UnicornCpu::new(PARK_ADDR).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/timer-lifetime-startup");
        std::fs::create_dir_all(directory.join("owned-empty-game")).unwrap();
        adv_win32::Win32::install(
            &mut machine,
            adv_win32::Win32Config::new(
                directory.join("owned-empty-game"),
                "authored.exe",
                directory.join("profile"),
            ),
        )
        .unwrap();
        machine.sys.objects = Objects::with_limits(ObjectLimits {
            objects: limit,
            ..ObjectLimits::default()
        });
        machine.cpu.map(CODE, PAGE, Prot::RX).unwrap();
        machine.cpu.map(DATA, PAGE, Prot::RW).unwrap();
        let snapshot = machine.resolve_import("test.dll", "objects").unwrap();
        let startup_enter = machine.resolve_import("<win32>", "startup_enter").unwrap();
        let startup_leave = machine.resolve_import("<win32>", "startup_leave").unwrap();
        let set = machine.resolve_import("winmm.dll", "timeSetEvent").unwrap();
        let exit = machine
            .resolve_import("kernel32.dll", "ExitProcess")
            .unwrap();
        let mut asm = Asm::new(CODE);
        // The bare Machine harness does not start the PE process-init queue;
        // execute the real gate here before measuring the resource baseline.
        asm.call(startup_enter)
            .call(startup_leave)
            .call(snapshot)
            .mov_mem_eax(OUT);
        for value in [0, 0, CALLBACK, 0, 10] {
            asm.push_imm(value);
        }
        asm.call(set)
            .mov_mem_eax(ID)
            .call(snapshot)
            .mov_mem_eax(OUT + 4)
            .push_imm(0)
            .call(exit);
        machine.cpu.write(CODE, &asm.bytes).unwrap();
        machine
            .spawn("full Win32 budget probe", CODE, &[], 0x10000)
            .unwrap();
        assert!(matches!(machine.run_virtual(100_000), RunResult::Exited(0)));
        assert_eq!(machine.cpu.read_u32(OUT).unwrap(), 2, "limit {limit}");
        assert_eq!(machine.cpu.read_u32(ID).unwrap(), 0, "limit {limit}");
        assert_eq!(machine.cpu.read_u32(OUT + 4).unwrap(), 2, "limit {limit}");
        assert_eq!(machine.sys.objects.object_count(), 2, "limit {limit}");
        assert_eq!(machine.sys.contexts.len(), 1);
        assert!(matches!(
            machine.sys.objects.get(0x108),
            Some(Object::Mutex { .. })
        ));
    }
}

#[test]
fn context_exhaustion_rolls_back_unpublished_timer_without_service_events() {
    let mut test = Test::new(ObjectLimits::default());
    for _ in 1..adv_machine::layout::MAX_CONTEXTS {
        test.machine
            .spawn("authored capacity only", CREATOR, &[], 0x10000)
            .unwrap();
    }
    test.set(10, CALLBACK, 0, ID);
    test.set(10, CALLBACK, 0, ID + 4);
    test.run();
    assert_eq!(test.word(ID), 0);
    assert_eq!(test.word(ID + 4), 0);
    assert_eq!(
        test.machine.sys.objects.object_count(),
        adv_machine::layout::MAX_CONTEXTS as usize
    );
    assert_eq!(
        test.machine.sys.contexts.len(),
        adv_machine::layout::MAX_CONTEXTS as usize
    );
}

#[test]
fn warmed_service_resource_exhaustion_can_recover_without_id_reuse_or_new_worker() {
    for limits in [
        ObjectLimits {
            objects: 5,
            ..ObjectLimits::default()
        },
        ObjectLimits {
            aliases: 5,
            ..ObjectLimits::default()
        },
    ] {
        let mut test = Test::new(limits);
        test.set(1000, 0, 1, ID);
        test.kill(ID, OUT);
        test.call("test.dll", "event", &[], ID + 4);
        test.set(1000, 0, 1, ID + 8);
        test.snapshot(OUT + 4);
        test.call("test.dll", "close", &[Stored(ID + 4)], OUT + 8);
        test.set(1000, 0, 1, ID + 12);
        test.kill(ID, OUT + 12);
        test.kill(ID + 12, OUT + 16);
        test.snapshot(OUT + 20);
        test.run();
        assert_eq!(test.word(OUT), 0);
        assert_ne!(test.word(ID + 4), 0);
        assert_eq!(test.word(ID + 8), 0);
        assert_eq!(test.word(OUT + 4), 5);
        assert_eq!(test.word(OUT + 8), 1);
        assert!(test.word(ID + 12) > test.word(ID));
        assert_eq!(test.word(OUT + 12), 97);
        assert_eq!(test.word(OUT + 16), 0);
        assert_eq!(test.word(OUT + 20), 4);
        assert_eq!(test.machine.sys.contexts.len(), 2);
    }
}

#[test]
fn running_cancel_preserves_payload_and_synchronous_cancel_waits_for_return() {
    for flags in [1, 0x100, 0x101] {
        let mut test = Test::new(ObjectLimits::default());
        let mut cb = Asm::new(CALLBACK);
        increment(&mut cb, COUNT);
        test.emit_call(&mut cb, "test.dll", "sleep", &[Value(80)], OUT + 0xe0);
        cb.mov_eax_mem(AFTER_KILL)
            .mov_mem_eax(CALLBACK_OBSERVED)
            .mov_mem_imm(ENDED, 1)
            .ret_n(20);
        test.code(cb, CALLBACK);
        test.set(10, CALLBACK, flags, ID);
        test.sleep(20);
        test.snapshot(OUT);
        test.kill(ID, OUT + 4);
        test.asm.mov_mem_imm(AFTER_KILL, 1);
        test.snapshot(OUT + 8);
        test.sleep(100);
        test.snapshot(OUT + 12);
        test.kill(ID, OUT + 16);
        test.set(1000, 0, 1, ID + 4);
        test.kill(ID + 4, OUT + 20);
        test.snapshot(OUT + 24);
        test.run();
        assert_ne!(test.word(ID), 0);
        assert_eq!(test.word(OUT), 5);
        assert_eq!(test.word(OUT + 4), 0);
        assert_eq!(test.word(OUT + 8), if flags & 0x100 != 0 { 4 } else { 5 });
        assert_eq!(test.word(CALLBACK_OBSERVED), u32::from(flags & 0x100 == 0));
        assert_eq!(test.word(ENDED), 1);
        assert_eq!(test.word(COUNT), 1);
        assert_eq!(test.word(OUT + 12), 4);
        assert_eq!(test.word(OUT + 16), 97);
        assert!(test.word(ID + 4) > test.word(ID));
        assert_eq!(test.word(OUT + 20), 0);
        assert_eq!(test.word(OUT + 24), 4);
        assert_eq!(test.machine.sys.contexts.len(), 2);
    }
}

#[test]
fn synchronous_self_cancel_returns_with_its_slot_live_then_retires() {
    let mut test = Test::new(ObjectLimits {
        objects: 5,
        aliases: 5,
        ..ObjectLimits::default()
    });
    let mut cb = Asm::new(CALLBACK);
    increment(&mut cb, COUNT);
    test.emit_call(&mut cb, "winmm.dll", "timeKillEvent", &[Stored(ID)], OUT);
    test.emit_call(&mut cb, "test.dll", "objects", &[], OUT + 4);
    test.emit_call(
        &mut cb,
        "winmm.dll",
        "timeSetEvent",
        &[Value(10), Value(0), Value(0), Value(0), Value(0)],
        ID + 4,
    );
    cb.ret_n(20);
    test.code(cb, CALLBACK);
    test.set(10, CALLBACK, 0x101, ID);
    test.sleep(50);
    test.snapshot(OUT + 8);
    test.set(1000, 0, 1, ID + 8);
    test.kill(ID + 8, OUT + 12);
    test.run();
    assert_eq!(test.word(COUNT), 1);
    assert_eq!(test.word(OUT), 0);
    assert_eq!(test.word(OUT + 4), 5);
    assert_eq!(test.word(ID + 4), 0);
    assert_eq!(test.word(OUT + 8), 4);
    assert_ne!(test.word(ID + 8), 0);
    assert_eq!(test.word(OUT + 12), 0);
}

#[test]
fn worker_exit_wakes_sync_kill_reclaims_canceled_timer_and_does_not_restart() {
    for flags in [1, 0x101] {
        let mut test = Test::new(ObjectLimits::default());
        let gate = test.event();
        let mut cb = Asm::new(CALLBACK);
        increment(&mut cb, COUNT);
        test.emit_call(&mut cb, "test.dll", "sleep", &[Value(60)], OUT + 0xe0);
        test.emit_call(&mut cb, "test.dll", "exit_thread", &[], OUT + 0xe4);
        test.code(cb, CALLBACK);
        let mut releaser = Asm::new(CREATOR);
        test.emit_call(
            &mut releaser,
            "test.dll",
            "sleep",
            &[Value(100)],
            OUT + 0xd0,
        );
        test.emit_call(
            &mut releaser,
            "test.dll",
            "signal",
            &[Value(gate)],
            OUT + 0xd4,
        );
        test.emit_call(&mut releaser, "test.dll", "exit_thread", &[], OUT + 0xd8);
        test.code(releaser, CREATOR);
        test.machine
            .spawn("authored bounded releaser", CREATOR, &[], 0x10000)
            .unwrap();
        test.set(10, CALLBACK, flags, ID);
        test.set(1000, 0, 1, ID + 4);
        test.sleep(20);
        test.kill(ID, OUT);
        test.call("test.dll", "wait", &[Value(gate)], OUT + 4);
        test.set(10, CALLBACK, 0, ID + 8);
        test.kill(ID, OUT + 8);
        test.kill(ID + 4, OUT + 12);
        test.snapshot(OUT + 16);
        test.run();
        assert_eq!(test.word(OUT), 0);
        assert_eq!(test.word(ID + 8), 0);
        assert_eq!(test.word(OUT + 8), 97);
        assert_eq!(test.word(OUT + 12), 0);
        assert_eq!(test.word(COUNT), 1);
        // Three thread objects, two service events and our gate. No timers.
        assert_eq!(test.word(OUT + 16), 6);
        assert_eq!(test.machine.sys.contexts.len(), 3);
        assert!(test.machine.sys.contexts[2].is_done());
    }
}

#[test]
fn dead_active_and_pending_timers_can_be_reaped_or_explicitly_killed() {
    for flags in [0x100, 0x101] {
        let mut test = Test::new(ObjectLimits::default());
        let mut cb = Asm::new(CALLBACK);
        increment(&mut cb, COUNT);
        test.emit_call(&mut cb, "test.dll", "exit_thread", &[], OUT + 0xe0);
        test.code(cb, CALLBACK);
        test.set(10, CALLBACK, flags, ID);
        test.set(1000, 0, 1, ID + 4);
        test.sleep(50);
        test.kill(ID, OUT);
        test.kill(ID + 4, OUT + 4);
        test.set(10, CALLBACK, 0, ID + 8);
        test.snapshot(OUT + 8);
        test.run();
        assert_eq!(test.word(OUT), if flags & 1 != 0 { 0 } else { 97 });
        assert_eq!(test.word(OUT + 4), 0);
        assert_eq!(test.word(ID + 8), 0);
        assert_eq!(test.word(OUT + 8), 4);
        assert_eq!(test.word(COUNT), 1);
        assert_eq!(test.machine.sys.contexts.len(), 2);
    }
}

#[test]
fn creator_thread_exit_does_not_cancel_process_timer_or_callback_arguments() {
    let mut test = Test::new(ObjectLimits::default());
    let mut cb = Asm::new(CALLBACK);
    increment(&mut cb, COUNT);
    for (index, offset) in [4, 8, 12, 16, 20].into_iter().enumerate() {
        cb.mov_eax_esp(offset).mov_mem_eax(OUT + index as u32 * 4);
    }
    cb.ret_n(20);
    test.code(cb, CALLBACK);
    let mut creator = Asm::new(CREATOR);
    test.emit_call(
        &mut creator,
        "winmm.dll",
        "timeSetEvent",
        &[
            Value(10),
            Value(0),
            Value(CALLBACK),
            Value(0x12345678),
            Value(0),
        ],
        ID,
    );
    test.emit_call(&mut creator, "test.dll", "exit_thread", &[], OUT + 0xe0);
    test.code(creator, CREATOR);
    test.machine
        .spawn("authored timer creator", CREATOR, &[], 0x10000)
        .unwrap();
    test.sleep(100);
    test.snapshot(OUT + 20);
    test.run();
    assert_eq!(test.word(COUNT), 1);
    assert_eq!(test.word(OUT), test.word(ID));
    assert_eq!(
        [
            test.word(OUT + 4),
            test.word(OUT + 8),
            test.word(OUT + 12),
            test.word(OUT + 16)
        ],
        [0, 0x12345678, 0, 0]
    );
    assert_eq!(test.word(OUT + 20), 5);
    assert!(test.machine.sys.contexts[1].is_done());
    assert_eq!(test.machine.sys.contexts.len(), 3);
}

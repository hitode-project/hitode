//! Authored timer API and scheduler regression; no game or host sleep loop.
use super::*;
use crate::{Win32, Win32Config};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, Reg, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
struct Fixture {
    m: Machine<JitCpu>,
}
impl Fixture {
    fn new() -> Self {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        registry.add(
            "fixture",
            "tick",
            adv_machine::Conv::Stdcall { argc: 0 },
            |env| {
                let now = env.sys.clock.now_us();
                env.ext.get_mut::<Vec<u64>>().push(now);
                Outcome::Return(0)
            },
        );
        let mut m = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        Win32::install(
            &mut m,
            Win32Config::new(".", "timer-fixture.exe", "target/timer-fixture"),
        )
        .unwrap();
        m.cpu.map(CODE, 4096, Prot::RX).unwrap();
        m.cpu.map(DATA, 8192, Prot::RW).unwrap();
        let context = m.spawn("timer fixture", CODE, &[], 65536).unwrap();
        m.sys.current = context;
        m.ext.insert(Vec::<u64>::new());
        Self { m }
    }
    fn call(&mut self, name: &str, args: &[u32]) -> Outcome {
        let address = self.m.resolve_import("kernel32.dll", name).unwrap();
        let thunk = (address - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
        let handler = self.m.sys.thunks.get(thunk).f.handler;
        self.m.cpu.set_reg(Reg::Esp, DATA + 0x800);
        for (i, &arg) in args.iter().enumerate() {
            self.m
                .cpu
                .write_u32(DATA + 0x804 + i as u32 * 4, arg)
                .unwrap();
        }
        handler(&mut Env {
            context: self.m.sys.current,
            cpu: &mut self.m.cpu,
            sys: &mut self.m.sys,
            ext: &mut self.m.ext,
            thunk,
        })
    }
    fn invoke(&mut self, name: &str, args: &[u32]) -> u32 {
        let Outcome::Return(value) = self.call(name, args) else {
            panic!("expected immediate timer API return");
        };
        value
    }
    fn error(&self) -> u32 {
        self.m
            .cpu
            .read_u32(self.m.sys.contexts[0].teb + adv_machine::layout::teb::LAST_ERROR)
            .unwrap()
    }
    fn due(&mut self, value: i64) -> u32 {
        self.m.cpu.write(DATA, &value.to_le_bytes()).unwrap();
        DATA
    }
    fn set(&mut self, timer: u32, due: i64, period: u32) -> u32 {
        let pointer = self.due(due);
        self.invoke("SetWaitableTimer", &[timer, pointer, period, 0, 0, 0])
    }
    fn wait(&mut self, timer: u32) -> u32 {
        self.invoke("WaitForSingleObject", &[timer, 0])
    }
}

#[test]
fn relative_periodic_timer_blocks_scheduler_until_each_due_time() {
    let mut f = Fixture::new();
    let timer = f.invoke("CreateWaitableTimerW", &[0, 0, 0]);
    assert_ne!(timer, 0);
    let start = f.m.sys.clock.now_us();
    assert_eq!(f.set(timer, -20_000, 3), 1);
    assert_eq!(
        f.wait(timer),
        0x102,
        "arming must not immediately signal the timer"
    );
    let wait =
        f.m.resolve_import("kernel32.dll", "WaitForSingleObject")
            .unwrap();
    let record = f.m.resolve_import("fixture", "tick").unwrap();
    let cancel =
        f.m.resolve_import("kernel32.dll", "CancelWaitableTimer")
            .unwrap();
    let mut asm = Asm::new(CODE);
    for _ in 0..3 {
        asm.push_imm(u32::MAX);
        asm.push_imm(timer);
        asm.call(wait);
        asm.call(record);
    }
    asm.push_imm(timer);
    asm.call(cancel);
    asm.mov_mem_imm(DATA + 512, 0);
    asm.mov_eax_mem(DATA + 512);
    asm.ret();
    f.m.cpu.write(CODE, &asm.bytes).unwrap();
    assert!(matches!(f.m.run_virtual(20_000), RunResult::Exited(0)));
    assert_eq!(
        f.m.ext.get::<Vec<u64>>(),
        &[start + 2000, start + 5000, start + 8000]
    );
    assert_eq!(f.wait(timer), 0x102);
    assert_eq!(f.invoke("CloseHandle", &[timer]), 1);
}

#[test]
fn absolute_manual_rearm_cancel_and_invalid_requests_preserve_native_signal_rules() {
    let mut f = Fixture::new();
    let timer = f.invoke("CreateWaitableTimerA", &[0, 1, 0]);
    let now = super::super::time::filetime_now(&mut Env {
        context: f.m.sys.current,
        cpu: &mut f.m.cpu,
        sys: &mut f.m.sys,
        ext: &mut f.m.ext,
        thunk: 0,
    });
    let start = f.m.sys.clock.now_us();
    assert_eq!(f.set(timer, (now + 30_000) as i64, 3), 1);
    f.m.sys.clock.advance_to(start + 2999);
    assert_eq!(f.wait(timer), 0x102);
    f.m.sys.clock.advance_to(start + 3000);
    assert_eq!(f.wait(timer), 0);
    assert_eq!(f.wait(timer), 0, "manual timer signal is not consumed");
    assert_eq!(f.invoke("CancelWaitableTimer", &[timer]), 1);
    assert_eq!(f.wait(timer), 0);
    assert_eq!(f.set(timer, -50_000, 0), 1);
    assert_eq!(f.wait(timer), 0x102, "Set resets the prior signal");
    let due = f.due(-10_000);
    assert_eq!(
        f.invoke("SetWaitableTimer", &[timer, due, u32::MAX, 0, 0, 0]),
        0
    );
    assert_eq!(f.error(), 87);
    assert_eq!(
        f.invoke("SetWaitableTimer", &[timer, due, 0, CODE, 0, 0]),
        0
    );
    assert_eq!(f.error(), 50);
    assert_eq!(
        f.invoke("SetWaitableTimer", &[timer, DATA + 8190, 0, 0, 0, 0]),
        0
    );
    assert_eq!(f.error(), 998);
    assert_eq!(
        f.wait(timer),
        0x102,
        "failed sets retain the old inactive signal state/deadline"
    );
    assert_eq!(f.invoke("CancelWaitableTimer", &[timer]), 1);
    f.m.sys.clock.advance_to(start + 100_000);
    assert_eq!(f.wait(timer), 0x102);
    assert_eq!(f.set(timer, 1, 0), 1);
    assert_eq!(f.wait(timer), 0, "past absolute time fires immediately");
    assert_eq!(f.set(timer, -1, 0), 1);
    let at = f.m.sys.clock.now_us();
    assert_eq!(f.wait(timer), 0x102);
    f.m.sys.clock.advance_to(at + 1);
    assert_eq!(f.wait(timer), 0, "submicrosecond intervals round up");
}

#[test]
fn named_timer_aliases_share_state_and_last_close_destroys_deadline_and_name() {
    let mut f = Fixture::new();
    f.m.cpu.write(DATA + 64, b"Local\\shared-timer\0").unwrap();
    let wide: Vec<u8> = "shared-timer\0"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    f.m.cpu.write(DATA + 128, &wide).unwrap();
    let original = f.invoke("CreateWaitableTimerA", &[0, 0, DATA + 64]);
    let second = f.invoke("CreateWaitableTimerW", &[0, 1, DATA + 128]);
    assert_ne!(original, second);
    assert_eq!(f.error(), 183);
    let opened = f.invoke("OpenWaitableTimerW", &[0x001f0003, 0, DATA + 128]);
    assert!(f.m.sys.objects.same_object(original, opened));
    assert_eq!(f.invoke("CreateEventA", &[0, 0, 0, DATA + 64]), 0);
    assert_eq!(f.error(), 6);
    assert_eq!(
        f.invoke(
            "DuplicateHandle",
            &[u32::MAX, original, u32::MAX, DATA + 256, 0, 0, 2]
        ),
        1
    );
    let duplicate = f.m.cpu.read_u32(DATA + 256).unwrap();
    assert_eq!(f.set(original, -10_000, 0), 1);
    assert_eq!(f.invoke("CloseHandle", &[original]), 1);
    let now = f.m.sys.clock.now_us();
    f.m.sys.clock.advance_to(now + 1000);
    assert_eq!(f.wait(duplicate), 0);
    assert_eq!(f.wait(opened), 0x102, "existing manual flag was preserved");
    assert_eq!(f.set(second, -100_000_000, 0), 1);
    for handle in [second, opened, duplicate] {
        assert_eq!(f.invoke("CloseHandle", &[handle]), 1);
    }
    assert_eq!(
        f.invoke("OpenWaitableTimerA", &[0x001f0003, 0, DATA + 64]),
        0
    );
    assert_eq!(f.error(), 2);
    let recreated = f.invoke("CreateWaitableTimerW", &[0, 1, DATA + 128]);
    assert_ne!(recreated, 0);
    assert_eq!(f.error(), 0);
    assert_eq!(f.wait(recreated), 0x102);
    let event = f.invoke("CreateEventA", &[0, 0, 0, 0]);
    assert_eq!(f.set(event, -1, 0), 0);
    assert_eq!(f.error(), 6);
    assert_eq!(f.invoke("CancelWaitableTimer", &[event]), 0);
    assert_eq!(f.error(), 6);
}

#[test]
fn inherited_timer_alias_flags_preserve_real_deadlines_and_validate_security_before_creation() {
    let mut f = Fixture::new();
    let attributes = DATA + 192;
    for (offset, value) in [(0, 12), (4, 0), (8, 1)] {
        f.m.cpu.write_u32(attributes + offset, value).unwrap();
    }
    f.m.cpu
        .write(DATA + 64, b"Local\\inherited-timer\0")
        .unwrap();
    let wide: Vec<u8> = "inherited-timer\0"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    f.m.cpu.write(DATA + 128, &wide).unwrap();
    let original = f.invoke("CreateWaitableTimerA", &[attributes, 0, DATA + 64]);
    assert_ne!(original, 0);
    assert!(matches!(
        f.m.sys.objects.get(original),
        Some(Object::Timer { .. })
    ));
    assert_eq!(f.m.sys.objects.handle_flags(original), Some(1));
    let second = f.invoke("CreateWaitableTimerW", &[0, 1, DATA + 128]);
    assert_ne!(second, 0);
    assert!(f.m.sys.objects.same_object(original, second));
    assert_eq!(f.m.sys.objects.handle_flags(second), Some(0));
    assert_eq!(
        f.invoke(
            "DuplicateHandle",
            &[u32::MAX, original, u32::MAX, DATA + 256, 0, 1, 2]
        ),
        1
    );
    let duplicate = f.m.cpu.read_u32(DATA + 256).unwrap();
    assert_eq!(f.m.sys.objects.handle_flags(duplicate), Some(1));
    let start = f.m.sys.clock.now_us();
    assert_eq!(f.set(original, -10_000, 0), 1);
    assert_eq!(f.invoke("CloseHandle", &[original]), 1);
    assert_eq!(f.m.sys.objects.handle_flags(original), None);
    assert_eq!(f.wait(duplicate), 0x102);
    f.m.sys.clock.advance_to(start + 1000);
    assert_eq!(f.wait(duplicate), 0);
    assert_eq!(
        f.wait(second),
        0x102,
        "original auto-reset behavior remains shared"
    );
    let count = f.m.sys.objects.object_count();
    f.m.cpu.write_u32(attributes + 4, 1).unwrap();
    assert_eq!(
        f.invoke("CreateWaitableTimerA", &[attributes, 0, DATA + 64]),
        0
    );
    assert_eq!(f.error(), 50);
    assert_eq!(f.m.sys.objects.object_count(), count);
    assert_eq!(f.invoke("CreateWaitableTimerW", &[DATA + 8190, 0, 0]), 0);
    assert_eq!(f.error(), 998);
    assert_eq!(f.m.sys.objects.object_count(), count);
}

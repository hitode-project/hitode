//! Priority changes affect ready selection, never wait deadlines or thread life.
use adv_abi::Registry;
use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{Guest, Prot};
use adv_machine::layout::PARK_ADDR;
use adv_machine::{Clock, Conv, Machine, Outcome, RunResult, Wait};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x0040_0000;

#[test]
fn higher_relative_priority_runs_first_but_its_sleep_is_not_shortened() {
    let mut registry = Registry::default();
    registry.add("test", "mark", Conv::Stdcall { argc: 0 }, |e| {
        let point = (e.sys.current, e.sys.clock.now_us());
        e.ext.get_mut::<Vec<(u32, u64)>>().push(point);
        Outcome::Return(0)
    });
    registry.add("test", "until", Conv::Stdcall { argc: 1 }, |e| {
        Outcome::block_then_return(Wait::Until(u64::from(e.arg(0))), 0)
    });
    registry.add("test", "done", Conv::Stdcall { argc: 0 }, |_| {
        Outcome::ExitProcess(0)
    });
    let mut m = Machine::new(
        UnicornCpu::new(PARK_ADDR).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    m.ext.insert(Vec::<(u32, u64)>::new());
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    let mark = m.resolve_import("test", "mark").unwrap();
    let until = m.resolve_import("test", "until").unwrap();
    let done = m.resolve_import("test", "done").unwrap();
    let origin = m.sys.clock.now_us();
    let mut main = Asm::new(CODE);
    main.call(mark)
        .push_imm((origin + 20000) as u32)
        .call(until)
        .call(done);
    let mut worker = Asm::new(CODE + 0x100);
    worker
        .call(mark)
        .push_imm((origin + 10000) as u32)
        .call(until)
        .call(mark)
        .ret();
    m.cpu.write(CODE, &main.bytes).unwrap();
    m.cpu.write(CODE + 0x100, &worker.bytes).unwrap();
    m.spawn("ordinary", CODE, &[], 65536).unwrap();
    let id = m.spawn("preferred", CODE + 0x100, &[], 65536).unwrap();
    assert_eq!(m.sys.relative_priority(id), Some(0));
    assert!(!m.sys.set_relative_priority(99, 15));
    assert_eq!(m.sys.relative_priority(99), None);
    assert!(m.sys.set_relative_priority(id, 15));
    assert!(matches!(m.run_virtual(100000), RunResult::Exited(0)));
    assert_eq!(
        *m.ext.get::<Vec<(u32, u64)>>(),
        [(id, origin), (0, origin), (id, origin + 10000)]
    );
    assert!(m.sys.scheduling.relative_priority_dispatches >= 1);
    assert_eq!(m.sys.scheduling.wake_priority_dispatches, 0);
}

#[test]
fn higher_priority_explicit_yields_give_the_ready_peer_a_turn_and_then_resume() {
    let mut registry = Registry::default();
    registry.add("test", "mark", Conv::Stdcall { argc: 0 }, |e| {
        let context = e.sys.current;
        let order = e.ext.get_mut::<Vec<u32>>();
        order.push(context);
        if order.len() == 12 {
            return Outcome::ExitProcess(0);
        }
        Outcome::Return(0)
    });
    registry.add("test", "yield", Conv::Stdcall { argc: 0 }, |_| {
        Outcome::block_then_return(Wait::Yield, 0)
    });
    let mut m = Machine::new(
        UnicornCpu::new(PARK_ADDR).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    m.ext.insert(Vec::<u32>::new());
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    let mark = m.resolve_import("test", "mark").unwrap();
    let yield_ = m.resolve_import("test", "yield").unwrap();
    for (id, priority) in [(0, 0), (1, 1)] {
        let at = CODE + id * 0x100;
        let mut code = Asm::new(at);
        code.call(mark).call(yield_).jmp(at);
        m.cpu.write(at, &code.bytes).unwrap();
        let context = m.spawn("peer", at, &[], 65536).unwrap();
        assert!(m.sys.set_relative_priority(context, priority));
    }
    assert!(matches!(m.run_virtual(100000), RunResult::Exited(0)));
    assert_eq!(
        *m.ext.get::<Vec<u32>>(),
        [1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0]
    );
}

//! Productive polling (PROC-CLOCK-5): a Sleep(0) loop doing new work between
//! yields runs on without host idles; once it only repeats itself, it idles.
use adv_abi::Registry;
use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::layout::PARK_ADDR;
use adv_machine::{Clock, Conv, Machine, Outcome, RunResult, Wait};
use adv_unicorn::UnicornCpu;
use std::time::Duration;

const CODE: u32 = 0x0040_0000;
const DATA: u32 = 0x0050_0000;
/// Distinct work steps, each a different host call between two yields.
const STEPS: usize = 32;

/// Step addresses in order, then the steps taken.
struct Steps(Vec<u32>, usize);

fn machine() -> Machine<UnicornCpu> {
    let mut registry = Registry::default();
    registry.add("test", "poll", Conv::Stdcall { argc: 0 }, |_| {
        Outcome::block_then_return(Wait::Yield, 0)
    });
    // Each step points the loop at the next one; after the last, at `idle`.
    for step in 0..STEPS {
        registry.add(
            "test",
            &format!("step{step}"),
            Conv::Stdcall { argc: 0 },
            |e| {
                let steps = e.ext.get_mut::<Steps>();
                steps.1 += 1;
                let next = steps.0.get(steps.1).copied().unwrap_or(steps.0[STEPS]);
                e.cpu.write_u32(DATA, next).unwrap();
                Outcome::Return(0)
            },
        );
    }
    registry.add("test", "idle", Conv::Stdcall { argc: 0 }, |_| {
        Outcome::Return(0)
    });
    let mut m = Machine::new(UnicornCpu::new(PARK_ADDR).unwrap(), registry, Clock::real()).unwrap();
    m.set_execution_budget(Some(Duration::from_secs(2)));
    let mut steps: Vec<u32> = (0..STEPS)
        .map(|step| m.resolve_import("test", &format!("step{step}")).unwrap())
        .collect();
    steps.push(m.resolve_import("test", "idle").unwrap());
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.write_u32(DATA, steps[0]).unwrap();
    m.ext.insert(Steps(steps, 0));
    let poll = m.resolve_import("test", "poll").unwrap();
    let mut code = Asm::new(CODE);
    // call [DATA]; call poll; jmp CODE
    code.bytes.extend_from_slice(&[0xff, 0x15]);
    code.bytes.extend_from_slice(&DATA.to_le_bytes());
    code.call(poll).jmp(CODE);
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("main", CODE, &[], 65536).unwrap();
    m
}

// spec: PROC-CLOCK-5
#[test]
fn new_work_between_yields_runs_before_the_first_host_idle() {
    let mut m = machine();
    assert!(matches!(m.run(), RunResult::Idle { .. }));
    assert_eq!(
        m.ext.get::<Steps>().1,
        STEPS,
        "the host idled during new work"
    );
    let before = m.sys.scheduling.polling_rounds;
    assert!(matches!(m.run(), RunResult::Idle { .. }));
    assert_eq!(
        m.sys.scheduling.polling_rounds,
        before + 1,
        "repeats idle each round"
    );
}

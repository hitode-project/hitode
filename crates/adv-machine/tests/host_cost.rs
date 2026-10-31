//! Synthetic ordinary host-call cost through translated guest code.
use adv_cpu::{
    JitCpu,
    backend::{Guest, Prot},
};
use adv_machine::{Clock, Conv, HostFn, Machine, Outcome, Resolver, RunResult, layout::PARK_ADDR};

struct Empty;
impl Resolver for Empty {
    fn provides(&self, _: &str) -> bool {
        true
    }
    fn host_fn(&self, _: &str, _: &str) -> Option<HostFn> {
        Some(HostFn {
            conv: Conv::Stdcall { argc: 0 },
            handler: |_| Outcome::Return(0),
            data: 0,
            trace: false,
            guest: None,
            leaf: None,
        })
    }
}

#[test]
#[ignore = "synthetic host-call microbenchmark"]
fn empty_host_call_cost() {
    const CODE: u32 = 0x400000;
    const CALLS: u32 = 1_000_000;
    let mut machine =
        Machine::new(JitCpu::new(PARK_ADDR, true).unwrap(), Empty, Clock::real()).unwrap();
    machine.cpu.map(CODE, 0x1000, Prot::RX).unwrap();
    let thunk = machine.resolve_import("fixture", "empty").unwrap();
    let mut code = vec![0xbd]; // mov ebp, CALLS
    code.extend_from_slice(&CALLS.to_le_bytes());
    code.push(0xe8);
    code.extend_from_slice(&thunk.wrapping_sub(CODE + 10).to_le_bytes());
    code.extend_from_slice(&[0x4d, 0x0f, 0x85]);
    code.extend_from_slice(&(-12i32).to_le_bytes());
    code.push(0xc3);
    machine.cpu.write(CODE, &code).unwrap();
    let start = std::time::Instant::now();
    for _ in 0..CALLS {
        std::hint::black_box(machine.sys.clock.now_us());
    }
    eprintln!(
        "machine clock: {:.1} ns/read",
        start.elapsed().as_nanos() as f64 / f64::from(CALLS)
    );
    for pass in 0..3 {
        machine.spawn("empty call", CODE, &[], 0x10000).unwrap();
        let start = std::time::Instant::now();
        let result = machine.run();
        let elapsed = start.elapsed();
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        if pass == 2 {
            eprintln!(
                "empty ordinary host call: {:.1} ns/call",
                elapsed.as_nanos() as f64 / f64::from(CALLS)
            );
        }
    }
}

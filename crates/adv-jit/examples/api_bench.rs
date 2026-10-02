//! Native loop cost with and without a synchronous guest API round trip.
use adv_jit::*;
use iced_x86::code_asm::*;
use std::time::Instant;

const CODE: u32 = 0x10000;
const API: u32 = 0x11000;
const STOP: u32 = 0x18000;
const STACK: u32 = 0x2f000;
const ITERATIONS: u32 = 1_000_000;

struct Services;
impl HostServices for Services {
    fn call(&mut self, c: ApiCall<'_>, _: &mut ApiMemory<'_>) -> Result<ApiOutcome, String> {
        Ok(ApiOutcome::Return(c.args[0].wrapping_add(7)))
    }
}
fn setup(api: bool) -> Machine {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    a.mov(ecx, ITERATIONS).unwrap();
    a.xor(ebx, ebx).unwrap();
    a.set_label(&mut top).unwrap();
    if api {
        a.push(ecx).unwrap();
        a.call(u64::from(API)).unwrap();
    } else {
        a.mov(eax, ecx).unwrap();
        a.add(eax, 7).unwrap();
    }
    a.add(ebx, eax).unwrap();
    a.dec(ecx).unwrap();
    a.jnz(top).unwrap();
    a.ret().unwrap();
    let code = a.assemble(u64::from(CODE)).unwrap();
    let mut m = Machine::new(CODE, 0x20000).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    m.write(CODE, &code).unwrap();
    m.register_api(ApiSpec::new(
        1,
        "bench",
        "add7",
        API,
        Convention::Stdcall(1),
    ))
    .unwrap();
    m.protect(CODE, 0x2000, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    m.set_region(Region::new(CODE, if api { 0x1010 } else { code.len() }))
        .unwrap();
    m
}
fn run(m: &mut Machine) {
    m.state.eip = CODE;
    m.state.regs[4] = STACK;
    assert_eq!(
        m.run_with_services(STOP, u64::from(ITERATIONS) * 10, &mut Services)
            .unwrap(),
        RunOutcome::ReachedStop
    );
}
#[cfg(windows)]
fn pin() {
    use std::ffi::c_void;
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn SetProcessAffinityMask(p: *mut c_void, mask: usize) -> i32;
    }
    assert_ne!(unsafe { SetProcessAffinityMask(GetCurrentProcess(), 1) }, 0);
}
#[cfg(not(windows))]
fn pin() {}
fn main() {
    let samples: usize = std::env::args().nth(1).map_or(9, |s| s.parse().unwrap());
    assert!(samples > 0);
    pin();
    let mut plain = setup(false);
    let mut api = setup(true);
    run(&mut plain);
    run(&mut api);
    assert_eq!(plain.state.regs[3], api.state.regs[3]);
    let expected = (u64::from(ITERATIONS) * (u64::from(ITERATIONS) + 1) / 2
        + u64::from(ITERATIONS) * 7) as u32;
    assert_eq!(api.state.regs[3], expected);
    let mut times = [Vec::new(), Vec::new()];
    for sample in 0..samples {
        for which in [sample % 2, 1 - sample % 2] {
            let m = if which == 0 { &mut plain } else { &mut api };
            let start = Instant::now();
            run(m);
            times[which].push(start.elapsed().as_secs_f64() * 1e9 / f64::from(ITERATIONS));
            assert_eq!(m.state.regs[3], expected);
        }
    }
    for t in &mut times {
        t.sort_by(f64::total_cmp);
    }
    let (base, full) = (times[0][samples / 2], times[1][samples / 2]);
    assert_eq!(
        api.stats.host_calls,
        u64::from(ITERATIONS) * (samples as u64 + 1)
    );
    assert_eq!(api.stats.native_entries, samples as u64 + 1);
    assert_eq!(api.stats.interpreter_entries, 0);
    println!(
        "iterations,samples,loop_ns,api_loop_ns,incremental_ns,native_entries_per_run,interpreter_entries"
    );
    println!(
        "{ITERATIONS},{samples},{base:.2},{full:.2},{:.2},1,0",
        full - base
    );
}

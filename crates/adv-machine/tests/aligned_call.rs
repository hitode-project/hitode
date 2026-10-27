//! Calls into code built for the i386 System V ABI start their arguments on
//! a 16-byte boundary, whatever the caller's stack alignment.
use adv_abi::Registry;
use adv_cpu::JitCpu;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::layout::PARK_ADDR;
use adv_machine::{Clock, Conv, Machine, Outcome, RunResult};

const CODE: u32 = 0x400000;
const SEEN: u32 = 0x500000;

// spec: WINE-SYS-9
#[test]
fn aligned_calls_start_arguments_on_a_16_byte_boundary() {
    for count in 0..5u32 {
        let mut registry = Registry::default();
        registry.add("test", "outer", Conv::Stdcall { argc: 0 }, |env| {
            let count = env.cpu.read_u32(SEEN + 4).unwrap();
            let args: Vec<u32> = (1..=count).collect();
            Outcome::call_aligned(env, CODE, args, move |env, _| {
                let esp = env.cpu.read_u32(SEEN).unwrap();
                assert_eq!((esp + 4) % 16, 0, "{count} arguments at {esp:#x}");
                for index in 0..count {
                    assert_eq!(env.cpu.read_u32(esp + 4 + 4 * index).unwrap(), index + 1);
                }
                Outcome::Return(0)
            })
        });
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, true).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        machine.cpu.map(CODE, 4096, Prot::RWX).unwrap();
        machine.cpu.map(SEEN, 4096, Prot::RW).unwrap();
        // mov [SEEN], esp; ret
        let mut code = vec![0x89, 0x25];
        code.extend(SEEN.to_le_bytes());
        code.push(0xc3);
        machine.cpu.write(CODE, &code).unwrap();
        machine.cpu.write_u32(SEEN + 4, count).unwrap();
        let outer = machine.resolve_import("test", "outer").unwrap();
        machine.spawn("main", outer, &[], 0x10000).unwrap();
        let result = machine.run_virtual(100000);
        assert!(matches!(result, RunResult::Exited(_)), "{result:?}");
    }
}

//! An owned stack-pivot fixture preserves failure policy while exposing reservations.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu, Region,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Conv, Machine, Outcome, RunResult, layout::PARK_ADDR};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x500000;

#[test]
fn continuation_escape_reports_the_original_and_current_stack_reservations() {
    for jit in [false, true] {
        let mut registry = Registry::default();
        registry.add("test", "outer", Conv::Stdcall { argc: 0 }, |env| {
            env.cpu
                .write_u32(DATA, env.cpu.reg(adv_cpu::backend::Reg::Esp))
                .unwrap();
            Outcome::call(CODE, [], |_, _| Outcome::Return(0))
        });
        registry.add("test", "probe", Conv::Stdcall { argc: 0 }, |env| {
            env.cpu.write_u32(DATA + 4, 1).unwrap();
            Outcome::Return(0)
        });
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        let outer = machine.resolve_import("test", "outer").unwrap();
        let probe = machine.resolve_import("test", "probe").unwrap();
        machine
            .spawn("authored separate-stack pivot", outer, &[], 65536)
            .unwrap();
        let pivot = machine
            .sys
            .vm
            .alloc(&mut machine.cpu, 65536, Prot::RW)
            .unwrap();
        let mut code = Asm::new(CODE);
        code.bytes.push(0xbc); // Authored MOV ESP, immediate; the outer frame remains live.
        code.bytes.extend((pivot + 0x800).to_le_bytes());
        code.call(probe).ret();
        machine.cpu.write(CODE, &code.bytes).unwrap();
        if jit {
            machine
                .cpu
                .executor_mut()
                .add_region(Region::new(CODE, code.bytes.len()))
                .unwrap();
        }
        let RunResult::Failed(error) = machine.run_virtual(100000) else {
            panic!("diagnostics must preserve the current unsupported-unwind policy")
        };
        let origin = machine.cpu.read_u32(DATA).unwrap();
        let (base, size) = machine.sys.vm.containing(origin).unwrap();
        assert_ne!(base, pivot);
        assert!(error.message.contains("across test!outer"));
        assert!(
            error
                .message
                .contains(&format!("origin_esp={origin:#010x}"))
        );
        assert!(
            error
                .message
                .contains(&format!("frame_stack={origin:#010x}"))
        );
        assert!(
            error
                .message
                .contains(&format!("current_reservation={pivot:#010x}+0x10000"))
        );
        assert!(
            error
                .message
                .contains(&format!("frame_reservation={base:#010x}+{size:#x}"))
        );
        for field in [
            "current_pc=",
            "current_esp=",
            "frame_id=",
            "return_floor=",
            "thread_stack=",
        ] {
            assert!(error.message.contains(field));
        }
        assert_eq!(machine.cpu.read_u32(DATA + 4).unwrap(), 0);
    }
}

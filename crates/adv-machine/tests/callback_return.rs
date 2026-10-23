//! A host call that ends the guest call it made from deep inside it, as
//! NtCallbackReturn does, returns to its caller with the registers that
//! caller expects preserved.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu, Region,
    backend::{Guest, Prot},
};
use adv_machine::{Clock, Conv, Machine, Outcome, RunResult, layout::PARK_ADDR};

const MAIN: u32 = 0x400000;
const CALLBACK: u32 = 0x400800;
const DATA: u32 = 0x500000;

/// `mov ebx, ebp, esi, edi, imm32` for the four callee-saved registers.
fn set_callee_saved(code: &mut Vec<u8>, values: [u32; 4]) {
    for (opcode, value) in [0xbb, 0xbd, 0xbe, 0xbf].into_iter().zip(values) {
        code.push(opcode);
        code.extend(value.to_le_bytes());
    }
}

fn call(code: &mut Vec<u8>, at: u32, target: u32) {
    let next = at + code.len() as u32 + 5;
    code.push(0xe8);
    code.extend(target.wrapping_sub(next).to_le_bytes());
}

// spec: PROC-MACH-12
#[test]
fn callback_return_restores_the_callers_callee_saved_registers() {
    for jit in [false, true] {
        let mut registry = Registry::default();
        registry.add("test", "enter", Conv::Stdcall { argc: 0 }, |_| {
            Outcome::call(CALLBACK, [], |_, returned| {
                Outcome::Return(returned.value())
            })
        });
        registry.add("test", "leave", Conv::Stdcall { argc: 0 }, |_| {
            Outcome::ReturnFromCall(7)
        });
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        machine.cpu.map(MAIN, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        let enter = machine.resolve_import("test", "enter").unwrap();
        let leave = machine.resolve_import("test", "leave").unwrap();

        let mut main = Vec::new();
        set_callee_saved(&mut main, [0x11, 0x22, 0x33, 0x44]);
        call(&mut main, MAIN, enter);
        // mov [DATA + 4 * n], ebx / ebp / esi / edi, then eax.
        for (index, modrm) in [0x1d, 0x2d, 0x35, 0x3d].into_iter().enumerate() {
            main.extend([0x89, modrm]);
            main.extend((DATA + 4 * index as u32).to_le_bytes());
        }
        main.push(0xa3);
        main.extend((DATA + 16).to_le_bytes());
        main.push(0xc3);

        // The callback clobbers them, as callee code may, and leaves from a
        // nested frame without returning.
        let mut callback = Vec::new();
        set_callee_saved(
            &mut callback,
            [0xdead_0001, 0xdead_0002, 0xdead_0003, 0xdead_0004],
        );
        call(&mut callback, CALLBACK, leave);
        callback.push(0xcc);

        machine.cpu.write(MAIN, &main).unwrap();
        machine.cpu.write(CALLBACK, &callback).unwrap();
        if jit {
            let executor = machine.cpu.executor_mut();
            executor.add_region(Region::new(MAIN, main.len())).unwrap();
            executor
                .add_region(Region::new(CALLBACK, callback.len()))
                .unwrap();
        }
        machine.spawn("main", MAIN, &[], 65536).unwrap();
        let result = machine.run_virtual(100_000);
        assert!(!matches!(result, RunResult::Failed(_)), "{result:?}");
        let mut saved = [0u8; 20];
        machine.cpu.read(DATA, &mut saved).unwrap();
        let words: Vec<u32> = saved
            .chunks(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect();
        assert_eq!(words, [0x11, 0x22, 0x33, 0x44, 7], "jit={jit}");
    }
}

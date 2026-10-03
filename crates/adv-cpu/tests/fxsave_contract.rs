//! Independent expectations for the guest's non-64-bit FXSAVE memory image.
use adv_cpu::interpreter::{Interpreter, Step};
use adv_cpu::{CpuState, EXEC, Memory, READ, WRITE, X87Register};

const CODE: u32 = 0x10000;
const DATA: u32 = 0x20000;
const FXSAVE: [u8; 3] = [0x0f, 0xae, 0x07]; // FXSAVE [EDI]
const FXRSTOR: [u8; 3] = [0x0f, 0xae, 0x0f]; // FXRSTOR [EDI]
const USED: usize = 288; // MXCSR/x87/MMX and XMM0..7; no XMM8..15 in 32-bit mode.

fn memory(instruction: &[u8]) -> Memory {
    let mut memory = Memory::new(CODE, 0x20000).unwrap();
    memory.protect(CODE, 0x20000, READ | WRITE).unwrap();
    memory.write(CODE, instruction).unwrap();
    memory.protect(CODE, 4096, READ | EXEC).unwrap();
    memory
}

fn context(address: u32) -> CpuState {
    let mut state = CpuState {
        regs: [1, 2, 3, 4, 0x2f800, 6, 7, address],
        eip: CODE,
        eflags: 0x8d7,
        mxcsr: 0x5fa0,
        xmm: std::array::from_fn(|r| std::array::from_fn(|b| (r * 29 + b * 7) as u8)),
        fuel: 19,
        fault_addr: 0x1234_5678,
        ..CpuState::default()
    };
    state.x87.control = 0x27f;
    state
        .x87
        .push(X87Register::from_parts(0xa000_0000_0000_1234, 0x3fff));
    state.x87.push(X87Register::from_parts(0, 0));
    state.x87.ip = 0x1234_0000;
    state.x87.dp = 0x2345_0000;
    state.x87.cs = 8;
    state.x87.ds = 16;
    state.x87.opcode = 0x123;
    state.refresh_mmx();
    state
}

#[test]
fn fxsave_write_only_destination_preserves_unused_and_software_bytes() {
    let mut memory = memory(&FXSAVE);
    memory.write(DATA, &[0xa5; 512]).unwrap();
    memory.protect(DATA, 4096, WRITE).unwrap();
    let mut state = context(DATA);
    let mut expected = state.clone();
    expected.eip += FXSAVE.len() as u32;
    assert_eq!(
        Interpreter::new().step(&mut state, &mut memory),
        Step::Executed
    );
    assert_eq!(state, expected, "FXSAVE preserves the processor context");
    let image = memory.read(DATA, 512).unwrap();
    assert_eq!(&image[USED..], &[0xa5; 512 - USED]);
    assert_eq!(&image[..2], &state.x87.control.to_le_bytes());
    assert_eq!(&image[2..4], &state.x87.status.to_le_bytes());
    assert_eq!(
        image[4], 0xc0,
        "abridged tags are in physical register order"
    );
    assert_eq!(&image[24..28], &state.mxcsr.to_le_bytes());
    for slot in 0..8 {
        assert_eq!(&image[160 + slot * 16..176 + slot * 16], &state.xmm[slot]);
        let physical = state.x87.physical_index(slot);
        assert_eq!(
            &image[32 + slot * 16..42 + slot * 16],
            &state.x87.regs[physical].0
        );
    }
}

#[test]
fn fxsave_does_not_access_an_inaccessible_page_containing_only_unused_tail() {
    let address = DATA + 4096 - USED as u32;
    let mut memory = memory(&FXSAVE);
    memory.write(address, &[0xa5; 512]).unwrap();
    memory.protect(DATA, 4096, WRITE).unwrap();
    memory.protect(DATA + 4096, 4096, 0).unwrap();
    let mut state = context(address);
    assert_eq!(
        Interpreter::new().step(&mut state, &mut memory),
        Step::Executed
    );
    assert_eq!(state.eip, CODE + 3);
    assert_eq!(
        memory.read(address + USED as u32, 512 - USED).unwrap(),
        &[0xa5; 512 - USED]
    );
}

#[test]
fn fxsave_fault_in_a_written_field_is_atomic_and_keeps_the_faulting_pc() {
    let address = DATA + 4096 - (USED - 16) as u32;
    let mut memory = memory(&FXSAVE);
    memory.write(address, &[0xa5; 512]).unwrap();
    memory.protect(DATA + 4096, 4096, READ).unwrap();
    let mut state = context(address);
    let before = state.clone();
    assert_eq!(
        Interpreter::new().step(&mut state, &mut memory),
        Step::WriteFault {
            pc: CODE,
            address: DATA + 4096
        }
    );
    assert_eq!(state, before);
    assert_eq!(memory.read(address, 512).unwrap(), &[0xa5; 512]);
}

#[test]
fn fxrstor_restores_exact_state_without_reading_unused_tail() {
    let address = DATA + 4096 - USED as u32;
    let mut memory = memory(&FXSAVE);
    let mut saved = context(address);
    let mut interpreter = Interpreter::new();
    assert_eq!(interpreter.step(&mut saved, &mut memory), Step::Executed);
    memory
        .write(address + USED as u32, &[0x5a; 512 - USED])
        .unwrap();
    memory.write(CODE, &FXRSTOR).unwrap();
    memory.protect(DATA, 4096, READ).unwrap();
    memory.protect(DATA + 4096, 4096, 0).unwrap();

    let mut restored = CpuState {
        regs: [91, 92, 93, 94, 0x2f800, 96, 97, address],
        eip: CODE,
        eflags: 0x202,
        fuel: 71,
        fault_addr: 0xabcd_1234,
        ..CpuState::default()
    };
    let mut expected = restored.clone();
    expected.x87 = saved.x87;
    expected.xmm = saved.xmm;
    expected.mxcsr = saved.mxcsr;
    expected.refresh_mmx();
    expected.eip += FXRSTOR.len() as u32;
    assert_eq!(interpreter.step(&mut restored, &mut memory), Step::Executed);
    assert_eq!(restored, expected);
    assert_eq!(
        memory.read(address + USED as u32, 512 - USED).unwrap(),
        &[0x5a; 512 - USED]
    );
}

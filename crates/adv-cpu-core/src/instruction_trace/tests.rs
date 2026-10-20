use super::*;
use crate::WRITE;

const CODE: u32 = 0x10000;
const DATA: u32 = 0x18000;

/// Authored code at CODE in a 64 KiB window, with readable data at DATA.
fn authored(bytes: &[u8]) -> Memory {
    let mut memory = Memory::new(CODE, 0x10000).unwrap();
    memory.protect(CODE, 0x10000, READ | WRITE).unwrap();
    memory.write(CODE, bytes).unwrap();
    memory.protect(CODE, PAGE_SIZE, READ | EXEC).unwrap();
    memory
}

#[test]
fn unreadable_overwide_and_unknown_operands_are_null_without_mutating_execution() {
    let mut memory = authored(&[0x83, 0x38, 1]); // Authored cmp dword [eax],1.
    memory
        .protect(DATA + PAGE_SIZE as u32, PAGE_SIZE, 0)
        .unwrap();
    let mut state = CpuState::default();
    for address in [DATA + PAGE_SIZE as u32 - 2, u32::MAX - 1] {
        state.regs[0] = address;
        let before = state.clone();
        let i = instruction(&memory, CODE).unwrap();
        let entries = operands(&i, &state);
        let row: serde_json::Value =
            serde_json::from_str(&operand_snapshot(&entries, &state, &memory)).unwrap();
        assert_eq!(row[0]["ea"], address);
        assert!(row[0]["scalar"].is_null());
        assert_eq!(state, before);
    }
    let memory = authored(&[0x0f, 0x10, 0x00]); // Authored movups xmm0,[eax].
    let state = CpuState {
        regs: [DATA, 0, 0, 0, 0, 0, 0, 0],
        ..CpuState::default()
    };
    let i = instruction(&memory, CODE).unwrap();
    let row: serde_json::Value =
        serde_json::from_str(&operand_snapshot(&operands(&i, &state), &state, &memory)).unwrap();
    assert!(row[0]["value"].is_null());
    assert_eq!(row[1]["width"], 16);
    assert!(row[1]["scalar"].is_null());
}

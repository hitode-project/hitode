//! The i386 CONTEXT record (716 bytes) that NtContinue and exception
//! delivery exchange with ntdll.dll, independent of host pointer size.
use adv_cpu::backend::ArchitecturalState;
use adv_cpu::{X87Register, X87State};

pub const SIZE: usize = 716;
const CONTEXT_I386: u32 = 0x0001_0000;
const FLAGS: u32 = 0x0001_003f; // control, integer, segments, x87, debug, extended.
fn put(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn get(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn word(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

pub fn encode(state: &ArchitecturalState) -> [u8; SIZE] {
    let mut bytes = [0; SIZE];
    put(&mut bytes, 0, FLAGS);
    for (offset, index) in [
        (0x9c, 7),
        (0xa0, 6),
        (0xa4, 3),
        (0xa8, 2),
        (0xac, 1),
        (0xb0, 0),
        (0xb4, 5),
        (0xc4, 4),
    ] {
        put(&mut bytes, offset, state.regs[index]);
    }
    put(&mut bytes, 0xb8, state.eip);
    put(&mut bytes, 0xc0, state.eflags);
    for (offset, index) in [
        (0x8c, 5),
        (0x90, 4),
        (0x94, 0),
        (0x98, 3),
        (0xbc, 1),
        (0xc8, 2),
    ] {
        put(&mut bytes, offset, u32::from(state.selectors[index]));
    }
    let x87 = &state.x87;
    for (offset, value) in [
        (0x1c, u32::from(x87.control)),
        (0x20, u32::from(x87.status)),
        (0x24, u32::from(x87.tag)),
        (0x28, x87.ip),
        (0x2c, u32::from(x87.cs) | (u32::from(x87.opcode) << 16)),
        (0x30, x87.dp),
        (0x34, u32::from(x87.ds)),
    ] {
        put(&mut bytes, offset, value);
    }
    for logical in 0..8 {
        bytes[0x38 + logical * 10..0x42 + logical * 10]
            .copy_from_slice(&x87.regs[(x87.top() + logical) & 7].0);
    }
    let extended = &mut bytes[0xcc..];
    extended[0..2].copy_from_slice(&x87.control.to_le_bytes());
    extended[2..4].copy_from_slice(&x87.status.to_le_bytes());
    extended[4] = (0..8).fold(0, |tag, n| {
        tag | (u8::from((x87.tag >> (n * 2)) & 3 != 3) << n)
    });
    extended[6..8].copy_from_slice(&x87.opcode.to_le_bytes());
    put(extended, 8, x87.ip);
    extended[12..14].copy_from_slice(&x87.cs.to_le_bytes());
    put(extended, 16, x87.dp);
    extended[20..22].copy_from_slice(&x87.ds.to_le_bytes());
    put(extended, 24, state.mxcsr);
    put(extended, 28, 0xffff);
    for logical in 0..8 {
        extended[32 + logical * 16..42 + logical * 16]
            .copy_from_slice(&x87.regs[(x87.top() + logical) & 7].0);
    }
    for n in 0..8 {
        extended[160 + n * 16..176 + n * 16].copy_from_slice(&state.xmm[n]);
    }
    bytes
}

fn extended(bytes: &[u8]) -> X87State {
    let bytes = &bytes[0xcc..];
    let mut state = X87State {
        control: word(bytes, 0),
        status: word(bytes, 2),
        opcode: word(bytes, 6),
        ip: get(bytes, 8),
        cs: word(bytes, 12),
        dp: get(bytes, 16),
        ds: word(bytes, 20),
        ..X87State::default()
    };
    for logical in 0..8 {
        let physical = (state.top() + logical) & 7;
        let register = X87Register(
            bytes[32 + logical * 16..42 + logical * 16]
                .try_into()
                .unwrap(),
        );
        state.regs[physical] = register;
        let tag = if bytes[4] & (1 << physical) != 0 {
            register.tag()
        } else {
            3
        };
        state.tag = (state.tag & !(3 << (physical * 2))) | (tag << (physical * 2));
    }
    state
}

pub fn decode(bytes: &[u8; SIZE], state: &mut ArchitecturalState) -> Result<(), String> {
    let flags = get(bytes, 0);
    // Any of control, integer, segments and extended registers restore their
    // part; FloatSave is read only through the extended registers, as Wine's
    // NtContinue takes it. Debug registers live in the NT thread state.
    // XSTATE is not restored yet.
    let parts = flags & !CONTEXT_I386;
    let supported =
        parts & !(1 | 2 | 4 | 8 | 0x10 | 0x20) == 0 && (parts & 8 == 0 || parts & 0x20 != 0);
    if flags & CONTEXT_I386 == 0 || !supported {
        return Err(format!("x86 CONTEXT flags unsupported: {flags:#x}"));
    }
    let mut candidate = state.clone();
    if flags & 1 != 0 {
        candidate.regs[5] = get(bytes, 0xb4);
        candidate.eip = get(bytes, 0xb8);
        candidate.selectors[1] = get(bytes, 0xbc) as u16;
        candidate.eflags = get(bytes, 0xc0);
        candidate.regs[4] = get(bytes, 0xc4);
        candidate.selectors[2] = get(bytes, 0xc8) as u16;
    }
    if flags & 2 != 0 {
        for (offset, index) in [
            (0x9c, 7),
            (0xa0, 6),
            (0xa4, 3),
            (0xa8, 2),
            (0xac, 1),
            (0xb0, 0),
        ] {
            candidate.regs[index] = get(bytes, offset);
        }
    }
    if flags & 4 != 0 {
        for (offset, index) in [(0x8c, 5), (0x90, 4), (0x94, 0), (0x98, 3)] {
            candidate.selectors[index] = get(bytes, offset) as u16;
        }
    }
    if flags & 0x20 != 0 {
        // The owned x86 Windows oracle demonstrates that ExtendedRegisters wins
        // over FloatSave whenever both flags are present, even if only FloatSave
        // was edited. Preserve the full captured tags when x87 itself is unchanged.
        candidate.x87 = state.x87.clone();
        let original = encode(state);
        if bytes[0xcc..0xcc + 24] != original[0xcc..0xcc + 24]
            || bytes[0xcc + 32..0xcc + 160] != original[0xcc + 32..0xcc + 160]
        {
            candidate.x87 = extended(bytes);
        }
        candidate.mxcsr = get(bytes, 0xcc + 24);
        for n in 0..8 {
            candidate.xmm[n].copy_from_slice(&bytes[0xcc + 160 + n * 16..0xcc + 176 + n * 16]);
        }
    }
    *state = candidate;
    Ok(())
}

/// Wine retains the debug registers per thread independently of CPU execution.
#[derive(Default)]
pub struct DebugRegisters(std::collections::HashMap<adv_machine::CtxId, [u8; 24]>);

pub fn save_debug(env: &mut adv_machine::Env<'_>, target: adv_machine::CtxId, bytes: &[u8]) {
    if get(bytes, 0) & 0x10 != 0 {
        env.ext
            .get_or_default::<DebugRegisters>()
            .0
            .insert(target, bytes[4..28].try_into().unwrap());
    }
}

pub fn load_debug(env: &mut adv_machine::Env<'_>, target: adv_machine::CtxId, bytes: &mut [u8]) {
    let registers = env
        .ext
        .get_or_default::<DebugRegisters>()
        .0
        .get(&target)
        .copied()
        .unwrap_or([0; 24]);
    bytes[4..28].copy_from_slice(&registers);
}

/// Wine's i386 fields within context_data's architecture unions.
pub(crate) fn to_server(image: &[u8; SIZE]) -> [u8; 1720] {
    let mut wire = [0; 1720];
    put(&mut wire, 0, 0x14c);
    put(&mut wire, 4, get(image, 0) & 0x3f);
    for (to, from, len) in [
        (8, 0xb8, 4),
        (12, 0xb4, 4),
        (16, 0xc4, 4),
        (20, 0xc0, 4),
        (24, 0xbc, 4),
        (28, 0xc8, 4),
        (40, 0xb0, 4),
        (44, 0xa4, 4),
        (48, 0xac, 4),
        (52, 0xa8, 4),
        (56, 0xa0, 4),
        (60, 0x9c, 4),
        (288, 0x98, 4),
        (292, 0x94, 4),
        (296, 0x90, 4),
        (300, 0x8c, 4),
        (304, 0x1c, 112),
        (824, 4, 24),
        (944, 0xcc, 512),
    ] {
        wire[to..to + len].copy_from_slice(&image[from..from + len]);
    }
    wire
}

pub(crate) fn from_server(wire: &[u8], image: &mut [u8; SIZE]) {
    let flags = get(wire, 4);
    let flags_out = get(image, 0) | 0x10000 | flags & 0x3f;
    put(image, 0, flags_out);
    for (part, from, to, len) in [
        (1, 8, 0xb8, 4),
        (1, 12, 0xb4, 4),
        (1, 16, 0xc4, 4),
        (1, 20, 0xc0, 4),
        (1, 24, 0xbc, 4),
        (1, 28, 0xc8, 4),
        (2, 40, 0xb0, 4),
        (2, 44, 0xa4, 4),
        (2, 48, 0xac, 4),
        (2, 52, 0xa8, 4),
        (2, 56, 0xa0, 4),
        (2, 60, 0x9c, 4),
        (4, 288, 0x98, 4),
        (4, 292, 0x94, 4),
        (4, 296, 0x90, 4),
        (4, 300, 0x8c, 4),
        (8, 304, 0x1c, 112),
        (16, 824, 4, 24),
        (32, 944, 0xcc, 512),
    ] {
        if flags & part != 0 {
            image[to..to + len].copy_from_slice(&wire[from..from + len]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> ArchitecturalState {
        let mut cpu = adv_cpu::CpuState {
            regs: [1, 2, 3, 4, 5, 6, 7, 8],
            eip: 0x12345678,
            eflags: 0x246,
            ..adv_cpu::CpuState::default()
        };
        cpu.x87.status = 0x2800;
        cpu.x87.tag = 0xff03;
        for n in 0..8 {
            cpu.x87.regs[n] = X87Register::from_parts(0x8000000000000000 + n as u64, 0x3fff);
            cpu.xmm[n] = [n as u8; 16];
        }
        cpu.capture_architecture()
    }
    #[test]
    fn full_image_keeps_physical_top_empty_payloads_and_ignores_legacy_when_extended_present() {
        let original = state();
        let mut bytes = encode(&original);
        let mut restored = original.clone();
        decode(&bytes, &mut restored).unwrap();
        assert_eq!(restored, original);
        put(&mut bytes, 0x1c, 0x0b7f);
        decode(&bytes, &mut restored).unwrap();
        assert_eq!(restored, original);
        bytes[0xcc..0xcc + 2].copy_from_slice(&0x0f7fu16.to_le_bytes());
        bytes[0xcc + 160..0xcc + 176].fill(42);
        decode(&bytes, &mut restored).unwrap();
        assert_eq!(restored.x87.control, 0x0f7f);
        assert_eq!(restored.x87.top(), 5);
        assert_eq!(restored.x87.regs, original.x87.regs);
        assert_eq!(restored.xmm[0], [42; 16]);
    }
    #[test]
    fn partial_context_restores_only_its_parts() {
        let original = state();
        let mut bytes = encode(&original);
        let mut restored = original.clone();
        put(&mut bytes, 0, 0x1_0007);
        put(&mut bytes, 0xb8, 0xdeadbeef);
        put(&mut bytes, 0xcc + 24, 0x1f80 ^ original.mxcsr ^ 1);
        decode(&bytes, &mut restored).unwrap();
        assert_eq!(restored.eip, 0xdeadbeef);
        assert_eq!(restored.mxcsr, original.mxcsr);
    }
    #[test]
    fn unsupported_context_fails_before_any_architecture_is_published() {
        for (flags, dr7) in [(0, 0), (0x7, 0), (0x1000f, 0), (0x1006f, 0)] {
            let original = state();
            let mut bytes = encode(&original);
            let mut restored = original.clone();
            put(&mut bytes, 0, flags);
            put(&mut bytes, 0x18, dr7);
            put(&mut bytes, 0xb8, 0xdeadbeef);
            assert!(decode(&bytes, &mut restored).is_err());
            assert_eq!(restored, original);
        }
    }

    // spec: WINE-THR-5
    #[test]
    fn debug_registers_do_not_change_cpu_execution_state() {
        let original = state();
        let mut bytes = encode(&original);
        put(&mut bytes, 0, 0x1003f);
        for offset in (4..28).step_by(4) {
            put(&mut bytes, offset, 0x12345678);
        }
        let mut restored = original.clone();
        decode(&bytes, &mut restored).unwrap();
        assert_eq!(restored, original);
    }
}

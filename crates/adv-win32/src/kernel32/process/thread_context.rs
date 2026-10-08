//! Partial i386 CONTEXT for stopped-owner thread access. The SEH restore contract
//! remains a separate exact-flags operation.
use adv_cpu::backend::ArchitecturalState;
use adv_cpu::{X87Register, X87State};
use adv_os::processes::Error;

use super::super::exception::context;
pub(super) const SIZE: usize = context::SIZE;
const I386: u32 = 0x10000;
const SUPPORTED: u32 = 0x2f;

fn dword(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

pub(super) fn flags(bytes: &[u8; SIZE]) -> Result<u32, Error> {
    let flags = dword(bytes, 0);
    if flags & !SUPPORTED != I386 || flags & SUPPORTED == 0 {
        return Err(Error::new(87, "unsupported i386 CONTEXT flags"));
    }
    Ok(flags)
}

fn groups(flags: u32) -> impl Iterator<Item = std::ops::Range<usize>> {
    [
        (1, 0xb4..0xcc), // EBP, EIP, CS, EFLAGS, ESP, SS
        (2, 0x9c..0xb4), // EDI, ESI, EBX, EDX, ECX, EAX
        (4, 0x8c..0x9c), // GS, FS, ES, DS
        (8, 0x1c..0x8c), // FLOATING_SAVE_AREA
        (0x20, 0xcc..SIZE),
    ]
    .into_iter()
    .filter_map(move |(bit, range)| (flags & bit != 0).then_some(range))
}

pub(super) fn capture(
    state: &ArchitecturalState,
    mut bytes: [u8; SIZE],
) -> Result<[u8; SIZE], Error> {
    let flags = flags(&bytes)?;
    let full = context::encode(state);
    for range in groups(flags) {
        bytes[range.clone()].copy_from_slice(&full[range]);
    }
    Ok(bytes)
}

pub(super) fn restore(bytes: &[u8; SIZE], state: &mut ArchitecturalState) -> Result<(), Error> {
    let flags = flags(bytes)?;
    let mut full = context::encode(state);
    for range in groups(flags) {
        full[range.clone()].copy_from_slice(&bytes[range]);
    }
    let mut candidate = state.clone();
    context::decode(&full, &mut candidate).map_err(|message| Error::new(87, message))?;
    // The thread API also accepts FloatSave by itself. SEH always captures the
    // extended image, where ExtendedRegisters has precedence over FloatSave.
    if flags & 8 != 0 && flags & 0x20 == 0 {
        let word = |offset| dword(bytes, offset) as u16;
        let mut x87 = X87State {
            control: word(0x1c),
            status: word(0x20),
            tag: word(0x24),
            ip: dword(bytes, 0x28),
            cs: word(0x2c),
            opcode: (dword(bytes, 0x2c) >> 16) as u16,
            dp: dword(bytes, 0x30),
            ds: word(0x34),
            ..X87State::default()
        };
        for logical in 0..8 {
            x87.regs[(x87.top() + logical) & 7] = X87Register(
                bytes[0x38 + logical * 10..0x42 + logical * 10]
                    .try_into()
                    .unwrap(),
            );
        }
        candidate.x87 = x87;
    }
    *state = candidate;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> ArchitecturalState {
        adv_cpu::CpuState {
            regs: [1, 2, 3, 4, 5, 6, 7, 8],
            eip: 0x12345678,
            eflags: 0x246,
            ..adv_cpu::CpuState::default()
        }
        .capture_architecture()
    }

    fn image(flags: u32) -> [u8; SIZE] {
        let mut bytes = [0xa5; SIZE];
        bytes[..4].copy_from_slice(&flags.to_le_bytes());
        bytes
    }

    #[test]
    fn partial_capture_and_restore_leave_unrequested_groups_untouched() {
        let original = state();
        let bytes = capture(&original, image(I386 | 1)).unwrap();
        assert_eq!(&bytes[0x9c..0xb4], &[0xa5; 24]);
        assert_eq!(dword(&bytes, 0xb8), original.eip);
        let mut target = original.clone();
        target.regs[0] = 0xabcdef;
        target.eip = 0;
        restore(&bytes, &mut target).unwrap();
        assert_eq!(target.eip, original.eip);
        assert_eq!(target.regs[0], 0xabcdef);
        assert_eq!(target.x87, original.x87);
    }

    #[test]
    fn float_only_restores_legacy_control_and_preserves_sse() {
        let original = state();
        let mut bytes = capture(&original, image(I386 | 8)).unwrap();
        bytes[0x1c..0x20].copy_from_slice(&0x0f7fu32.to_le_bytes());
        let mut restored = original.clone();
        restore(&bytes, &mut restored).unwrap();
        assert_eq!(restored.x87.control, 0x0f7f);
        assert_eq!(restored.mxcsr, original.mxcsr);
        assert_eq!(restored.xmm, original.xmm);
    }

    #[test]
    fn unsupported_flags_do_not_publish_any_changes() {
        for flags in [0, I386, I386 | 0x10, 0x20001, I386 | 0x40] {
            let original = state();
            let mut target = original.clone();
            assert!(restore(&image(flags), &mut target).is_err());
            assert_eq!(target, original);
        }
    }
}

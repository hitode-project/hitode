//! Conservative writes for normal exits only. Faults and stateful helpers keep
//! the full canonical writeback, including MMX/x87 aliases and arbitrary traps.
use super::*;
use iced_x86::{InstructionInfoFactory, OpAccess};

/// An internal loop with only a final indirect return does not repeatedly use
/// the cross-region footer. Keep its original shared CFG: adding another large
/// writeback can disturb allocation in the loop even though it is rarely used.
/// A sliced loop has an external direct edge and remains eligible.
pub(super) fn crosses_regions(instructions: &BTreeMap<u32, Instruction>) -> bool {
    let mut internal_backedge = false;
    for instruction in instructions.values() {
        match instruction.flow_control() {
            FlowControl::ConditionalBranch
            | FlowControl::UnconditionalBranch
            | FlowControl::Call => {
                let target = instruction.near_branch_target() as u32;
                if !instructions.contains_key(&target) {
                    return true;
                }
                if instruction.flow_control() != FlowControl::Call && target <= instruction.ip32() {
                    internal_backedge = true;
                }
            }
            FlowControl::IndirectCall | FlowControl::IndirectBranch => return true,
            _ => {}
        }
        if matches!(
            instruction.flow_control(),
            FlowControl::Next | FlowControl::ConditionalBranch
        ) && !instructions.contains_key(&instruction.next_ip32())
        {
            return true;
        }
    }
    !internal_backedge
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Summary {
    pub gpr: u8,
    pub xmm: u8,
    pub flags: bool,
}
impl Summary {
    pub fn for_region(instructions: &BTreeMap<u32, Instruction>) -> Option<Self> {
        let mut summary = Self::default();
        let mut factory = InstructionInfoFactory::new();
        for instruction in instructions.values() {
            if !supported(instruction)
                || crate::strings::operation(instruction).is_some()
                || mmx::supported(instruction)
                || matches!(instruction.mnemonic(), M::Int | M::Rdtsc)
            {
                return None;
            }
            // This whitelist is the existing integer/SSE2 emitter. Memory-only
            // PCM/FILE helpers cannot mutate CPU registers/flags. All other
            // stateful or unsupported paths are deliberately excluded above.
            summary.flags |= instruction.rflags_modified() != 0;
            for register in factory.info(instruction).used_registers() {
                if !matches!(
                    register.access(),
                    OpAccess::Write
                        | OpAccess::CondWrite
                        | OpAccess::ReadWrite
                        | OpAccess::ReadCondWrite
                ) {
                    continue;
                }
                let register = register.register();
                if let Some(index) = reg_index(register.full_register32()) {
                    summary.gpr |= 1 << index;
                } else {
                    let index = simd::register(register)?;
                    summary.xmm |= 1 << index;
                }
            }
        }
        Some(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn summary(bytes: &[u8]) -> Option<Summary> {
        let mut decoder = Decoder::with_ip(32, bytes, 0x10000, DecoderOptions::NONE);
        let mut instructions = BTreeMap::new();
        while decoder.can_decode() {
            let instruction = decoder.decode();
            instructions.insert(instruction.ip32(), instruction);
        }
        Summary::for_region(&instructions)
    }
    #[test]
    fn implicit_and_partial_writes_are_included_without_read_only_vector_sources() {
        // mov ah,1; mul ebx; push eax; paddb xmm0,xmm1
        let s = summary(&[0xb4, 1, 0xf7, 0xe3, 0x50, 0x66, 0x0f, 0xfc, 0xc1]).unwrap();
        assert_eq!(s.gpr, (1 << 0) | (1 << 2) | (1 << 4));
        assert_eq!(s.xmm, 1);
        assert!(s.flags);
    }
    #[test]
    fn helpers_and_mmx_aliases_require_full_canonical_state() {
        assert!(summary(&[0xf3, 0xa5]).is_none()); // rep movsd
        assert!(summary(&[0x0f, 0x6f, 0xc1]).is_none()); // movq mm0,mm1
        assert!(summary(&[0x0f, 0x31]).is_none()); // rdtsc
        assert!(summary(&[0xcd, 0x2e]).is_none()); // host trap
    }
    #[test]
    fn local_loops_keep_the_original_footer_but_external_transfers_do_not() {
        let eligible = |bytes: &[u8]| {
            let mut decoder = Decoder::with_ip(32, bytes, 0x10000, DecoderOptions::NONE);
            let mut instructions = BTreeMap::new();
            while decoder.can_decode() {
                let instruction = decoder.decode();
                instructions.insert(instruction.ip32(), instruction);
            }
            crosses_regions(&instructions)
        };
        assert!(!eligible(&[0x49, 0x75, 0xfd, 0xc3])); // local dec/jnz loop; ret
        assert!(eligible(&[0x49, 0x75, 0xed, 0xc3])); // external loop frontier
        assert!(eligible(&[0xe8, 0, 1, 0, 0, 0xc3])); // external call
        assert!(eligible(&[0x49, 0x75, 0xfd, 0xff, 0xe0])); // local loop; jmp eax
        assert!(eligible(&[0x40, 0xc3])); // short leaf still transfers every entry
    }
}

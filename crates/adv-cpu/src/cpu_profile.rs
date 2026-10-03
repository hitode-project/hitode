//! Stable virtual processor identity. Never expose host features to guest code:
//! doing so would make instruction selection depend on x64 versus ARM hosts.

/// Guest-visible flat segment selectors and descriptor-table identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentState {
    /// x86 encoding order: ES, CS, SS, DS, FS, GS.
    pub selectors: [u16; 6],
    pub gdt_base: u32,
    pub gdt_limit: u16,
}
impl Default for SegmentState {
    fn default() -> Self {
        Self {
            selectors: [16, 8, 16, 16, 24, 0],
            gdt_base: 0,
            gdt_limit: 0,
        }
    }
}

/// CPUID result in architectural EAX, EBX, ECX, EDX order.
pub(crate) fn cpuid(leaf: u32, _subleaf: u32) -> [u32; 4] {
    match leaf {
        0 => [
            1,
            u32::from_le_bytes(*b"Genu"),
            u32::from_le_bytes(*b"ntel"),
            u32::from_le_bytes(*b"ineI"),
        ],
        // Family 6; user-mode x87, CMOV, MMX, FXSAVE, SSE and SSE2.
        // TSC uses the owner's reference clock, never the host CPU counter.
        // No AVX/XSAVE/HT or RDTSCP claim.
        1 => [
            0x0000_0690,
            0,
            0,
            (1 << 0) | (1 << 4) | (1 << 15) | (1 << 23) | (1 << 24) | (1 << 25) | (1 << 26),
        ],
        0x8000_0000 => [0x8000_0000, 0, 0, 0],
        _ => [0; 4],
    }
}

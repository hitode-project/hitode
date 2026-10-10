//! Stable virtual processor identity. Never expose host features to guest code:
//! doing so would make instruction selection depend on x64 versus ARM hosts.

/// Identity of the emulated processor, shared by instruction and OS queries.
#[derive(Clone, Copy, Debug)]
pub struct ProcessorIdentity {
    pub vendor: &'static str,
    pub signature: u32,
    pub name: &'static str,
}
impl ProcessorIdentity {
    pub const fn family(self) -> u32 {
        let base = (self.signature >> 8) & 15;
        base + if base == 15 {
            (self.signature >> 20) & 255
        } else {
            0
        }
    }
    pub const fn model(self) -> u32 {
        let base_family = (self.signature >> 8) & 15;
        ((self.signature >> 4) & 15)
            | if base_family == 6 || base_family == 15 {
                (self.signature >> 12) & 240
            } else {
                0
            }
    }
    pub const fn stepping(self) -> u32 {
        self.signature & 15
    }
}
pub const PROCESSOR_IDENTITY: ProcessorIdentity = ProcessorIdentity {
    vendor: "GenuineIntel",
    signature: 0x0000_0690,
    name: "ADV virtual x86 processor",
};

/// Guest-visible flat segment selectors and descriptor-table identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentState {
    /// x86 encoding order: ES, CS, SS, DS, FS, GS.
    pub selectors: [u16; 6],
    pub gdt_base: u32,
    pub gdt_limit: u16,
    /// Guest-owned table identity, never a host kernel address. Exception
    /// delivery stays with the execution owner's existing exception pipeline.
    pub idt_base: u32,
    pub idt_limit: u16,
    /// The flat process has no local descriptor table unless its owner installs one.
    pub ldt_selector: u16,
}
impl Default for SegmentState {
    fn default() -> Self {
        Self {
            selectors: [16, 8, 16, 16, 24, 0],
            gdt_base: 0,
            gdt_limit: 0,
            idt_base: 0,
            idt_limit: 0,
            ldt_selector: 0,
        }
    }
}

/// CPUID result in architectural EAX, EBX, ECX, EDX order.
pub(crate) fn cpuid(leaf: u32, _subleaf: u32) -> [u32; 4] {
    let vendor = PROCESSOR_IDENTITY.vendor.as_bytes();
    match leaf {
        0 => [
            1,
            u32::from_le_bytes(vendor[0..4].try_into().unwrap()),
            u32::from_le_bytes(vendor[8..12].try_into().unwrap()),
            u32::from_le_bytes(vendor[4..8].try_into().unwrap()),
        ],
        // Family 6; user-mode x87, CMOV, MMX, FXSAVE, SSE and SSE2.
        // TSC uses the owner's reference clock, never the host CPU counter.
        // No AVX/XSAVE/HT or RDTSCP claim.
        1 => [
            PROCESSOR_IDENTITY.signature,
            0,
            0,
            (1 << 0) | (1 << 4) | (1 << 15) | (1 << 23) | (1 << 24) | (1 << 25) | (1 << 26),
        ],
        0x8000_0000 => [0x8000_0000, 0, 0, 0],
        _ => [0; 4],
    }
}

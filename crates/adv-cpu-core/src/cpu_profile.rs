//! Stable virtual processor identity. Never expose host features to guest code:
//! doing so would make instruction selection depend on host capabilities.

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

impl SegmentState {
    /// Writes the WoW64 flat GDT at `addr` and an empty IDT after it, and
    /// selects them.
    pub fn wow64(memory: &mut crate::Memory, addr: u32) -> Result<Self, String> {
        use crate::backend::SegmentSetup;
        memory.pages(addr, 4096)?;
        memory.committed(addr, 4096)?;
        let mut gdt = [0u8; 128];
        for (selector, code, dpl) in [
            (0x23u16, true, 3),
            (0x2b, false, 3),
            (0x30, false, 0),
            (0x53, false, 3),
        ] {
            let offset = usize::from(selector >> 3) * 8;
            gdt[offset..offset + 8].copy_from_slice(&descriptor(0, code, dpl).to_le_bytes());
        }
        memory.write(addr, &gdt)?;
        let idt_base = addr + SegmentSetup::IDT_OFFSET;
        memory.write(idt_base, &[0; SegmentSetup::IDT_LIMIT as usize + 1])?;
        Ok(Self {
            selectors: [0x2b, 0x23, 0x30, 0x2b, 0x53, 0],
            gdt_base: addr,
            gdt_limit: 127,
            idt_base,
            idt_limit: SegmentSetup::IDT_LIMIT,
            ldt_selector: 0,
        })
    }

    /// Points the FS descriptor, if there is a GDT, at `base`.
    pub fn set_fs_base(&self, memory: &mut crate::Memory, base: u32) -> Result<(), String> {
        if self.gdt_limit != 0 {
            let offset = u32::from(self.selectors[4] >> 3) * 8;
            let addr = self
                .gdt_base
                .checked_add(offset)
                .ok_or("GDT address overflow")?;
            memory.write(addr, &descriptor(base, false, 3).to_le_bytes())?;
        }
        Ok(())
    }
}

/// A flat 4 GiB code or data descriptor at `base` with privilege `dpl`.
fn descriptor(base: u32, code: bool, dpl: u64) -> u64 {
    let base = u64::from(base);
    0xffff
        | ((base & 0xffff) << 16)
        | (((base >> 16) & 0xff) << 32)
        | (((if code { 0x9b } else { 0x93 }) | (dpl << 5)) << 40)
        | (0xcf << 48)
        | ((base >> 24) << 56)
}

/// Whether a bit scan searches from the top. TZCNT and LZCNT are
/// REP-prefixed BSF and BSR, which a processor without BMI1 and LZCNT, as
/// this one reports itself, runs as BSF and BSR.
pub fn reverse_scan(mnemonic: iced_x86::Mnemonic) -> bool {
    use iced_x86::Mnemonic as M;
    match mnemonic {
        M::Bsf | M::Tzcnt => false,
        M::Bsr | M::Lzcnt => true,
        _ => unreachable!("{mnemonic:?} is not a bit scan"),
    }
}

/// CPUID result in architectural EAX, EBX, ECX, EDX order.
pub fn cpuid(leaf: u32, _subleaf: u32) -> [u32; 4] {
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

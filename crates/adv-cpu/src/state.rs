//! Executor-independent floating state for the supported 32-bit flat machine.
use crate::CpuState;

/// The last committed indirect control transfer in this CPU context.
/// This is diagnostic state, never an input to guest instruction semantics.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IndirectBranch {
    pub source: u32,
    pub target: u32,
    pub kind: IndirectBranchKind,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IndirectBranchKind {
    #[default]
    None = 0,
    Call = 1,
    Jump = 2,
    Return = 3,
}

impl IndirectBranch {
    pub fn observed(self) -> Option<Self> {
        (self.kind != IndirectBranchKind::None).then_some(self)
    }
}

/// The exact little-endian x87 encoding, including the explicit integer bit.
/// No host float conversion is involved in a context save or executor handoff.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct X87Register(pub [u8; 10]);

impl X87Register {
    pub fn from_parts(significand: u64, sign_exponent: u16) -> Self {
        let mut bytes = [0; 10];
        bytes[..8].copy_from_slice(&significand.to_le_bytes());
        bytes[8..].copy_from_slice(&sign_exponent.to_le_bytes());
        Self(bytes)
    }
    pub fn significand(self) -> u64 {
        u64::from_le_bytes(self.0[..8].try_into().unwrap())
    }
    pub fn sign_exponent(self) -> u16 {
        u16::from_le_bytes(self.0[8..].try_into().unwrap())
    }
    /// Full two-bit x87 tag: valid=0, zero=1, special=2. Empty is stored separately.
    pub fn tag(self) -> u16 {
        let exponent = self.sign_exponent() & 0x7fff;
        let significand = self.significand();
        if exponent == 0 && significand == 0 {
            1
        } else if exponent == 0 || exponent == 0x7fff || significand >> 63 == 0 {
            2
        } else {
            0
        }
    }
}

#[repr(C)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct X87State {
    pub regs: [X87Register; 8],
    pub control: u16,
    /// Includes exception/condition flags and TOP in bits 11–13.
    pub status: u16,
    /// Full physical two-bit tags, including payload-independent empty entries.
    pub tag: u16,
    pub opcode: u16,
    pub ip: u32,
    pub dp: u32,
    pub cs: u16,
    pub ds: u16,
}

impl Default for X87State {
    fn default() -> Self {
        Self {
            regs: [X87Register::default(); 8],
            // The existing Win32 runtime starts with double precision, masked exceptions.
            // Guest FINIT/FNINIT may replace this with architectural 0x037f.
            control: 0x027f,
            status: 0,
            tag: 0xffff,
            opcode: 0,
            ip: 0,
            dp: 0,
            cs: 0,
            ds: 0,
        }
    }
}

impl X87State {
    pub fn top(&self) -> usize {
        usize::from((self.status >> 11) & 7)
    }
    pub fn set_top(&mut self, top: usize) {
        self.status = (self.status & !(7 << 11)) | (((top & 7) as u16) << 11);
    }
    pub fn physical_index(&self, logical: usize) -> usize {
        (self.top() + logical) & 7
    }
    pub fn physical_tag(&self, physical: usize) -> u16 {
        (self.tag >> ((physical & 7) * 2)) & 3
    }
    pub fn set_physical_tag(&mut self, physical: usize, tag: u16) {
        let shift = (physical & 7) * 2;
        self.tag = (self.tag & !(3 << shift)) | ((tag & 3) << shift);
    }
    /// Host floating return helper. The caller supplies an exact extended encoding.
    pub fn push(&mut self, value: X87Register) {
        let slot = self.top().wrapping_sub(1) & 7;
        self.set_top(slot);
        self.regs[slot] = value;
        self.set_physical_tag(slot, value.tag());
    }
    pub fn pop(&mut self) -> Option<X87Register> {
        let slot = self.top();
        if self.physical_tag(slot) == 3 {
            return None;
        }
        let value = self.regs[slot];
        self.set_physical_tag(slot, 3);
        self.set_top(slot + 1);
        Some(value)
    }
}

impl CpuState {
    /// Materialize deferred native MMX changes at an executor/context boundary.
    /// Integer/SSE native region dispatch does not copy any floating registers.
    pub fn commit_mmx(&mut self) {
        let mode = self.mmx_mode;
        if mode & 1 != 0 {
            let written = (mode >> 8) & 0xff;
            for (slot, bits) in self.mmx.iter().copied().enumerate() {
                // mode=1 remains a supported way to seed all MMX aliases externally.
                if mode & 4 == 0 || written & (1 << slot) != 0 {
                    self.x87.regs[slot] = X87Register::from_parts(bits, 0xffff);
                }
            }
            self.x87.set_top(0);
            self.x87.tag = 0;
        }
        if mode & 2 != 0 {
            self.x87.tag = 0xffff;
        }
        self.mmx_mode = 0;
    }
    /// Refresh the native MMX cache after restoring or editing canonical x87 state.
    pub fn refresh_mmx(&mut self) {
        self.commit_mmx();
        self.mmx = self.x87.regs.map(X87Register::significand);
    }
}

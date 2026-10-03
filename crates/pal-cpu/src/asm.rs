//! Minimal x86-32 emitter for host-generated guest code (thunks, return pads) and tests.

/// Trap vector used by host thunks: `int 0x2e`.
pub const TRAP_VECTOR: u8 = 0x2e;

pub struct Asm {
    base: u32,
    pub bytes: Vec<u8>,
}

impl Asm {
    pub fn new(base: u32) -> Self {
        Self {
            base,
            bytes: Vec::new(),
        }
    }

    /// Guest address of the next emitted byte.
    pub fn here(&self) -> u32 {
        self.base + self.bytes.len() as u32
    }

    fn emit(&mut self, b: &[u8]) -> &mut Self {
        self.bytes.extend_from_slice(b);
        self
    }

    fn rel32(&mut self, target: u32) -> &mut Self {
        let next = self.here() + 4;
        self.emit(&target.wrapping_sub(next).to_le_bytes())
    }

    pub fn trap(&mut self) -> &mut Self {
        self.emit(&[0xcd, TRAP_VECTOR])
    }
    pub fn ret(&mut self) -> &mut Self {
        self.emit(&[0xc3])
    }
    /// `ret imm16` (stdcall callee cleanup).
    pub fn ret_n(&mut self, n: u16) -> &mut Self {
        if n == 0 {
            return self.ret();
        }
        self.emit(&[0xc2]).emit(&n.to_le_bytes())
    }
    pub fn int3(&mut self) -> &mut Self {
        self.emit(&[0xcc])
    }
    pub fn hlt(&mut self) -> &mut Self {
        self.emit(&[0xf4])
    }
    pub fn call(&mut self, target: u32) -> &mut Self {
        self.emit(&[0xe8]).rel32(target)
    }
    pub fn jmp(&mut self, target: u32) -> &mut Self {
        self.emit(&[0xe9]).rel32(target)
    }
    pub fn jnz(&mut self, target: u32) -> &mut Self {
        self.emit(&[0x0f, 0x85]).rel32(target)
    }
    pub fn push_imm(&mut self, v: u32) -> &mut Self {
        self.emit(&[0x68]).emit(&v.to_le_bytes())
    }
    pub fn push_ecx(&mut self) -> &mut Self {
        self.emit(&[0x51])
    }
    pub fn pop_ecx(&mut self) -> &mut Self {
        self.emit(&[0x59])
    }
    pub fn add_esp(&mut self, n: u8) -> &mut Self {
        self.emit(&[0x83, 0xc4, n])
    }
    pub fn mov_ecx_imm(&mut self, v: u32) -> &mut Self {
        self.emit(&[0xb9]).emit(&v.to_le_bytes())
    }
    pub fn mov_ebx_eax(&mut self) -> &mut Self {
        self.emit(&[0x89, 0xc3])
    }
    pub fn dec_ecx(&mut self) -> &mut Self {
        self.emit(&[0x49])
    }
    /// `mov eax, fs:[off]`
    pub fn mov_eax_fs(&mut self, off: u32) -> &mut Self {
        self.emit(&[0x64, 0xa1]).emit(&off.to_le_bytes())
    }
    /// `mov eax, [addr]`
    pub fn mov_eax_mem(&mut self, addr: u32) -> &mut Self {
        self.emit(&[0xa1]).emit(&addr.to_le_bytes())
    }
    pub fn jz(&mut self, target: u32) -> &mut Self {
        self.emit(&[0x0f, 0x84]).rel32(target)
    }
    /// `cmp dword [addr], imm8`
    pub fn cmp_mem_imm8(&mut self, addr: u32, v: i8) -> &mut Self {
        self.emit(&[0x83, 0x3d])
            .emit(&addr.to_le_bytes())
            .emit(&[v as u8])
    }
    /// `mov dword [addr], imm32`
    pub fn mov_mem_imm(&mut self, addr: u32, v: u32) -> &mut Self {
        self.emit(&[0xc7, 0x05])
            .emit(&addr.to_le_bytes())
            .emit(&v.to_le_bytes())
    }
    /// `mov eax, [esp + off]`
    pub fn mov_eax_esp(&mut self, off: u8) -> &mut Self {
        self.emit(&[0x8b, 0x44, 0x24, off])
    }
    /// `add eax, imm32`
    pub fn add_eax_imm(&mut self, v: u32) -> &mut Self {
        self.emit(&[0x05]).emit(&v.to_le_bytes())
    }
    /// `push eax`
    /// `mov eax, [eax]`
    pub fn mov_eax_ptr_eax(&mut self) -> &mut Self {
        self.emit(&[0x8b, 0x00])
    }
    pub fn push_ebx(&mut self) -> &mut Self {
        self.emit(&[0x53])
    }
    /// `mov [addr], eax`
    pub fn mov_mem_eax(&mut self, addr: u32) -> &mut Self {
        self.emit(&[0xa3]).emit(&addr.to_le_bytes())
    }
    pub fn push_eax(&mut self) -> &mut Self {
        self.emit(&[0x50])
    }
    pub fn pad_to(&mut self, align: u32, fill: u8) -> &mut Self {
        while self.here() % align != 0 {
            self.bytes.push(fill);
        }
        self
    }
}

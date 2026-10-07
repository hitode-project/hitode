//! Integer NEON templates with architectural state committed per instruction.
//!
//! CpuState::xmm contains eight raw 16-byte registers; MMX aliases the low
//! eight bytes of each physical CpuState::x87.regs slot (10-byte raw80).
//! Offsets use offset_of!, not a second floating-state ABI. Entry::state owns
//! the exclusive pointer. v0-v3 are caller-saved scratch: load on demand and
//! write through before every successor. No vector is live across a helper.
//!
//! Like floating/simd.rs and compile/mmx.rs, successful MMX instructions set
//! TOP=0 and all tags valid, including stores and MOVD to GPR. Only a written
//! MM slot gets exponent=0xffff; untouched raw80 payloads, condition flags,
//! control and last x87 pointers survive. Entry materializes deferred MMX
//! effects first. Canonical reads also permit interleaved x87 instructions.
//! Pending x87 exceptions leave before MMX memory access. EMMS stays Tier 0.
//!
//! Helpers implement the scalar Tier 1 protocol on one complete 4/8/16-byte
//! transaction: refusal changes no architecture; stores return 0/1/2 for
//! continue/committed-and-dispatch/refused. Alignment precedes permission
//! checks, matching Tier 0 (#GP for aligned SSE forms, even on missing pages).
//! MOVSS register writes preserve high lanes, memory loads zero them; MOVD
//! zeros the rest of the destination. No guest MXCSR or EFLAGS are changed.
use super::FALLBACK;
use super::emit::{Branch, Emitter, Label};
use super::native::Entry;
use crate::{CpuState, X87State};
use iced_x86::{Instruction, Mnemonic as M, OpKind, Register};
use std::mem::offset_of;

const STATE: u32 = offset_of!(Entry, state) as u32;
const VECTOR: u32 = offset_of!(Entry, vector) as u32;
const X87: usize = offset_of!(CpuState, x87);
const STATUS: u32 = (X87 + offset_of!(X87State, status)) as u32;
const TAG: u32 = (X87 + offset_of!(X87State, tag)) as u32;
const ALIGNED: u32 = 0x100;

pub(super) extern "C" fn load(entry: *mut Entry, address: u32, kind: u32) -> u32 {
    // SAFETY: emitted caller owns Entry and both pointers for this invocation.
    let entry = unsafe { &mut *entry };
    let memory = unsafe { &*entry.memory };
    let width = (kind & 31) as u8;
    if kind & ALIGNED != 0 && address & 15 != 0
        || !crate::interpreter::accessible(memory, address, width, crate::READ)
    {
        return 2;
    }
    let Ok(bytes) = memory.read_data(address, width as usize) else {
        return 2;
    };
    let mut raw = [0; 16];
    raw[..bytes.len()].copy_from_slice(&bytes);
    entry.vector = [
        u64::from_le_bytes(raw[..8].try_into().unwrap()),
        u64::from_le_bytes(raw[8..].try_into().unwrap()),
    ];
    entry.inline += 1;
    0
}

pub(super) extern "C" fn store(entry: *mut Entry, address: u32, kind: u32) -> u32 {
    // SAFETY: same exclusive owner as load. The entire range is checked first.
    let entry = unsafe { &mut *entry };
    let memory = unsafe { &mut *entry.memory };
    let width = (kind & 31) as u8;
    if kind & ALIGNED != 0 && address & 15 != 0
        || !crate::interpreter::accessible(memory, address, width, crate::WRITE)
    {
        return 2;
    }
    let mut raw = [0; 16];
    raw[..8].copy_from_slice(&entry.vector[0].to_le_bytes());
    raw[8..].copy_from_slice(&entry.vector[1].to_le_bytes());
    let epoch = memory.epoch;
    if memory.write(address, &raw[..width as usize]).is_err() {
        return 2;
    }
    entry.inline += 1;
    u32::from(
        memory.epoch != epoch
            || memory.executable(address, 1)
            || memory.executable(address.wrapping_add(u32::from(width) - 1), 1),
    )
}

fn mm(r: Register) -> bool {
    (Register::MM0..=Register::MM7).contains(&r)
}
fn vector_offset(r: Register) -> u32 {
    if mm(r) {
        (X87 + offset_of!(X87State, regs) + (r as usize - Register::MM0 as usize) * 10) as u32
    } else {
        (offset_of!(CpuState, xmm) + (r as usize - Register::XMM0 as usize) * 16) as u32
    }
}
impl Emitter {
    fn cpu_pointer(&mut self) {
        self.word(0xf940_000b | (STATE / 8) << 10 | 27 << 5); // ldr x11,[x27,#state]
    }
    fn vector_pointer(&mut self, base: u8, offset: u32) {
        self.word(0x9100_000c | offset << 10 | (base as u32) << 5); // add x12,base,#offset
    }
    fn vector_read(&mut self, v: u8, r: Register) {
        if let Some(gpr) = super::forms::register(r) {
            self.word(0x1e27_0000 | (gpr as u32) << 5 | u32::from(v)); // fmov sv,wgpr
        } else {
            self.cpu_pointer();
            self.vector_pointer(11, vector_offset(r));
            self.word((if mm(r) { 0xfd40_0000 } else { 0x3dc0_0000 }) | 12 << 5 | u32::from(v));
        }
    }
    fn mmx_guard(&mut self, pc: u32) {
        self.cpu_pointer();
        self.word(0x7940_0009 | (STATUS / 2) << 10 | 11 << 5); // ldrh w9,status
        let branch = self.words.len();
        self.word(0); // tbz w9,#7,continue
        self.constant(5, pc);
        self.jump(Label::Exit(FALLBACK), Branch::B);
        self.words[branch] = 0x3638_0009 | ((self.words.len() - branch) as u32) << 5;
    }
    fn mmx_enter(&mut self) {
        self.cpu_pointer();
        self.word(0x7940_0009 | (STATUS / 2) << 10 | 11 << 5);
        self.mask(9, 9, !(7 << 11));
        self.word(0x7900_0009 | (STATUS / 2) << 10 | 11 << 5);
        self.word(0x7900_001f | (TAG / 2) << 10 | 11 << 5);
    }
    fn vector_write(&mut self, r: Register) {
        if let Some(gpr) = super::forms::register(r) {
            self.word(0x1e26_0000 | u32::from(gpr)); // fmov wgpr,s0
            return;
        }
        self.cpu_pointer();
        self.vector_pointer(11, vector_offset(r));
        self.word((if mm(r) { 0xfd00_0000 } else { 0x3d80_0000 }) | 12 << 5);
        if mm(r) {
            self.constant(9, 0xffff);
            self.word(0x7900_1189); // strh w9,[x12,#8]
            let offset = offset_of!(CpuState, mmx) as u32 + (r as u32 - Register::MM0 as u32) * 8;
            self.vector_pointer(11, offset);
            self.word(0xfd00_0180); // str d0,[x12]
        }
    }
    fn insert_s(&mut self, dst: u8, lane: u32, src: u8, source_lane: u32) {
        self.word(
            0x6e00_0400
                | (4 + lane * 8) << 16
                | source_lane << 13
                | u32::from(src) << 5
                | u32::from(dst),
        );
    }
    pub(super) fn simd_instruction(&mut self, i: &Instruction) {
        let m = i.mnemonic();
        let is_mmx = (0..i.op_count()).any(|op| mm(i.op_register(op)));
        let q = if is_mmx { 0 } else { 1 << 30 };
        if is_mmx {
            self.mmx_guard(i.ip32());
        }
        let moves = matches!(
            m,
            M::Movq | M::Movd | M::Movaps | M::Movups | M::Movdqa | M::Movdqu | M::Movss
        );
        let width = match m {
            M::Movd | M::Movss => 4,
            M::Movq => 8,
            _ if is_mmx => 8,
            _ => 16,
        };
        let kind = width
            | if width == 16 && !matches!(m, M::Movups | M::Movdqu) {
                ALIGNED
            } else {
                0
            };
        if i.op0_kind() == OpKind::Memory {
            self.vector_read(0, i.op1_register());
            self.vector_pointer(27, VECTOR);
            self.word(0x3d80_0180); // str q0,[x12]
            self.ea(i, true);
            self.mov(1, 9);
            self.constant(2, kind);
            self.call(self.helpers.vector_store);
            self.stored(i.ip32(), Some(i.next_ip32()), false, |e| {
                if is_mmx {
                    e.mmx_enter();
                }
            });
            return;
        }
        if i.op1_kind() == OpKind::Memory {
            self.ea(i, true);
            self.mov(1, 9);
            self.constant(2, kind);
            self.call(self.helpers.vector_load);
            let ok = self.words.len();
            self.word(0);
            self.constant(5, i.ip32());
            self.jump(Label::Exit(FALLBACK), Branch::B);
            self.words[ok] = 0x3400_0000 | ((self.words.len() - ok) as u32) << 5;
            self.vector_pointer(27, VECTOR);
            self.word(0x3dc0_0181); // ldr q1,[x12]
        } else if i.op1_kind() != OpKind::Immediate8 {
            self.vector_read(1, i.op1_register());
        }
        if moves {
            if m == M::Movss && i.op1_kind() == OpKind::Register {
                self.vector_read(0, i.op0_register());
                self.insert_s(0, 0, 1, 0);
            } else if m == M::Movd {
                self.word(0x5e04_0420); // dup s0,v1.s[0], clears upper bits
            } else if m == M::Movq {
                self.word(0x5e08_0420); // dup d0,v1.d[0], clears upper bits
            } else {
                self.word(0x4ea1_1c20); // mov v0.16b,v1.16b
            }
        } else {
            self.vector_read(0, i.op0_register());
            self.packed(i, q);
        }
        self.vector_write(i.op0_register());
        if is_mmx {
            self.mmx_enter();
        }
        self.charge();
        self.jump(Label::Guest(i.next_ip32()), Branch::B);
    }
    fn packed(&mut self, i: &Instruction, q: u32) {
        let m = i.mnemonic();
        let size = match m {
            M::Paddw
            | M::Psubw
            | M::Paddsw
            | M::Psubsw
            | M::Paddusw
            | M::Psubusw
            | M::Pmullw
            | M::Punpcklwd
            | M::Punpckhwd
            | M::Psrlw
            | M::Psllw
            | M::Psraw => 1,
            M::Paddd | M::Psubd | M::Punpckldq | M::Punpckhdq | M::Psrld | M::Pslld | M::Psrad => 2,
            M::Psrlq | M::Psllq => 3,
            _ => 0,
        };
        let op = match m {
            M::Paddb | M::Paddw | M::Paddd => 0x0e21_8400,
            M::Psubb | M::Psubw | M::Psubd => 0x2e21_8400,
            M::Paddsb | M::Paddsw => 0x0e21_0c00,
            M::Paddusb | M::Paddusw => 0x2e21_0c00,
            M::Psubsb | M::Psubsw => 0x0e21_2c00,
            M::Psubusb | M::Psubusw => 0x2e21_2c00,
            M::Pmullw => 0x0e21_9c00,
            M::Punpcklbw | M::Punpcklwd | M::Punpckldq => 0x0e01_3800,
            M::Punpckhbw | M::Punpckhwd | M::Punpckhdq => 0x0e01_7800,
            M::Pand => 0x0e21_1c00,
            M::Por => 0x0ea1_1c00,
            M::Pxor => 0x2e21_1c00,
            M::Pandn => 0x0e60_1c20,
            _ => 0,
        };
        if op != 0 {
            self.word(op | q | size << 22);
            return;
        }
        match m {
            M::Pmulhw => {
                self.word(0x0e61_c002); // smull v2.4s,v0.4h,v1.4h
                if q != 0 {
                    self.word(0x4e61_c003);
                } // smull2 v3.4s,v0.8h,v1.8h
                self.word(0x0f10_8440); // shrn v0.4h,v2.4s,#16
                if q != 0 {
                    self.word(0x4f10_8460);
                }
            }
            M::Packuswb | M::Packsswb | M::Packssdw => {
                let op = match m {
                    M::Packuswb => 0x2e21_2800,
                    M::Packssdw => 0x0e61_4800,
                    _ => 0x0e21_4800,
                };
                // Narrow separately, then concatenate low 32/64-bit halves.
                self.word(op | 2);
                self.word(op | 1 << 5 | 3);
                self.word(if q == 0 { 0x0e83_3840 } else { 0x4ec3_3840 });
            }
            M::Shufps => {
                let mask = u32::from(i.immediate8());
                for lane in 0..4 {
                    self.insert_s(
                        2,
                        lane,
                        if lane < 2 { 0 } else { 1 },
                        (mask >> (lane * 2)) & 3,
                    );
                }
                self.word(0x4ea2_1c40); // mov v0.16b,v2.16b
            }
            M::Psrlw
            | M::Psrld
            | M::Psrlq
            | M::Psllw
            | M::Pslld
            | M::Psllq
            | M::Psraw
            | M::Psrad => {
                // x86 takes the unsigned low 64-bit count, without a mask.
                // NEON reads signed low bytes: clamp before DUP, then negate
                // for right shifts. Width is sufficient for sign-fill too.
                let bits = 8 << size;
                if i.op1_kind() == OpKind::Immediate8 {
                    self.constant(9, u32::from(i.immediate8()).min(bits));
                } else {
                    self.word(0x4e08_3c29); // umov x9,v1.d[0]
                    self.word(0xf100_013f | bits << 10); // cmp x9,#bits
                    self.constant(10, bits);
                    self.word(0x9a8a_3129); // csel x9,x9,x10,lo
                }
                if !matches!(m, M::Psllw | M::Pslld | M::Psllq) {
                    self.word(0x4b09_03e9);
                }
                self.word(0x4e00_0d21 | (1 << size) << 16); // dup v1, count
                let signed = matches!(m, M::Psraw | M::Psrad);
                // Use Q=1 even for MMX .1d (reserved); only low 64 bits commit.
                self.word((if signed { 0x4e21_4400 } else { 0x6e21_4400 }) | size << 22);
            }
            _ => unreachable!("shared SIMD form guard"),
        }
    }
}

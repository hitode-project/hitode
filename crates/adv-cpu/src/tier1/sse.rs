//! Transactional SSE floating helpers for Tier 1's per-instruction ABI.
//!
//! Only default MXCSR controls enter AArch64 FP. Normal finite operands and
//! signed zeros are admitted; NaNs, infinities and denormals rerun in Tier 0.
//! Each lane uses an isolated FPCR/FPSR scope in one asm block. Only IXC may
//! commit (as guest PE); invalid, divide-by-zero, overflow, underflow and input
//! denormal flags refuse the entire instruction, including earlier lanes.
//! This also avoids ARM saturation versus x86 integer indefinite differences.
//! MIN/MAX select the second raw operand on equality, preserving signed zero.
//! No host FP status, register result, EFLAGS or guest status leaks on refusal.
use super::native::Entry;
use crate::{CpuState, Memory, READ, WRITE};
use iced_x86::{Instruction, Mnemonic as M, OpKind, Register};
use std::collections::BTreeMap;

pub(super) struct Operations(Box<[Instruction]>);
impl Operations {
    pub fn new(
        instructions: &BTreeMap<u32, Instruction>,
        _host_features: crate::HostFeatures,
    ) -> Self {
        Self(
            instructions
                .values()
                .filter(|i| super::forms::sse(i))
                .copied()
                .collect(),
        )
    }
    pub fn get(&self, pc: u32) -> Option<u64> {
        self.0
            .binary_search_by_key(&pc, Instruction::ip32)
            .ok()
            .map(|n| std::ptr::from_ref(&self.0[n]) as u64)
    }
}

fn xmm(r: Register) -> Option<usize> {
    (Register::XMM0..=Register::XMM7)
        .contains(&r)
        .then(|| r as usize - Register::XMM0 as usize)
}
fn gpr(r: Register) -> usize {
    usize::from(super::forms::register(r).unwrap() - 19)
}
fn bits(bytes: &[u8]) -> u64 {
    let mut raw = [0; 8];
    raw[..bytes.len()].copy_from_slice(bytes);
    u64::from_le_bytes(raw)
}
fn ordinary(bits: u64, width: usize) -> bool {
    let (sign, exponent) = if width == 4 {
        (1 << 31, 0x7f80_0000)
    } else {
        (1 << 63, 0x7ff0_0000_0000_0000)
    };
    bits & !sign == 0 || (bits & exponent != 0 && bits & exponent != exponent)
}

fn floating_result(value: u64, width: usize, status: u32) -> bool {
    let (sign, minimum) = if width == 4 {
        (1 << 31, 0x0080_0000)
    } else {
        (1 << 63, 0x0010_0000_0000_0000)
    };
    // Tier 0 detects tininess before rounding. A tiny inexact value can round
    // up to the smallest normal, so checking only the rounded exponent and
    // host UFC cannot prove its guest UE. Keep that boundary in Tier 0 too.
    status & !0x10 == 0
        && ordinary(value, width)
        && !(status & 0x10 != 0 && value & !sign == minimum)
}
fn compare(a: u64, b: u64, width: usize) -> std::cmp::Ordering {
    let sign = if width == 4 { 1 << 31 } else { 1 << 63 };
    if a & !sign == 0 && b & !sign == 0 {
        return std::cmp::Ordering::Equal;
    }
    let key = |x: u64| if x & sign != 0 { !x } else { x | sign };
    // Mask the unused upper half before comparing negative single values.
    let mask = if width == 4 {
        u64::from(u32::MAX)
    } else {
        u64::MAX
    };
    (key(a) & mask).cmp(&(key(b) & mask))
}

/// The asm block owns both FP operands and the complete host environment.
/// Integer moves preserve raw zeros; no Rust FP operation can be reordered
/// across the save/restore. No guest memory is accessed inside this scope.
fn calculate(a: u64, b: u64, operation: u8, width: usize) -> (u64, u32) {
    macro_rules! fp {
        ($instruction:literal) => {{
            let result: u64;
            let status: u64;
            // SAFETY: scalar AArch64 FP is mandatory on supported targets.
            // Both system registers are restored even when the result refuses.
            unsafe {
                core::arch::asm!(
                    "mrs {control}, fpcr", "mrs {saved}, fpsr",
                    "msr fpcr, xzr", "msr fpsr, xzr",
                    "fmov d0, {a}", "fmov d1, {b}", $instruction,
                    "fmov {result}, d0", "mrs {status}, fpsr",
                    "msr fpsr, {saved}", "msr fpcr, {control}",
                    a = in(reg) a, b = in(reg) b,
                    control = out(reg) _, saved = out(reg) _,
                    result = out(reg) result, status = out(reg) status,
                    out("v0") _, out("v1") _, out("x10") _,
                    options(nostack, preserves_flags),
                );
            }
            (result, status as u32)
        }};
    }
    match (operation, width) {
        (0, 4) => fp!("fadd s0, s0, s1"),
        (0, _) => fp!("fadd d0, d0, d1"),
        (1, 4) => fp!("fsub s0, s0, s1"),
        (1, _) => fp!("fsub d0, d0, d1"),
        (2, 4) => fp!("fmul s0, s0, s1"),
        (2, _) => fp!("fmul d0, d0, d1"),
        (3, 4) => fp!("fdiv s0, s0, s1"),
        (3, _) => fp!("fdiv d0, d0, d1"),
        (4, 4) => fp!("fsqrt s0, s1"),
        (4, _) => fp!("fsqrt d0, d1"),
        (5, _) => fp!("fcvt d0, s1"),
        (6, _) => fp!("fcvt s0, d1"),
        (7, 4) => fp!("fmov w10, s1\nscvtf s0, w10"),
        (7, _) => fp!("fmov w10, s1\nscvtf d0, w10"),
        (8, 4) => fp!("fcvtns w10, s1\nfmov s0, w10"),
        (8, _) => fp!("fcvtns w10, d1\nfmov s0, w10"),
        (9, 4) => fp!("fcvtzs w10, s1\nfmov s0, w10"),
        (9, _) => fp!("fcvtzs w10, d1\nfmov s0, w10"),
        _ => unreachable!("shared SSE operation"),
    }
}

/// (source kind, destination kind, lanes, packed destination, truncation).
/// Kinds are float32, float64 and signed int32, matching Tier 0 conversions.
fn conversion(m: M) -> Option<(u8, u8, usize, bool, bool)> {
    Some(match m {
        M::Cvtss2sd => (0, 1, 1, false, false),
        M::Cvtsd2ss => (1, 0, 1, false, false),
        M::Cvtsi2ss => (2, 0, 1, false, false),
        M::Cvtsi2sd => (2, 1, 1, false, false),
        M::Cvtss2si | M::Cvttss2si => (0, 2, 1, false, m == M::Cvttss2si),
        M::Cvtsd2si | M::Cvttsd2si => (1, 2, 1, false, m == M::Cvttsd2si),
        M::Cvtdq2ps => (2, 0, 4, true, false),
        M::Cvtdq2pd => (2, 1, 2, true, false),
        M::Cvtps2dq | M::Cvttps2dq => (0, 2, 4, true, m == M::Cvttps2dq),
        M::Cvtpd2dq | M::Cvttpd2dq => (1, 2, 2, true, m == M::Cvttpd2dq),
        M::Cvtps2pd => (0, 1, 2, true, false),
        M::Cvtpd2ps => (1, 0, 2, true, false),
        _ => return None,
    })
}

fn source(
    i: &Instruction,
    state: &CpuState,
    entry: &Entry,
    memory: &Memory,
    address: u32,
    width: usize,
) -> Option<[u8; 16]> {
    let mut bytes = [0; 16];
    if i.op1_kind() == OpKind::Memory {
        if width == 16 && address & 15 != 0
            || !crate::interpreter::accessible(memory, address, width as u8, READ)
        {
            return None;
        }
        bytes[..width].copy_from_slice(&memory.read_data(address, width).ok()?);
    } else if let Some(n) = xmm(i.op1_register()) {
        bytes = state.xmm[n];
    } else {
        bytes[..4].copy_from_slice(&entry.frame.regs[gpr(i.op1_register())].to_le_bytes());
    }
    Some(bytes)
}

fn execute(entry: &mut Entry, i: &Instruction, address: u32) -> Option<u32> {
    // SAFETY: the native owner pins both exclusive pointers for this call.
    let state = unsafe { &mut *entry.state };
    let memory = unsafe { &mut *entry.memory };
    let m = i.mnemonic();
    if matches!(m, M::Ldmxcsr | M::Stmxcsr) {
        let permission = if m == M::Ldmxcsr { READ } else { WRITE };
        if !crate::interpreter::accessible(memory, address, 4, permission) {
            return None;
        }
        let mut status = 0;
        if m == M::Ldmxcsr {
            let value = bits(&memory.read_data(address, 4).ok()?) as u32;
            if value & !0xffff != 0 {
                return None;
            }
            state.mxcsr = value;
        } else {
            let epoch = memory.epoch;
            memory.write(address, &state.mxcsr.to_le_bytes()).ok()?;
            status = u32::from(
                memory.epoch != epoch
                    || memory.executable(address, 1)
                    || memory.executable(address.wrapping_add(3), 1),
            );
        }
        entry.inline += 1;
        return Some(status);
    }
    // Also exclude RC and FTZ, beyond the conversion mask used by Cranelift.
    if state.mxcsr & !0x3f != 0x1f80 {
        return None;
    }
    let dst = xmm(i.op0_register());
    let mut result = dst.map_or([0; 16], |n| state.xmm[n]);
    let mut flags = 0;
    let mut eflags = entry.frame.flags;
    if let Some((from, to, lanes, packed, truncate)) = conversion(m) {
        let sw = if from == 1 { 8 } else { 4 };
        let tw = if to == 1 { 8 } else { 4 };
        let input = source(i, state, entry, memory, address, sw * lanes)?;
        if packed {
            result = [0; 16];
        }
        for lane in 0..lanes {
            let b = bits(&input[lane * sw..(lane + 1) * sw]);
            if from != 2 && !ordinary(b, sw) {
                return None;
            }
            let op = if from == 2 {
                7
            } else if to == 2 {
                if truncate { 9 } else { 8 }
            } else if to == 1 {
                5
            } else {
                6
            };
            let width = if from == 2 { tw } else { sw };
            let (value, status) = calculate(0, b, op, width);
            if status & !0x10 != 0 || to != 2 && !floating_result(value, tw, status) {
                return None;
            }
            flags |= (status & 0x10) << 1;
            result[lane * tw..(lane + 1) * tw].copy_from_slice(&value.to_le_bytes()[..tw]);
        }
    } else {
        let wide = matches!(
            m,
            M::Addsd
                | M::Subsd
                | M::Mulsd
                | M::Divsd
                | M::Sqrtsd
                | M::Minsd
                | M::Maxsd
                | M::Addpd
                | M::Subpd
                | M::Mulpd
                | M::Divpd
                | M::Sqrtpd
                | M::Minpd
                | M::Maxpd
                | M::Cmpsd
                | M::Cmppd
                | M::Comisd
                | M::Ucomisd
        );
        let packed = matches!(
            m,
            M::Addps
                | M::Subps
                | M::Mulps
                | M::Divps
                | M::Sqrtps
                | M::Minps
                | M::Maxps
                | M::Cmpps
                | M::Addpd
                | M::Subpd
                | M::Mulpd
                | M::Divpd
                | M::Sqrtpd
                | M::Minpd
                | M::Maxpd
                | M::Cmppd
        );
        let width = if wide { 8 } else { 4 };
        let lanes = if packed { 16 / width } else { 1 };
        let input = source(i, state, entry, memory, address, width * lanes)?;
        for lane in 0..lanes {
            let range = lane * width..(lane + 1) * width;
            let a = if matches!(m, M::Sqrtss | M::Sqrtsd | M::Sqrtps | M::Sqrtpd) {
                0
            } else {
                bits(&result[range.clone()])
            };
            let b = bits(&input[range.clone()]);
            if !ordinary(a, width) || !ordinary(b, width) {
                return None;
            }
            let order = compare(a, b, width);
            let value = match m {
                M::Comiss | M::Ucomiss | M::Comisd | M::Ucomisd => {
                    eflags = (eflags & !0x8d5)
                        | if order.is_eq() {
                            0x40
                        } else if order.is_lt() {
                            1
                        } else {
                            0
                        };
                    continue;
                }
                M::Cmpss | M::Cmpsd | M::Cmpps | M::Cmppd => {
                    let yes = match i.immediate8() & 7 {
                        0 => order.is_eq(),
                        1 => order.is_lt(),
                        2 => !order.is_gt(),
                        3 => false,
                        4 => !order.is_eq(),
                        5 => !order.is_lt(),
                        6 => order.is_gt(),
                        _ => true,
                    };
                    if yes { u64::MAX } else { 0 }
                }
                M::Minss | M::Minsd | M::Minps | M::Minpd => {
                    if order.is_lt() {
                        a
                    } else {
                        b
                    }
                }
                M::Maxss | M::Maxsd | M::Maxps | M::Maxpd => {
                    if order.is_gt() {
                        a
                    } else {
                        b
                    }
                }
                _ => {
                    let op = match m {
                        M::Addss | M::Addsd | M::Addps | M::Addpd => 0,
                        M::Subss | M::Subsd | M::Subps | M::Subpd => 1,
                        M::Mulss | M::Mulsd | M::Mulps | M::Mulpd => 2,
                        M::Divss | M::Divsd | M::Divps | M::Divpd => 3,
                        _ => 4,
                    };
                    let (value, status) = calculate(a, b, op, width);
                    if !floating_result(value, width, status) {
                        return None;
                    }
                    flags |= (status & 0x10) << 1;
                    value
                }
            };
            result[range].copy_from_slice(&value.to_le_bytes()[..width]);
        }
    }
    // All lanes and accesses passed. Commit one instruction atomically.
    if let Some(n) = dst {
        state.xmm[n] = result;
    } else {
        entry.frame.regs[gpr(i.op0_register())] = bits(&result[..4]) as u32;
    }
    entry.frame.flags = eflags;
    state.mxcsr |= flags;
    if i.op1_kind() == OpKind::Memory {
        entry.inline += 1;
    }
    Some(0)
}

pub(super) extern "C" fn call(entry: *mut Entry, op: *const Instruction, address: u32) -> u32 {
    // SAFETY: the immutable operation array and exclusive Entry outlive BLR.
    let (entry, op) = unsafe { (&mut *entry, &*op) };
    execute(entry, op, address).unwrap_or(2)
}

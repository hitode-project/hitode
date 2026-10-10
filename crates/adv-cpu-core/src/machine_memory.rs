//! Explicit x64 machine loads, stores and REP moves over pinned guest RAM.
//! No Rust reference into guest memory is live across these accesses.

/// # Safety
/// The caller pins and validates the whole range. No Rust reference into guest
/// RAM may remain live. Alignment guarantees follow the actual x86 operand.
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn load(pointer: *const u8, width: usize) -> u64 {
    let value: u64;
    unsafe {
        match width {
            1 => {
                core::arch::asm!("movzx {value:e}, byte ptr [{pointer}]", pointer=in(reg) pointer, value=out(reg) value, options(nostack, preserves_flags))
            }
            2 => {
                core::arch::asm!("movzx {value:e}, word ptr [{pointer}]", pointer=in(reg) pointer, value=out(reg) value, options(nostack, preserves_flags))
            }
            4 => {
                core::arch::asm!("mov {value:e}, dword ptr [{pointer}]", pointer=in(reg) pointer, value=out(reg) value, options(nostack, preserves_flags))
            }
            8 => {
                core::arch::asm!("mov {value}, qword ptr [{pointer}]", pointer=in(reg) pointer, value=out(reg) value, options(nostack, preserves_flags))
            }
            _ => unreachable!("invalid foreign load width"),
        }
    }
    value
}

/// # Safety
/// Same pinned, validated machine-memory contract as load; range must be writable.
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn store(pointer: *mut u8, value: u64, width: usize) {
    unsafe {
        match width {
            1 => {
                core::arch::asm!("mov byte ptr [{pointer}], {value}", pointer=in(reg) pointer, value=in(reg_byte) value as u8, options(nostack, preserves_flags))
            }
            2 => {
                core::arch::asm!("mov word ptr [{pointer}], {value:x}", pointer=in(reg) pointer, value=in(reg) value, options(nostack, preserves_flags))
            }
            4 => {
                core::arch::asm!("mov dword ptr [{pointer}], {value:e}", pointer=in(reg) pointer, value=in(reg) value, options(nostack, preserves_flags))
            }
            8 => {
                core::arch::asm!("mov qword ptr [{pointer}], {value}", pointer=in(reg) pointer, value=in(reg) value, options(nostack, preserves_flags))
            }
            _ => unreachable!("invalid foreign store width"),
        }
    }
}

/// # Safety
/// Validated, pinned forward REP operands in ordinary machine memory, with no
/// live Rust references. Overlap is allowed: each element is read before it is
/// written, retaining guest width and propagation (not memmove semantics).
#[cfg(target_arch = "x86_64")]
pub unsafe fn repeat(
    source: *const u8,
    destination: *mut u8,
    count: usize,
    width: usize,
    fill: Option<u32>,
) {
    unsafe {
        if let Some(value) = fill {
            match width {
                1 => {
                    core::arch::asm!("rep stosb", in("eax") value, inout("rdi") destination => _, inout("rcx") count => _, options(nostack, preserves_flags))
                }
                2 => {
                    core::arch::asm!("rep stosw", in("eax") value, inout("rdi") destination => _, inout("rcx") count => _, options(nostack, preserves_flags))
                }
                4 => {
                    core::arch::asm!("rep stosd", in("eax") value, inout("rdi") destination => _, inout("rcx") count => _, options(nostack, preserves_flags))
                }
                _ => unreachable!(),
            }
        } else {
            match width {
                1 => {
                    core::arch::asm!("rep movsb", inout("rsi") source => _, inout("rdi") destination => _, inout("rcx") count => _, options(nostack, preserves_flags))
                }
                2 => {
                    core::arch::asm!("rep movsw", inout("rsi") source => _, inout("rdi") destination => _, inout("rcx") count => _, options(nostack, preserves_flags))
                }
                4 => {
                    core::arch::asm!("rep movsd", inout("rsi") source => _, inout("rdi") destination => _, inout("rcx") count => _, options(nostack, preserves_flags))
                }
                _ => unreachable!(),
            }
        }
    }
}

#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn repeat(_: *const u8, _: *mut u8, _: usize, _: usize, _: Option<u32>) {
    unreachable!("machine memory helpers require x64")
}

// Callers fall back before reaching these helpers on other ISAs.
#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn load(_: *const u8, _: usize) -> u64 {
    unreachable!("machine memory helpers require x64")
}
#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn store(_: *mut u8, _: u64, _: usize) {
    unreachable!("machine memory helpers require x64")
}

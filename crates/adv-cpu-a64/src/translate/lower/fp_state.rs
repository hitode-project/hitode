//! Shared FPCR shadow/lazy-sync and lazy FPSR sticky-bit boundary emitters
//! (A64F-FP-5, A64-OPT-4, A64-OPT-6).
//!
//! Each host execution thread owns one `Runtime` (pointed to by `x29`), which
//! records `host_fpcr`, `active_fpcr` and `fpsr_owner`. Root entries capture
//! `host_fpcr` once without writing FPCR; translated code lazily synchronizes
//! FPCR only when `active_fpcr` differs from the desired mode, keeping
//! `FPCR.NEP` clear; root exits and non-`FpClean` host calls restore `host_fpcr`
//! only on mismatch.
//!
//! With `FEAT_AFP`, lazy FPSR accumulation is armed only when all SSE exceptions
//! are masked (`MXCSR & 0x1f80 == 0x1f80`). When any SSE exception is unmasked,
//! translated code keeps the exact path so pre-computation and post-computation
//! flag ordering before `#XM` stays exact. Speculative cold exits (`opt_exit`,
//! `resident_exit`) discard uncommitted hardware FPSR (`fpsr_owner = Clean`)
//! rather than folding it into `CpuState::mxcsr` (A64-OPT-6).
//!
//! Every emitter here preserves guest NZCV flags (using `eor`/`cbz`/`tbz`
//! instead of flag-setting compares).

use crate::asm::Asm;
use crate::encode::Shift::Lsl;
use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::runtime;

use super::RUNTIME;

/// `FPCR.FIZ` (bit 0): flush input denormals to zero under `FEAT_AFP`.
#[allow(dead_code)]
pub(crate) const FPCR_FIZ: u32 = 1 << 0;
/// `FPCR.AH` (bit 1): alternate floating-point handling (`FEAT_AFP`).
pub(crate) const FPCR_AH: u32 = 1 << 1;
/// `FPCR.NEP` (bit 2): MUST remain clear (A64F-FP-5).
pub(crate) const FPCR_NEP: u32 = 1 << 2;
/// `FPCR.RMode` (bits 23:22).
pub(crate) const FPCR_RMODE_SHIFT: u32 = 22;
/// `FPCR.FZ` (bit 24): flush output denormals to zero.
#[allow(dead_code)]
pub(crate) const FPCR_FZ: u32 = 1 << 24;
/// Canonical `FEAT_AFP` FPCR shared by default MXCSR (`0x1f80`) and x87 RN.
#[allow(dead_code)]
pub(crate) const FPCR_X87_DEFAULT: u32 = FPCR_AH;

/// MXCSR bits 12:7 (PM|UM|OM|ZM|DM|IM): all six SSE exceptions masked.
pub(crate) const MXCSR_ALL_MASKED: u32 = 0x1f80;
/// Hardware FPSR check mask for non-inexact exceptions, including `IDC` (bit 7).
pub(crate) const FPSR_CHECK_MASK: u32 = 0x8f;
/// All six hardware FPSR sticky exception bits (`IDC|IXC|UFC|OFC|DZC|IOC`).
pub(crate) const FPSR_STICKY_MASK: u32 = 0x9f;
/// `FPSR.IXC` (bit 4).
const FPSR_IXC: u32 = 1 << 4;

/// Computes the `FEAT_AFP` hardware FPCR matching `mxcsr` control fields.
///
/// Maps `DAZ -> FIZ`, `FTZ -> FZ`, swaps MXCSR RC `01`/`10` (RD/RU) to AArch64
/// `RMode` `10`/`01` (RM/RP), sets `AH = 1`, and keeps `DN = 0` and `NEP = 0`.
#[allow(dead_code)]
pub(crate) const fn fpcr_from_mxcsr(mxcsr: u32) -> u32 {
    let fiz = (mxcsr >> 6) & 1;
    let rc = (mxcsr >> 13) & 3;
    let rmode = match rc {
        0 => 0, // RN
        1 => 2, // RD (-Inf) -> RM
        2 => 1, // RU (+Inf) -> RP
        _ => 3, // RZ (0) -> RZ
    };
    let fz = (mxcsr >> 15) & 1;
    FPCR_AH | fiz | (rmode << FPCR_RMODE_SHIFT) | (fz << 24)
}

/// Returns true iff all six SSE exceptions are masked in `mxcsr`.
#[allow(dead_code)]
pub(crate) const fn sse_exceptions_masked(mxcsr: u32) -> bool {
    mxcsr & MXCSR_ALL_MASKED == MXCSR_ALL_MASKED
}

/// Pure-Rust reference mapping from hardware `FPSR` sticky bits `{0,7,1,2,3,4}`
/// to x86 `MXCSR[5:0]` (`{IE,DE,ZE,OE,UE,PE}`).
#[cfg(test)]
pub(crate) const fn map_fpsr_to_mxcsr_bits(fpsr: u32) -> u32 {
    (fpsr & 0x01) | ((fpsr & 0x1e) << 1) | (((fpsr >> 7) & 1) << 1)
}

/// Captures `host_fpcr` at a root entry (`enter` / `opt_enter`) and initializes
/// `active_fpcr = host_fpcr` and `fpsr_owner = Clean` with zero `msr` writes.
pub(crate) fn emit_init_fp_boundary(asm: &mut Asm, scratch: Reg) {
    asm.emit_all(&[
        mrs_fpcr(scratch),
        str(W, scratch, RUNTIME, runtime::HOST_FPCR),
        str(W, scratch, RUNTIME, runtime::ACTIVE_FPCR),
        strb(Reg::ZR, RUNTIME, runtime::FPSR_OWNER),
    ]);
}

/// Synchronizes hardware FPCR to the 32-bit value in `desired` (`W` register,
/// zero-extended in `X`), skipping `msr fpcr` when `active_fpcr == desired`.
///
/// Preserves guest NZCV flags. `desired` and `tmp` must be distinct.
#[allow(dead_code)]
pub(crate) fn emit_sync_fpcr(asm: &mut Asm, desired: Reg, tmp: Reg) {
    assert_ne!(desired, tmp);
    let done = asm.label();
    asm.emit_all(&[
        ldr(W, tmp, RUNTIME, runtime::ACTIVE_FPCR),
        eor(W, tmp, tmp, desired, Lsl, 0),
    ]);
    asm.cbz(W, false, tmp, done);
    asm.emit_all(&[
        msr_fpcr(desired),
        str(W, desired, RUNTIME, runtime::ACTIVE_FPCR),
    ]);
    runtime::emit_inc_fp_counter(asm, runtime::FP_FPCR_WRITES, 1, tmp);
    asm.bind(done);
}

/// Synchronizes hardware FPCR to constant `desired` (which must keep `NEP = 0`).
///
/// Preserves guest NZCV flags. `tmp1` and `tmp2` must be distinct.
#[allow(dead_code)]
pub(crate) fn emit_sync_fpcr_imm(asm: &mut Asm, desired: u32, tmp1: Reg, tmp2: Reg) {
    assert_eq!(desired & FPCR_NEP, 0, "FPCR.NEP must remain clear");
    assert_ne!(tmp1, tmp2);
    asm.emit(movz(W, tmp1, desired as u16, 0));
    if desired >> 16 != 0 {
        asm.emit(movk(W, tmp1, (desired >> 16) as u16, 16));
    }
    emit_sync_fpcr(asm, tmp1, tmp2);
}

/// Restores `host_fpcr` if `active_fpcr != host_fpcr` and updates `active_fpcr`.
///
/// Preserves guest NZCV flags. `tmp1` and `tmp2` must be distinct.
pub(crate) fn emit_restore_host_fpcr(asm: &mut Asm, tmp1: Reg, tmp2: Reg) {
    assert_ne!(tmp1, tmp2);
    let done = asm.label();
    asm.emit_all(&[
        ldr(W, tmp1, RUNTIME, runtime::HOST_FPCR),
        ldr(W, tmp2, RUNTIME, runtime::ACTIVE_FPCR),
        eor(W, tmp2, tmp2, tmp1, Lsl, 0),
    ]);
    asm.cbz(W, false, tmp2, done);
    asm.emit_all(&[msr_fpcr(tmp1), str(W, tmp1, RUNTIME, runtime::ACTIVE_FPCR)]);
    runtime::emit_inc_fp_counter(asm, runtime::FP_FPCR_WRITES, 1, tmp2);
    asm.bind(done);
}

/// Arms lazy SSE sticky-bit accumulation (`Clean -> SseLive`).
///
/// Caller MUST ensure `FEAT_AFP` is active and ALL SSE exceptions are masked
/// (`sse_exceptions_masked(mxcsr)`). Preserves guest NZCV flags.
#[allow(dead_code)]
pub(crate) fn emit_arm_sse_fpsr(asm: &mut Asm, tmp: Reg) {
    let armed = asm.label();
    asm.emit(ldrb(tmp, RUNTIME, runtime::FPSR_OWNER));
    asm.cbz(W, true, tmp, armed);
    asm.emit_all(&[
        msr_fpsr(Reg::ZR),
        movz(W, tmp, u16::from(runtime::FPSR_SSE_LIVE), 0),
        strb(tmp, RUNTIME, runtime::FPSR_OWNER),
    ]);
    asm.bind(armed);
}

/// Branchlessly maps hardware FPSR sticky bits `{0,7,1,2,3,4}` in `src` to x86
/// `MXCSR[5:0]` in `dst` (5 instructions, preserves NZCV).
///
/// `dst`, `src` and `tmp` must be distinct.
pub(crate) fn emit_map_fpsr_to_mxcsr(asm: &mut Asm, dst: Reg, src: Reg, tmp: Reg) {
    assert!(dst != src && dst != tmp && src != tmp);
    asm.emit_all(&[
        and_imm(W, dst, src, 0x01),
        and_imm(W, tmp, src, 0x1e),
        orr(W, dst, dst, tmp, Lsl, 1),
        ubfx(W, tmp, src, 7, 1),
        orr(W, dst, dst, tmp, Lsl, 1),
    ]);
}

/// Folds a previously read hardware FPSR value in `fpsr` into `CpuState::mxcsr`,
/// transitions `fpsr_owner` to `Clean`, and increments `FP_FPSR_FOLDS`.
///
/// Clobbers `fpsr`, `tmp1` and `tmp2` (all three must be distinct). Preserves NZCV.
pub(crate) fn emit_fold_fpsr_value(asm: &mut Asm, fpsr: Reg, tmp1: Reg, tmp2: Reg) {
    assert!(fpsr != tmp1 && fpsr != tmp2 && tmp1 != tmp2);
    emit_map_fpsr_to_mxcsr(asm, tmp1, fpsr, tmp2);
    asm.emit_all(&[
        ldr(X, tmp2, RUNTIME, runtime::CPU_STATE),
        ldr(W, fpsr, tmp2, runtime::MXCSR),
        orr(W, fpsr, fpsr, tmp1, Lsl, 0),
        str(W, fpsr, tmp2, runtime::MXCSR),
        strb(Reg::ZR, RUNTIME, runtime::FPSR_OWNER),
    ]);
    runtime::emit_inc_fp_counter(asm, runtime::FP_FPSR_FOLDS, 1, tmp1);
}

/// If `fpsr_owner == SseLive`, reads hardware FPSR, folds sticky bits into
/// `CpuState::mxcsr`, sets `fpsr_owner = Clean` (without `msr fpsr`), and
/// increments `FP_FPSR_READS` and `FP_FPSR_FOLDS`.
///
/// Preserves guest NZCV flags. `tmp1`, `tmp2` and `tmp3` must be distinct.
pub(crate) fn emit_fold_fpsr(asm: &mut Asm, tmp1: Reg, tmp2: Reg, tmp3: Reg) {
    assert!(tmp1 != tmp2 && tmp1 != tmp3 && tmp2 != tmp3);
    let clean = asm.label();
    asm.emit(ldrb(tmp1, RUNTIME, runtime::FPSR_OWNER));
    asm.cbz(W, false, tmp1, clean);
    asm.emit(mrs_fpsr(tmp1));
    runtime::emit_inc_fp_counter(asm, runtime::FP_FPSR_READS, 1, tmp2);
    emit_fold_fpsr_value(asm, tmp1, tmp2, tmp3);
    asm.bind(clean);
}

/// Commits live hardware FPSR before a store that later rollbacks cannot undo.
///
/// Folds into `CpuState::mxcsr` only when FPSR holds a bit the fold would
/// change: any of `0x8f`, or `IXC` while `MXCSR.PE` is clear. Otherwise every
/// pending bit is already in MXCSR, so `fpsr_owner` stays `SseLive` and a later
/// cold exit may still discard FPSR exactly. The common path is one `mrs` with
/// no `msr fpsr`, so a store per loop iteration does not re-arm FPSR.
///
/// Preserves guest NZCV flags. `tmp1`, `tmp2` and `tmp3` must be distinct.
pub(crate) fn emit_commit_fpsr(asm: &mut Asm, tmp1: Reg, tmp2: Reg, tmp3: Reg) {
    assert!(tmp1 != tmp2 && tmp1 != tmp3 && tmp2 != tmp3);
    let (done, fold) = (asm.label(), asm.label());
    asm.emit(ldrb(tmp1, RUNTIME, runtime::FPSR_OWNER));
    asm.cbz(W, false, tmp1, done);
    asm.emit(mrs_fpsr(tmp1));
    runtime::emit_inc_fp_counter(asm, runtime::FP_FPSR_READS, 1, tmp2);
    // 0x8f is not a logical immediate.
    asm.emit_all(&[
        movz(W, tmp2, FPSR_CHECK_MASK as u16, 0),
        and(W, tmp2, tmp1, tmp2, Lsl, 0),
    ]);
    asm.cbz(W, true, tmp2, fold);
    asm.tbz(false, tmp1, 4, done);
    asm.emit_all(&[
        ldr(X, tmp2, RUNTIME, runtime::CPU_STATE),
        ldr(W, tmp2, tmp2, runtime::MXCSR),
    ]);
    asm.tbz(true, tmp2, 5, done);
    asm.bind(fold);
    emit_fold_fpsr_value(asm, tmp1, tmp2, tmp3);
    asm.bind(done);
}

/// Discards uncommitted hardware FPSR (`fpsr_owner = Clean`) without folding
/// into `CpuState::mxcsr`.
///
/// Used at `LDMXCSR`/`FXRSTOR` and speculative cold exits (`opt_exit`,
/// `resident_exit`) per A64-OPT-6. Preserves guest NZCV flags.
pub(crate) fn emit_discard_fpsr(asm: &mut Asm) {
    asm.emit(strb(Reg::ZR, RUNTIME, runtime::FPSR_OWNER));
}

/// Captures pre-call hardware FPSR into `pre_dst` when `fpsr_owner == SseLive`
/// before a non-MXCSR-observing host call.
///
/// Caller must preserve `pre_dst` across the call and pass it to
/// [`emit_isolate_fpsr_after`]. Preserves guest NZCV flags.
pub(crate) fn emit_isolate_fpsr_before(asm: &mut Asm, pre_dst: Reg, tmp: Reg) {
    assert_ne!(pre_dst, tmp);
    let clean = asm.label();
    asm.emit(ldrb(tmp, RUNTIME, runtime::FPSR_OWNER));
    asm.cbz(W, false, tmp, clean);
    asm.emit(mrs_fpsr(pre_dst));
    runtime::emit_inc_fp_counter(asm, runtime::FP_FPSR_READS, 1, tmp);
    asm.bind(clean);
}

/// Completes FPSR isolation after a non-MXCSR-observing host call using the
/// `pre_val` saved by [`emit_isolate_fpsr_before`].
///
/// When `fpsr_owner == SseLive`, computes sticky-bit changes
/// `delta = (post ^ pre_val) & 0x9f` (catching both host-raised bits and
/// host-cleared guest sticky bits, e.g. `feclearexcept`/`fesetenv`):
/// - If `delta == 0`, or `delta == IXC` while `CpuState::mxcsr.PE` is already
///   set, stays `SseLive` with zero `msr` writes.
/// - Otherwise folds `pre_val` (preserving guest bits and excluding host
///   pollution) into `CpuState::mxcsr` and resets `fpsr_owner = Clean`.
///
/// Clobbers `pre_val`, `tmp1` and `tmp2` (all distinct). Preserves NZCV.
pub(crate) fn emit_isolate_fpsr_after(asm: &mut Asm, pre_val: Reg, tmp1: Reg, tmp2: Reg) {
    assert!(pre_val != tmp1 && pre_val != tmp2 && tmp1 != tmp2);
    let done = asm.label();
    let fold_pre = asm.label();
    asm.emit(ldrb(tmp1, RUNTIME, runtime::FPSR_OWNER));
    asm.cbz(W, false, tmp1, done);
    runtime::emit_inc_fp_counter(asm, runtime::FP_FPSR_ISOLATIONS, 1, tmp2);
    asm.emit(mrs_fpsr(tmp1));
    runtime::emit_inc_fp_counter(asm, runtime::FP_FPSR_READS, 1, tmp2);
    asm.emit_all(&[
        eor(W, tmp1, tmp1, pre_val, Lsl, 0),
        movz(W, tmp2, FPSR_STICKY_MASK as u16, 0),
        and(W, tmp1, tmp1, tmp2, Lsl, 0),
    ]);
    asm.cbz(W, false, tmp1, done);
    // If only IXC changed and guest MXCSR.PE (bit 5) is already set, staying
    // SseLive is exact without clearing FPSR or folding.
    asm.emit(eor_imm(W, tmp2, tmp1, u64::from(FPSR_IXC)));
    asm.cbz(W, true, tmp2, fold_pre);
    asm.emit_all(&[
        ldr(X, tmp2, RUNTIME, runtime::CPU_STATE),
        ldr(W, tmp2, tmp2, runtime::MXCSR),
    ]);
    asm.tbz(true, tmp2, 5, done);
    asm.bind(fold_pre);
    emit_fold_fpsr_value(asm, pre_val, tmp1, tmp2);
    asm.bind(done);
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: A64F-FP-5
    #[test]
    fn fpcr_from_mxcsr_maps_controls_and_keeps_nep_clear() {
        assert_eq!(fpcr_from_mxcsr(0x1f80), FPCR_X87_DEFAULT);
        assert_eq!(fpcr_from_mxcsr(0x1f80) & FPCR_NEP, 0);
        // DAZ -> FIZ (bit 0), FTZ -> FZ (bit 24).
        assert_eq!(fpcr_from_mxcsr(0x1fc0), FPCR_AH | FPCR_FIZ);
        assert_eq!(fpcr_from_mxcsr(0x9f80), FPCR_AH | FPCR_FZ);
        assert_eq!(fpcr_from_mxcsr(0x9fc0), FPCR_AH | FPCR_FIZ | FPCR_FZ);
        // Rounding modes: MXCSR RD=1 -> AArch64 RM=2; MXCSR RU=2 -> AArch64 RP=1.
        assert_eq!(fpcr_from_mxcsr(0x3f80), FPCR_AH | (2 << FPCR_RMODE_SHIFT));
        assert_eq!(fpcr_from_mxcsr(0x5f80), FPCR_AH | (1 << FPCR_RMODE_SHIFT));
        assert_eq!(fpcr_from_mxcsr(0x7f80), FPCR_AH | (3 << FPCR_RMODE_SHIFT));
        // NEP and DN stay 0 across every control combination.
        for daz in [0u32, 1 << 6] {
            for rc in 0..4u32 {
                for ftz in [0u32, 1 << 15] {
                    let fpcr = fpcr_from_mxcsr(0x1f80 | daz | (rc << 13) | ftz);
                    assert_eq!(fpcr & FPCR_NEP, 0);
                    assert_eq!(fpcr & (1 << 25), 0); // DN=0
                    assert_ne!(fpcr & FPCR_AH, 0);
                }
            }
        }
    }

    // spec: A64F-FP-5
    #[test]
    fn fpsr_sticky_bit_mapping_covers_all_six_x86_flags() {
        assert_eq!(map_fpsr_to_mxcsr_bits(0), 0);
        assert_eq!(map_fpsr_to_mxcsr_bits(1 << 0), 1 << 0); // IOC -> IE
        assert_eq!(map_fpsr_to_mxcsr_bits(1 << 7), 1 << 1); // IDC -> DE
        assert_eq!(map_fpsr_to_mxcsr_bits(1 << 1), 1 << 2); // DZC -> ZE
        assert_eq!(map_fpsr_to_mxcsr_bits(1 << 2), 1 << 3); // OFC -> OE
        assert_eq!(map_fpsr_to_mxcsr_bits(1 << 3), 1 << 4); // UFC -> UE
        assert_eq!(map_fpsr_to_mxcsr_bits(1 << 4), 1 << 5); // IXC -> PE
        assert_eq!(map_fpsr_to_mxcsr_bits(FPSR_STICKY_MASK), 0x3f);
        // Unrelated FPSR bits (QC=27, N/Z/C/V=31:28, bits 5..6) are ignored.
        assert_eq!(map_fpsr_to_mxcsr_bits(!FPSR_STICKY_MASK), 0);
    }

    // spec: A64F-FP-5
    #[test]
    fn sse_exceptions_masked_requires_all_six_mask_bits() {
        assert!(sse_exceptions_masked(0x1f80));
        assert!(sse_exceptions_masked(0x9fff));
        for bit in 7..13 {
            assert!(!sse_exceptions_masked(0x1f80 & !(1 << bit)));
        }
    }
}

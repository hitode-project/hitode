//! FPCR shadow/lazy-sync, lazy FPSR fold/discard/isolation, `Op::FpUnary` under
//! `FPCR.AH=1`, and `ADV_HOST_FEATURES=-afp` fallback tests (A64F-FP-5,
//! A64-OPT-4, A64-OPT-6, A64-CALL-3, A64-REG-4, A64-TR-9, A64-ISA-4).

use super::*;
use crate::asm::Asm;
use crate::cache::CodeCache;
use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::runtime::{self, AccessResult, Enter, Event, Exit, PlanEvent, Runtime};
use crate::translate::lower::{body as lower_body, fp_state, stubs};
use crate::translate::opt::back;
use adv_cpu_core::HostFeatures;
use adv_cpu_core::body::{BodyContext, BodyResult, DirectBody, EAX, Input};
use std::cell::Cell;

const RT: Reg = Reg::new(29);

fn host_fpcr() -> u32 {
    let value: u64;
    // SAFETY: reading FPCR on AArch64 EL0 is always valid.
    unsafe {
        std::arch::asm!("mrs {}, fpcr", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value as u32
}

fn has_hw_afp() -> bool {
    HostFeatures::detected().contains(HostFeatures::AFP)
}

/// Emits a real hardware FP operation that raises specific FPSR sticky bit(s).
/// Uses v0/v1 and x16/x17 scratch.
fn emit_raise_fpsr_ioc(asm: &mut Asm) {
    // 0.0 / 0.0 -> IOC (bit 0)
    asm.emit_all(&[
        vzero(false, VReg::new(0)),
        dfloat(Float::Fdiv, VReg::new(0), VReg::new(0), VReg::new(0)),
    ]);
}

fn emit_raise_fpsr_dzc(asm: &mut Asm) {
    // 1.0 / 0.0 -> DZC (bit 1)
    asm.emit_all(&[
        movz(X, Reg::new(16), 0x3ff0, 48),
        fmov_to_v(X, VReg::new(0), Reg::new(16)),
        vzero(false, VReg::new(1)),
        dfloat(Float::Fdiv, VReg::new(0), VReg::new(0), VReg::new(1)),
    ]);
}

fn emit_raise_fpsr_ofc_ixc(asm: &mut Asm) {
    // f32::MAX * 2.0 -> OFC (bit 2) | IXC (bit 4)
    asm.emit_all(&[
        movz(W, Reg::new(16), 0xffff, 0),
        movk(W, Reg::new(16), 0x7f7f, 16),
        fmov_to_v(W, VReg::new(0), Reg::new(16)),
        movz(W, Reg::new(16), 0x4000, 16),
        fmov_to_v(W, VReg::new(1), Reg::new(16)),
        sfloat(Float::Fmul, VReg::new(0), VReg::new(0), VReg::new(1)),
    ]);
}

fn emit_raise_fpsr_ufc_ixc(asm: &mut Asm) {
    // f32::MIN_POSITIVE * (1.0 / 3.0) -> tiny AND inexact -> UFC (bit 3) | IXC (bit 4)
    asm.emit_all(&[
        movz(W, Reg::new(16), 0x0080, 16),
        fmov_to_v(W, VReg::new(0), Reg::new(16)),
        movz(W, Reg::new(16), 0xaaab, 0),
        movk(W, Reg::new(16), 0x3eaa, 16),
        fmov_to_v(W, VReg::new(1), Reg::new(16)),
        sfloat(Float::Fmul, VReg::new(0), VReg::new(0), VReg::new(1)),
    ]);
}

fn emit_raise_fpsr_idc_ixc(asm: &mut Asm) {
    // subnormal f32 (bits 0x0000_0001) + 1.0 -> IDC (bit 7, under AH=1) | IXC (bit 4)
    asm.emit_all(&[
        movz(W, Reg::new(16), 1, 0),
        fmov_to_v(W, VReg::new(0), Reg::new(16)),
        movz(W, Reg::new(16), 0x3f80, 16),
        fmov_to_v(W, VReg::new(1), Reg::new(16)),
        sfloat(Float::Fadd, VReg::new(0), VReg::new(0), VReg::new(1)),
    ]);
}

fn run_stub_snippet(
    afp: bool,
    cpu: &mut CpuState,
    setup_runtime: impl FnOnce(&mut Runtime),
    body: impl FnOnce(&mut Asm, &stubs::Stubs),
) -> (Runtime, runtime::RawExit) {
    let stubs = stubs::stubs_for(afp);
    let mut asm = Asm::new();
    asm.emit_all(&stubs.code);
    let target = asm.offset();
    body(&mut asm, &stubs);
    let words = asm.finish();

    let mut cache = CodeCache::new(64 << 10).unwrap();
    let base = cache.append(&words).unwrap();
    cache.sync();

    let mut runtime = Runtime::new(cpu, 0);
    setup_runtime(&mut runtime);
    let enter = Enter(base + stubs.enter);
    // SAFETY: `base + stubs.enter` and `base + target` live in `cache`, and
    // `runtime` points to `cpu` for the duration of the call.
    let raw = unsafe { enter.run(&mut runtime, base + target) };
    (runtime, raw)
}

// spec: A64F-FP-5
#[test]
fn fpcr_shadow_lazy_sync_and_root_exit_restore() {
    if !has_hw_afp() {
        return;
    }
    let before_fpcr = host_fpcr();
    let mut cpu = CpuState {
        mxcsr: 0x1f80,
        ..Default::default()
    };

    // 1. Entering and exiting without syncing FPCR writes FPCR zero times.
    let (rt, raw) = run_stub_snippet(
        true,
        &mut cpu,
        |_| {},
        |asm, stubs| {
            asm.emit_all(&[
                movz(W, Reg::new(0), Exit::Dispatch.word(), 0),
                movz(W, Reg::new(1), 0x1000, 0),
            ]);
            let here = asm.offset();
            asm.emit(b(stubs.exit as i32 - here as i32));
        },
    );
    assert_eq!(raw.reason as u32, Exit::Dispatch.raw());
    assert_eq!(host_fpcr(), before_fpcr);
    assert_eq!(rt.host_fpcr, before_fpcr);
    assert_eq!(rt.active_fpcr, before_fpcr);
    assert_eq!(rt.fpsr_owner, runtime::FPSR_CLEAN);
    assert_eq!(rt.fp.fpcr_writes, 0);
    assert_eq!(rt.fp.fpsr_reads, 0);
    assert_eq!(rt.fp.fpsr_folds, 0);

    // 2. Sync to AH=1 twice (second is skipped), verify NZCV preserved, then
    // sync to DAZ+FTZ+RD, and exit -> host FPCR restored cleanly.
    let directed_fpcr = fp_state::fpcr_from_mxcsr(0xbfc0); // FTZ + RD + DAZ + masked
    assert_eq!(directed_fpcr & fp_state::FPCR_NEP, 0);
    let (rt, _) = run_stub_snippet(
        true,
        &mut cpu,
        |_| {},
        |asm, stubs| {
            // Set a distinct NZCV pattern (N=1, Z=0, C=0, V=0 from 0 - 1).
            // Note: register 31 in `cmp_imm` is SP, so load 0 into w0 first.
            asm.emit_all(&[
                movz(W, Reg::new(0), 0, 0),
                cmp_imm(W, Reg::new(0), 1, false),
            ]);
            fp_state::emit_sync_fpcr_imm(asm, fp_state::FPCR_X87_DEFAULT, Reg::new(0), Reg::new(1));
            // Redundant sync to the same value must skip `msr fpcr`.
            fp_state::emit_sync_fpcr_imm(asm, fp_state::FPCR_X87_DEFAULT, Reg::new(0), Reg::new(1));
            // Capture hardware FPCR in guest EAX (w19) and NZCV in guest ECX (w20).
            asm.emit_all(&[mrs_fpcr(Reg::new(19)), mrs_nzcv(Reg::new(20))]);
            // Sync to directed+DAZ+FTZ mode and capture in guest EDX (w21).
            fp_state::emit_sync_fpcr_imm(asm, directed_fpcr, Reg::new(0), Reg::new(1));
            asm.emit(mrs_fpcr(Reg::new(21)));
            asm.emit_all(&[
                movz(W, Reg::new(0), Exit::Dispatch.word(), 0),
                movz(W, Reg::new(1), 0x1004, 0),
            ]);
            let here = asm.offset();
            asm.emit(b(stubs.exit as i32 - here as i32));
        },
    );
    assert_eq!(host_fpcr(), before_fpcr, "host FPCR restored on root exit");
    assert_eq!(rt.active_fpcr, before_fpcr);
    assert_eq!(rt.fpsr_owner, runtime::FPSR_CLEAN);
    assert_eq!(cpu.regs[0], fp_state::FPCR_X87_DEFAULT);
    assert_eq!(cpu.regs[1] >> 28, 0b1000, "NZCV preserved across FPCR sync");
    assert_eq!(cpu.regs[2], directed_fpcr);
    assert_eq!(cpu.regs[2] & fp_state::FPCR_NEP, 0, "FPCR.NEP must stay 0");
    // 1 (AH=1) + 0 (redundant skip) + 1 (directed) + 1 (root exit restore) = 3.
    assert_eq!(rt.fp.fpcr_writes, 3);
}

// spec: A64F-FP-5
#[test]
fn fpsr_arm_fold_and_discard_map_all_six_sticky_bits() {
    if !has_hw_afp() {
        return;
    }
    let before_fpcr = host_fpcr();
    type Trigger = fn(&mut Asm);
    let cases: [(&str, Trigger, u32); 5] = [
        ("IOC->IE", emit_raise_fpsr_ioc, 0x01),
        ("DZC->ZE", emit_raise_fpsr_dzc, 0x04),
        ("OFC|IXC->OE|PE", emit_raise_fpsr_ofc_ixc, 0x08 | 0x20),
        ("UFC|IXC->UE|PE", emit_raise_fpsr_ufc_ixc, 0x10 | 0x20),
        ("IDC|IXC->DE|PE", emit_raise_fpsr_idc_ixc, 0x02 | 0x20),
    ];
    for (name, trigger, expected_sticky) in cases {
        let mut cpu = CpuState {
            mxcsr: 0x1f80,
            ..Default::default()
        };
        let (rt, _) = run_stub_snippet(
            true,
            &mut cpu,
            |_| {},
            |asm, stubs| {
                fp_state::emit_sync_fpcr_imm(
                    asm,
                    fp_state::FPCR_X87_DEFAULT,
                    Reg::new(0),
                    Reg::new(1),
                );
                fp_state::emit_arm_sse_fpsr(asm, Reg::new(0));
                trigger(asm);
                // Re-arming while already SseLive MUST NOT wipe accumulated FPSR!
                fp_state::emit_arm_sse_fpsr(asm, Reg::new(0));
                asm.emit_all(&[
                    movz(W, Reg::new(0), Exit::Dispatch.word(), 0),
                    movz(W, Reg::new(1), 0x1000, 0),
                ]);
                let here = asm.offset();
                asm.emit(b(stubs.exit as i32 - here as i32));
            },
        );
        assert_eq!(host_fpcr(), before_fpcr, "{name}");
        assert_eq!(rt.fpsr_owner, runtime::FPSR_CLEAN, "{name}");
        assert_eq!(cpu.mxcsr & 0x3f, expected_sticky, "{name}");
        assert_eq!(rt.fp.fpsr_reads, 1, "{name}");
        assert_eq!(rt.fp.fpsr_folds, 1, "{name}");
    }

    // Discard path must drop uncommitted FPSR bits without folding into MXCSR.
    let mut cpu = CpuState {
        mxcsr: 0x1f80,
        ..Default::default()
    };
    let (rt, _) = run_stub_snippet(
        true,
        &mut cpu,
        |_| {},
        |asm, stubs| {
            fp_state::emit_sync_fpcr_imm(asm, fp_state::FPCR_X87_DEFAULT, Reg::new(0), Reg::new(1));
            fp_state::emit_arm_sse_fpsr(asm, Reg::new(0));
            emit_raise_fpsr_ioc(asm);
            emit_raise_fpsr_dzc(asm);
            fp_state::emit_discard_fpsr(asm);
            asm.emit_all(&[
                movz(W, Reg::new(0), Exit::Dispatch.word(), 0),
                movz(W, Reg::new(1), 0x1000, 0),
            ]);
            let here = asm.offset();
            asm.emit(b(stubs.exit as i32 - here as i32));
        },
    );
    assert_eq!(host_fpcr(), before_fpcr);
    assert_eq!(rt.fpsr_owner, runtime::FPSR_CLEAN);
    assert_eq!(cpu.mxcsr, 0x1f80, "discarded FPSR must not fold into MXCSR");
    assert_eq!(rt.fp.fpsr_reads, 0);
    assert_eq!(rt.fp.fpsr_folds, 0);
}

// spec: A64F-FP-5, A64-MEM-5
#[test]
fn fault_stub_folds_fpsr_and_restores_host_fpcr() {
    if !has_hw_afp() {
        return;
    }
    let before_fpcr = host_fpcr();
    let mut cpu = CpuState {
        mxcsr: 0x1f80,
        ..Default::default()
    };
    let (rt, raw) = run_stub_snippet(
        true,
        &mut cpu,
        |_| {},
        |asm, stubs| {
            fp_state::emit_sync_fpcr_imm(asm, fp_state::FPCR_X87_DEFAULT, Reg::new(0), Reg::new(1));
            fp_state::emit_arm_sse_fpsr(asm, Reg::new(0));
            emit_raise_fpsr_ioc(asm);
            let here = asm.offset();
            asm.emit(b(stubs.fault as i32 - here as i32));
        },
    );
    assert_eq!(raw.reason as u32, Exit::Event(Event::Replay).raw());
    assert_eq!(host_fpcr(), before_fpcr);
    assert_eq!(rt.active_fpcr, before_fpcr);
    assert_eq!(rt.fpsr_owner, runtime::FPSR_CLEAN);
    assert_eq!(cpu.mxcsr, 0x1f81, "fault stub must fold committed SSE FPSR");
    assert_eq!(rt.fp.fpsr_folds, 1);
}

// spec: A64-OPT-4, A64-OPT-6, A64F-FP-5
#[test]
fn opt_exit_and_resident_exit_discard_uncommitted_fpsr_and_restore_fpcr() {
    if !has_hw_afp() {
        return;
    }
    let before_fpcr = host_fpcr();
    for (resident, expected_exit) in [
        (false, Exit::Optimized),
        (true, Exit::Plan(PlanEvent::ResidentRecovery)),
    ] {
        let mut cpu = CpuState {
            mxcsr: 0x1f80,
            ..Default::default()
        };
        let stubs = stubs::stubs_for(true);
        let mut asm = Asm::new();
        asm.emit_all(&stubs.code);
        let target = asm.offset();
        fp_state::emit_sync_fpcr_imm(
            &mut asm,
            fp_state::FPCR_X87_DEFAULT,
            Reg::new(0),
            Reg::new(1),
        );
        fp_state::emit_arm_sse_fpsr(&mut asm, Reg::new(0));
        // Speculative FP ops dirty hardware FPSR before cold rollback.
        emit_raise_fpsr_ioc(&mut asm);
        emit_raise_fpsr_dzc(&mut asm);
        let here = asm.offset();
        let exit_stub = if resident {
            stubs.resident_exit
        } else {
            stubs.opt_exit
        };
        asm.emit(b(exit_stub as i32 - here as i32));
        let words = asm.finish();

        let mut cache = CodeCache::new(64 << 10).unwrap();
        let base = cache.append(&words).unwrap();
        cache.sync();

        let mut runtime = Runtime::new(&mut cpu, 0);
        let entry = Enter(
            base + if resident {
                stubs.enter
            } else {
                stubs.opt_enter
            },
        );
        // SAFETY: `cache` keeps stubs and snippet live; `runtime` points at `cpu`.
        let raw = unsafe { entry.run(&mut runtime, base + target) };
        assert_eq!(raw.reason as u32, expected_exit.raw());
        assert_eq!(
            host_fpcr(),
            before_fpcr,
            "host FPCR must be restored on rollback (resident={resident})"
        );
        assert_eq!(runtime.active_fpcr, before_fpcr);
        assert_eq!(runtime.fpsr_owner, runtime::FPSR_CLEAN);
        assert_eq!(
            cpu.mxcsr, 0x1f80,
            "speculative rollback MUST discard uncommitted FPSR (resident={resident})"
        );
        assert_eq!(runtime.fp.fpsr_folds, 0);
        assert_eq!(runtime.fp.fpsr_reads, 0);
        assert_eq!(runtime.fp.fpcr_writes, 2); // AH=1 sync + rollback restore
    }
}

thread_local! {
    static OBSERVED_FPCR: Cell<u32> = const { Cell::new(0) };
    static OBSERVED_ARG1: Cell<u32> = const { Cell::new(0) };
    static OBSERVED_ARG2: Cell<u32> = const { Cell::new(0) };
    static OBSERVED_MXCSR: Cell<u32> = const { Cell::new(0) };
    static OBSERVED_OWNER: Cell<u32> = const { Cell::new(0) };
    static POLLUTE_MODE: Cell<u32> = const { Cell::new(0) };
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn test_body_fn(
    _ctx: *const BodyContext,
    a: u32,
    b: u32,
    _c: u32,
    _d: u32,
    _e: u32,
    _f: u32,
    _g: u32,
) -> BodyResult {
    OBSERVED_FPCR.set(host_fpcr());
    OBSERVED_ARG1.set(a);
    OBSERVED_ARG2.set(b);
    match POLLUTE_MODE.get() {
        1 => {
            // Raise IXC only (1.0 + 1e-20).
            let x = std::hint::black_box(1.0f64) + std::hint::black_box(1e-20f64);
            std::hint::black_box(x);
        }
        2 => {
            // Raise DZC (1.0 / 0.0).
            let x = std::hint::black_box(1.0f64) / std::hint::black_box(0.0f64);
            std::hint::black_box(x);
        }
        3 => {
            // Clear hardware FPSR (simulating host C feclearexcept/fesetenv).
            // SAFETY: writing FPSR on AArch64 EL0 is always valid.
            unsafe {
                std::arch::asm!("msr fpsr, xzr", options(nomem, nostack, preserves_flags));
            }
        }
        _ => {}
    }
    BodyResult {
        value: 0x1234_5678,
        extra: 0,
    }
}

// spec: A64-CALL-3, A64F-FP-5
#[test]
fn direct_body_fp_clean_vs_default_and_mxcsr_observing_boundaries() {
    if !has_hw_afp() {
        return;
    }
    let before_fpcr = host_fpcr();

    // Case 1: `fp_clean: true` skips FPCR restore and FPSR isolation completely.
    let fp_clean_body = DirectBody {
        call: test_body_fn,
        inline: None,
        inputs: [None; 7],
        outputs: EAX,
        pop: 0,
        fp_clean: true,
    };
    POLLUTE_MODE.set(0);
    let mut cpu = CpuState {
        mxcsr: 0x1f80,
        ..Default::default()
    };
    let (rt, _) = run_stub_snippet(
        true,
        &mut cpu,
        |_| {},
        |asm, stubs| {
            fp_state::emit_sync_fpcr_imm(asm, fp_state::FPCR_X87_DEFAULT, Reg::new(0), Reg::new(1));
            fp_state::emit_arm_sse_fpsr(asm, Reg::new(0));
            emit_raise_fpsr_ioc(asm);
            lower_body::emit_call(asm, fp_clean_body, true);
            asm.emit(mov(W, Reg::new(19), Reg::new(0)));
            asm.emit_all(&[
                movz(W, Reg::new(0), Exit::Dispatch.word(), 0),
                movz(W, Reg::new(1), 0x1000, 0),
            ]);
            let here = asm.offset();
            asm.emit(b(stubs.exit as i32 - here as i32));
        },
    );
    assert_eq!(OBSERVED_FPCR.get(), fp_state::FPCR_X87_DEFAULT);
    assert_eq!(cpu.regs[0], 0x1234_5678);
    assert_eq!(
        cpu.mxcsr, 0x1f81,
        "lazy IOC survived FpClean body and folded on exit"
    );
    assert_eq!(rt.fp.fpsr_isolations, 0);
    assert_eq!(rt.fp.fpsr_folds, 1);
    assert_eq!(rt.fp.fpcr_writes, 2); // AH=1 sync + root exit restore only

    // Case 2: default body (`fp_clean: false`) restores FPCR and isolates FPSR.
    let default_body = DirectBody {
        fp_clean: false,
        ..fp_clean_body
    };
    for (mode, init_mxcsr, expect_mxcsr, expect_folds, label) in [
        // No host FP pollution -> stays SseLive across body, folds once at exit.
        (0u32, 0x1f80u32, 0x1f81u32, 1u64, "clean body"),
        // Host raises IXC only, PE already set -> stays SseLive, 0 extra fold.
        (1, 0x1fa0, 0x1fa1, 1, "IXC-only with PE set"),
        // Host raises IXC only, PE clear -> folds pre (IOC only, NOT host IXC!),
        // sets Clean, so exit does not fold again.
        (1, 0x1f80, 0x1f81, 1, "IXC-only with PE clear"),
        // Host raises DZC -> folds pre (IOC only, NOT host DZC!), sets Clean.
        (2, 0x1f80, 0x1f81, 1, "DZC pollution"),
        // Host clears hardware FPSR -> folds pre (preserves guest IOC!), sets Clean.
        (3, 0x1f80, 0x1f81, 1, "host clears FPSR"),
    ] {
        POLLUTE_MODE.set(mode);
        let mut cpu = CpuState {
            mxcsr: init_mxcsr,
            ..Default::default()
        };
        let (rt, _) = run_stub_snippet(
            true,
            &mut cpu,
            |_| {},
            |asm, stubs| {
                fp_state::emit_sync_fpcr_imm(
                    asm,
                    fp_state::FPCR_X87_DEFAULT,
                    Reg::new(0),
                    Reg::new(1),
                );
                fp_state::emit_arm_sse_fpsr(asm, Reg::new(0));
                emit_raise_fpsr_ioc(asm);
                lower_body::emit_call(asm, default_body, true);
                // Record fpsr_owner right after body returns into EAX (w19).
                asm.emit(ldrb(Reg::new(19), RT, runtime::FPSR_OWNER));
                asm.emit_all(&[
                    movz(W, Reg::new(0), Exit::Dispatch.word(), 0),
                    movz(W, Reg::new(1), 0x1000, 0),
                ]);
                let here = asm.offset();
                asm.emit(b(stubs.exit as i32 - here as i32));
            },
        );
        assert_eq!(OBSERVED_FPCR.get(), before_fpcr, "{label}");
        assert_eq!(host_fpcr(), before_fpcr, "{label}");
        assert_eq!(cpu.mxcsr, expect_mxcsr, "{label}");
        assert_eq!(rt.fp.fpsr_isolations, 1, "{label}");
        assert_eq!(rt.fp.fpsr_folds, expect_folds, "{label}");
        let expected_owner_after_body = if mode == 0 || (mode == 1 && init_mxcsr & 0x20 != 0) {
            u32::from(runtime::FPSR_SSE_LIVE)
        } else {
            u32::from(runtime::FPSR_CLEAN)
        };
        assert_eq!(cpu.regs[0], expected_owner_after_body, "{label}");
    }

    // Case 3: MXCSR / FloatControl observing body folds lazy FPSR AND refreshes
    // argument registers w1/w2 before entering the body.
    let mxcsr_body = DirectBody {
        call: test_body_fn,
        inline: None,
        inputs: [
            Some(Input::Mxcsr),
            Some(Input::FloatControl),
            None,
            None,
            None,
            None,
            None,
        ],
        outputs: EAX,
        pop: 0,
        fp_clean: false,
    };
    POLLUTE_MODE.set(0);
    let mut cpu = CpuState {
        mxcsr: 0x1f80,
        ..Default::default()
    };
    cpu.x87.control = 0x037f;
    let (rt, _) = run_stub_snippet(
        true,
        &mut cpu,
        |_| {},
        |asm, stubs| {
            fp_state::emit_sync_fpcr_imm(asm, fp_state::FPCR_X87_DEFAULT, Reg::new(0), Reg::new(1));
            fp_state::emit_arm_sse_fpsr(asm, Reg::new(0));
            emit_raise_fpsr_ioc(asm);
            // Simulate jit/ops.rs loading stale pre-fold MXCSR into w1 and
            // (x87.control << 16 | stale mxcsr) into w2.
            asm.emit_all(&[
                movz(W, Reg::new(1), 0x1f80, 0),
                movz(W, Reg::new(2), 0x1f80, 0),
                movk(W, Reg::new(2), 0x037f, 16),
            ]);
            lower_body::emit_call(asm, mxcsr_body, true);
            asm.emit_all(&[
                movz(W, Reg::new(0), Exit::Dispatch.word(), 0),
                movz(W, Reg::new(1), 0x1000, 0),
            ]);
            let here = asm.offset();
            asm.emit(b(stubs.exit as i32 - here as i32));
        },
    );
    assert_eq!(OBSERVED_FPCR.get(), before_fpcr);
    assert_eq!(OBSERVED_ARG1.get(), 0x1f81);
    assert_eq!(OBSERVED_ARG2.get(), (0x037f << 16) | 0x1f81);
    assert_eq!(rt.fp.fpsr_folds, 1);
    assert_eq!(rt.fp.fpsr_isolations, 0);
}

unsafe extern "C" fn mock_call_callback(runtime: *mut Runtime, _site: u64, _cd: u64) -> u64 {
    // SAFETY: test runtime and cpu are live.
    let rt = unsafe { &*runtime };
    let cpu = unsafe { &*rt.cpu };
    OBSERVED_FPCR.set(host_fpcr());
    OBSERVED_MXCSR.set(cpu.mxcsr);
    OBSERVED_OWNER.set(u32::from(rt.fpsr_owner));
    0
}

unsafe extern "C" fn mock_dispatch_callback(
    runtime: *mut Runtime,
    _eip: u32,
    _link: usize,
) -> usize {
    // SAFETY: test runtime and cpu are live.
    let rt = unsafe { &*runtime };
    let cpu = unsafe { &*rt.cpu };
    OBSERVED_FPCR.set(host_fpcr());
    OBSERVED_MXCSR.set(cpu.mxcsr);
    OBSERVED_OWNER.set(u32::from(rt.fpsr_owner));
    0 // return 0 so dispatch leaves via exit
}

unsafe extern "C" fn mock_access_callback(
    _runtime: *mut Runtime,
    _addr: u32,
    _site: usize,
    _val: u32,
    _kind: u32,
) -> AccessResult {
    OBSERVED_FPCR.set(host_fpcr());
    // Pollute hardware FPSR with DZC during host access callback.
    let x = std::hint::black_box(1.0f64) / std::hint::black_box(0.0f64);
    std::hint::black_box(x);
    AccessResult {
        value: 0x42,
        recovery: std::ptr::null_mut(),
    }
}

unsafe extern "C" fn mock_resident_access_callback(
    _runtime: *mut Runtime,
    _addr: u32,
    _site: usize,
    _val: u32,
    _kind: u32,
) -> u64 {
    OBSERVED_FPCR.set(host_fpcr());
    let x = std::hint::black_box(1.0f64) / std::hint::black_box(0.0f64);
    std::hint::black_box(x);
    0x77
}

// spec: A64-REG-4, A64F-FP-5
#[test]
fn stubs_call_dispatch_access_and_resident_access_obey_fp_boundaries() {
    if !has_hw_afp() {
        return;
    }
    let before_fpcr = host_fpcr();

    // 1. `stubs.call` folds FPSR and restores host FPCR before calling `CALL`.
    let mut cpu = CpuState {
        mxcsr: 0x1f80,
        ..Default::default()
    };
    let (rt, _) = run_stub_snippet(
        true,
        &mut cpu,
        |rt| rt.call = mock_call_callback as *const () as u64,
        |asm, stubs| {
            fp_state::emit_sync_fpcr_imm(asm, fp_state::FPCR_X87_DEFAULT, Reg::new(0), Reg::new(1));
            fp_state::emit_arm_sse_fpsr(asm, Reg::new(0));
            emit_raise_fpsr_ioc(asm);
            asm.emit(movz(X, Reg::new(1), 0, 0));
            let here = asm.offset();
            asm.emit(crate::encode::bl(stubs.call as i32 - here as i32));
            asm.emit_all(&[
                movz(W, Reg::new(0), Exit::Dispatch.word(), 0),
                movz(W, Reg::new(1), 0x1000, 0),
            ]);
            let here = asm.offset();
            asm.emit(b(stubs.exit as i32 - here as i32));
        },
    );
    assert_eq!(OBSERVED_FPCR.get(), before_fpcr);
    assert_eq!(OBSERVED_MXCSR.get(), 0x1f81);
    assert_eq!(OBSERVED_OWNER.get(), u32::from(runtime::FPSR_CLEAN));
    assert_eq!(rt.fp.fpsr_folds, 1);

    // 2. `stubs.dispatch` folds FPSR and restores host FPCR before calling `HELPER`.
    let mut cpu = CpuState {
        mxcsr: 0x1f80,
        ..Default::default()
    };
    let (rt, _) = run_stub_snippet(
        true,
        &mut cpu,
        |rt| rt.helper = mock_dispatch_callback as *const () as u64,
        |asm, stubs| {
            fp_state::emit_sync_fpcr_imm(asm, fp_state::FPCR_X87_DEFAULT, Reg::new(0), Reg::new(1));
            fp_state::emit_arm_sse_fpsr(asm, Reg::new(0));
            emit_raise_fpsr_dzc(asm);
            asm.emit_all(&[movz(W, Reg::new(1), 0x2000, 0), movz(X, Reg::new(2), 0, 0)]);
            let here = asm.offset();
            asm.emit(b(stubs.dispatch as i32 - here as i32));
        },
    );
    assert_eq!(OBSERVED_FPCR.get(), before_fpcr);
    assert_eq!(OBSERVED_MXCSR.get(), 0x1f84);
    assert_eq!(OBSERVED_OWNER.get(), u32::from(runtime::FPSR_CLEAN));
    assert_eq!(rt.fp.fpsr_folds, 1);

    // 3. `stubs.access` restores FPCR and isolates FPSR so host DZC does not leak.
    let mut cpu = CpuState {
        mxcsr: 0x1f80,
        ..Default::default()
    };
    let (rt, _) = run_stub_snippet(
        true,
        &mut cpu,
        |rt| rt.access = mock_access_callback as *const () as u64,
        |asm, stubs| {
            fp_state::emit_sync_fpcr_imm(asm, fp_state::FPCR_X87_DEFAULT, Reg::new(0), Reg::new(1));
            fp_state::emit_arm_sse_fpsr(asm, Reg::new(0));
            emit_raise_fpsr_ioc(asm);
            asm.emit_all(&[
                movz(W, Reg::new(1), 0x100, 0),
                movz(X, Reg::new(2), 0, 0),
                movz(W, Reg::new(3), 0, 0),
                movz(W, Reg::new(4), 4, 0),
            ]);
            let here = asm.offset();
            asm.emit(crate::encode::bl(stubs.access as i32 - here as i32));
            asm.emit(mov(W, Reg::new(19), Reg::new(0)));
            asm.emit_all(&[
                movz(W, Reg::new(0), Exit::Dispatch.word(), 0),
                movz(W, Reg::new(1), 0x1000, 0),
            ]);
            let here = asm.offset();
            asm.emit(b(stubs.exit as i32 - here as i32));
        },
    );
    assert_eq!(OBSERVED_FPCR.get(), before_fpcr);
    assert_eq!(cpu.regs[0], 0x42);
    assert_eq!(
        cpu.mxcsr, 0x1f81,
        "host DZC in access must not leak into MXCSR"
    );
    assert_eq!(rt.fp.fpsr_isolations, 1);
    assert_eq!(rt.fp.fpsr_folds, 1);

    // 4. `resident_access` (`access_stub_for(..., true)`) restores FPCR and isolates FPSR.
    let mut cpu = CpuState {
        mxcsr: 0x1f80,
        ..Default::default()
    };
    let stubs = stubs::stubs_for(true);
    let access_stub =
        back::access_stub_for(mock_resident_access_callback as *const () as u64, true);
    let mut asm = Asm::new();
    asm.emit_all(&stubs.code);
    let resident_access_at = asm.offset();
    asm.emit_all(&access_stub);
    let target = asm.offset();
    fp_state::emit_sync_fpcr_imm(
        &mut asm,
        fp_state::FPCR_X87_DEFAULT,
        Reg::new(0),
        Reg::new(1),
    );
    fp_state::emit_arm_sse_fpsr(&mut asm, Reg::new(0));
    emit_raise_fpsr_ioc(&mut asm);
    asm.emit_all(&[
        movz(W, Reg::new(1), 0x100, 0),
        movz(X, Reg::new(2), 0, 0),
        movz(W, Reg::new(3), 0, 0),
        movz(W, Reg::new(4), 4, 0),
    ]);
    let here = asm.offset();
    asm.emit(crate::encode::bl(resident_access_at as i32 - here as i32));
    asm.emit(mov(W, Reg::new(19), Reg::new(0)));
    asm.emit_all(&[
        movz(W, Reg::new(0), Exit::Dispatch.word(), 0),
        movz(W, Reg::new(1), 0x1000, 0),
    ]);
    let here = asm.offset();
    asm.emit(b(stubs.exit as i32 - here as i32));
    let words = asm.finish();

    let mut cache = CodeCache::new(64 << 10).unwrap();
    let base = cache.append(&words).unwrap();
    cache.sync();

    let mut runtime = Runtime::new(&mut cpu, 0);
    let enter = Enter(base + stubs.enter);
    // SAFETY: `cache` keeps stubs and code live.
    unsafe { enter.run(&mut runtime, base + target) };
    assert_eq!(OBSERVED_FPCR.get(), before_fpcr);
    assert_eq!(host_fpcr(), before_fpcr);
    assert_eq!(cpu.regs[0], 0x77);
    assert_eq!(
        cpu.mxcsr, 0x1f81,
        "host DZC in resident_access must not leak"
    );
    assert_eq!(runtime.fp.fpsr_isolations, 1);
    assert_eq!(runtime.fp.fpsr_folds, 1);
}

// spec: A64-OPT-4, A64F-FP-5
#[test]
fn opt_fp_unary_neg_and_abs_flip_nan_sign_under_ah1() {
    if !has_hw_afp() {
        return;
    }
    use crate::translate::opt::ir::{End, FpUnary, Func, Head, Op, RecordId, Source, Type};
    use crate::translate::opt::recovery::{Host, IrRecord, Location, X87};

    let before_fpcr = host_fpcr();
    let test_bits: [u64; 8] = [
        0x7ff8_0000_0000_0123, // +QNaN
        0xfff8_0000_0000_0123, // -QNaN
        0x7ff0_0000_0000_0456, // +SNaN
        0xfff0_0000_0000_0456, // -SNaN
        0x0000_0000_0000_0000, // +0.0
        0x8000_0000_0000_0000, // -0.0
        1.5f64.to_bits(),
        (-1.5f64).to_bits(),
    ];

    for op in [FpUnary::Neg, FpUnary::Abs] {
        let mut func = Func::default();
        let b0 = func.block(Some(Head {
            record: RecordId(0),
        }));
        let v0 = func.param(b0, Type::F64);
        func.entry.push(Source::X87(0));
        let v1 = func.push(b0, Op::FpUnary(op, v0)).unwrap();
        let entry_rec = IrRecord {
            eip: 0x1000,
            refund: 0,
            regs: [Some(Location::Constant(0)); 8],
            eflags: Some(Location::Constant(0x202)),
            flag_groups: Vec::new(),
            x87: Some(X87 {
                top: 0,
                valid: 1,
                control: 0x027f,
                stack: [
                    Some(Location::At(v0)),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                ],
                status: 0x20,
                c1: Some(Location::Constant(0)),
                ip: Some(Location::Constant(0x1000)),
                opcode: Some(Location::Constant(0)),
            }),
            xmm: [Some(Location::Constant(0)); 8],
            mmx: [Some(Location::Constant(0)); 8],
            mmx_meta: None,
            frame: Vec::new(),
        };
        let mut exit_rec = entry_rec.clone();
        exit_rec.x87.as_mut().unwrap().stack[0] = Some(Location::At(v1));
        func.records.push(entry_rec);
        func.records.push(exit_rec);
        func.blocks[b0.0 as usize].end = End::Exit(RecordId(1));

        let lowered = back::lower(&func);
        // Confirm Op::FpUnary did not emit scalar `fneg`/`fabs` (0x1e614000 / 0x1e60c000).
        for &word in &lowered.words {
            assert_ne!(word & 0xffff_fc00, 0x1e61_4000, "unexpected scalar fneg");
            assert_ne!(word & 0xffff_fc00, 0x1e60_c000, "unexpected scalar fabs");
        }

        // Execute the lowered form under FPCR.AH=1 via stubs_for(true).
        let stubs = stubs::stubs_for(true);
        let mut cache = CodeCache::new(64 << 10).unwrap();
        let stub_base = cache.append(&stubs.code).unwrap();
        let mut opt_lowered = lowered;
        let opt_at = cache.next();
        opt_lowered.place(opt_at, stub_base + stubs.opt_exit);
        let form_base = cache.append(&opt_lowered.words).unwrap();
        assert_eq!(form_base, opt_at);

        // Wrapper: sync FPCR to AH=1, then jump straight into `form_base`.
        let mut wrapper = Asm::new();
        fp_state::emit_sync_fpcr_imm(
            &mut wrapper,
            fp_state::FPCR_X87_DEFAULT,
            Reg::new(16),
            Reg::new(17),
        );
        let wrap_at = cache.next();
        let branch_at = wrap_at + wrapper.offset();
        wrapper.emit(b((form_base as i64 - branch_at as i64) as i32));
        let wrap_base = cache.append(&wrapper.finish()).unwrap();
        cache.sync();

        for &bits in &test_bits {
            let mut cpu = CpuState::default();
            let mut runtime = Runtime::new(&mut cpu, 0);
            let params = [bits as u128];
            let mut saved = vec![0u128; opt_lowered.spill_cells.max(1)];
            runtime.opt_params = params.as_ptr() as u64;
            runtime.recovery.saved = saved.as_mut_ptr() as u64;
            runtime.recovery.cells = opt_lowered.spill_cells as u64;

            let enter = Enter(stub_base + stubs.opt_enter);
            // SAFETY: `cache` keeps stubs, wrapper, and lowered opt form live.
            let raw = unsafe { enter.run(&mut runtime, wrap_base) };
            assert_eq!(raw.reason as u32, Exit::Optimized.raw());
            assert_eq!(host_fpcr(), before_fpcr);

            let record = &opt_lowered.records[runtime.recovery.record as usize];
            let x87 = record.x87.as_ref().unwrap();
            let actual_bits = match x87.stack[0].unwrap() {
                Location::At(Host::Vector(v)) => {
                    runtime.recovery.dump.v[v.number() as usize] as u64
                }
                Location::At(Host::Spill(s, _)) => saved[s as usize] as u64,
                other => panic!("unexpected location {other:?}"),
            };
            let expected = match op {
                FpUnary::Neg => bits ^ (1u64 << 63),
                FpUnary::Abs => bits & !(1u64 << 63),
            };
            assert_eq!(
                actual_bits, expected,
                "op={op:?} input={bits:#018x} under AH=1"
            );
        }
    }
}

// spec: A64-TR-9, A64F-FP-5
#[test]
fn worker_offload_sticky_merge_folds_lazy_fpsr_across_blocks() {
    if !has_hw_afp() {
        return;
    }
    let before_fpcr = host_fpcr();

    // Run two worker blocks on separate host threads; each thread owns its own
    // Runtime, syncs FPCR to AH=1, arms SseLive, raises a distinct exception,
    // and exits via `stubs.exit` (folding lazy FPSR into `cpu.mxcsr`).
    let handles: Vec<_> = [
        emit_raise_fpsr_ioc as fn(&mut Asm),
        emit_raise_fpsr_dzc as fn(&mut Asm),
    ]
    .into_iter()
    .map(|trigger| {
        std::thread::spawn(move || {
            let thread_fpcr = host_fpcr();
            let mut cpu = CpuState {
                mxcsr: 0x1f80,
                ..Default::default()
            };
            let (rt, raw) = run_stub_snippet(
                true,
                &mut cpu,
                |_| {},
                |asm, stubs| {
                    fp_state::emit_sync_fpcr_imm(
                        asm,
                        fp_state::FPCR_X87_DEFAULT,
                        Reg::new(0),
                        Reg::new(1),
                    );
                    fp_state::emit_arm_sse_fpsr(asm, Reg::new(0));
                    trigger(asm);
                    asm.emit_all(&[
                        movz(W, Reg::new(0), Exit::Dispatch.word(), 0),
                        movz(W, Reg::new(1), 0x1000, 0),
                    ]);
                    let here = asm.offset();
                    asm.emit(b(stubs.exit as i32 - here as i32));
                },
            );
            assert_eq!(raw.reason as u32, Exit::Dispatch.raw());
            assert_eq!(host_fpcr(), thread_fpcr);
            assert_eq!(rt.fpsr_owner, runtime::FPSR_CLEAN);
            assert_eq!(rt.active_fpcr, rt.host_fpcr);
            assert_eq!(rt.fp.fpsr_folds, 1);
            u64::from(cpu.mxcsr & 0x3f)
        })
    })
    .collect();

    let mut sticky = 0u32;
    for h in handles {
        sticky |= h.join().unwrap() as u32;
    }
    assert_eq!(sticky, 0x01 | 0x04, "IE | ZE merged across worker blocks");
    assert_eq!(host_fpcr(), before_fpcr);
}

// spec: A64-ISA-4, A64F-FP-5
#[test]
fn afp_disabled_fallback_emits_no_fp_state_accesses_and_matches_interpreter() {
    let stubs = stubs::stubs_for(false);
    back::assert_no_fp_state(&stubs.code);
    let access_stub = back::access_stub_for(0, false);
    back::assert_no_fp_state(&access_stub);

    let before_fpcr = host_fpcr();
    let mut program = fixture(|a| {
        a.addss(xmm0, xmm1).unwrap();
        a.mulps(xmm2, xmm3).unwrap();
        a.stmxcsr(dword_ptr(ebx)).unwrap();
        a.fld1().unwrap();
        a.fchs().unwrap();
        a.fstp(qword_ptr(ebx + 8)).unwrap();
    });
    program.state.mxcsr = 0x1f80;
    program.state.xmm[0][..4].copy_from_slice(&1.5f32.to_bits().to_le_bytes());
    program.state.xmm[1][..4].copy_from_slice(&2.25f32.to_bits().to_le_bytes());
    program.state.xmm[2][..4].copy_from_slice(&3.0f32.to_bits().to_le_bytes());
    program.state.xmm[3][..4].copy_from_slice(&0.5f32.to_bits().to_le_bytes());

    for afp in [false, HostFeatures::get().contains(HostFeatures::AFP)] {
        let config = (
            Features {
                afp,
                ..Features::default()
            },
            false,
            Checks::Adaptive,
        );
        let mut native = executor(&program, config, false);
        let mut oracle = executor(&program, config, true);
        assert_eq!(
            native.run(program.stop, 100).unwrap(),
            oracle.run(program.stop, 100).unwrap()
        );
        same(&native, &oracle);
        assert_eq!(host_fpcr(), before_fpcr);
        native.native.as_ref().unwrap().assert_fp_boundary_clean();
        // Context save/restore must also uphold boundary invariants.
        let saved = native.save_context().unwrap();
        native.restore_context(&saved).unwrap();
    }
}

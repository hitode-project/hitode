//! Canonical entry, exit and call stubs for the runtime ABI (A64-REG-1).
//!
//! `enter` saves the host's callee-saved registers, loads the guest and
//! branches to translated code. Translated code leaves through `exit` with
//! the exit reason in w0 and the guest EIP in w1; `exit` stores the guest
//! back and returns to `enter`'s caller. Translated code charges x9, the
//! countdown, with the guest instructions it completes.
//!
//! Dispatches do not leave: they branch to `dispatch` with the EIP in w1
//! and in x2 the host address of a link point to patch, or 0. That stub
//! saves only the caller-saved registers translated code keeps live (x7-x12
//! and NZCV; the guest GPRs are callee-saved) and calls the owner's helper,
//! which returns the unit to continue at, or 0 to leave through `exit`.
//!
//! Dynamic transfers carry the guest EIP in w16. `lookup` probes the
//! indirect table (A64-CTL-6), which the helper fills, and `lookup_slow` asks
//! the helper. A site that misses the table first loads x4 with its
//! source EIP and kind (`Runtime::branch`) on its cold path, so the owner
//! records the indirect branch without a cost on hits.
//! Returns pop the shadow stack (A64-CTL-4) at x28, whose entries
//! are {host continuation, guest return EIP} and grow down to a guard page;
//! `ret_miss` resynchronizes with a few entries below the top before it
//! looks up, and `shadow_reset` empties the stack after an overflow.
//!
//! Checked accesses (A64-MEM-7) call `access` when the page class table rules
//! out the window: it saves every register translated code may keep live,
//! has the owner's `access` helper perform the access, and returns the
//! loaded scalar in w0 or the vector in the operand frame. Refusal
//! captures the register state and leaves through the x87 publication
//! continuation when present. The stub also preserves the x87 run in v24-v31.
//!
//! Units cache SIMD in d8-d15 and v16-v23 (`translate::jit::simd`). They publish
//! dirty registers before stubs other than `access`, which preserves the
//! cache and v0-v7 temporaries. Arithmetic flags cross the boundary
//! in the canonical block-exit form of A64-FLAG-1: NZCV in subtraction sense, x10 a
//! value whose low byte has PF's parity, x11 a value whose bit 4 is AF, and
//! x12 DF. x87 state stays in `CpuState` outside x87 runs (A64F-FP-1, A64F-FP-7).
//!
//! Each admitted execution gets a private runtime. Recall, deadlines and
//! host notifications are observed only at exhausted instruction budgets.
//! Optimized forms enter and leave under the canonical host FPCR (A64-OPT-4).

use std::mem::offset_of;

use adv_cpu_core::{CpuState, floating::ValueAccess};

use crate::asm::Asm;
use crate::encode::Extend::Uxtw;
use crate::encode::Index::{Offset, Post, Pre};
use crate::encode::Shift::{Lsl, Lsr};
use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::runtime::{
    COMPLETED, CPU_STATE as CPU, DEFERRED, EFLAGS, EIP, Event, Exit, FaultRegisters, MAPPED_FLAGS,
    PlanEvent, RECOVERY_CELLS, RegisterDump, Runtime, SHADOW_SENTINELS, TABLE, VALUE_OPERAND,
    field,
};

/// Byte offsets of the stubs in their code.
pub struct Stubs {
    pub code: Vec<u32>,
    pub enter: usize,
    pub opt_enter: usize,
    pub opt_exit: usize,
    pub resident_exit: usize,
    pub exit: usize,
    /// Where the fault handler resumes a faulting site.
    pub fault: usize,
    /// Where translated code dispatches without leaving.
    pub dispatch: usize,
    pub lookup: usize,
    pub lookup_slow: usize,
    pub ret_miss: usize,
    /// Where the fault handler resumes a shadow stack overflow.
    pub shadow_reset: usize,
    /// Where a checked access the window cannot take calls the owner.
    pub access: usize,
    /// Where translated code calls an INT 0x2e handler.
    pub call: usize,
    /// Shared cold helpers for Tier-1 x87 runs.
    pub x87: super::X87Stubs,
}

const fn r(number: u8) -> Reg {
    Reg::new(number)
}

const fn v(number: u8) -> VReg {
    VReg::new(number)
}

const SP: Reg = Reg::new(31);
const RUNTIME: Reg = Reg::new(29);
const WINDOW: Reg = Reg::new(27);
const FRAME: i32 = 160;

const WINDOW_BASE: u32 = field(offset_of!(Runtime, window));
const SHADOW: u32 = field(offset_of!(Runtime, shadow));
const COUNTDOWN: u32 = field(offset_of!(Runtime, countdown));
const HOST_SP: u32 = field(offset_of!(Runtime, host_sp));
const HELPER: u32 = field(offset_of!(Runtime, helper));
const SHADOW_TOP: u32 = field(offset_of!(Runtime, shadow_top));
const RESUME: u32 = field(offset_of!(Runtime, resume));
const ACCESS: u32 = field(offset_of!(Runtime, access));
const CALL: u32 = field(offset_of!(Runtime, call));
const LOOKUP_MISS: u32 = field(offset_of!(Runtime, lookup_miss));
const REGS: i32 = offset_of!(CpuState, regs) as i32;

/// Host callee-saved registers, in frame order after x29/x30.
const SAVED: [(u8, u8); 5] = [(19, 20), (21, 22), (23, 24), (25, 26), (27, 28)];
const SAVED_V: [(u8, u8); 4] = [(8, 9), (10, 11), (12, 13), (14, 15)];

/// Guest register pairs by `CpuState::regs` index.
const GUEST: [(u8, u8); 4] = [(19, 20), (21, 22), (23, 24), (25, 26)];

pub fn stubs() -> Stubs {
    let mut asm = Asm::new();
    let enter = asm.offset();
    emit_enter(&mut asm);
    let opt_enter = asm.offset();
    emit_opt_enter(&mut asm);
    let exit_label = asm.label();
    asm.bind(exit_label);
    let exit = asm.offset();
    emit_exit(&mut asm);
    let fault_label = asm.label();
    asm.bind(fault_label);
    let fault = asm.offset();
    asm.emit_all(&[
        movz(W, r(0), Exit::Event(Event::Replay).word(), 0),
        movz(W, r(1), 0, 0),
    ]);
    asm.b(exit_label);
    let dispatch_label = asm.label();
    asm.bind(dispatch_label);
    let dispatch = asm.offset();
    emit_dispatch(&mut asm, HELPER, exit_label);
    let (lookup_label, slow_label) = (asm.label(), asm.label());
    asm.bind(lookup_label);
    let lookup = asm.offset();
    emit_probe(&mut asm, slow_label);
    asm.bind(slow_label);
    let lookup_slow = asm.offset();
    asm.emit_all(&[mov(W, r(1), r(16)), mov(X, r(2), r(4))]);
    emit_dispatch(&mut asm, LOOKUP_MISS, exit_label);
    let ret_miss = asm.offset();
    emit_ret_miss(&mut asm, lookup_label);
    let shadow_reset = asm.offset();
    asm.emit_all(&[
        ldr(X, r(28), RUNTIME, SHADOW_TOP),
        ldr(X, Reg::LR, RUNTIME, RESUME),
        br(Reg::LR),
    ]);
    let access = asm.offset();
    emit_access(&mut asm, fault_label);
    let call = asm.offset();
    emit_call(&mut asm);
    let x87 = super::x87_stubs::emit(&mut asm);
    let opt_exit = asm.offset();
    emit_opt_exit(&mut asm, Exit::Optimized);
    let resident_exit = asm.offset();
    emit_opt_exit(&mut asm, Exit::Plan(PlanEvent::ResidentRecovery));
    Stubs {
        call,
        x87,
        code: asm.finish(),
        enter,
        opt_enter,
        opt_exit,
        resident_exit,
        exit,
        fault,
        dispatch,
        lookup,
        lookup_slow,
        ret_miss,
        shadow_reset,
        access,
    }
}

/// The full host-call stub, entered with
/// x1 its argument and canonical flags. Keeps x7-x9 and x27-x29; the guest
/// registers and flags are what the owner left in `CpuState`, x0-x6 and
/// x13-x17 are free.
fn emit_call(asm: &mut Asm) {
    asm.emit_all(&[
        stp(X, Pre, r(7), r(8), SP, -32),
        stp(X, Offset, r(9), Reg::LR, SP, 16),
        mov(X, r(6), r(1)),
        ldr(X, r(0), RUNTIME, CPU),
    ]);
    emit_store_guest(asm, r(0));
    asm.emit_all(&[
        mov(X, r(0), RUNTIME),
        mov(X, r(1), r(6)),
        mov(X, r(2), r(9)),
        ldr(X, r(16), RUNTIME, CALL),
        // The frame chain records the guest return address for samplers.
        stp(X, Pre, RUNTIME, Reg::LR, SP, -16),
        add_imm(X, RUNTIME, SP, 0, false),
        blr(r(16)),
        ldp(X, Post, RUNTIME, Reg::LR, SP, 16),
        mov(X, r(6), r(0)),
        ldr(X, r(0), RUNTIME, CPU),
    ]);
    emit_load_guest(asm);
    asm.emit_all(&[
        ldp(X, Offset, r(9), Reg::LR, SP, 16),
        ldp(X, Post, r(7), r(8), SP, 32),
        sbfx(X, r(1), r(6), 32, 32),
        sub(X, r(9), r(9), r(1), Lsl, 0),
        mov(W, r(0), r(6)),
        ret(Reg::LR),
    ]);
}

const VALUE_ACCESS: u32 = field(offset_of!(Runtime, value_access));
const VALUE_CALL: u32 = field(offset_of!(Runtime, value));
const LOCKED_ACCESS: u32 = field(offset_of!(Runtime, locked_access));
const _: () = assert!(VALUE_OPERAND.is_multiple_of(16));

/// Calls the value entry with the prepared instruction index in w1. Preserve
/// the guest flag representation and any live caller-saved vector lanes.
pub(crate) enum ValueTarget {
    Float(Option<ValueAccess>),
    Locked,
    StackSpan { width: u16, push: bool },
}

pub(crate) fn emit_value_call(
    asm: &mut Asm,
    index: u32,
    xmm: u8,
    x87: Option<u8>,
    target: ValueTarget,
) {
    let (callback, access) = match target {
        ValueTarget::Float(access) => (Some(VALUE_CALL), access),
        ValueTarget::Locked => (Some(LOCKED_ACCESS), None),
        ValueTarget::StackSpan { .. } => (None, None),
    };
    let vectors = (0..8)
        .filter(|n| xmm & (1 << n) != 0)
        .map(|n| v(16 + n))
        .chain(x87.into_iter().flat_map(|slots| {
            std::iter::once(v(7)).chain(
                (0..8)
                    .filter(move |n| slots & (1 << n) != 0)
                    .map(|n| v(24 + n)),
            )
        }))
        .collect::<Vec<_>>();
    let scalar_bytes = if x87.is_some() { 96 } else { 80 };
    let frame = scalar_bytes + vectors.len() as i32 * 16;
    asm.emit_all(&[
        stp(X, Pre, r(7), r(8), SP, -frame),
        stp(X, Offset, r(9), r(10), SP, 16),
        stp(X, Offset, r(11), r(12), SP, 32),
        mrs_nzcv(r(16)),
        stp(X, Offset, r(5), r(16), SP, 48),
        str(X, Reg::LR, SP, 64),
    ]);
    if x87.is_some() {
        asm.emit(stp(X, Offset, r(13), r(15), SP, 80));
    }
    for (i, chunk) in vectors.chunks(2).enumerate() {
        let at = scalar_bytes + i as i32 * 32;
        asm.emit(match chunk {
            [a, b] => stp_v(VSize::Q, Offset, *a, *b, SP, at),
            [a] => vstr(VMem::Q, *a, SP, at as u32),
            _ => unreachable!(),
        });
    }
    let done = access.map(|_| asm.label());
    if let Some(access) = access {
        let (width, write) = match access {
            ValueAccess::Read(width) => (width, false),
            ValueAccess::Write(width) => (width, true),
        };
        value_access(asm, width, u16::from(write));
        asm.cbz(W, true, r(0), done.unwrap());
    }
    if let ValueTarget::StackSpan { width, push } = target {
        value_access(asm, width, if push { 3 } else { 0 });
    } else {
        asm.emit(movz(W, r(1), index as u16, 0));
        if index >> 16 != 0 {
            asm.emit(movk(W, r(1), (index >> 16) as u16, 16));
        }
        if matches!(target, ValueTarget::Locked) {
            asm.emit_all(&[
                mov(W, r(2), r(16)),
                mov(W, r(3), r(10)),
                mov(W, r(4), r(11)),
                mov(W, r(5), r(12)),
            ]);
        }
        asm.emit_all(&[
            mov(X, r(0), RUNTIME),
            ldr(X, r(16), RUNTIME, callback.unwrap()),
            blr(r(16)),
        ]);
    }
    if let Some(ValueAccess::Write(width)) = access {
        asm.tbz(false, r(0), 8, done.unwrap());
        value_access(asm, width, 2);
    }
    if let Some(done) = done {
        asm.bind(done);
    }
    for (i, chunk) in vectors.chunks(2).enumerate() {
        let at = scalar_bytes + i as i32 * 32;
        asm.emit(match chunk {
            [a, b] => ldp_v(VSize::Q, Offset, *a, *b, SP, at),
            [a] => vldr(VMem::Q, *a, SP, at as u32),
            _ => unreachable!(),
        });
    }
    if x87.is_some() {
        asm.emit(ldp(X, Offset, r(13), r(15), SP, 80));
    }
    asm.emit_all(&[
        ldr(X, Reg::LR, SP, 64),
        ldp(X, Offset, r(5), r(16), SP, 48),
        msr_nzcv(r(16)),
        ldp(X, Offset, r(11), r(12), SP, 32),
        ldp(X, Offset, r(9), r(10), SP, 16),
        ldp(X, Post, r(7), r(8), SP, frame),
    ]);
}

fn value_access(asm: &mut Asm, width: u16, phase: u16) {
    asm.emit_all(&[
        mov(X, r(0), RUNTIME),
        movz(W, r(1), width, 0),
        movz(W, r(2), phase, 0),
        ldr(X, r(16), RUNTIME, VALUE_ACCESS),
        blr(r(16)),
    ]);
}

/// Saves what light helpers keep (A64-REG-4): x5-x17, NZCV, LR and the
/// XMM registers units cache.
pub(crate) fn emit_save_light(asm: &mut Asm) {
    asm.emit_all(&[
        stp(X, Pre, r(5), r(6), SP, -128),
        stp(X, Offset, r(7), r(8), SP, 16),
        stp(X, Offset, r(9), r(10), SP, 32),
        stp(X, Offset, r(11), r(12), SP, 48),
        stp(X, Offset, r(13), r(14), SP, 64),
        stp(X, Offset, r(15), r(16), SP, 80),
        mrs_nzcv(r(16)),
        stp(X, Offset, r(17), r(16), SP, 96),
        str(X, Reg::LR, SP, 112),
        stp_v(VSize::Q, Pre, v(16), v(17), SP, -128),
        stp_v(VSize::Q, Offset, v(18), v(19), SP, 32),
        stp_v(VSize::Q, Offset, v(20), v(21), SP, 64),
        stp_v(VSize::Q, Offset, v(22), v(23), SP, 96),
    ]);
}

/// Restores what `emit_save_light` saved; uses x1.
pub(crate) fn emit_restore_light(asm: &mut Asm) {
    asm.emit_all(&[
        ldp_v(VSize::Q, Offset, v(22), v(23), SP, 96),
        ldp_v(VSize::Q, Offset, v(20), v(21), SP, 64),
        ldp_v(VSize::Q, Offset, v(18), v(19), SP, 32),
        ldp_v(VSize::Q, Post, v(16), v(17), SP, 128),
        ldr(X, Reg::LR, SP, 112),
        ldp(X, Offset, r(17), r(1), SP, 96),
        msr_nzcv(r(1)),
        ldp(X, Offset, r(15), r(16), SP, 80),
        ldp(X, Offset, r(13), r(14), SP, 64),
        ldp(X, Offset, r(11), r(12), SP, 48),
        ldp(X, Offset, r(9), r(10), SP, 32),
        ldp(X, Offset, r(7), r(8), SP, 16),
        ldp(X, Post, r(5), r(6), SP, 128),
    ]);
}

/// `access`, called with w1 the guest address, x2 the site, w3 a value to
/// store and w4 the kind. Keeps x5-x17 and NZCV; x0-x4 and x30 are free
/// wherever translated code accesses memory.
/// Also preserves v0-v7 temporaries, the v16-v23 cache and v24-v31 x87 pairs;
/// AAPCS64 keeps the MMX halves of v8-v15.
fn emit_access(asm: &mut Asm, fault: crate::asm::Label) {
    emit_save_light(asm);
    asm.emit_all(&[
        stp_v(VSize::Q, Pre, v(0), v(1), SP, -256),
        stp_v(VSize::Q, Offset, v(2), v(3), SP, 32),
        stp_v(VSize::Q, Offset, v(4), v(5), SP, 64),
        stp_v(VSize::Q, Offset, v(6), v(7), SP, 96),
        stp_v(VSize::Q, Offset, v(24), v(25), SP, 128),
        stp_v(VSize::Q, Offset, v(26), v(27), SP, 160),
        stp_v(VSize::Q, Offset, v(28), v(29), SP, 192),
        stp_v(VSize::Q, Offset, v(30), v(31), SP, 224),
        mov(X, r(0), RUNTIME),
        ldr(X, r(16), RUNTIME, ACCESS),
        // The frame chain records the guest return address for samplers.
        stp(X, Pre, RUNTIME, Reg::LR, SP, -16),
        add_imm(X, RUNTIME, SP, 0, false),
        blr(r(16)),
        mov(X, r(2), r(1)),
        ldp(X, Post, RUNTIME, Reg::LR, SP, 16),
        ldp_v(VSize::Q, Offset, v(30), v(31), SP, 224),
        ldp_v(VSize::Q, Offset, v(28), v(29), SP, 192),
        ldp_v(VSize::Q, Offset, v(26), v(27), SP, 160),
        ldp_v(VSize::Q, Offset, v(24), v(25), SP, 128),
        ldp_v(VSize::Q, Offset, v(6), v(7), SP, 96),
        ldp_v(VSize::Q, Offset, v(4), v(5), SP, 64),
        ldp_v(VSize::Q, Offset, v(2), v(3), SP, 32),
        ldp_v(VSize::Q, Post, v(0), v(1), SP, 256),
    ]);
    emit_restore_light(asm);
    let refused = asm.label();
    asm.cbz(X, true, r(2), refused);
    asm.emit(ret(Reg::LR));
    // Refused: the site faults, with the state exactly that before it,
    // dirty SIMD registers included.
    asm.bind(refused);
    let temps = offset_of!(FaultRegisters, temps) as i32;
    asm.emit(stp(X, Offset, r(7), r(8), r(2), temps));
    for n in 0..8u8 {
        let (low, high) = (v(8 + 2 * n), v(9 + 2 * n));
        asm.emit(stp_v(VSize::Q, Offset, low, high, r(2), 32 * i32::from(n)));
    }
    asm.cbz(X, false, r(0), fault);
    asm.emit(br(r(0)));
}

/// The indirect table probe for the EIP in w16: branches to its unit, or to
/// `miss` with w16 intact. Uses x6 and x17.
fn emit_probe(asm: &mut Asm, miss: crate::asm::Label) {
    asm.emit_all(&[
        ldr(X, r(17), RUNTIME, TABLE),
        // `table_slot`: fold the high half into the low 16 bits.
        eor(W, r(6), r(16), r(16), Lsr, 16),
        ubfx(W, r(6), r(6), 0, 16),
        add(X, r(17), r(17), r(6), Lsl, 4),
        ldp(X, Offset, r(6), r(17), r(17), 0),
        sub(X, r(6), r(6), r(16), Lsl, 0),
    ]);
    asm.cbz(X, true, r(6), miss);
    asm.emit(br(r(17)));
}

/// A return to the EIP in w16 missed the top shadow entry: returns through
/// the first of the next entries that matches, popping it and those above,
/// or looks the EIP up and leaves the stack alone.
fn emit_ret_miss(asm: &mut Asm, lookup: crate::asm::Label) {
    let hits: Vec<_> = (1..SHADOW_SENTINELS).map(|_| asm.label()).collect();
    for (depth, &hit) in (1u32..).zip(&hits) {
        asm.emit_all(&[
            ldr(X, r(17), r(28), 16 * depth + 8),
            sub(X, r(17), r(17), r(16), Lsl, 0),
        ]);
        asm.cbz(X, false, r(17), hit);
    }
    asm.b(lookup);
    for (depth, hit) in (1u32..).zip(hits) {
        asm.bind(hit);
        asm.emit_all(&[
            ldr(X, Reg::LR, r(28), 16 * depth),
            add_imm(X, r(28), r(28), 16 * (depth + 1), false),
            ret(Reg::LR),
        ]);
    }
}

/// `dispatch`: w1 the guest EIP, x2 a link point or 0. Calls the helper with
/// the runtime, and continues at the unit it returns or leaves with
/// `Exit::Dispatch` when it returns 0.
fn emit_dispatch(asm: &mut Asm, helper: u32, exit: crate::asm::Label) {
    asm.emit_all(&[
        // The helper may empty the shadow stack.
        str(X, r(28), RUNTIME, SHADOW),
        stp(X, Pre, r(1), r(2), SP, -80),
        stp(X, Offset, r(7), r(8), SP, 16),
        stp(X, Offset, r(9), r(10), SP, 32),
        stp(X, Offset, r(11), r(12), SP, 48),
        mrs_nzcv(r(17)),
        stp(X, Offset, r(17), Reg::LR, SP, 64),
        mov(X, r(0), RUNTIME),
        ldr(X, r(16), RUNTIME, helper),
        blr(r(16)),
        ldp(X, Offset, r(17), Reg::LR, SP, 64),
        msr_nzcv(r(17)),
        ldp(X, Offset, r(11), r(12), SP, 48),
        ldp(X, Offset, r(9), r(10), SP, 32),
        ldp(X, Offset, r(7), r(8), SP, 16),
        ldp(X, Post, r(1), r(2), SP, 80),
        ldr(X, r(28), RUNTIME, SHADOW),
    ]);
    let leave = asm.label();
    asm.cbz(X, false, r(0), leave);
    asm.emit(br(r(0)));
    asm.bind(leave);
    asm.emit(movz(W, r(0), Exit::Dispatch.word(), 0));
    asm.b(exit);
}

fn emit_enter_frame(asm: &mut Asm) {
    asm.emit(stp(X, Pre, r(29), Reg::LR, SP, -FRAME));
    for (i, (a, b)) in SAVED.into_iter().enumerate() {
        asm.emit(stp(X, Offset, r(a), r(b), SP, 16 + 16 * i as i32));
    }
    for (i, (a, b)) in SAVED_V.into_iter().enumerate() {
        asm.emit(stp_v(VSize::D, Offset, v(a), v(b), SP, 96 + 16 * i as i32));
    }
    asm.emit_all(&[
        mov_sp(RUNTIME, r(0)),
        mov_sp(r(16), SP),
        str(X, r(16), RUNTIME, HOST_SP),
        mov(X, r(17), r(1)),
    ]);
}

/// `enter(runtime: *mut Runtime, target: usize)` in x0 and x1.
fn emit_enter(asm: &mut Asm) {
    emit_enter_frame(asm);
    asm.emit_all(&[
        ldr(X, WINDOW, RUNTIME, WINDOW_BASE),
        ldr(X, r(28), RUNTIME, SHADOW),
        ldr(X, r(9), RUNTIME, COUNTDOWN),
        ldr(X, r(0), RUNTIME, CPU),
    ]);
    emit_load_guest(asm);
    asm.emit(br(r(17)));
}

/// `opt_enter(runtime: *mut Runtime, target: usize)` for optimized forms,
/// which load all live state from `opt_params` and need no guest register setup.
fn emit_opt_enter(asm: &mut Asm) {
    emit_enter_frame(asm);
    asm.emit_all(&[
        ldr(X, r(28), RUNTIME, SHADOW),
        ldr(X, r(9), RUNTIME, COUNTDOWN),
        br(r(17)),
    ]);
}

/// Loads the guest registers and flags from the `CpuState` at x0; uses x1
/// to x3 and x16.
fn emit_load_guest(asm: &mut Asm) {
    for (i, (a, b)) in GUEST.into_iter().enumerate() {
        if a == 23 {
            // ESP and EBP live biased by the window base.
            asm.emit_all(&[
                ldp(W, Offset, r(16), r(1), r(0), REGS + 8 * i as i32),
                add_ext(X, r(23), WINDOW, r(16), Uxtw, 0),
                add_ext(X, r(24), WINDOW, r(1), Uxtw, 0),
            ]);
        } else {
            asm.emit(ldp(W, Offset, r(a), r(b), r(0), REGS + 8 * i as i32));
        }
    }
    asm.emit(ldr(W, r(1), r(0), EFLAGS));
    emit_load_flags(asm);
}

/// Establish canonical NZCV and lazy PF, AF and DF from w1.
pub(crate) fn emit_load_flags(asm: &mut Asm) {
    asm.emit_all(&[
        // NZCV in subtraction sense: SF ZF !CF OF.
        and_imm(W, r(2), r(1), 0xc0),
        ubfx(W, r(3), r(1), 0, 1),
        eor_imm(W, r(3), r(3), 1),
        orr(W, r(2), r(2), r(3), Lsl, 5),
        ubfx(W, r(3), r(1), 11, 1),
        orr(W, r(2), r(2), r(3), Lsl, 4),
        lsl_imm(W, r(2), r(2), 24),
        msr_nzcv(r(2)),
        // 0 has even parity and 1 odd, so x10 = !PF reproduces PF.
        ubfx(W, r(10), r(1), 2, 1),
        eor_imm(W, r(10), r(10), 1),
        and_imm(W, r(11), r(1), 0x10),
        ubfx(W, r(12), r(1), 10, 1),
    ]);
}

/// Translated code arrives with the `Exit` in w0 and the guest EIP in w1.
fn emit_exit(asm: &mut Asm) {
    asm.emit_all(&[ldr(X, r(16), RUNTIME, CPU), str(W, r(1), r(16), EIP)]);
    emit_store_guest(asm, r(16));
    asm.emit_all(&[
        ldr(X, r(1), RUNTIME, COMPLETED),
        sub(X, r(1), r(9), r(1), Lsl, 0),
        mov(X, r(2), r(28)),
        ldr(W, r(3), RUNTIME, DEFERRED),
        ldr(X, r(16), RUNTIME, HOST_SP),
        mov_sp(SP, r(16)),
    ]);
    for (i, (a, b)) in SAVED_V.into_iter().enumerate() {
        asm.emit(ldp_v(VSize::D, Offset, v(a), v(b), SP, 96 + 16 * i as i32));
    }
    for (i, (a, b)) in SAVED.into_iter().enumerate() {
        asm.emit(ldp(X, Offset, r(a), r(b), SP, 16 + 16 * i as i32));
    }
    asm.emit_all(&[ldp(X, Post, r(29), Reg::LR, SP, FRAME), ret(Reg::LR)]);
}

// spec: A64-OPT-4, A64-OPT-6, A64-OPT-7
fn emit_opt_exit(asm: &mut Asm, exit: Exit) {
    let dump = offset_of!(Runtime, recovery.dump) as u32;
    // Capture before borrowing any allocatable register. x16/x17 are emission scratch.
    for n in 0..31 {
        asm.emit(str(X, r(n), RUNTIME, dump + 8 * u32::from(n)));
    }
    let vectors = dump + offset_of!(RegisterDump, v) as u32;
    for n in 0..32 {
        asm.emit(str_v(VSize::Q, v(n), RUNTIME, vectors + 16 * u32::from(n)));
    }
    // The dump holds every register now; save the spill cells, which the
    // caller's next frames would overwrite.
    let (copy, done) = (asm.label(), asm.label());
    asm.emit_all(&[
        mov_sp(r(16), SP),
        str(
            X,
            r(16),
            RUNTIME,
            dump + offset_of!(RegisterDump, sp) as u32,
        ),
        ldr(
            X,
            r(15),
            RUNTIME,
            offset_of!(Runtime, recovery.saved) as u32,
        ),
        ldr(X, r(17), RUNTIME, RECOVERY_CELLS),
    ]);
    asm.bind(copy);
    asm.cbz(X, false, r(17), done);
    asm.emit_all(&[
        ldp(X, Post, r(0), r(1), r(16), 16),
        stp(X, Post, r(0), r(1), r(15), 16),
        sub_imm(X, r(17), r(17), 1, false),
    ]);
    asm.b(copy);
    asm.bind(done);
    asm.emit_all(&[
        movz(W, r(0), exit.word(), 0),
        ldr(X, r(1), RUNTIME, COMPLETED),
        sub(X, r(1), r(9), r(1), Lsl, 0),
        mov(X, r(2), r(28)),
        ldr(W, r(3), RUNTIME, DEFERRED),
        ldr(X, r(16), RUNTIME, HOST_SP),
        mov_sp(SP, r(16)),
    ]);
    for (i, (a, b)) in SAVED_V.into_iter().enumerate() {
        asm.emit(ldp_v(VSize::D, Offset, v(a), v(b), SP, 96 + 16 * i as i32));
    }
    for (i, (a, b)) in SAVED.into_iter().enumerate() {
        asm.emit(ldp(X, Offset, r(a), r(b), SP, 16 + 16 * i as i32));
    }
    asm.emit_all(&[ldp(X, Post, r(29), Reg::LR, SP, FRAME), ret(Reg::LR)]);
}

/// Stores the guest registers and flags to `cpu`; uses x1 to x5.
pub(crate) fn emit_store_registers(asm: &mut Asm, cpu: Reg) {
    for (i, (a, b)) in GUEST.into_iter().enumerate() {
        // The low halves of x23 and x24 are ESP and EBP themselves.
        asm.emit(stp(W, Offset, r(a), r(b), cpu, REGS + 8 * i as i32));
    }
}

pub(crate) fn emit_store_guest(asm: &mut Asm, cpu: Reg) {
    emit_store_registers(asm, cpu);
    emit_read_flags(asm, cpu);
    asm.emit(str(W, r(4), cpu, EFLAGS));
}

/// Assemble EFLAGS in w4 using the unmapped bits in `cpu`.
pub(crate) fn emit_read_flags(asm: &mut Asm, cpu: Reg) {
    asm.emit_all(&[
        mrs_nzcv(r(1)),
        // N and Z land on SF and ZF; C is !CF and V is OF.
        lsr_imm(W, r(2), r(1), 24),
        and_imm(W, r(3), r(2), 0xc0),
        ubfx(W, r(4), r(2), 5, 1),
        eor_imm(W, r(4), r(4), 1),
        orr(W, r(3), r(3), r(4), Lsl, 0),
        ubfx(W, r(4), r(2), 4, 1),
        orr(W, r(3), r(3), r(4), Lsl, 11),
        and_imm(W, r(4), r(11), 0x10),
        orr(W, r(3), r(3), r(4), Lsl, 0),
        // Fold the low byte of x10 to its parity in bit 0.
        eor(W, r(4), r(10), r(10), Lsr, 4),
        eor(W, r(4), r(4), r(4), Lsr, 2),
        eor(W, r(4), r(4), r(4), Lsr, 1),
        and_imm(W, r(4), r(4), 1),
        eor_imm(W, r(4), r(4), 1),
        orr(W, r(3), r(3), r(4), Lsl, 2),
        orr(W, r(3), r(3), r(12), Lsl, 10),
        ldr(W, r(4), cpu, EFLAGS),
        movz(W, r(5), MAPPED_FLAGS as u16, 0),
        bic(W, r(4), r(4), r(5), Lsl, 0),
        orr(W, r(4), r(4), r(3), Lsl, 0),
    ]);
}

#[cfg(all(test, emulator))]
mod tests {
    use super::*;
    use crate::emulator::{CODE, Emulator};

    const DATA: u64 = 0x2000_0000;
    const STATE: u64 = DATA + 0x1000;
    const STACK_TOP: u64 = 0x3001_0000;
    const BASE: u64 = 7 << 32;
    const XMM: u32 = field(offset_of!(CpuState, xmm));

    /// Bytes of a `Runtime` for the emulator, whose `cpu` is at `STATE`.
    fn runtime_bytes() -> Vec<u8> {
        let runtime = Runtime::new(std::ptr::null_mut(), BASE);
        let mut bytes = vec![0; size_of::<Runtime>()];
        bytes[CPU as usize..][..8].copy_from_slice(&STATE.to_le_bytes());
        bytes[WINDOW_BASE as usize..][..8].copy_from_slice(&runtime.window.to_le_bytes());
        bytes[SHADOW as usize..][..8].copy_from_slice(&0x5000u64.to_le_bytes());
        bytes[COUNTDOWN as usize..][..8].copy_from_slice(&runtime.countdown.to_le_bytes());
        bytes
    }

    /// The fields the stubs touch, at their `CpuState` offsets.
    fn state_bytes(cpu: &CpuState) -> Vec<u8> {
        let mut bytes = vec![0; size_of::<CpuState>()];
        for (i, reg) in cpu.regs.iter().enumerate() {
            bytes[REGS as usize + 4 * i..][..4].copy_from_slice(&reg.to_le_bytes());
        }
        bytes[EIP as usize..][..4].copy_from_slice(&cpu.eip.to_le_bytes());
        bytes[EFLAGS as usize..][..4].copy_from_slice(&cpu.eflags.to_le_bytes());
        bytes[XMM as usize..][..128].copy_from_slice(cpu.xmm.as_flattened());
        bytes
    }

    struct Run {
        regs: [u32; 8],
        eip: u32,
        eflags: u32,
        xmm: [[u8; 16]; 8],
        exit: u32,
        countdown: u64,
    }

    /// Enters `body` (translated code that must end at `exit`) with `cpu`.
    fn run(cpu: &CpuState, body: impl FnOnce(&mut Asm, crate::asm::Label)) -> (Emulator, Run) {
        let stubs = stubs();
        let mut asm = Asm::new();
        asm.emit_all(&stubs.code);
        let exit = asm.label();
        let target = asm.offset();
        body(&mut asm, exit);
        // `exit` is a jump back to the exit stub.
        let tail = asm.offset();
        asm.bind(exit);
        asm.emit(b(stubs.exit as i32 - tail as i32));
        let code = asm.finish();

        let mut emu = Emulator::new();
        emu.map(DATA, 0x2000);
        emu.map(STACK_TOP - 0x1_0000, 0x1_0000);
        emu.write(DATA, &runtime_bytes());
        emu.write(STATE, &state_bytes(cpu));
        emu.set_sp(STACK_TOP);
        emu.set_x(r(0), DATA);
        emu.set_x(r(1), CODE + target as u64);
        for n in 19..=29 {
            emu.set_x(r(n), 0xC0DE_0000_0000_0000 | n as u64);
        }
        for n in 8..16 {
            emu.set_q(n, 0xF00D_0000_0000_0000 | n as u128);
        }
        emu.call_at(&code, stubs.enter)
            .expect("stubs run to completion");

        assert_eq!(emu.sp(), STACK_TOP, "host SP restored");
        for n in 19..=29 {
            assert_eq!(
                emu.x(r(n)),
                0xC0DE_0000_0000_0000 | n as u64,
                "x{n} restored"
            );
        }
        for n in 8..16 {
            // AAPCS64 preserves only the low 64 bits of v8-v15.
            assert_eq!(
                emu.q(n) as u64,
                0xF00D_0000_0000_0000 | n as u64,
                "d{n} restored"
            );
        }
        let state = emu.read(STATE, size_of::<CpuState>());
        let word = |offset: usize| u32::from_le_bytes(state[offset..][..4].try_into().unwrap());
        let mut regs = [0; 8];
        for (i, reg) in regs.iter_mut().enumerate() {
            *reg = word(REGS as usize + 4 * i);
        }
        let mut xmm = [[0; 16]; 8];
        for (i, lane) in xmm.iter_mut().enumerate() {
            lane.copy_from_slice(&state[XMM as usize + 16 * i..][..16]);
        }
        let run = Run {
            regs,
            eip: word(EIP as usize),
            eflags: word(EFLAGS as usize),
            xmm,
            exit: emu.x(r(0)) as u32,
            countdown: emu.x(r(1)),
        };
        (emu, run)
    }

    fn guest() -> CpuState {
        let mut cpu = CpuState {
            regs: [
                0x1111_1111,
                0x2222_2222,
                0x3333_3333,
                0x4444_4444,
                0x0012_ff80,
                0xffff_fffc,
                0x6666_6666,
                0x7777_7777,
            ],
            eip: 0x40_1000,
            ..CpuState::default()
        };
        for (i, lane) in cpu.xmm.iter_mut().enumerate() {
            *lane = [0x10 + i as u8; 16];
            lane[15] = 0xa0 + i as u8;
        }
        cpu
    }

    fn leave(asm: &mut Asm, exit: crate::asm::Label, reason: Exit, eip: u32) {
        asm.emit_all(&[
            movz(W, r(0), reason.word(), 0),
            movz(W, r(1), eip as u16, 0),
            movk(W, r(1), (eip >> 16) as u16, 16),
        ]);
        asm.b(exit);
    }

    // spec: A64-REG-2, A64-REG-3
    #[test]
    fn every_mapped_flag_combination_and_register_round_trips() {
        for bits in 0..1u32 << 7 {
            let mut cpu = guest();
            // Spread the seven bits over CF PF AF ZF SF DF OF; keep IF and bit 1.
            let flags = [0, 2, 4, 6, 7, 10, 11]
                .into_iter()
                .enumerate()
                .fold(0x202, |acc, (i, bit)| acc | (bits >> i & 1) << bit);
            cpu.eflags = flags;
            let (_, run) = run(&cpu, |asm, exit| {
                leave(asm, exit, Exit::Dispatch, 0x40_2345)
            });
            assert_eq!(run.eflags, flags, "flags {flags:#x}");
            assert_eq!(run.regs, cpu.regs);
            assert_eq!(run.xmm, cpu.xmm);
            assert_eq!(run.eip, 0x40_2345);
            assert_eq!(run.exit, Exit::Dispatch.raw());
        }
    }

    // spec: A64-REG-1
    #[test]
    fn translated_code_sees_the_register_map() {
        let cpu = guest();
        let (emu, run) = run(&cpu, |asm, exit| {
            // Capture the mapping in temporaries the exit stub leaves alone.
            asm.emit_all(&[
                mov(X, r(6), r(23)),
                mov(X, r(7), r(24)),
                mov(X, r(8), WINDOW),
                sub(X, r(17), r(23), WINDOW, Lsl, 0),
                // push: ESP -= 4 through the biased register.
                sub_imm(X, r(23), r(23), 4, false),
                add_imm(W, r(19), r(19), 1, false),
                sub_imm(X, r(9), r(9), 1, false),
            ]);
            leave(asm, exit, Exit::Event(Event::Fetch), 0x40_1003);
        });
        assert_eq!(emu.x(r(6)), BASE + 0x0012_ff80);
        assert_eq!(emu.x(r(7)), BASE + 0xffff_fffc);
        assert_eq!(emu.x(r(8)), BASE);
        assert_eq!(emu.x(r(17)), 0x0012_ff80);
        let mut expected = cpu.regs;
        expected[0] += 1;
        expected[4] -= 4;
        assert_eq!(run.regs, expected);
        assert_eq!(run.exit, Exit::Event(Event::Fetch).raw());
        assert_eq!(run.countdown, (u64::MAX >> 1) - 1);
    }

    /// `cmp ecx, edx` in subtraction sense, with the canonical PF/AF sources,
    /// must leave the eflags the interpreter computes for the same compare.
    // spec: A64-REG-3
    #[test]
    fn a_native_compare_exits_with_the_interpreters_flags() {
        let pairs = [
            (0, 0),
            (1, 2),
            (2, 1),
            (0x8000_0000, 1),
            (0x7fff_ffff, 0xffff_ffff),
            (0x10, 0x01),
            (0xff, 0x0f),
        ];
        for (ecx, edx) in pairs {
            let mut cpu = guest();
            cpu.regs[1] = ecx;
            cpu.regs[2] = edx;
            cpu.eflags = 0x202 | 0x400;
            let (_, run) = run(&cpu, |asm, exit| {
                asm.emit_all(&[
                    sub(W, r(10), r(20), r(21), Lsl, 0),
                    eor(W, r(11), r(20), r(21), Lsl, 0),
                    eor(W, r(11), r(11), r(10), Lsl, 0),
                    cmp(W, r(20), r(21), Lsl, 0),
                ]);
                leave(asm, exit, Exit::Dispatch, 0x40_1002);
            });
            assert_eq!(run.eflags, interpreted_compare(&cpu), "{ecx:#x} - {edx:#x}");
        }
    }

    fn interpreted_compare(cpu: &CpuState) -> u32 {
        const CODE: u32 = 0x40_1000;
        let mut executor = crate::Executor::new(CODE, 0x1000).unwrap();
        executor.write(CODE, &[0x39, 0xd1]).unwrap(); // cmp ecx, edx
        executor
            .protect(CODE, 0x1000, adv_cpu_core::READ | adv_cpu_core::EXEC)
            .unwrap();
        executor.state = cpu.clone();
        executor.state.eip = CODE;
        let outcome = executor.run(CODE + 2, 1).unwrap();
        assert_eq!(outcome, adv_cpu_core::RunOutcome::ReachedStop);
        executor.state.eflags
    }
}

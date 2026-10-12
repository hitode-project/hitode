//! The execution thread's runtime context and the stubs that move guest
//! state between `CpuState` and the mapped host registers (execution ABI §A2).
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
//! indirect table (§A5.3), which the helper fills, and `lookup_slow` asks
//! the helper. Returns pop the shadow stack (§A5.2) at x28, whose entries
//! are {host continuation, guest return EIP} and grow down to a guard page;
//! `ret_miss` resynchronizes with a few entries below the top before it
//! looks up, and `shadow_reset` empties the stack after an overflow.
//!
//! Checked accesses (§A4.3) call `access` when the page class table rules
//! out the window: it saves every register translated code may keep live,
//! has the owner's `access` helper perform the access, and returns the
//! loaded value in w0; when the owner refuses, it leaves as the fault stub
//! would for a host fault at the access, whose site the helper recorded.
//!
//! Instructions the translator leaves to the interpreter call `interpret`
//! with canonical flags: it stores the guest as `exit` does, has the
//! owner's `interpret` helper run the instruction, loads the guest back as
//! `enter` does and returns the helper's status, 0 to continue.
//!
//! Units keep `CpuState` current for MMX and XMM registers and cache them
//! in d8-d15 and v16-v23 (`translate::simd`), so only `access`, which
//! returns into the unit, keeps v16-v23. Arithmetic flags cross the boundary
//! in the canonical block-exit form of §A3: NZCV in subtraction sense, x10 a
//! value whose low byte has PF's parity, x11 a value whose bit 4 is AF, and
//! x12 DF. x87 state stays in `CpuState` (execution ABI §A6.1).

use std::mem::offset_of;

use adv_cpu_core::CpuState;

use crate::asm::Asm;
use crate::encode::Extend::Uxtw;
use crate::encode::Index::{Offset, Post, Pre};
use crate::encode::Shift::{Lsl, Lsr};
use crate::encode::Size::{W, X};
use crate::encode::*;

/// What `x29` points at while translated code runs.
#[repr(C)]
#[derive(Debug)]
pub struct Runtime {
    pub cpu: *mut CpuState,
    /// Guest window base, 4 GiB aligned: `x27`.
    pub window: u64,
    /// Shadow return stack pointer: `x28`.
    pub shadow: u64,
    /// Safepoint countdown: `x9`.
    pub countdown: u64,
    /// Host stack pointer inside `enter`'s frame, for helpers and `exit`.
    pub host_sp: u64,
    /// `helper(runtime, eip, link) -> unit or 0`, called by `dispatch`.
    pub helper: u64,
    /// Owner data for `helper`.
    pub owner: u64,
    /// The indirect table: 65536 entries of {guest EIP, unit}.
    pub table: u64,
    /// The empty shadow stack.
    pub shadow_top: u64,
    /// Where `shadow_reset` resumes.
    pub resume: u64,
    /// `access(runtime, address, site, value, kind) -> value`, called by
    /// `access`; bit 32 set refuses.
    pub access: u64,
    /// The page class table: a byte per guest page, its permissions as
    /// `Memory::native_permissions` has them.
    pub classes: u64,
    /// `interpret(runtime, helper, countdown) -> status`, called by
    /// `interpret`: 0 continues, else the `Exit` + 1 to leave with at the
    /// EIP in `CpuState`. Bits 32 and up are the instructions it charges
    /// beyond the one prepaid, signed.
    pub interpret: u64,
    /// The `Exit` that left translated code.
    pub exit: u32,
    /// The recipe temporaries x7 and x8 at the exit, for fault recovery.
    pub temps: [u64; 2],
}

impl Runtime {
    pub fn new(cpu: *mut CpuState, window: u64) -> Self {
        assert_eq!(window % (1 << 32), 0, "the window is 4 GiB aligned");
        Self {
            cpu,
            window,
            shadow: 0,
            countdown: u64::MAX >> 1,
            host_sp: 0,
            helper: 0,
            owner: 0,
            table: 0,
            shadow_top: 0,
            resume: 0,
            access: 0,
            classes: 0,
            interpret: 0,
            exit: 0,
            temps: [0; 2],
        }
    }
}

/// Why translated code returned to the owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Exit {
    /// Interpret the instruction at EIP, then dispatch.
    Interpret = 0,
    /// Look up or translate the unit at EIP.
    Dispatch = 1,
    /// A host fault at a side-table site. EIP is not set; the owner takes it
    /// from the site, where guest state is exactly that before the
    /// instruction (§A3.2).
    Fault = 2,
    /// The `interpret` helper left a step for the owner to complete.
    Stepped = 3,
}

impl Exit {
    pub fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            0 => Some(Self::Interpret),
            1 => Some(Self::Dispatch),
            2 => Some(Self::Fault),
            3 => Some(Self::Stepped),
            _ => None,
        }
    }
}

/// Byte offsets of the stubs in their code.
pub struct Stubs {
    pub code: Vec<u32>,
    pub enter: usize,
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
    /// Where translated code has the owner interpret an instruction.
    pub interpret: usize,
}

/// Indirect table entries; a guest EIP indexes them by bits 2 to 17.
pub const TABLE_LEN: usize = 1 << 16;

/// The indirect table slot of `eip`.
pub const fn table_slot(eip: u32) -> usize {
    (eip as usize >> 2) & (TABLE_LEN - 1)
}

/// Shadow entries above the top that never match, for `ret_miss` to read.
pub const SHADOW_SENTINELS: usize = 4;

/// An `access` kind is the width in bytes, with these bits.
pub const ACCESS_STORE: u32 = 8;
pub const ACCESS_SIGNED: u32 = 16;

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

const fn field(offset: usize) -> u32 {
    offset as u32
}

const CPU: u32 = field(offset_of!(Runtime, cpu));
const WINDOW_BASE: u32 = field(offset_of!(Runtime, window));
const SHADOW: u32 = field(offset_of!(Runtime, shadow));
const COUNTDOWN: u32 = field(offset_of!(Runtime, countdown));
const HOST_SP: u32 = field(offset_of!(Runtime, host_sp));
const EXIT: u32 = field(offset_of!(Runtime, exit));
const HELPER: u32 = field(offset_of!(Runtime, helper));
pub(crate) const TABLE: u32 = field(offset_of!(Runtime, table));
const SHADOW_TOP: u32 = field(offset_of!(Runtime, shadow_top));
const RESUME: u32 = field(offset_of!(Runtime, resume));
const ACCESS: u32 = field(offset_of!(Runtime, access));
const INTERPRET: u32 = field(offset_of!(Runtime, interpret));
pub(crate) const CLASSES: u32 = field(offset_of!(Runtime, classes));
pub(crate) const CPU_STATE: u32 = CPU;
const TEMPS: i32 = offset_of!(Runtime, temps) as i32;
const REGS: i32 = offset_of!(CpuState, regs) as i32;
pub(crate) const EIP: u32 = field(offset_of!(CpuState, eip));
pub(crate) const FS_BASE: u32 = field(offset_of!(CpuState, fs_base));
pub(crate) const XMM: u32 = field(offset_of!(CpuState, xmm));
pub(crate) const MMX: u32 = field(offset_of!(CpuState, mmx));
pub(crate) const X87_REGS: u32 = field(offset_of!(CpuState, x87.regs));
pub(crate) const X87_STATUS: u32 = field(offset_of!(CpuState, x87.status));
pub(crate) const X87_TAG: u32 = field(offset_of!(CpuState, x87.tag));
pub(crate) const X87_CONTROL: u32 = field(offset_of!(CpuState, x87.control));
pub(crate) const X87_OPCODE: u32 = field(offset_of!(CpuState, x87.opcode));
pub(crate) const X87_IP: u32 = field(offset_of!(CpuState, x87.ip));
pub(crate) const X87_DP: u32 = field(offset_of!(CpuState, x87.dp));
pub(crate) const MMX_MODE: u32 = field(offset_of!(CpuState, mmx_mode));
pub(crate) const MXCSR: u32 = field(offset_of!(CpuState, mxcsr));
const EFLAGS: u32 = field(offset_of!(CpuState, eflags));

/// CF PF AF ZF SF DF OF: the flags the stubs map.
pub const MAPPED_FLAGS: u32 = 0xcd5;

/// Host callee-saved registers, in frame order after x29/x30.
const SAVED: [(u8, u8); 5] = [(19, 20), (21, 22), (23, 24), (25, 26), (27, 28)];
const SAVED_V: [(u8, u8); 4] = [(8, 9), (10, 11), (12, 13), (14, 15)];

/// Guest register pairs by `CpuState::regs` index.
const GUEST: [(u8, u8); 4] = [(19, 20), (21, 22), (23, 24), (25, 26)];

pub fn stubs() -> Stubs {
    let mut asm = Asm::new();
    let enter = asm.offset();
    emit_enter(&mut asm);
    let exit_label = asm.label();
    asm.bind(exit_label);
    let exit = asm.offset();
    emit_exit(&mut asm);
    let fault_label = asm.label();
    asm.bind(fault_label);
    let fault = asm.offset();
    asm.emit_all(&[movz(W, r(0), Exit::Fault as u16, 0), movz(W, r(1), 0, 0)]);
    asm.b(exit_label);
    let dispatch_label = asm.label();
    asm.bind(dispatch_label);
    let dispatch = asm.offset();
    emit_dispatch(&mut asm, exit_label);
    let (lookup_label, slow_label) = (asm.label(), asm.label());
    asm.bind(lookup_label);
    let lookup = asm.offset();
    emit_probe(&mut asm, slow_label);
    asm.bind(slow_label);
    let lookup_slow = asm.offset();
    asm.emit_all(&[mov(W, r(1), r(16)), movz(X, r(2), 0, 0)]);
    asm.b(dispatch_label);
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
    let interpret = asm.offset();
    emit_interpret(&mut asm);
    Stubs {
        code: asm.finish(),
        enter,
        exit,
        fault,
        dispatch,
        lookup,
        lookup_slow,
        ret_miss,
        shadow_reset,
        access,
        interpret,
    }
}

/// `interpret`, called with w1 the helper's index and canonical flags.
/// Keeps x7-x9 and x27-x29; the guest registers and flags are what the
/// helper left in `CpuState`, x0-x6 and x13-x17 are free.
fn emit_interpret(asm: &mut Asm) {
    asm.emit_all(&[
        stp(X, Pre, r(7), r(8), SP, -32),
        stp(X, Offset, r(9), Reg::LR, SP, 16),
        mov(W, r(6), r(1)),
        ldr(X, r(0), RUNTIME, CPU),
    ]);
    emit_store_guest(asm);
    asm.emit_all(&[
        mov(X, r(0), RUNTIME),
        mov(W, r(1), r(6)),
        mov(X, r(2), r(9)),
        ldr(X, r(16), RUNTIME, INTERPRET),
        blr(r(16)),
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

/// `access`, called with w1 the guest address, x2 the site, w3 a value to
/// store and w4 the kind. Keeps x5-x17 and NZCV; x0-x4 and x30 are free
/// wherever translated code accesses memory.
fn emit_access(asm: &mut Asm, fault: crate::asm::Label) {
    asm.emit_all(&[
        stp(X, Pre, r(5), r(6), SP, -128),
        stp(X, Offset, r(7), r(8), SP, 16),
        stp(X, Offset, r(9), r(10), SP, 32),
        stp(X, Offset, r(11), r(12), SP, 48),
        stp(X, Offset, r(13), r(14), SP, 64),
        stp(X, Offset, r(15), r(16), SP, 80),
        mrs_nzcv(r(0)),
        stp(X, Offset, r(17), r(0), SP, 96),
        str(X, Reg::LR, SP, 112),
        // The XMM registers units cache.
        stp_v(VSize::Q, Pre, v(16), v(17), SP, -128),
        stp_v(VSize::Q, Offset, v(18), v(19), SP, 32),
        stp_v(VSize::Q, Offset, v(20), v(21), SP, 64),
        stp_v(VSize::Q, Offset, v(22), v(23), SP, 96),
        mov(X, r(0), RUNTIME),
        ldr(X, r(16), RUNTIME, ACCESS),
        blr(r(16)),
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
    // Refused: the site faults, with the state exactly that before it.
    asm.tbz(true, r(0), 32, fault);
    asm.emit(ret(Reg::LR));
}

/// The indirect table probe for the EIP in w16: branches to its unit, or to
/// `miss` with w16 intact. Uses x6 and x17.
fn emit_probe(asm: &mut Asm, miss: crate::asm::Label) {
    asm.emit_all(&[
        ldr(X, r(17), RUNTIME, TABLE),
        ubfx(X, r(6), r(16), 2, 16),
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
fn emit_dispatch(asm: &mut Asm, exit: crate::asm::Label) {
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
        ldr(X, r(16), RUNTIME, HELPER),
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
    asm.emit(movz(W, r(0), Exit::Dispatch as u16, 0));
    asm.b(exit);
}

/// `enter(runtime: *mut Runtime, target: usize)` in x0 and x1.
fn emit_enter(asm: &mut Asm) {
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
        ldr(X, WINDOW, RUNTIME, WINDOW_BASE),
        ldr(X, r(28), RUNTIME, SHADOW),
        ldr(X, r(9), RUNTIME, COUNTDOWN),
        ldr(X, r(0), RUNTIME, CPU),
    ]);
    emit_load_guest(asm);
    asm.emit(br(r(17)));
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
    asm.emit_all(&[
        ldr(W, r(1), r(0), EFLAGS),
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
    asm.emit_all(&[
        str(W, r(0), RUNTIME, EXIT),
        stp(X, Offset, r(7), r(8), RUNTIME, TEMPS),
        ldr(X, r(0), RUNTIME, CPU),
        str(W, r(1), r(0), EIP),
    ]);
    emit_store_guest(asm);
    asm.emit_all(&[
        str(X, r(9), RUNTIME, COUNTDOWN),
        str(X, r(28), RUNTIME, SHADOW),
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

/// Stores the guest registers and flags to the `CpuState` at x0; uses x1
/// to x5.
fn emit_store_guest(asm: &mut Asm) {
    for (i, (a, b)) in GUEST.into_iter().enumerate() {
        // The low halves of x23 and x24 are ESP and EBP themselves.
        asm.emit(stp(W, Offset, r(a), r(b), r(0), REGS + 8 * i as i32));
    }
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
        ldr(W, r(4), r(0), EFLAGS),
        movz(W, r(5), MAPPED_FLAGS as u16, 0),
        bic(W, r(4), r(4), r(5), Lsl, 0),
        orr(W, r(4), r(4), r(3), Lsl, 0),
        str(W, r(4), r(0), EFLAGS),
    ]);
}

#[cfg(all(test, emulator))]
mod tests {
    use super::*;
    use crate::emulator::{CODE, Emulator};

    const DATA: u64 = 0x2000_0000;
    const STATE: u64 = DATA + 0x100;
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
        emu.map(DATA, 0x1000);
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
        let runtime = emu.read(DATA, size_of::<Runtime>());
        let run = Run {
            regs,
            eip: word(EIP as usize),
            eflags: word(EFLAGS as usize),
            xmm,
            exit: u32::from_le_bytes(runtime[EXIT as usize..][..4].try_into().unwrap()),
            countdown: u64::from_le_bytes(runtime[COUNTDOWN as usize..][..8].try_into().unwrap()),
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
            movz(W, r(0), reason as u16, 0),
            movz(W, r(1), eip as u16, 0),
            movk(W, r(1), (eip >> 16) as u16, 16),
        ]);
        asm.b(exit);
    }

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
            assert_eq!(run.exit, Exit::Dispatch as u32);
        }
    }

    #[test]
    fn translated_code_sees_the_section_four_register_map() {
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
            leave(asm, exit, Exit::Interpret, 0x40_1003);
        });
        assert_eq!(emu.x(r(6)), BASE + 0x0012_ff80);
        assert_eq!(emu.x(r(7)), BASE + 0xffff_fffc);
        assert_eq!(emu.x(r(8)), BASE);
        assert_eq!(emu.x(r(17)), 0x0012_ff80);
        let mut expected = cpu.regs;
        expected[0] += 1;
        expected[4] -= 4;
        assert_eq!(run.regs, expected);
        assert_eq!(run.exit, Exit::Interpret as u32);
        assert_eq!(run.countdown, (u64::MAX >> 1) - 1);
    }

    /// `cmp ecx, edx` in subtraction sense, with the canonical PF/AF sources,
    /// must leave the eflags the interpreter computes for the same compare.
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

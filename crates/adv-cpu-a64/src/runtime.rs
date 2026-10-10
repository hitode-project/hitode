//! The execution thread's runtime context and the stubs that move guest
//! state between `CpuState` and the mapped host registers (workbook §4).
//!
//! `enter` saves the host's callee-saved registers, loads the guest and
//! branches to translated code. Translated code leaves through `exit` with
//! the exit reason in w0, the guest EIP in w1 and in x2 the host address of
//! a link point the owner may patch, or 0; `exit` stores the guest back and
//! returns to `enter`'s caller. Translated code charges x9, the countdown,
//! with the guest instructions it completes. Arithmetic flags cross the boundary
//! in the canonical block-exit form of §5: NZCV in subtraction sense, x10 a
//! value whose low byte has PF's parity, x11 a value whose bit 4 is AF, and
//! x12 DF. x87 state stays in `CpuState` until M2 maps it.

use std::mem::offset_of;

use adv_cpu_core::CpuState;

use crate::asm::Asm;
use crate::encode::Extend::Uxtw;
use crate::encode::Index::{Offset, Post, Pre};
use crate::encode::Shift::{Lsl, Lsr};
use crate::encode::Size::{W, X};
use crate::encode::VSize::Q;
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
    /// The link point of the exit, or 0.
    pub link: u64,
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
            link: 0,
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
    /// instruction (§5.2).
    Fault = 2,
}

impl Exit {
    pub fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            0 => Some(Self::Interpret),
            1 => Some(Self::Dispatch),
            2 => Some(Self::Fault),
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

const fn field(offset: usize) -> u32 {
    offset as u32
}

const CPU: u32 = field(offset_of!(Runtime, cpu));
const WINDOW_BASE: u32 = field(offset_of!(Runtime, window));
const SHADOW: u32 = field(offset_of!(Runtime, shadow));
const COUNTDOWN: u32 = field(offset_of!(Runtime, countdown));
const HOST_SP: u32 = field(offset_of!(Runtime, host_sp));
const EXIT: u32 = field(offset_of!(Runtime, exit));
const LINK: u32 = field(offset_of!(Runtime, link));
const TEMPS: i32 = offset_of!(Runtime, temps) as i32;
const REGS: i32 = offset_of!(CpuState, regs) as i32;
const EIP: u32 = field(offset_of!(CpuState, eip));
const EFLAGS: u32 = field(offset_of!(CpuState, eflags));
const XMM: u32 = field(offset_of!(CpuState, xmm));

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
    let fault = asm.offset();
    asm.emit_all(&[
        movz(W, r(0), Exit::Fault as u16, 0),
        movz(W, r(1), 0, 0),
        movz(X, r(2), 0, 0),
    ]);
    asm.b(exit_label);
    Stubs {
        code: asm.finish(),
        enter,
        exit,
        fault,
    }
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
        add_imm(X, r(16), r(0), XMM, false),
    ]);
    for i in 0..4 {
        asm.emit(ldp_v(
            Q,
            Offset,
            v(16 + 2 * i),
            v(17 + 2 * i),
            r(16),
            32 * i as i32,
        ));
    }
    asm.emit(br(r(17)));
}

/// Translated code arrives with the `Exit` in w0 and the guest EIP in w1.
fn emit_exit(asm: &mut Asm) {
    asm.emit_all(&[
        str(W, r(0), RUNTIME, EXIT),
        str(X, r(2), RUNTIME, LINK),
        stp(X, Offset, r(7), r(8), RUNTIME, TEMPS),
        ldr(X, r(0), RUNTIME, CPU),
        str(W, r(1), r(0), EIP),
    ]);
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
        add_imm(X, r(16), r(0), XMM, false),
    ]);
    for i in 0..4 {
        asm.emit(stp_v(
            Q,
            Offset,
            v(16 + 2 * i),
            v(17 + 2 * i),
            r(16),
            32 * i as i32,
        ));
    }
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

#[cfg(all(test, emulator))]
mod tests {
    use super::*;
    use crate::emulator::{CODE, Emulator};

    const DATA: u64 = 0x2000_0000;
    const STATE: u64 = DATA + 0x100;
    const STACK_TOP: u64 = 0x3001_0000;
    const BASE: u64 = 7 << 32;

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

//! Random programs over the translated subset, run through translated code
//! and through the interpreter alone: registers, EIP, EFLAGS, memory and
//! the outcome must match exactly, at the stop and at budgets that end
//! inside units. Programs mix in instructions units leave to the
//! interpreter, so every exit form meets every flag state.

use adv_cpu_core::CpuState;
use adv_cpu_core::RunOutcome;
use adv_cpu_core::interpreter::Interpreter;
use adv_cpu_core::memory::Memory;
use iced_x86::BlockEncoderOptions;
use iced_x86::code_asm::*;

#[cfg(native)]
use super::Executor;
#[cfg(native)]
use crate::native::Checks;
#[cfg(native)]
use crate::translate::Features;

const BASE: u32 = 0x1_0000;
const LEN: usize = 0x4_0000;
const CODE: u32 = 0x1_0000;
const DATA: u32 = 0x3_0000;
/// One 16 KiB host page, so data is not soft on such hosts.
const DATA_LEN: usize = 0x4000;
const STACK: u32 = 0x4_8000;
const STACK_TOP: u32 = 0x4_f000;
/// A second stack inside the mapped one, for calls that switch to it.
const ALT_STACK_TOP: u32 = STACK + 0x3000;
/// Mapped but inaccessible: loads from it fault.
const HOLE: u32 = 0x2_f000;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }

    fn imm(&mut self) -> i32 {
        match self.below(5) {
            0 => self.below(16) as i32,
            1 => -(self.below(16) as i32),
            2 => self.below(5000) as i32 - 2500,
            3 => self.pick(&[
                i32::MIN,
                i32::MAX,
                -1,
                0x7f,
                0x80,
                0xff,
                0x1000,
                0x8000_0000u32 as i32,
            ]),
            _ => self.next() as i32,
        }
    }
}

const WRITABLE: [AsmRegister32; 5] = [eax, ecx, edx, edi, ebp];
const READABLE: [AsmRegister32; 8] = [eax, ecx, edx, ebx, esp, ebp, esi, edi];
const INDEXES: [AsmRegister32; 7] = [eax, ecx, edx, ebx, ebp, esi, edi];
const BYTES: [AsmRegister8; 8] = [al, cl, dl, bl, ah, ch, dh, bh];
const SET_BYTES: [AsmRegister8; 6] = [al, cl, dl, ah, ch, dh];
const WORDS: [AsmRegister16; 8] = [ax, cx, dx, bx, sp, bp, si, di];

struct Program {
    code: Vec<u8>,
    stop: u32,
    data: Vec<u8>,
    state: CpuState,
}

/// A data operand, and whether it indexes with ECX, which the caller masks
/// first.
fn memory_operand(rng: &mut Rng) -> (AsmMemoryOperand, bool) {
    match rng.below(4) {
        0 => (ebx + (rng.below(0x7fc) as i32), false),
        1 => (esi - (rng.below(0x7fc) as i32), false),
        2 => (ebx + (4 * rng.below(0x1ff)) as i32, false),
        _ => (ebx + ecx * 4 + (rng.below(0x600) as i32), true),
    }
}

fn alu(a: &mut CodeAssembler, op: usize, dst: AsmRegister32, src: AsmRegister32) {
    match op {
        0 => a.add(dst, src),
        1 => a.sub(dst, src),
        2 => a.and(dst, src),
        3 => a.or(dst, src),
        4 => a.xor(dst, src),
        5 => a.cmp(dst, src),
        _ => a.test(dst, src),
    }
    .unwrap();
}

fn alu_imm(a: &mut CodeAssembler, op: usize, dst: AsmRegister32, imm: i32) {
    match op {
        0 => a.add(dst, imm),
        1 => a.sub(dst, imm),
        2 => a.and(dst, imm),
        3 => a.or(dst, imm),
        4 => a.xor(dst, imm),
        5 => a.cmp(dst, imm),
        _ => a.test(dst, imm),
    }
    .unwrap();
}

/// One straight-line instruction, after the ECX mask an indexed memory
/// operand needs.
/// Shifts by CL, rotates through carry, exchanges, the widening multiply
/// and divide, bit scans, byte swaps and `sahf`; a divide may raise #DE.
fn integer(a: &mut CodeAssembler, rng: &mut Rng, w: AsmRegister32, r: AsmRegister32) {
    let mem = |a: &mut CodeAssembler, rng: &mut Rng| {
        let (operand, indexed) = memory_operand(rng);
        if indexed {
            a.and(ecx, 0x7f).unwrap();
        }
        operand
    };
    let other = rng.pick(&WRITABLE);
    match rng.below(10) {
        0 => {
            if rng.below(2) == 0 {
                a.mov(ecx, rng.pick(&[0u32, 1, 2, 31, 32, 33])).unwrap();
            }
            match rng.below(5) {
                0 => a.shl(w, cl),
                1 => a.shr(w, cl),
                2 => a.sar(w, cl),
                3 => a.rol(w, cl),
                _ => a.ror(w, cl),
            }
            .unwrap()
        }
        1 => {
            let count = rng.pick(&[1u32, 1, 2, 7, 31, 32, 33]);
            if rng.below(2) == 0 {
                a.rcl(w, count).unwrap()
            } else {
                a.rcr(w, count).unwrap()
            }
        }
        2 => a.xadd(w, other).unwrap(),
        3 => {
            let m = mem(a, rng);
            if rng.below(2) == 0 {
                a.lock().xadd(dword_ptr(m), other).unwrap()
            } else {
                a.xadd(dword_ptr(m), other).unwrap()
            }
        }
        4 => {
            if rng.below(2) == 0 {
                a.xchg(w, other).unwrap()
            } else {
                let m = mem(a, rng);
                a.xchg(dword_ptr(m), other).unwrap()
            }
        }
        5 => a.bswap(w).unwrap(),
        6 => {
            let signed = rng.below(2) == 0;
            if rng.below(2) == 0 {
                if signed { a.imul(r) } else { a.mul(r) }.unwrap()
            } else {
                let m = mem(a, rng);
                if signed {
                    a.imul(dword_ptr(m))
                } else {
                    a.mul(dword_ptr(m))
                }
                .unwrap()
            }
        }
        7 => {
            // Mostly a quotient that fits; otherwise #DE.
            let signed = rng.below(2) == 0;
            match rng.below(4) {
                0 => {}
                _ if signed => a.cdq().unwrap(),
                _ => a.xor(edx, edx).unwrap(),
            }
            if rng.below(2) == 0 {
                let divisor = rng.pick(&[ebx, esi, edi]);
                if signed {
                    a.idiv(divisor)
                } else {
                    a.div(divisor)
                }
                .unwrap()
            } else {
                let m = mem(a, rng);
                if signed {
                    a.idiv(dword_ptr(m))
                } else {
                    a.div(dword_ptr(m))
                }
                .unwrap()
            }
        }
        8 => {
            if rng.below(2) == 0 {
                a.mov(ah, rng.imm() as u32 & 0xff).unwrap();
            }
            a.sahf().unwrap()
        }
        _ => {
            let reverse = rng.below(2) == 0;
            if rng.below(2) == 0 {
                if reverse { a.bsr(w, r) } else { a.bsf(w, r) }.unwrap()
            } else {
                let m = mem(a, rng);
                if reverse {
                    a.bsr(w, dword_ptr(m))
                } else {
                    a.bsf(w, dword_ptr(m))
                }
                .unwrap()
            }
        }
    }
}

fn simple(a: &mut CodeAssembler, rng: &mut Rng) {
    let w = rng.pick(&WRITABLE);
    let r = rng.pick(&READABLE);
    let mem = |a: &mut CodeAssembler, rng: &mut Rng| {
        let (operand, indexed) = memory_operand(rng);
        if indexed {
            a.and(ecx, 0x7f).unwrap();
        }
        operand
    };
    match rng.below(39) {
        0 => a.mov(w, r).unwrap(),
        1 => a.mov(w, rng.imm()).unwrap(),
        2 => {
            let m = mem(a, rng);
            a.mov(w, dword_ptr(m)).unwrap()
        }
        3 => {
            let m = mem(a, rng);
            a.mov(dword_ptr(m), r).unwrap()
        }
        4 => {
            let m = mem(a, rng);
            a.mov(dword_ptr(m), rng.imm()).unwrap()
        }
        5 => a
            .lea(
                w,
                ebx + rng.pick(&INDEXES) * rng.pick(&[1, 2, 4, 8]) + rng.imm(),
            )
            .unwrap(),
        6 | 7 => alu(a, rng.below(7), w, r),
        8 | 9 => alu_imm(a, rng.below(7), w, rng.imm()),
        10 => {
            let m = mem(a, rng);
            match rng.below(7) {
                0 => a.add(w, dword_ptr(m)),
                1 => a.sub(w, dword_ptr(m)),
                2 => a.and(w, dword_ptr(m)),
                3 => a.or(w, dword_ptr(m)),
                4 => a.xor(w, dword_ptr(m)),
                5 => a.cmp(w, dword_ptr(m)),
                _ => a.test(dword_ptr(m), w),
            }
            .unwrap()
        }
        11 => {
            let m = mem(a, rng);
            match rng.below(6) {
                0 => a.add(dword_ptr(m), r),
                1 => a.sub(dword_ptr(m), r),
                2 => a.and(dword_ptr(m), r),
                3 => a.or(dword_ptr(m), r),
                4 => a.xor(dword_ptr(m), r),
                _ => a.cmp(dword_ptr(m), r),
            }
            .unwrap()
        }
        12 => {
            let m = mem(a, rng);
            let imm = rng.imm();
            match rng.below(7) {
                0 => a.add(dword_ptr(m), imm),
                1 => a.sub(dword_ptr(m), imm),
                2 => a.and(dword_ptr(m), imm),
                3 => a.or(dword_ptr(m), imm),
                4 => a.xor(dword_ptr(m), imm),
                5 => a.cmp(dword_ptr(m), imm),
                _ => a.test(dword_ptr(m), imm),
            }
            .unwrap()
        }
        13 => match rng.below(4) {
            0 => a.inc(w),
            1 => a.dec(w),
            2 => a.neg(w),
            _ => a.not(w),
        }
        .unwrap(),
        14 => match rng.below(4) {
            0 => a.movzx(w, rng.pick(&BYTES)),
            1 => a.movsx(w, rng.pick(&BYTES)),
            2 => a.movzx(w, rng.pick(&WORDS)),
            _ => a.movsx(w, rng.pick(&WORDS)),
        }
        .unwrap(),
        15 => {
            let m = mem(a, rng);
            match rng.below(4) {
                0 => a.movzx(w, byte_ptr(m)),
                1 => a.movsx(w, byte_ptr(m)),
                2 => a.movzx(w, word_ptr(m)),
                _ => a.movsx(w, word_ptr(m)),
            }
            .unwrap()
        }
        16 => a.cdq().unwrap(),
        17 => {
            let t = rng.pick(&SET_BYTES);
            match rng.below(14) {
                0 => a.seto(t),
                1 => a.setno(t),
                2 => a.setb(t),
                3 => a.setae(t),
                4 => a.sete(t),
                5 => a.setne(t),
                6 => a.setbe(t),
                7 => a.seta(t),
                8 => a.sets(t),
                9 => a.setns(t),
                10 => a.setl(t),
                11 => a.setge(t),
                12 => a.setle(t),
                _ => a.setg(t),
            }
            .unwrap()
        }
        18 => {
            if rng.below(2) == 0 {
                match rng.below(5) {
                    0 => a.cmovb(w, r),
                    1 => a.cmova(w, r),
                    2 => a.cmovl(w, r),
                    3 => a.cmovne(w, r),
                    _ => a.cmovbe(w, r),
                }
                .unwrap()
            } else {
                let m = mem(a, rng);
                match rng.below(3) {
                    0 => a.cmovae(w, dword_ptr(m)),
                    1 => a.cmovg(w, dword_ptr(m)),
                    _ => a.cmove(w, dword_ptr(m)),
                }
                .unwrap()
            }
        }
        // Left to the interpreter, between translated neighbours.
        19 => match rng.below(5) {
            0 => a.bt(w, r),
            1 => a.rcl(w, cl),
            2 => a.cmpxchg(w, r),
            3 => a.shld(w, r, cl),
            _ => a.add(al, 7),
        }
        .unwrap(),
        20 => {
            a.sub(esp, 8).unwrap();
            a.mov(dword_ptr(esp + 4), r).unwrap();
            a.add(esp, 8).unwrap();
        }
        21 => {
            // Counts are masked to 5 bits: 32 does nothing and 33 is 1.
            let count = rng.pick(&[0u32, 1, 1, 2, 7, 31, 32, 33]);
            match rng.below(5) {
                0 => a.shl(w, count),
                1 => a.shr(w, count),
                2 => a.sar(w, count),
                3 => a.rol(w, count),
                _ => a.ror(w, count),
            }
            .unwrap()
        }
        22 => match rng.below(4) {
            0 => a.imul_2(w, r).unwrap(),
            1 => {
                let m = mem(a, rng);
                a.imul_2(w, dword_ptr(m)).unwrap()
            }
            2 => a.imul_3(w, r, rng.imm()).unwrap(),
            _ => {
                let m = mem(a, rng);
                a.imul_3(w, dword_ptr(m), rng.imm()).unwrap()
            }
        },
        23 => {
            let sbb = rng.below(2) == 1;
            match rng.below(3) {
                0 if sbb => a.sbb(w, r),
                0 => a.adc(w, r),
                1 if sbb => a.sbb(w, rng.imm()),
                1 => a.adc(w, rng.imm()),
                _ => {
                    let m = mem(a, rng);
                    if sbb {
                        a.sbb(w, dword_ptr(m))
                    } else {
                        a.adc(w, dword_ptr(m))
                    }
                }
            }
            .unwrap()
        }
        // More the interpreter runs inside units: narrow and
        // read-modify-write memory forms, and REP elements.
        24 => {
            let m = mem(a, rng);
            match rng.below(7) {
                0 => a.mov(word_ptr(m), rng.pick(&WORDS)).unwrap(),
                1 => a.add(byte_ptr(m), rng.imm() & 0x7f).unwrap(),
                2 => a.cmp(word_ptr(m), rng.imm() & 0x7fff).unwrap(),
                3 => a.xadd(dword_ptr(m), w).unwrap(),
                4 => a.inc(dword_ptr(m)).unwrap(),
                5 => {
                    a.push(dword_ptr(m)).unwrap();
                    a.pop(w).unwrap();
                }
                _ => {
                    a.mov(edi, DATA as i32 + 0x200).unwrap();
                    a.and(ecx, 15).unwrap();
                    a.rep().stosb().unwrap();
                }
            }
        }
        25 => narrow(a, rng, mem),
        // FS-relative accesses; FS points into the data.
        26 => {
            let m = mem(a, rng).fs();
            match rng.below(5) {
                0 => a.mov(w, dword_ptr(m)).unwrap(),
                1 => a.mov(dword_ptr(m), r).unwrap(),
                2 => a.add(dword_ptr(m), r).unwrap(),
                3 => a.cmp(byte_ptr(m), rng.imm() & 0x7f).unwrap(),
                _ => a.dec(dword_ptr(m)).unwrap(),
            }
        }
        27..=29 => simd(a, rng, mem),
        30 => strings(a, rng),
        32 => integer(a, rng, w, r),
        33 | 34 => floating(a, rng, mem),
        35..=38 => x87(a, rng, mem),
        _ => a.nop().unwrap(),
    }
}

/// A string instruction of any width, alone or repeated, in either
/// direction, over overlapping operands; one running down from the start
/// of the data faults in the hole below it part way.
fn strings(a: &mut CodeAssembler, rng: &mut Rng) {
    let near = |rng: &mut Rng| DATA as i32 + rng.below(0x1000) as i32;
    let esi_at = near(rng);
    let edi_at = match rng.below(4) {
        0 => esi_at,
        1 => esi_at + rng.below(9) as i32 - 4,
        2 => DATA as i32 + rng.below(12) as i32,
        _ => near(rng),
    };
    a.mov(esi, esi_at).unwrap();
    a.mov(edi, edi_at).unwrap();
    let compare = rng.below(2) == 0;
    // A compare may count from -1, as the idioms that scan for a value do.
    let count = if compare && rng.below(4) == 0 {
        -1
    } else {
        rng.pick(&[0, 1, 2, 3, 7, 16, 33])
    };
    a.mov(ecx, count).unwrap();
    if rng.below(3) == 0 {
        a.std().unwrap();
    } else {
        a.cld().unwrap();
    }
    let prefix = rng.below(4);
    let a = match prefix {
        0 => a,
        1 => a.rep(),
        2 => a.repe(),
        _ => a.repne(),
    };
    let width = rng.below(3);
    match (rng.below(3), compare, width) {
        (_, true, 0) if prefix % 2 == 0 => a.cmpsb(),
        (_, true, 1) if prefix % 2 == 0 => a.cmpsw(),
        (_, true, _) if prefix % 2 == 0 => a.cmpsd(),
        (_, true, 0) => a.scasb(),
        (_, true, 1) => a.scasw(),
        (_, true, _) => a.scasd(),
        (0, false, 0) => a.movsb(),
        (0, false, 1) => a.movsw(),
        (0, false, _) => a.movsd(),
        (1, false, 0) => a.stosb(),
        (1, false, 1) => a.stosw(),
        (1, false, _) => a.stosd(),
        (_, false, 0) => a.lodsb(),
        (_, false, 1) => a.lodsw(),
        _ => a.lodsd(),
    }
    .unwrap();
}

const MMX: [AsmRegisterMm; 8] = [mm0, mm1, mm2, mm3, mm4, mm5, mm6, mm7];
const XMM: [AsmRegisterXmm; 8] = [xmm0, xmm1, xmm2, xmm3, xmm4, xmm5, xmm6, xmm7];

/// An MMX or SSE integer, logic, shuffle or move instruction on registers,
/// memory and immediates; 16-byte operands that must be aligned are.
fn simd(
    a: &mut CodeAssembler,
    rng: &mut Rng,
    mem: impl Fn(&mut CodeAssembler, &mut Rng) -> AsmMemoryOperand,
) {
    let (m, m2, x, x2) = (
        rng.pick(&MMX),
        rng.pick(&MMX),
        rng.pick(&XMM),
        rng.pick(&XMM),
    );
    let aligned = |rng: &mut Rng| ebx + 16 * rng.below(0x7f) as i32;
    let form = rng.below(4);
    // Each form of a two-operand operation: MMX, then XMM, from a register
    // or memory.
    macro_rules! binary {
        ($op:ident) => {
            match form {
                0 => a.$op(m, m2),
                1 => {
                    let p = mem(a, rng);
                    a.$op(m, qword_ptr(p))
                }
                2 => a.$op(x, x2),
                _ => {
                    let p = aligned(rng);
                    a.$op(x, xmmword_ptr(p))
                }
            }
            .unwrap()
        };
    }
    macro_rules! sse {
        ($op:ident) => {
            match form {
                0 | 2 => a.$op(x, x2),
                _ => {
                    let p = aligned(rng);
                    a.$op(x, xmmword_ptr(p))
                }
            }
            .unwrap()
        };
    }
    macro_rules! unpack {
        ($op:ident) => {
            match form {
                0 => a.$op(m, m2),
                1 => {
                    let p = mem(a, rng);
                    a.$op(m, dword_ptr(p))
                }
                2 => a.$op(x, x2),
                _ => {
                    let p = aligned(rng);
                    a.$op(x, xmmword_ptr(p))
                }
            }
            .unwrap()
        };
    }
    macro_rules! shift {
        ($op:ident) => {{
            let count = rng.pick(&[0, 1, 3, 7, 8, 15, 16, 17, 31, 32, 33, 63, 64, 200]);
            match rng.below(5) {
                0 => a.$op(m, count),
                1 => a.$op(x, count),
                2 => {
                    a.mov(eax, count).unwrap();
                    a.movd(m2, eax).unwrap();
                    a.$op(m, m2)
                }
                3 => {
                    a.mov(eax, count).unwrap();
                    a.movd(x2, eax).unwrap();
                    a.$op(x, x2)
                }
                _ => {
                    let p = mem(a, rng);
                    a.$op(m, qword_ptr(p))
                }
            }
            .unwrap()
        }};
    }
    match rng.below(46) {
        0 => match form {
            0 => a.movd(m, rng.pick(&READABLE)),
            1 => a.movd(rng.pick(&WRITABLE), m),
            2 => {
                let p = mem(a, rng);
                a.movd(m, dword_ptr(p))
            }
            _ => {
                let p = mem(a, rng);
                a.movd(dword_ptr(p), m)
            }
        }
        .unwrap(),
        1 => match form {
            0 => a.movq(m, m2),
            1 => {
                let p = mem(a, rng);
                a.movq(m, qword_ptr(p))
            }
            2 => {
                let p = mem(a, rng);
                a.movq(qword_ptr(p), m)
            }
            _ => {
                let p = mem(a, rng);
                a.movntq(qword_ptr(p), m)
            }
        }
        .unwrap(),
        2 => match form {
            0 => a.movd(x, rng.pick(&READABLE)),
            1 => a.movd(rng.pick(&WRITABLE), x),
            2 => {
                let p = mem(a, rng);
                a.movd(x, dword_ptr(p))
            }
            _ => {
                let p = mem(a, rng);
                a.movd(dword_ptr(p), x)
            }
        }
        .unwrap(),
        3 => match form {
            0 => a.movq(x, x2),
            1 => {
                let p = mem(a, rng);
                a.movq(x, qword_ptr(p))
            }
            _ => {
                let p = mem(a, rng);
                a.movq(qword_ptr(p), x)
            }
        }
        .unwrap(),
        4 => match form {
            0 => a.movss(x, x2),
            1 => {
                let p = mem(a, rng);
                a.movss(x, dword_ptr(p))
            }
            _ => {
                let p = mem(a, rng);
                a.movss(dword_ptr(p), x)
            }
        }
        .unwrap(),
        5 => {
            let p = mem(a, rng);
            match form {
                0 => a.movaps(x, x2),
                1 => a.movups(x, xmmword_ptr(p)),
                2 => a.movdqu(xmmword_ptr(p), x),
                _ => {
                    let p = aligned(rng);
                    match rng.below(4) {
                        0 => a.movaps(x, xmmword_ptr(p)),
                        1 => a.movdqa(xmmword_ptr(p), x),
                        2 => a.movntps(xmmword_ptr(p), x),
                        _ => a.movapd(x, xmmword_ptr(p)),
                    }
                }
            }
            .unwrap()
        }
        6 => binary!(paddb),
        7 => binary!(paddw),
        8 => binary!(paddd),
        9 => binary!(paddq),
        10 => binary!(psubb),
        11 => binary!(psubw),
        12 => binary!(psubd),
        13 => binary!(psubq),
        14 => binary!(paddsb),
        15 => binary!(paddusw),
        16 => binary!(psubsw),
        17 => binary!(psubusb),
        18 => binary!(pcmpeqb),
        19 => binary!(pcmpeqd),
        20 => binary!(pcmpgtw),
        21 => binary!(pcmpgtb),
        22 => binary!(pmullw),
        23 => binary!(pmulhw),
        24 => binary!(pmulhuw),
        25 => binary!(pmaddwd),
        26 => binary!(pand),
        27 => binary!(pandn),
        28 => binary!(por),
        29 => binary!(pxor),
        30 => binary!(packsswb),
        31 => binary!(packssdw),
        32 => binary!(packuswb),
        33 => binary!(pavgb),
        34 => binary!(pminsw),
        35 => binary!(pmaxub),
        36 => unpack!(punpcklbw),
        37 => unpack!(punpcklwd),
        38 => binary!(punpckhbw),
        39 => binary!(punpckhdq),
        40 => match rng.below(3) {
            0 => shift!(psllw),
            1 => shift!(psrld),
            _ => shift!(psraw),
        },
        41 => match rng.below(3) {
            0 => shift!(psllq),
            1 => shift!(psrlq),
            _ => shift!(psrad),
        },
        42 => {
            let imm = rng.below(256) as i32;
            match form {
                0 => a.pshufw(m, m2, imm),
                1 => {
                    let p = mem(a, rng);
                    a.pshufw(m, qword_ptr(p), imm)
                }
                2 => a.pshufd(x, x2, imm),
                _ => a.shufps(x, x2, imm),
            }
            .unwrap()
        }
        43 => match rng.below(6) {
            0 => sse!(xorps),
            1 => sse!(andps),
            2 => sse!(andnps),
            3 => sse!(orps),
            4 => sse!(unpcklps),
            _ => sse!(punpckhqdq),
        },
        44 => {
            let p = aligned(rng);
            a.shufps(x, xmmword_ptr(p), rng.below(256) as i32).unwrap()
        }
        _ => a.emms().unwrap(),
    }
}

/// Single-precision values at the edges: zeros, denormals, the smallest
/// and largest normals, infinities, quiet and signalling NaNs, and values
/// whose products overflow or underflow.
const SPECIAL: [u32; 18] = [
    0,
    0x8000_0000,
    1,
    0x807f_ffff,
    0x0080_0000,
    0x0080_0001,
    0x7f7f_ffff,
    0x7f80_0000,
    0xff80_0000,
    0x7fc0_0001,
    0xffc1_2345,
    0x7f80_0001,
    0xffa0_0000,
    0x3f80_0000,
    0xbfc0_0000,
    0x5f00_0000,
    0x1f00_0000,
    0x4b00_0001,
];

/// SSE single-precision arithmetic, compares and conversions over
/// special values, with MXCSR now and then in another rounding mode, with
/// DAZ or FTZ, or with an exception unmasked.
fn floating(
    a: &mut CodeAssembler,
    rng: &mut Rng,
    mem: impl Fn(&mut CodeAssembler, &mut Rng) -> AsmMemoryOperand,
) {
    let (x, x2, m) = (rng.pick(&XMM), rng.pick(&XMM), rng.pick(&MMX));
    let aligned = |rng: &mut Rng| ebx + 16 * rng.below(0x7f) as i32;
    macro_rules! packed {
        ($op:ident) => {
            if rng.below(2) == 0 {
                a.$op(x, x2)
            } else {
                let p = aligned(rng);
                a.$op(x, xmmword_ptr(p))
            }
            .unwrap()
        };
    }
    macro_rules! scalar {
        ($op:ident) => {
            if rng.below(2) == 0 {
                a.$op(x, x2)
            } else {
                let p = mem(a, rng);
                a.$op(x, dword_ptr(p))
            }
            .unwrap()
        };
    }
    match rng.below(24) {
        0..=3 => {
            a.mov(eax, rng.pick(&SPECIAL)).unwrap();
            a.movd(x, eax).unwrap();
            if rng.below(2) == 0 {
                a.pshufd(x, x, rng.below(256) as i32).unwrap();
            } else {
                a.shufps(x, x2, rng.below(256) as i32).unwrap();
            }
        }
        4 => {
            let mxcsr = rng.pick(&[
                0x1f80u32, 0x1fbf, 0x3f80, 0x5f80, 0x7f80, 0x9f80, 0x1fc0, 0x1b80, 0x1d80, 0x0f80,
            ]);
            a.mov(dword_ptr(ebx + 0x7f0), mxcsr).unwrap();
            a.ldmxcsr(dword_ptr(ebx + 0x7f0)).unwrap();
        }
        5 => a.stmxcsr(dword_ptr(ebx + 0x7f4)).unwrap(),
        6 => packed!(addps),
        7 => packed!(subps),
        8 | 9 => packed!(mulps),
        10 => packed!(divps),
        11 => packed!(minps),
        12 => packed!(maxps),
        13 => scalar!(addss),
        14 => scalar!(subss),
        15 => scalar!(mulss),
        16 => scalar!(divss),
        17 => {
            if rng.below(2) == 0 {
                scalar!(minss)
            } else {
                scalar!(maxss)
            }
        }
        18 | 19 => {
            let truncate = rng.below(2) == 0;
            if rng.below(2) == 0 {
                if truncate {
                    a.cvttps2pi(m, x)
                } else {
                    a.cvtps2pi(m, x)
                }
            } else {
                let p = mem(a, rng);
                if truncate {
                    a.cvttps2pi(m, qword_ptr(p))
                } else {
                    a.cvtps2pi(m, qword_ptr(p))
                }
            }
            .unwrap()
        }
        _ => {
            let p = mem(a, rng);
            match rng.below(4) {
                0 => a.movlps(x, qword_ptr(p)),
                1 => a.movhps(x, qword_ptr(p)),
                2 => a.movlps(qword_ptr(p), x),
                _ => a.movhps(qword_ptr(p), x),
            }
            .unwrap()
        }
    }
}

/// Doubles at the edges of what x87 units run natively: zeros, small
/// integers and halves, inexact fractions, the binary64 extremes and
/// denormals, infinities, NaNs, values beyond 2^±400 and the integer
/// conversion limits.
const DOUBLES: [u64; 22] = [
    0,
    1 << 63,
    0x3ff0_0000_0000_0000,
    0xbff8_0000_0000_0000,
    0x3fe0_0000_0000_0000,
    0x4004_0000_0000_0000,
    0x4000_0000_0000_0001,
    0x3fd5_5555_5555_5555,
    0xc059_0ccc_cccc_cccd,
    0x0010_0000_0000_0000,
    0x000f_ffff_ffff_ffff,
    0x7fef_ffff_ffff_ffff,
    0x7ff0_0000_0000_0000,
    0x7ff8_0000_0000_0000,
    0xfff4_0000_0000_0001,
    0x5fe0_0000_0000_0001,
    0x2000_0000_0000_0000,
    0x4330_0000_0000_0001,
    0x41e0_0000_0000_0000,
    0xc1e0_0000_0000_0000,
    0x43e0_0000_0000_0000,
    0x47ef_ffff_e000_0000,
];

/// Control words: precision 24, 53 and 64, every rounding mode, the
/// MSVC defaults and `ftol`'s chop, and some with exceptions unmasked.
const CONTROLS: [u16; 12] = [
    0x037f, 0x027f, 0x133f, 0x1f3f, 0x007f, 0x0f7f, 0x077f, 0x0b7f, 0x027e, 0x035f, 0x0373, 0x037b,
];

const STS: [AsmRegisterSt; 8] = [st0, st1, st2, st3, st4, st5, st6, st7];

/// A short run of x87 instructions over the stack the program left,
/// sometimes after `fninit`: loads of special and random values, stores,
/// arithmetic in every form, compares, exchanges and control words.
fn x87(
    a: &mut CodeAssembler,
    rng: &mut Rng,
    mem: impl Fn(&mut CodeAssembler, &mut Rng) -> AsmMemoryOperand,
) {
    // Scratch operands at the top of the data.
    let slot = |rng: &mut Rng| ebx + 0x7c0 + 8 * rng.below(6) as i32;
    if rng.below(4) == 0 {
        a.fninit().unwrap();
    }
    for _ in 0..1 + rng.below(4) {
        let i = STS[rng.below(8)];
        let near = STS[rng.below(3)];
        match rng.below(30) {
            0 | 1 => {
                let at = slot(rng);
                a.mov(dword_ptr(at), rng.pick(&SPECIAL)).unwrap();
                a.fld(dword_ptr(at)).unwrap();
            }
            2 | 3 => {
                let at = slot(rng);
                let value = rng.pick(&DOUBLES);
                a.mov(dword_ptr(at), value as u32).unwrap();
                a.mov(dword_ptr(at + 4), (value >> 32) as u32).unwrap();
                a.fld(qword_ptr(at)).unwrap();
            }
            4 => {
                let at = slot(rng);
                a.mov(dword_ptr(at), rng.imm()).unwrap();
                if rng.below(2) == 0 {
                    a.fild(dword_ptr(at)).unwrap();
                } else {
                    a.fild(word_ptr(at)).unwrap();
                }
            }
            5 => {
                let m = mem(a, rng);
                match rng.below(3) {
                    0 => a.fld(dword_ptr(m)),
                    1 => a.fld(qword_ptr(m)),
                    _ => a.fild(dword_ptr(m)),
                }
                .unwrap();
            }
            6 => match rng.below(3) {
                0 => a.fld1().unwrap(),
                1 => a.fldz().unwrap(),
                _ => a.fld(i).unwrap(),
            },
            7 | 8 => {
                let m = mem(a, rng);
                match rng.below(7) {
                    0 => a.fst(dword_ptr(m)),
                    1 => a.fstp(dword_ptr(m)),
                    2 => a.fst(qword_ptr(m)),
                    3 => a.fstp(qword_ptr(m)),
                    4 => a.fist(dword_ptr(m)),
                    5 => a.fistp(dword_ptr(m)),
                    _ => a.fistp(qword_ptr(m)),
                }
                .unwrap();
            }
            9 => {
                if rng.below(2) == 0 {
                    a.fst(i).unwrap();
                } else {
                    a.fstp(i).unwrap();
                }
            }
            10..=17 => {
                macro_rules! arith {
                    ($op:ident, $op2:ident, $popped:ident, $int:ident) => {
                        match rng.below(6) {
                            0 => a.$op2(st0, i),
                            1 => a.$op2(i, st0),
                            2 => a.$popped(near, st0),
                            3 => {
                                let at = slot(rng);
                                a.mov(dword_ptr(at), rng.pick(&SPECIAL)).unwrap();
                                a.$op(dword_ptr(at))
                            }
                            4 => {
                                let at = slot(rng);
                                let value = rng.pick(&DOUBLES);
                                a.mov(dword_ptr(at), value as u32).unwrap();
                                a.mov(dword_ptr(at + 4), (value >> 32) as u32).unwrap();
                                a.$op(qword_ptr(at))
                            }
                            _ => {
                                let at = slot(rng);
                                a.mov(dword_ptr(at), rng.imm() >> rng.below(31)).unwrap();
                                if rng.below(2) == 0 {
                                    a.$int(dword_ptr(at))
                                } else {
                                    a.$int(word_ptr(at))
                                }
                            }
                        }
                        .unwrap()
                    };
                }
                match rng.below(6) {
                    0 => arith!(fadd, fadd_2, faddp, fiadd),
                    1 => arith!(fsub, fsub_2, fsubp, fisub),
                    2 => arith!(fsubr, fsubr_2, fsubrp, fisubr),
                    3 => arith!(fmul, fmul_2, fmulp, fimul),
                    4 => arith!(fdiv, fdiv_2, fdivp, fidiv),
                    _ => arith!(fdivr, fdivr_2, fdivrp, fidivr),
                }
            }
            18 | 19 => match rng.below(8) {
                0 => a.fcom_2(st0, i).unwrap(),
                1 => a.fcomp_2(st0, i).unwrap(),
                2 => a.fcompp().unwrap(),
                3 => a.fucom(st0, i).unwrap(),
                4 => a.fucomp(st0, i).unwrap(),
                5 => a.fucompp().unwrap(),
                6 => {
                    let at = slot(rng);
                    a.mov(dword_ptr(at), rng.pick(&SPECIAL)).unwrap();
                    a.fcomp(dword_ptr(at)).unwrap();
                }
                _ => {
                    let at = slot(rng);
                    let value = rng.pick(&DOUBLES);
                    a.mov(dword_ptr(at), value as u32).unwrap();
                    a.mov(dword_ptr(at + 4), (value >> 32) as u32).unwrap();
                    a.fcom(qword_ptr(at)).unwrap();
                }
            },
            20 => a.fxch(st0, i).unwrap(),
            21 => {
                if rng.below(2) == 0 {
                    a.fchs().unwrap();
                } else {
                    a.fabs().unwrap();
                }
            }
            22 | 23 => {
                a.mov(word_ptr(ebx + 0x7f8), rng.pick(&CONTROLS) as u32)
                    .unwrap();
                a.fldcw(word_ptr(ebx + 0x7f8)).unwrap();
            }
            24 => {
                let m = mem(a, rng);
                if rng.below(2) == 0 {
                    a.fnstcw(word_ptr(m)).unwrap();
                } else {
                    a.fnstsw(word_ptr(m)).unwrap();
                }
            }
            25 => a.fnstsw(ax).unwrap(),
            26 => a.wait().unwrap(),
            27 => a.ffree(i).unwrap(),
            _ => {
                // Two pops through the stack, as compiled expressions end.
                a.faddp(st1, st0).unwrap();
                a.fstp(dword_ptr(slot(rng))).unwrap();
            }
        }
    }
}

/// An 8- or 16-bit `mov` or ALU operation on registers, memory and
/// immediates, high byte registers and SP and BP included.
fn narrow(
    a: &mut CodeAssembler,
    rng: &mut Rng,
    mem: impl Fn(&mut CodeAssembler, &mut Rng) -> AsmMemoryOperand,
) {
    let (op, form) = (rng.below(8), rng.below(5));
    let imm = rng.imm();
    macro_rules! apply {
        ($dst:expr, $src:expr) => {
            match op {
                7 => a.test($dst, $src).unwrap(),
                _ => load!($dst, $src),
            }
        };
    }
    // `test` has no register, memory form.
    macro_rules! load {
        ($dst:expr, $src:expr) => {
            match op {
                0 => a.mov($dst, $src),
                1 => a.add($dst, $src),
                2 => a.sub($dst, $src),
                3 => a.and($dst, $src),
                4 => a.or($dst, $src),
                5 => a.xor($dst, $src),
                _ => a.cmp($dst, $src),
            }
            .unwrap()
        };
    }
    if rng.below(2) == 0 {
        let (dst, src) = (rng.pick(&SET_BYTES), rng.pick(&BYTES));
        match form {
            0 => apply!(dst, src),
            1 => apply!(dst, imm & 0xff),
            2 => {
                let m = mem(a, rng);
                load!(dst, byte_ptr(m))
            }
            3 => {
                let m = mem(a, rng);
                apply!(byte_ptr(m), src)
            }
            _ => {
                let m = mem(a, rng);
                apply!(byte_ptr(m), imm & 0x7f)
            }
        }
    } else {
        // SP stays put: its low half is written with its own value.
        let dst = rng.pick(&[ax, cx, dx, di, bp]);
        let src = rng.pick(&WORDS);
        match form {
            0 => apply!(dst, src),
            1 => apply!(dst, imm & 0x7fff),
            2 => {
                let m = mem(a, rng);
                load!(dst, word_ptr(m))
            }
            3 => {
                let m = mem(a, rng);
                apply!(word_ptr(m), src)
            }
            _ => {
                let m = mem(a, rng);
                apply!(word_ptr(m), imm & 0x7fff)
            }
        }
    }
}

fn condition(a: &mut CodeAssembler, cc: usize, label: CodeLabel) {
    match cc {
        0 => a.jo(label),
        1 => a.jno(label),
        2 => a.jb(label),
        3 => a.jae(label),
        4 => a.je(label),
        5 => a.jne(label),
        6 => a.jbe(label),
        7 => a.ja(label),
        8 => a.js(label),
        9 => a.jns(label),
        10 => a.jl(label),
        11 => a.jge(label),
        12 => a.jle(label),
        13 => a.jg(label),
        14 => a.jp(label),
        15 => a.jnp(label),
        16 => a.jecxz(label),
        17 => a.loop_(label),
        18 => a.loope(label),
        19 => a.loopne(label),
        _ => {
            // `jcxz` over a 2-byte `xor`, by hand: the assembler only has
            // its 16-bit form.
            a.db(&[0x67, 0xe3, 2]).unwrap();
            a.xor(edx, eax)
        }
    }
    .unwrap();
}

/// A function emitted after the stop.
enum Function {
    /// An ALU op on EAX, then `ret` or `ret 4` for a pushed argument; the
    /// address request for it, if any.
    Leaf(CodeLabel, bool, usize, i32, Option<usize>),
    /// A few instructions around a call to a leaf.
    Middle(CodeLabel, CodeLabel),
    /// Drops its return address and jumps back to the label.
    Escape(CodeLabel, CodeLabel),
}

/// Code addresses instructions need as immediates.
struct Addresses<'a> {
    /// From a first build; it uses placeholders.
    known: &'a [u32],
    /// The instruction index each request's label is set at.
    marks: Vec<Option<usize>>,
}

impl Addresses<'_> {
    /// A new request, and the address if the first build found it.
    fn request(&mut self) -> (usize, i32) {
        self.marks.push(None);
        let n = self.marks.len() - 1;
        (n, self.known.get(n).copied().unwrap_or(0x8000_0000) as i32)
    }

    /// Request `n` is at the next instruction.
    fn mark(&mut self, n: usize, a: &CodeAssembler) {
        self.marks[n] = Some(a.instructions().len());
    }
}

/// Builds the program of `seed`. Code addresses an instruction needs as an
/// immediate come from `addresses`, which a first build with none fills:
/// their encodings have fixed lengths, so both builds lay out the same.
fn build(seed: u64, addresses: &[u32]) -> (Program, Vec<u32>) {
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let mut a = CodeAssembler::new(32).unwrap();
    let mut end = a.create_label();
    let mut functions = Vec::new();
    let mut wanted = Addresses {
        known: addresses,
        marks: Vec::new(),
    };
    let leaf = |rng: &mut Rng, a: &mut CodeAssembler, functions: &mut Vec<Function>, argument| {
        let label = a.create_label();
        functions.push(Function::Leaf(
            label,
            argument,
            rng.below(7),
            rng.imm(),
            None,
        ));
        label
    };
    // A leaf whose address an instruction takes.
    let addressed = |rng: &mut Rng,
                     a: &mut CodeAssembler,
                     functions: &mut Vec<Function>,
                     wanted: &mut Addresses| {
        let label = a.create_label();
        let (n, address) = wanted.request();
        functions.push(Function::Leaf(
            label,
            false,
            rng.below(7),
            rng.imm(),
            Some(n),
        ));
        (label, address)
    };
    let mut recursive = None;
    let mut pushed = 0;
    for _ in 0..48 {
        match rng.below(20) {
            0 => {
                let mut skip = a.create_label();
                condition(&mut a, rng.below(21), skip);
                for _ in 0..1 + rng.below(3) {
                    simple(&mut a, &mut rng);
                }
                a.set_label(&mut skip).unwrap();
                // One label per instruction: the next jcc may target the next one.
                a.nop().unwrap();
            }
            1 if pushed < 8 => {
                match rng.below(3) {
                    0 => a.push(rng.pick(&READABLE)).unwrap(),
                    1 => a.push(rng.imm()).unwrap(),
                    _ => a.push(dword_ptr(ebx + (rng.below(0x7fc) as i32))).unwrap(),
                }
                pushed += 1;
            }
            2 if pushed > 0 => {
                a.pop(rng.pick(&WRITABLE)).unwrap();
                pushed -= 1;
            }
            3 => {
                let argument = rng.below(2) == 0;
                if argument {
                    a.push(rng.imm()).unwrap();
                }
                let label = leaf(&mut rng, &mut a, &mut functions, argument);
                a.call(label).unwrap();
            }
            5 if rng.below(3) == 0 => {
                // A counted loop: units that link to themselves and to each
                // other, cut by the budget anywhere inside.
                let mut top = a.create_label();
                a.push(2 + rng.below(30) as i32).unwrap();
                a.set_label(&mut top).unwrap();
                a.nop().unwrap();
                for _ in 0..1 + rng.below(4) {
                    simple(&mut a, &mut rng);
                }
                if rng.below(2) == 0 {
                    // A forward branch inside the loop body.
                    let mut skip = a.create_label();
                    condition(&mut a, rng.below(18), skip);
                    simple(&mut a, &mut rng);
                    a.set_label(&mut skip).unwrap();
                    a.nop().unwrap();
                    simple(&mut a, &mut rng);
                }
                a.sub(dword_ptr(esp), 1).unwrap();
                a.jne(top).unwrap();
                a.add(esp, 4).unwrap();
            }
            5 if rng.below(3) == 1 => {
                // A loop counted in ECX, which its body leaves alone.
                let mut top = a.create_label();
                a.mov(ecx, 1 + rng.below(24) as i32).unwrap();
                a.set_label(&mut top).unwrap();
                a.add(eax, rng.imm()).unwrap();
                if rng.below(2) == 0 {
                    let mut skip = a.create_label();
                    condition(&mut a, rng.below(16), skip);
                    a.xor(edx, eax).unwrap();
                    a.set_label(&mut skip).unwrap();
                    a.nop().unwrap();
                }
                match rng.below(3) {
                    0 => a.loop_(top),
                    1 => a.loope(top),
                    _ => a.loopne(top),
                }
                .unwrap();
            }
            4 if rng.below(40) == 0 => {
                // A fault in the middle of a unit, with pending flags.
                a.mov(eax, dword_ptr(ebx - (DATA - HOLE) as i32)).unwrap();
            }
            6 => {
                let inner = leaf(&mut rng, &mut a, &mut functions, false);
                let label = a.create_label();
                functions.push(Function::Middle(label, inner));
                a.call(label).unwrap();
            }
            7 => {
                // Recursion deeper than units inline.
                let label = *recursive.get_or_insert_with(|| a.create_label());
                a.push(rng.below(13) as i32).unwrap();
                a.call(label).unwrap();
                a.add(esp, 4).unwrap();
            }
            8 => {
                let (_, address) = addressed(&mut rng, &mut a, &mut functions, &mut wanted);
                let target = rng.pick(&WRITABLE);
                a.mov(target, address).unwrap();
                a.call(target).unwrap();
            }
            9 => {
                let (_, address) = addressed(&mut rng, &mut a, &mut functions, &mut wanted);
                let slot = ebx + (4 * rng.below(0x1ff)) as i32;
                a.mov(dword_ptr(slot), address).unwrap();
                a.call(dword_ptr(slot)).unwrap();
            }
            10 => {
                let (n, address) = wanted.request();
                if rng.below(2) == 0 {
                    let target = rng.pick(&WRITABLE);
                    a.mov(target, address).unwrap();
                    a.jmp(target).unwrap();
                } else {
                    // A return that no call matches.
                    a.push(address).unwrap();
                    a.ret().unwrap();
                }
                a.int3().unwrap();
                wanted.mark(n, &a);
                a.nop().unwrap();
            }
            11 => {
                // `call $+5; pop`: the address of the code.
                let mut next = a.create_label();
                a.call(next).unwrap();
                a.set_label(&mut next).unwrap();
                a.pop(rng.pick(&WRITABLE)).unwrap();
            }
            12 => {
                // Calls on another stack, which the shadow stack does not
                // know about.
                let slot = ebx + (4 * rng.below(0x1ff)) as i32;
                let argument = rng.below(2) == 0;
                let label = leaf(&mut rng, &mut a, &mut functions, argument);
                a.mov(dword_ptr(slot), esp).unwrap();
                a.mov(esp, ALT_STACK_TOP as i32).unwrap();
                if argument {
                    a.push(rng.imm()).unwrap();
                }
                a.call(label).unwrap();
                a.mov(esp, dword_ptr(slot)).unwrap();
            }
            13 => {
                let mut back = a.create_label();
                let label = a.create_label();
                functions.push(Function::Escape(label, back));
                a.call(label).unwrap();
                a.set_label(&mut back).unwrap();
                a.nop().unwrap();
            }
            _ => simple(&mut a, &mut rng),
        }
    }
    for _ in 0..pushed {
        a.pop(rng.pick(&WRITABLE)).unwrap();
    }
    a.set_label(&mut end).unwrap();
    a.nop().unwrap();
    for function in functions {
        match function {
            Function::Leaf(mut label, argument, op, imm, request) => {
                if let Some(n) = request {
                    wanted.mark(n, &a);
                }
                a.set_label(&mut label).unwrap();
                alu_imm(&mut a, op, eax, imm);
                if argument {
                    a.ret_1(4).unwrap();
                } else {
                    a.ret().unwrap();
                }
            }
            Function::Middle(mut label, inner) => {
                a.set_label(&mut label).unwrap();
                simple(&mut a, &mut rng);
                a.call(inner).unwrap();
                simple(&mut a, &mut rng);
                a.ret().unwrap();
            }
            Function::Escape(mut label, back) => {
                a.set_label(&mut label).unwrap();
                simple(&mut a, &mut rng);
                a.add(esp, 4).unwrap();
                a.jmp(back).unwrap();
            }
        }
    }
    if let Some(mut label) = recursive {
        // rec(n): n > 0 calls rec(n - 1) around a random instruction.
        let mut done = a.create_label();
        a.set_label(&mut label).unwrap();
        a.cmp(dword_ptr(esp + 4), 0).unwrap();
        a.je(done).unwrap();
        a.push(dword_ptr(esp + 4)).unwrap();
        a.sub(dword_ptr(esp), 1).unwrap();
        a.call(label).unwrap();
        a.add(esp, 4).unwrap();
        simple(&mut a, &mut rng);
        a.set_label(&mut done).unwrap();
        a.ret().unwrap();
    }
    let result = a
        .assemble_options(
            u64::from(CODE),
            BlockEncoderOptions::RETURN_NEW_INSTRUCTION_OFFSETS,
        )
        .unwrap();
    let stop = result.label_ip(&end).unwrap() as u32;
    let offsets = &result.inner.new_instruction_offsets;
    let found = wanted
        .marks
        .iter()
        .map(|mark| CODE + offsets[mark.expect("every request is placed")])
        .collect();
    let code = result.inner.code_buffer;
    let data = (0..DATA_LEN).map(|_| rng.next() as u8).collect();
    let mut state = CpuState {
        eip: CODE,
        eflags: 0x202 | (rng.next() as u32 & 0x8d5),
        fs_base: 0x800,
        ..CpuState::default()
    };
    for n in 0..8 {
        state.xmm[n] = (rng.next() as u128 | (rng.next() as u128) << 64).to_le_bytes();
        let sign_exponent = rng.next() as u16;
        state.x87.regs[n] = adv_cpu_core::X87Register::from_parts(rng.next(), sign_exponent);
    }
    state.x87.tag = rng.next() as u16;
    // Any TOP, and no exception pending.
    state.x87.status = rng.next() as u16 & 0x3800;
    state.refresh_mmx();
    for (index, value) in state.regs.iter_mut().enumerate() {
        *value = match index {
            3 => DATA,
            4 => STACK_TOP,
            6 => DATA + 0x800,
            _ => rng.next() as u32,
        };
    }
    let program = Program {
        code,
        stop,
        data,
        state,
    };
    (program, found)
}

fn program(seed: u64) -> Program {
    let (first, addresses) = build(seed, &[]);
    let (program, again) = build(seed, &addresses);
    assert_eq!(
        addresses, again,
        "seed {seed}: both builds lay out the same"
    );
    assert_eq!(first.code.len(), program.code.len());
    program
}

fn load(memory: &mut Memory, program: &Program) {
    memory.protect(CODE, 0x1_0000, 7).unwrap();
    memory.write(CODE, &program.code).unwrap();
    memory.protect(CODE, 0x1_0000, 5).unwrap();
    memory.protect(DATA, DATA_LEN, 3).unwrap();
    memory.write(DATA, &program.data).unwrap();
    memory
        .protect(STACK, (STACK_TOP + 0x1000 - STACK) as usize, 3)
        .unwrap();
}

/// What the interpreter alone does with `budget` instructions.
fn reference(program: &Program, budget: u64) -> (RunOutcome, CpuState, Vec<u8>) {
    let mut memory = Memory::new(BASE, LEN).unwrap();
    load(&mut memory, program);
    let mut state = program.state.clone();
    let mut interpreter = Interpreter::new();
    let mut left = budget;
    let outcome = loop {
        if state.eip == program.stop {
            break RunOutcome::ReachedStop;
        }
        if left == 0 {
            break RunOutcome::Budget;
        }
        let (execution, _) = interpreter.run_block(
            &mut state,
            &mut memory,
            left.min(256) as u32,
            program.stop,
            None,
        );
        left -= u64::from(execution.units);
        match execution.step {
            adv_cpu_core::interpreter::Step::Executed => {}
            adv_cpu_core::interpreter::Step::ReadFault { pc, address } => {
                let address =
                    adv_cpu_core::interpreter::fault_address(&memory, address, adv_cpu_core::READ);
                break RunOutcome::ReadFault { pc, address };
            }
            adv_cpu_core::interpreter::Step::WriteFault { pc, address } => {
                let address =
                    adv_cpu_core::interpreter::fault_address(&memory, address, adv_cpu_core::WRITE);
                break RunOutcome::WriteFault { pc, address };
            }
            adv_cpu_core::interpreter::Step::Trap { pc, vector } => {
                break RunOutcome::Trap { pc, vector };
            }
            other => panic!("unexpected step {other:?}"),
        }
    };
    (outcome, state, snapshot(&memory))
}

fn snapshot(memory: &Memory) -> Vec<u8> {
    let mut bytes = memory.read(DATA, DATA_LEN).unwrap().to_vec();
    bytes.extend_from_slice(
        memory
            .read(STACK, (STACK_TOP + 0x1000 - STACK) as usize)
            .unwrap(),
    );
    bytes
}

#[cfg(native)]
fn translated(
    program: &Program,
    budget: u64,
    (features, first_visits, checks): Config,
) -> (RunOutcome, CpuState, Vec<u8>, u64) {
    let mut executor = Executor::new(BASE, LEN).unwrap();
    let native = executor.native.as_mut().unwrap();
    native.features = features;
    native.first_visits = first_visits;
    native.checks = checks;
    load(executor.memory_mut(), program);
    executor.state = program.state.clone();
    let outcome = executor.run(program.stop, budget).unwrap();
    let native = executor.stats.native_entries;
    (
        outcome,
        executor.state.clone(),
        snapshot(executor.memory()),
        native,
    )
}

/// Host features, first visits, and which accesses check their page.
#[cfg(native)]
type Config = (Features, bool, Checks);

#[cfg(native)]
mod native_instructions;

#[cfg(native)]
fn compare(seed: u64, budget: u64, config: Config) -> u64 {
    let program = program(seed);
    let (outcome, state, memory) = reference(&program, budget);
    let (a64_outcome, a64_state, a64_memory, native) = translated(&program, budget, config);
    let context = format!("seed {seed} budget {budget} {config:?}");
    assert_eq!(a64_outcome, outcome, "{context}: outcome");
    assert_eq!(a64_state.regs, state.regs, "{context}: registers");
    assert_eq!(a64_state.eip, state.eip, "{context}: eip");
    assert_eq!(
        a64_state.eflags, state.eflags,
        "{context}: eflags {:#x} != {:#x}",
        a64_state.eflags, state.eflags
    );
    assert_eq!(a64_state.xmm, state.xmm, "{context}: xmm");
    assert_eq!(
        a64_state.mxcsr, state.mxcsr,
        "{context}: mxcsr {:#x} != {:#x}",
        a64_state.mxcsr, state.mxcsr
    );
    assert_eq!(a64_state.mmx, state.mmx, "{context}: mmx");
    assert_eq!(a64_state.x87, state.x87, "{context}: x87");
    assert!(a64_memory == memory, "{context}: memory");
    native
}

// spec: A64-ISA-4, A64-REG-5, A64-FLAG-2, A64-FLAG-3, A64-FLAG-4, A64-FLAG-5, A64-MEM-7, A64-CTL-4, A64-CTL-7, A64-CTL-9, A64F-FP-2, A64F-FP-3, A64F-FP-4, A64F-FP-7, A64-TR-4, A64-VER-1
#[cfg(native)]
#[test]
fn random_programs_match_the_interpreter_at_the_stop_and_inside_units() {
    let mut native = 0;
    // Without first visits, everything the programs run is translated.
    let configs = [
        (true, false, Checks::Adaptive),
        (false, false, Checks::Adaptive),
        (true, true, Checks::Adaptive),
        (true, false, Checks::All),
        (false, false, Checks::Slow),
    ];
    for (flagm, first_visits, checks) in configs {
        let config = (Features { flagm }, first_visits, checks);
        for seed in 1..=1000 {
            native += compare(seed, u64::MAX >> 1, config);
            for budget in [1, 5, 13, 40, 120, 400] {
                compare(seed, budget + seed % 7, config);
            }
        }
    }
    assert!(native > 1000, "translated code ran: {native} entries");
}

/// A loop of the x87 code compilers emit for float expressions, under the
/// MSVC control word with `ftol`'s switch to chop: it matches the
/// interpreter, and none of its x87 instructions leaves translated code
/// or calls a helper.
// spec: A64-FLAG-6, A64F-FP-1, A64F-FP-3, A64F-FP-7
#[cfg(native)]
#[test]
fn x87_loops_run_natively() {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    let constant = |a: &mut CodeAssembler, at: i32, value: u32| {
        a.mov(dword_ptr(ebx + at), value).unwrap();
    };
    constant(&mut a, 0x710, 0.25f32.to_bits());
    constant(&mut a, 0x714, 3.0f32.to_bits());
    constant(&mut a, 0x718, 2.0f32.to_bits());
    constant(&mut a, 0x7f0, 0x133f);
    constant(&mut a, 0x7f4, 0x1f3f);
    constant(&mut a, 0x7f8, 0x027f);
    a.fninit().unwrap();
    a.fldcw(word_ptr(ebx + 0x7f0)).unwrap();
    a.mov(ecx, 300).unwrap();
    a.fldz().unwrap();
    a.set_label(&mut top).unwrap();
    // acc += i * 0.25
    a.mov(dword_ptr(ebx + 0x700), ecx).unwrap();
    a.fild(dword_ptr(ebx + 0x700)).unwrap();
    a.fmul(dword_ptr(ebx + 0x710)).unwrap();
    a.faddp(st1, st0).unwrap();
    // Stores, a reload and a compare through the status word.
    a.fld(st0).unwrap();
    a.fmul(dword_ptr(ebx + 0x714)).unwrap();
    a.fst(qword_ptr(ebx + 0x730)).unwrap();
    a.fstp(dword_ptr(ebx + 0x720)).unwrap();
    a.fld(dword_ptr(ebx + 0x720)).unwrap();
    a.fcomp(dword_ptr(ebx + 0x714)).unwrap();
    a.fnstsw(ax).unwrap();
    a.mov(dword_ptr(ebx + 0x734), eax).unwrap();
    a.fld1().unwrap();
    a.fchs().unwrap();
    a.fabs().unwrap();
    a.fxch(st0, st1).unwrap();
    a.fxch(st0, st1).unwrap();
    a.fsubp(st1, st0).unwrap();
    a.fld(st0).unwrap();
    a.fdiv(dword_ptr(ebx + 0x718)).unwrap();
    a.fstp(qword_ptr(ebx + 0x738)).unwrap();
    // ftol.
    a.fnstcw(word_ptr(ebx + 0x7fc)).unwrap();
    a.fldcw(word_ptr(ebx + 0x7f4)).unwrap();
    a.fld(st0).unwrap();
    a.fistp(qword_ptr(ebx + 0x740)).unwrap();
    a.fld(st0).unwrap();
    a.fistp(dword_ptr(ebx + 0x748)).unwrap();
    a.fldcw(word_ptr(ebx + 0x7fc)).unwrap();
    // An inexact quotient at precision 53.
    a.fldcw(word_ptr(ebx + 0x7f8)).unwrap();
    a.fld(st0).unwrap();
    a.fdivr(dword_ptr(ebx + 0x714)).unwrap();
    a.fstp(dword_ptr(ebx + 0x74c)).unwrap();
    a.fldcw(word_ptr(ebx + 0x7f0)).unwrap();
    a.wait().unwrap();
    a.dec(ecx).unwrap();
    a.jnz(top).unwrap();
    a.fstp(dword_ptr(ebx + 0x750)).unwrap();
    a.nop().unwrap();
    let code = a.assemble(u64::from(CODE)).unwrap();
    let mut state = CpuState {
        eip: CODE,
        ..CpuState::default()
    };
    state.regs[3] = DATA;
    state.regs[4] = STACK_TOP;
    let program = Program {
        stop: CODE + code.len() as u32 - 1,
        code,
        data: vec![0; DATA_LEN],
        state,
    };
    let (outcome, state, memory) = reference(&program, u64::MAX >> 1);
    assert_eq!(outcome, RunOutcome::ReachedStop);
    let mut executor = Executor::new(BASE, LEN).unwrap();
    // Every instruction is translated.
    executor.native.as_mut().unwrap().first_visits = false;
    load(executor.memory_mut(), &program);
    executor.state = program.state.clone();
    assert_eq!(executor.run(program.stop, u64::MAX >> 1).unwrap(), outcome);
    assert_eq!(executor.state.regs, state.regs);
    assert_eq!(executor.state.x87, state.x87);
    assert_eq!(executor.state.mmx, state.mmx);
    assert!(snapshot(executor.memory()) == memory);
    let sum = (1..=300).sum::<u32>() as f32 * 0.25 - 300.0;
    assert_eq!(
        executor.memory().read(DATA + 0x750, 4).unwrap(),
        sum.to_le_bytes()
    );
    for which in [0, 2] {
        for (code, count) in executor.interpreted_forms(which) {
            let mnemonic = format!("{:?}", code.mnemonic());
            assert!(
                count == 0
                    || !mnemonic.starts_with('F')
                    || code.mnemonic() == iced_x86::Mnemonic::Fninit,
                "{code:?} left translated code {count} times ({which})"
            );
        }
    }
}

/// Data on a code page: each store faults until the instruction is checked,
/// then takes `access` without leaving translated code.
// spec: A64-MEM-6, A64-MEM-7, A64-CTL-10
#[cfg(native)]
#[test]
fn stores_to_data_on_a_code_page_stop_faulting() {
    const ITERATIONS: u32 = 2000;
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    a.mov(ecx, CODE + 0x800).unwrap();
    a.mov(edx, ITERATIONS).unwrap();
    a.set_label(&mut top).unwrap();
    a.mov(eax, dword_ptr(ecx)).unwrap();
    a.add(eax, edx).unwrap();
    a.mov(dword_ptr(ecx), eax).unwrap();
    a.push(eax).unwrap();
    a.pop(ebx).unwrap();
    a.dec(edx).unwrap();
    a.jnz(top).unwrap();
    a.nop().unwrap();
    let code = a.assemble(u64::from(CODE)).unwrap();
    let stop = CODE + code.len() as u32 - 1;

    let mut executor = Executor::new(BASE, LEN).unwrap();
    let memory = executor.memory_mut();
    memory.protect(CODE, 0x1_0000, 7).unwrap();
    memory.write(CODE, &code).unwrap();
    memory
        .protect(STACK, (STACK_TOP + 0x1000 - STACK) as usize, 3)
        .unwrap();
    executor.state.eip = CODE;
    executor.state.regs[4] = STACK_TOP;
    let outcome = executor.run(stop, u64::MAX >> 1).unwrap();
    assert_eq!(outcome, RunOutcome::ReachedStop);
    let sum = (1..=ITERATIONS).sum::<u32>();
    assert_eq!(executor.state.regs[0], sum);
    assert_eq!(executor.state.regs[3], sum);
    let stored = executor.memory().read(CODE + 0x800, 4).unwrap();
    assert_eq!(u32::from_le_bytes(stored.try_into().unwrap()), sum);
    // The loop's unit, then the one it loops through, each fault until
    // the store is checked.
    let faults = executor.native.as_ref().unwrap().unchecked_faults;
    let checked = u64::from(crate::native::CHECK_AFTER);
    assert!((checked..=2 * checked).contains(&faults), "{faults} faults");
}

/// A fault in the middle of an x87 run, with results only in host
/// registers and renamed slots: the state it leaves is the interpreter's
/// before the faulting instruction, for a store and for an operand.
// spec: A64F-FP-7
#[cfg(native)]
#[test]
fn x87_runs_publish_their_state_at_a_fault() {
    for store in [true, false] {
        let mut a = CodeAssembler::new(32).unwrap();
        a.mov(dword_ptr(ebx + 0x710), 2.5f32.to_bits()).unwrap();
        a.mov(eax, HOLE).unwrap();
        a.fninit().unwrap();
        a.fld1().unwrap();
        a.fld(dword_ptr(ebx + 0x710)).unwrap();
        a.fadd_2(st0, st1).unwrap();
        a.fld(st0).unwrap();
        a.fxch(st0, st2).unwrap();
        a.fstp(st1).unwrap();
        if store {
            a.fstp(dword_ptr(eax)).unwrap();
        } else {
            a.fmul(dword_ptr(eax)).unwrap();
        }
        a.nop().unwrap();
        let code = a.assemble(u64::from(CODE)).unwrap();
        let mut state = CpuState {
            eip: CODE,
            ..CpuState::default()
        };
        state.regs[3] = DATA;
        state.regs[4] = STACK_TOP;
        let program = Program {
            stop: CODE + code.len() as u32 - 1,
            code,
            data: vec![0; DATA_LEN],
            state,
        };
        let (outcome, state, memory) = reference(&program, u64::MAX >> 1);
        assert!(
            matches!(
                outcome,
                RunOutcome::ReadFault { .. } | RunOutcome::WriteFault { .. }
            ),
            "{outcome:?}"
        );
        let mut executor = Executor::new(BASE, LEN).unwrap();
        executor.native.as_mut().unwrap().first_visits = false;
        load(executor.memory_mut(), &program);
        executor.state = program.state.clone();
        assert_eq!(executor.run(program.stop, u64::MAX >> 1).unwrap(), outcome);
        assert_eq!(executor.state.eip, state.eip);
        assert_eq!(executor.state.x87, state.x87, "store {store}");
        assert_eq!(executor.state.mmx, state.mmx, "store {store}");
        assert!(snapshot(executor.memory()) == memory);
        assert!(executor.native.as_ref().unwrap().unchecked_faults > 0);
    }
}

/// Samplers see which guest instruction and which part of its code a
/// host address of a unit is in: an x87 run's entry checks, its body, its
/// publication and its out-of-line paths.
#[cfg(native)]
#[test]
fn units_map_host_code_to_instructions_and_parts() {
    use crate::jit_map::{Location, Part, enable_memory, locate, units_of};
    enable_memory();
    let mut a = CodeAssembler::new(32).unwrap();
    a.mov(dword_ptr(ebx + 0x710), 2.5f32.to_bits()).unwrap();
    a.fninit().unwrap();
    a.fld1().unwrap();
    a.fld(dword_ptr(ebx + 0x710)).unwrap();
    a.fadd_2(st0, st1).unwrap();
    a.fstp(dword_ptr(ebx + 0x714)).unwrap();
    a.fstp(st0).unwrap();
    a.nop().unwrap();
    let code = a.assemble(u64::from(CODE)).unwrap();
    let mut state = CpuState {
        eip: CODE,
        ..CpuState::default()
    };
    state.regs[3] = DATA;
    state.regs[4] = STACK_TOP;
    let program = Program {
        stop: CODE + code.len() as u32 - 1,
        code,
        data: vec![0; DATA_LEN],
        state,
    };
    let mut executor = Executor::new(BASE, LEN).unwrap();
    executor.native.as_mut().unwrap().first_visits = false;
    load(executor.memory_mut(), &program);
    executor.state = program.state.clone();
    executor.run(program.stop, u64::MAX >> 1).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let (base, marks) = units_of(CODE).pop().expect("the unit was recorded");
    let parts: Vec<_> = marks.iter().map(|mark| mark.part).collect();
    for part in [Part::Body, Part::X87Entry, Part::X87Publish, Part::Cold] {
        assert!(parts.contains(&part), "{part:?} in {marks:?}");
    }
    for mark in &marks {
        assert!((CODE..program.stop + 1).contains(&mark.eip), "{mark:?}");
        assert_eq!(
            locate(base + mark.offset as usize, now),
            Some(Location {
                unit: CODE,
                eip: Some(mark.eip),
                part: mark.part,
            })
        );
    }
}

#[test]
fn random_programs_assemble_and_reach_the_stop_or_a_fault() {
    let mut faults = 0;
    for seed in 1..=300 {
        let program = program(seed);
        match reference(&program, u64::MAX >> 1).0 {
            RunOutcome::ReachedStop => {}
            RunOutcome::ReadFault { .. }
            | RunOutcome::WriteFault { .. }
            | RunOutcome::Trap { .. } => faults += 1,
            other => panic!("seed {seed}: {other:?}"),
        }
    }
    assert!((1..100).contains(&faults), "{faults} programs fault");
}

/// Lists the program of seed `SEED`, for a failure to reproduce by hand.
#[test]
#[ignore]
fn dump_program() {
    let seed: u64 = std::env::var("SEED").unwrap().parse().unwrap();
    let program = program(seed);
    let mut decoder = iced_x86::Decoder::with_ip(32, &program.code, u64::from(CODE), 0);
    for i in &mut decoder {
        let ops: Vec<String> = (0..i.op_count())
            .map(|n| match i.op_kind(n) {
                iced_x86::OpKind::Register => format!("{:?}", i.op_register(n)),
                iced_x86::OpKind::Memory => format!(
                    "[{:?}+{:?}*{}+{:#x}]",
                    i.memory_base(),
                    i.memory_index(),
                    i.memory_index_scale(),
                    i.memory_displacement32()
                ),
                iced_x86::OpKind::NearBranch32 => format!("{:#x}", i.near_branch32()),
                k @ (iced_x86::OpKind::Immediate8
                | iced_x86::OpKind::Immediate32
                | iced_x86::OpKind::Immediate8to32
                | iced_x86::OpKind::Immediate16) => format!("{k:?}={:#x}", i.immediate(n)),
                k => format!("{k:?}"),
            })
            .collect();
        println!("{:x} {:?} {}", i.ip(), i.mnemonic(), ops.join(", "));
    }
    println!("{:x?}", program.state.regs);
}

/// The first budget at which seed `SEED` diverges, which names the
/// instruction a failure comes from.
#[cfg(native)]
#[test]
#[ignore]
fn first_divergence() {
    let seed: u64 = std::env::var("SEED").unwrap().parse().unwrap();
    let program = program(seed);
    let config = (Features { flagm: true }, false, Checks::Adaptive);
    for budget in 1..5000 {
        let (outcome, state, memory) = reference(&program, budget);
        let (a, b, m, _) = translated(&program, budget, config);
        if a != outcome
            || b.regs != state.regs
            || b.eflags != state.eflags
            || b.xmm != state.xmm
            || b.mmx != state.mmx
            || b.x87 != state.x87
            || m != memory
        {
            println!("budget {budget}: eip {:#x} vs {:#x}", b.eip, state.eip);
            println!("regs {:x?}\n  vs {:x?}", b.regs, state.regs);
            println!("eflags {:#x} vs {:#x}", b.eflags, state.eflags);
            println!("mmx {:x?}\n vs {:x?}", b.mmx, state.mmx);
            println!(
                "xmm eq {} x87 eq {} mem eq {}",
                b.xmm == state.xmm,
                b.x87 == state.x87,
                m == memory
            );
            println!("x87 {:x?}\n vs {:x?}", b.x87, state.x87);
            let (_, prev, _) = reference(&program, budget - 1);
            println!("previous eip {:#x}", prev.eip);
            return;
        }
    }
    println!("no divergence");
}

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
use crate::translate::Features;

const BASE: u32 = 0x1_0000;
const LEN: usize = 0x4_0000;
const CODE: u32 = 0x1_0000;
const DATA: u32 = 0x3_0000;
const DATA_LEN: usize = 0x2000;
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
    match rng.below(25) {
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
            0 => a.shl(w, cl),
            1 => a.rcl(w, 1),
            2 => a.imul(r),
            3 => a.bswap(w),
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
        _ => a.nop().unwrap(),
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
        _ => a.jnp(label),
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
                condition(&mut a, rng.below(16), skip);
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
                    condition(&mut a, rng.below(14), skip);
                    simple(&mut a, &mut rng);
                    a.set_label(&mut skip).unwrap();
                    a.nop().unwrap();
                    simple(&mut a, &mut rng);
                }
                a.sub(dword_ptr(esp), 1).unwrap();
                a.jne(top).unwrap();
                a.add(esp, 4).unwrap();
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
        ..CpuState::default()
    };
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
    features: Features,
    first_visits: bool,
) -> (RunOutcome, CpuState, Vec<u8>, u64) {
    let mut executor = Executor::new(BASE, LEN).unwrap();
    executor.native.features = features;
    executor.native.first_visits = first_visits;
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

#[cfg(native)]
fn compare(seed: u64, budget: u64, features: Features, first_visits: bool) -> u64 {
    let program = program(seed);
    let (outcome, state, memory) = reference(&program, budget);
    let (a64_outcome, a64_state, a64_memory, native) =
        translated(&program, budget, features, first_visits);
    let context = format!("seed {seed} budget {budget} {features:?} first visits {first_visits}");
    assert_eq!(a64_outcome, outcome, "{context}: outcome");
    assert_eq!(a64_state.regs, state.regs, "{context}: registers");
    assert_eq!(a64_state.eip, state.eip, "{context}: eip");
    assert_eq!(
        a64_state.eflags, state.eflags,
        "{context}: eflags {:#x} != {:#x}",
        a64_state.eflags, state.eflags
    );
    assert!(a64_memory == memory, "{context}: memory");
    native
}

#[cfg(native)]
#[test]
fn random_programs_match_the_interpreter_at_the_stop_and_inside_units() {
    let mut native = 0;
    // Without first visits, everything the programs run is translated.
    for (flagm, first_visits) in [(true, false), (false, false), (true, true)] {
        let features = Features { flagm };
        for seed in 1..=1000 {
            native += compare(seed, u64::MAX >> 1, features, first_visits);
            for budget in [1, 5, 13, 40, 120, 400] {
                compare(seed, budget + seed % 7, features, first_visits);
            }
        }
    }
    assert!(native > 1000, "translated code ran: {native} entries");
}

#[test]
fn random_programs_assemble_and_reach_the_stop_or_a_read_fault() {
    let mut faults = 0;
    for seed in 1..=300 {
        let program = program(seed);
        match reference(&program, u64::MAX >> 1).0 {
            RunOutcome::ReachedStop => {}
            RunOutcome::ReadFault { .. } => faults += 1,
            other => panic!("seed {seed}: {other:?}"),
        }
    }
    assert!((1..100).contains(&faults), "{faults} programs fault");
}

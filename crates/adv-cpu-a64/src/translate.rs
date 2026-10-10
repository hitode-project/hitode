//! Translation units (workbook §9).
//!
//! A unit starts at a guest EIP and follows the fall-through path and direct
//! jumps and calls until an indirect transfer, an instruction it does not
//! translate, a known stop address or `MAX_INSTRUCTIONS`. Conditional
//! branches become side exits.
//!
//! Linking and fuel (§7.1, §7.4): a unit starts with a safepoint that leaves
//! unless x9, the countdown, covers every instruction it may complete. Each
//! exit charges x9 with the instructions the unit completed on that path,
//! then reaches a link point: a static target's link point starts as a
//! return to the owner, which may patch its first word into a direct branch
//! to the target unit, whose own safepoint then decides. So budgets stay
//! exact at unit granularity, and a loop of linked units still returns to
//! the owner when its slice runs out.
//!
//! Flags follow §5. CF ZF SF OF live in NZCV, whose carry sense the
//! translator tracks statically; PF and AF stay as a recipe over values that
//! are still in registers until something would overwrite them. Every exit
//! and every fault site therefore knows how to rebuild the exact x86 flags:
//! exits by emitting the canonical form, sites by recording sense and
//! recipe in the side table for the owner to apply.
//!
//! Host registers beyond the guest map of §4: x16 and x17 address and exit
//! scratch, x6 a value scratch, and x7/x8 the two recipe temporaries, which
//! the exit stub saves for fault recovery.

use adv_cpu_core::memory::Memory;
use iced_x86::{ConditionCode, Decoder, DecoderOptions, Instruction, Mnemonic, OpKind, Register};

use crate::asm::{Asm, Label};
use crate::encode::Extend::Uxtw;
use crate::encode::Index::{Post, Pre};
use crate::encode::Shift::Lsl;
use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::runtime::Exit;

/// Longest unit, in guest instructions (§9).
pub const MAX_INSTRUCTIONS: u32 = 64;

/// Host features that change what the translator emits (§2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Features {
    pub flagm: bool,
}

/// How NZCV's C encodes CF (§5.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sense {
    /// C is NOT CF, as after `subs`: the canonical sense at exits.
    Subtraction,
    /// C is CF, as after `adds` and `ands`.
    Addition,
}

/// A 32-bit value a recipe reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operand {
    /// A guest register by `CpuState::regs` index.
    Guest(u8),
    /// Recipe temporary 0 (x7) or 1 (x8).
    Temp(u8),
    Imm(u32),
}

/// Where PF and AF come from (§5.3): PF from the low byte of a result and
/// AF from bit 4 of left ^ right ^ result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recipe {
    /// x10 and x11 hold them.
    Canonical,
    /// `result = left + right`, with the result and the right operand known.
    AddResult(Operand, Operand),
    /// `result = left - right`, with the result and the right operand known.
    SubResult(Operand, Operand),
    /// `left + right`.
    Add(Operand, Operand),
    /// `left - right`.
    Sub(Operand, Operand),
    /// A logic result; AF is clear.
    Logic(Operand),
    /// `left & right`; AF is clear.
    And(Operand, Operand),
    /// `left | right`; AF is clear.
    Or(Operand, Operand),
    /// `left ^ right`; AF is clear.
    Xor(Operand, Operand),
    /// `result = 0 - right`, with the result known.
    Neg(Operand),
    /// `result = x + x`, whose AF is bit 4 of the result: x ^ x is 0.
    Doubled(Operand),
}

impl Recipe {
    /// The PF source and the AF source, reading operands through `value`;
    /// `None` when they are already canonical.
    pub fn evaluate(self, value: impl Fn(Operand) -> u32) -> Option<(u32, u32)> {
        use Recipe::*;
        let pair = |a: Operand, b: Operand| (value(a), value(b));
        Some(match self {
            Canonical => return None,
            AddResult(r, b) => {
                let (r, b) = pair(r, b);
                (r, r.wrapping_sub(b) ^ b ^ r)
            }
            SubResult(r, b) => {
                let (r, b) = pair(r, b);
                (r, r.wrapping_add(b) ^ b ^ r)
            }
            Add(a, b) => {
                let (a, b) = pair(a, b);
                let r = a.wrapping_add(b);
                (r, a ^ b ^ r)
            }
            Sub(a, b) => {
                let (a, b) = pair(a, b);
                let r = a.wrapping_sub(b);
                (r, a ^ b ^ r)
            }
            Logic(r) => (value(r), 0),
            And(a, b) => (value(a) & value(b), 0),
            Or(a, b) => (value(a) | value(b), 0),
            Xor(a, b) => (value(a) ^ value(b), 0),
            Neg(r) => {
                let r = value(r);
                (r, r.wrapping_neg() ^ r)
            }
            Doubled(r) => (value(r), value(r)),
        })
    }

    fn operands(self) -> [Option<Operand>; 2] {
        use Recipe::*;
        match self {
            Canonical => [None, None],
            Logic(a) | Neg(a) | Doubled(a) => [Some(a), None],
            AddResult(a, b)
            | SubResult(a, b)
            | Add(a, b)
            | Sub(a, b)
            | And(a, b)
            | Or(a, b)
            | Xor(a, b) => [Some(a), Some(b)],
        }
    }

    fn uses(self, operand: Operand) -> bool {
        self.operands().contains(&Some(operand))
    }
}

/// What the owner needs at a host instruction that may fault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Site {
    /// The guest instruction, which has committed nothing (§5.2).
    pub eip: u32,
    /// Guest instructions the unit completed before it.
    pub retired: u32,
    pub sense: Sense,
    pub recipe: Recipe,
}

pub struct Unit {
    pub words: Vec<u32>,
    /// Byte offset of each site in `words`.
    pub sites: Vec<(usize, Site)>,
    /// The most guest instructions any exit completes.
    pub retired: u32,
    /// Guest bytes the unit was translated from, as (address, length).
    pub code: Vec<(u32, u32)>,
}

const X6: Reg = Reg::new(6);
const X9: Reg = Reg::new(9);
const X10: Reg = Reg::new(10);
const X11: Reg = Reg::new(11);
const X16: Reg = Reg::new(16);
const X17: Reg = Reg::new(17);
const WINDOW: Reg = Reg::new(27);
const ESP: u8 = 4;
const EBP: u8 = 5;

/// The host register of guest register `index`; ESP and EBP are biased.
const fn host(index: u8) -> Reg {
    Reg::new(19 + index)
}

const fn temp(index: u8) -> Reg {
    Reg::new(7 + index)
}

const fn biased(index: u8) -> bool {
    index == ESP || index == EBP
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Mem {
    base: Option<u8>,
    index: Option<u8>,
    scale: u32,
    disp: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Src {
    Reg(u8),
    Imm(u32),
    Mem(Mem),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Alu {
    Add,
    Sub,
    And,
    Or,
    Xor,
    Cmp,
    Test,
}

/// A narrow source of `movzx`/`movsx`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Narrow8or16 {
    /// A byte register: AL..BL are 0..3, AH..BH 4..7.
    Byte(u8),
    /// A word register by guest index.
    Word(u8),
    Mem(Mem, bool),
}

/// One guest instruction the translator handles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Nop,
    Mov(Src, Src),
    Lea(u8, Mem),
    Alu(Alu, Src, Src),
    Step(bool, u8),
    Neg(u8),
    Not(u8),
    Extend(u8, Narrow8or16, bool),
    Cdq,
    Push(Src),
    Pop(u8),
    Jmp(u32),
    Call(u32),
    Jcc(ConditionCode, u32),
    Ret(u16),
    JmpIndirect(Src),
    CallIndirect(Src),
    Setcc(ConditionCode, u8),
    Cmov(ConditionCode, u8, Src),
}

fn gpr32(register: Register) -> Option<u8> {
    register.is_gpr32().then(|| register.number() as u8)
}

fn memory(instruction: &Instruction) -> Option<Mem> {
    // Flat DS/SS/ES only: FS and GS have bases, and an override is rare
    // enough to leave to the interpreter.
    if instruction.segment_prefix() != Register::None {
        return None;
    }
    let register = |register| match register {
        Register::None => Some(None),
        register => gpr32(register).map(Some),
    };
    Some(Mem {
        base: register(instruction.memory_base())?,
        index: register(instruction.memory_index())?,
        scale: instruction.memory_index_scale(),
        disp: instruction.memory_displacement32(),
    })
}

fn operand(instruction: &Instruction, n: u32) -> Option<Src> {
    match instruction.op_kind(n) {
        OpKind::Register => gpr32(instruction.op_register(n)).map(Src::Reg),
        OpKind::Immediate32 | OpKind::Immediate8to32 => {
            Some(Src::Imm(instruction.immediate(n) as u32))
        }
        OpKind::Memory if instruction.memory_size().size() == 4 => {
            memory(instruction).map(Src::Mem)
        }
        _ => None,
    }
}

fn plan(instruction: &Instruction) -> Option<Op> {
    use Mnemonic as M;
    if instruction.has_lock_prefix() {
        return None;
    }
    let count = instruction.op_count();
    let two = || Some((operand(instruction, 0)?, operand(instruction, 1)?));
    let register = |n| match instruction.op_kind(n) {
        OpKind::Register => gpr32(instruction.op_register(n)),
        _ => None,
    };
    let near = || match instruction.op_kind(0) {
        OpKind::NearBranch32 => Some(instruction.near_branch32()),
        _ => None,
    };
    let alu = |alu| {
        let (dst, src) = two()?;
        match (dst, src) {
            (Src::Imm(_), _) | (Src::Mem(_), Src::Mem(_)) => None,
            _ => Some(Op::Alu(alu, dst, src)),
        }
    };
    let mnemonic = instruction.mnemonic();
    if instruction.is_jcc_short_or_near() {
        return Some(Op::Jcc(instruction.condition_code(), near()?)).filter(|op| supported(*op));
    }
    Some(match mnemonic {
        M::Nop => Op::Nop,
        M::Mov => match two()? {
            (Src::Imm(_), _) | (Src::Mem(_), Src::Mem(_)) => return None,
            (dst, src) => Op::Mov(dst, src),
        },
        M::Lea => Op::Lea(register(0)?, memory(instruction)?),
        M::Add => alu(Alu::Add)?,
        M::Sub => alu(Alu::Sub)?,
        M::And => alu(Alu::And)?,
        M::Or => alu(Alu::Or)?,
        M::Xor => alu(Alu::Xor)?,
        M::Cmp => alu(Alu::Cmp)?,
        M::Test => alu(Alu::Test)?,
        M::Inc => Op::Step(true, register(0)?),
        M::Dec => Op::Step(false, register(0)?),
        M::Neg => Op::Neg(register(0)?),
        M::Not => Op::Not(register(0)?),
        M::Movzx | M::Movsx => {
            let signed = mnemonic == M::Movsx;
            let source = match instruction.op_kind(1) {
                OpKind::Register => {
                    let source = instruction.op_register(1);
                    if source.is_gpr8() {
                        Narrow8or16::Byte(source.number() as u8)
                    } else if source.is_gpr16() {
                        Narrow8or16::Word(source.number() as u8)
                    } else {
                        return None;
                    }
                }
                OpKind::Memory => match instruction.memory_size().size() {
                    1 => Narrow8or16::Mem(memory(instruction)?, false),
                    2 => Narrow8or16::Mem(memory(instruction)?, true),
                    _ => return None,
                },
                _ => return None,
            };
            Op::Extend(register(0)?, source, signed)
        }
        M::Cdq => Op::Cdq,
        M::Push if count == 1 => Op::Push(operand(instruction, 0)?),
        M::Pop => match register(0)? {
            ESP => return None,
            register => Op::Pop(register),
        },
        M::Jmp => match instruction.op_kind(0) {
            OpKind::NearBranch32 => Op::Jmp(instruction.near_branch32()),
            OpKind::Register | OpKind::Memory => Op::JmpIndirect(operand(instruction, 0)?),
            _ => return None,
        },
        M::Call => match instruction.op_kind(0) {
            OpKind::NearBranch32 => Op::Call(instruction.near_branch32()),
            OpKind::Register | OpKind::Memory => Op::CallIndirect(operand(instruction, 0)?),
            _ => return None,
        },
        M::Ret => match instruction.code() {
            iced_x86::Code::Retnd => Op::Ret(0),
            iced_x86::Code::Retnd_imm16 if instruction.immediate16() < 4096 => {
                Op::Ret(instruction.immediate16())
            }
            _ => return None,
        },
        M::Seto
        | M::Setno
        | M::Setb
        | M::Setae
        | M::Sete
        | M::Setne
        | M::Setbe
        | M::Seta
        | M::Sets
        | M::Setns
        | M::Setp
        | M::Setnp
        | M::Setl
        | M::Setge
        | M::Setle
        | M::Setg => {
            let target = instruction.op_register(0);
            if instruction.op_kind(0) != OpKind::Register || !target.is_gpr8() {
                return None;
            }
            Op::Setcc(instruction.condition_code(), target.number() as u8)
        }
        M::Cmovo
        | M::Cmovno
        | M::Cmovb
        | M::Cmovae
        | M::Cmove
        | M::Cmovne
        | M::Cmovbe
        | M::Cmova
        | M::Cmovs
        | M::Cmovns
        | M::Cmovp
        | M::Cmovnp
        | M::Cmovl
        | M::Cmovge
        | M::Cmovle
        | M::Cmovg => {
            let source = operand(instruction, 1)?;
            match register(0)? {
                ESP | EBP => return None,
                target => Op::Cmov(instruction.condition_code(), target, source),
            }
        }
        _ => return None,
    })
    .filter(|op| supported(*op))
}

fn supported(op: Op) -> bool {
    // A condition on PF needs the parity materialized; leave it to the
    // interpreter until a unit can test it.
    let parity = |cc| {
        matches!(
            cc,
            ConditionCode::p | ConditionCode::np | ConditionCode::None
        )
    };
    match op {
        Op::Jcc(cc, _) | Op::Setcc(cc, _) | Op::Cmov(cc, ..) => !parity(cc),
        _ => true,
    }
}

/// The next guest instruction if it is entirely in executable memory.
fn fetch(memory: &Memory, pc: u32) -> Option<Instruction> {
    let page_left = 0x1000 - (pc as usize & 0xfff);
    let mut len = page_left.min(15);
    if !memory.executable(pc, len) {
        return None;
    }
    if len < 15
        && pc
            .checked_add(len as u32)
            .is_some_and(|next| memory.executable(next, 15 - len))
    {
        len = 15;
    }
    let bytes = memory.read(pc, len).ok()?;
    let mut decoder = Decoder::with_ip(32, bytes, u64::from(pc), DecoderOptions::NONE);
    let instruction = decoder.decode();
    (!instruction.is_invalid()).then_some(instruction)
}

/// An address operand ready for a load or store.
#[derive(Clone, Copy)]
enum Addr {
    /// A W register holding the guest address: `[x27, w, uxtw]`.
    Window(Reg),
    /// A biased stack register plus an offset the guards cover.
    Biased(Reg, i32),
}

/// Where an exit takes guest execution.
#[derive(Clone, Copy)]
enum Target {
    Static(u32),
    /// The EIP is in w16.
    Dynamic,
}

/// A side exit emitted after the unit body.
struct Cold {
    label: Label,
    sense: Sense,
    recipe: Recipe,
    target: u32,
    retired: u32,
}

struct Translator {
    asm: Asm,
    features: Features,
    sites: Vec<(usize, Site)>,
    cold: Vec<Cold>,
    sense: Sense,
    recipe: Recipe,
    /// The guest instruction being emitted.
    eip: u32,
    /// Guest instructions completed before it.
    retired: u32,
    /// Branches to the shared exit stub, as word indices.
    exits: Vec<usize>,
    /// The unit's first guest instruction.
    entry: u32,
}

/// What the unit does after an instruction.
enum Flow {
    Next,
    Jump(u32),
    /// The instruction left the unit.
    End,
}

/// Translates the unit at `eip`, to be placed at host address `at` with the
/// exit stub at `exit`. Units end before any of `stops`. Each translated byte
/// is registered with `watch_code`, so a store to it reaches the owner.
pub fn translate(
    memory: &mut Memory,
    eip: u32,
    stops: &[u32],
    features: Features,
    at: usize,
    exit: usize,
) -> Unit {
    let mut t = Translator {
        asm: Asm::new(),
        features,
        sites: Vec::new(),
        cold: Vec::new(),
        sense: Sense::Subtraction,
        recipe: Recipe::Canonical,
        eip,
        retired: 0,
        exits: Vec::new(),
        entry: eip,
    };
    // The safepoint; `finish` patches in the unit's instruction count.
    let refuse = t.asm.label();
    t.emit(0);
    t.asm.tbz(true, X16, 63, refuse);
    let mut code = Vec::new();
    let mut pc = eip;
    loop {
        let revisit = code.iter().any(|&(start, _)| start == pc);
        if t.retired == MAX_INSTRUCTIONS || (t.retired > 0 && stops.contains(&pc)) || revisit {
            t.leave(Exit::Dispatch, Target::Static(pc));
            break;
        }
        let Some((instruction, op)) = fetch(memory, pc).and_then(|i| Some((i, plan(&i)?))) else {
            t.leave(Exit::Interpret, Target::Static(pc));
            break;
        };
        t.eip = pc;
        let flow = t.op(op, instruction.next_ip32());
        memory.watch_code(pc, instruction.len());
        code.push((pc, instruction.len() as u32));
        match flow {
            Flow::Next => {
                t.retired += 1;
                pc = instruction.next_ip32();
            }
            Flow::Jump(target) => {
                t.retired += 1;
                pc = target;
            }
            Flow::End => break,
        }
    }
    let retired = t.retired;
    t.finish(at, exit, code, retired, refuse)
}

impl Translator {
    fn emit(&mut self, word: u32) {
        self.asm.emit(word);
    }

    fn mov_imm(&mut self, rd: Reg, value: u32) {
        let (low, high) = (value as u16, (value >> 16) as u16);
        if high == 0 {
            self.emit(movz(W, rd, low, 0));
        } else if low == 0 {
            self.emit(movz(W, rd, high, 16));
        } else {
            self.emit(movz(W, rd, low, 0));
            self.emit(movk(W, rd, high, 16));
        }
    }

    /// A register holding `value`, using `scratch` unless it is zero.
    fn imm(&mut self, value: u32, scratch: Reg) -> Reg {
        if value == 0 {
            return Reg::ZR;
        }
        self.mov_imm(scratch, value);
        scratch
    }

    fn site(&mut self) {
        self.sites.push((
            self.asm.offset(),
            Site {
                eip: self.eip,
                retired: self.retired,
                sense: self.sense,
                recipe: self.recipe,
            },
        ));
    }

    /// Makes x10 and x11 canonical for the current recipe; uses w17.
    fn materialize(&mut self) {
        use Recipe::*;
        let recipe = std::mem::replace(&mut self.recipe, Canonical);
        let value = |t: &mut Self, operand: Operand| match operand {
            Operand::Guest(index) => host(index),
            Operand::Temp(index) => temp(index),
            Operand::Imm(value) => t.imm(value, X17),
        };
        let (w10, w11) = (X10, X11);
        match recipe {
            Canonical => {}
            AddResult(r, b) | SubResult(r, b) => {
                let (r, b) = (value(self, r), value(self, b));
                self.emit(mov(W, w10, r));
                if matches!(recipe, AddResult(..)) {
                    self.emit(sub(W, w11, r, b, Lsl, 0));
                } else {
                    self.emit(add(W, w11, r, b, Lsl, 0));
                }
                self.emit(eor(W, w11, w11, b, Lsl, 0));
                self.emit(eor(W, w11, w11, r, Lsl, 0));
            }
            Add(a, b) | Sub(a, b) => {
                let (a, b) = (value(self, a), value(self, b));
                if matches!(recipe, Add(..)) {
                    self.emit(add(W, w10, a, b, Lsl, 0));
                } else {
                    self.emit(sub(W, w10, a, b, Lsl, 0));
                }
                self.emit(eor(W, w11, a, b, Lsl, 0));
                self.emit(eor(W, w11, w11, w10, Lsl, 0));
            }
            Logic(r) => {
                let r = value(self, r);
                self.emit_all(&[mov(W, w10, r), mov(W, w11, Reg::ZR)]);
            }
            And(a, b) | Or(a, b) | Xor(a, b) => {
                let (a, b) = (value(self, a), value(self, b));
                self.emit(match recipe {
                    And(..) => and(W, w10, a, b, Lsl, 0),
                    Or(..) => orr(W, w10, a, b, Lsl, 0),
                    _ => eor(W, w10, a, b, Lsl, 0),
                });
                self.emit(mov(W, w11, Reg::ZR));
            }
            Neg(r) => {
                let r = value(self, r);
                self.emit_all(&[
                    mov(W, w10, r),
                    sub(W, w11, Reg::ZR, r, Lsl, 0),
                    eor(W, w11, w11, r, Lsl, 0),
                ]);
            }
            Doubled(r) => {
                let r = value(self, r);
                self.emit_all(&[mov(W, w10, r), mov(W, w11, r)]);
            }
        }
    }

    fn emit_all(&mut self, words: &[u32]) {
        self.asm.emit_all(words);
    }

    /// Called before anything writes `operand` without redefining PF/AF.
    fn clobber(&mut self, operand: Operand) {
        if self.recipe.uses(operand) {
            self.materialize();
        }
    }

    /// A recipe temporary the current recipe does not read.
    fn take_temp(&self) -> u8 {
        if self.recipe.uses(Operand::Temp(0)) {
            1
        } else {
            0
        }
    }

    /// Flips C to subtraction sense; uses x17.
    fn canonical_carry(&mut self) {
        if self.sense == Sense::Subtraction {
            return;
        }
        if self.features.flagm {
            self.emit(CFINV);
        } else {
            self.emit_all(&[mrs_nzcv(X17), eor_imm(X, X17, X17, 1 << 29), msr_nzcv(X17)]);
        }
        self.sense = Sense::Subtraction;
    }

    /// Canonical flags, the charge for this path, then the exit.
    fn leave(&mut self, exit: Exit, target: Target) {
        self.canonical_carry();
        self.materialize();
        if self.retired != 0 {
            self.emit(sub_imm(X, X9, X9, self.retired, false));
        }
        let link = exit == Exit::Dispatch && matches!(target, Target::Static(_));
        self.exit(exit, target, link);
    }

    /// The exit protocol of `runtime`: w0 the exit, w1 the EIP and x2 the
    /// link point the owner may patch into a branch to the target, else 0.
    fn exit(&mut self, exit: Exit, target: Target, link: bool) {
        let point = self.asm.offset();
        self.emit(movz(W, Reg::new(0), exit as u16, 0));
        match target {
            Target::Static(eip) => {
                self.emit(movz(W, Reg::new(1), eip as u16, 0));
                self.emit(movk(W, Reg::new(1), (eip >> 16) as u16, 16));
            }
            Target::Dynamic => self.emit(mov(W, Reg::new(1), X16)),
        }
        if link {
            let here = self.asm.offset();
            self.emit(adr(Reg::new(2), point as i32 - here as i32));
        } else {
            self.emit(movz(X, Reg::new(2), 0, 0));
        }
        self.exits.push(self.asm.offset() / 4);
        self.emit(b(0));
    }

    /// The register a 32-bit result for guest `index` is computed into.
    fn result(index: u8) -> Reg {
        if biased(index) { X16 } else { host(index) }
    }

    /// Commits a result computed into `Self::result(index)`.
    fn commit(&mut self, index: u8) {
        if biased(index) {
            self.emit(add_ext(X, host(index), WINDOW, X16, Uxtw, 0));
        }
    }

    /// The guest address of `mem` in a W register; uses w16 and w17.
    fn effective(&mut self, mem: Mem) -> Reg {
        let mut current = mem.base.map(host);
        if let Some(index) = mem.index {
            let shift = mem.scale.trailing_zeros();
            current = Some(match current {
                Some(base) => {
                    self.emit(add(W, X16, base, host(index), Lsl, shift));
                    X16
                }
                None if shift == 0 => host(index),
                None => {
                    self.emit(lsl_imm(W, X16, host(index), shift));
                    X16
                }
            });
        }
        let disp = mem.disp;
        match current {
            None => self.imm(disp, X16),
            Some(base) if disp == 0 => base,
            Some(base) if disp < 4096 => {
                self.emit(add_imm(W, X16, base, disp, false));
                X16
            }
            Some(base) if disp.wrapping_neg() < 4096 => {
                self.emit(sub_imm(W, X16, base, disp.wrapping_neg(), false));
                X16
            }
            Some(base) => {
                self.mov_imm(X17, disp);
                self.emit(add(W, X16, base, X17, Lsl, 0));
                X16
            }
        }
    }

    /// An address for a 32-bit access to `mem`.
    fn address(&mut self, mem: Mem) -> Addr {
        if let (Some(base @ (ESP | EBP)), None) = (mem.base, mem.index) {
            // Offsets well inside the guards: an access past 0 or 4 GiB
            // faults there and the interpreter wraps it exactly.
            let disp = mem.disp as i32;
            if (0..16384).contains(&disp) && disp % 4 == 0 || (-256..256).contains(&disp) {
                return Addr::Biased(host(base), disp);
            }
        }
        Addr::Window(self.effective(mem))
    }

    fn load(&mut self, addr: Addr, rt: Reg) {
        self.site();
        self.emit(match addr {
            Addr::Window(a) => ldr_reg(W, rt, WINDOW, a, false),
            Addr::Biased(b, d) if d >= 0 && d % 4 == 0 => ldr(W, rt, b, d as u32),
            Addr::Biased(b, d) => ldur(W, rt, b, d),
        });
    }

    fn store(&mut self, addr: Addr, rt: Reg) {
        self.site();
        self.emit(match addr {
            Addr::Window(a) => str_reg(W, rt, WINDOW, a, false),
            Addr::Biased(b, d) if d >= 0 && d % 4 == 0 => str(W, rt, b, d as u32),
            Addr::Biased(b, d) => stur(W, rt, b, d),
        });
    }

    /// The A64 condition for `cc` in the current sense, flipping to
    /// subtraction sense where no single condition reads CF as needed.
    fn condition(&mut self, cc: ConditionCode) -> Cond {
        use ConditionCode as C;
        let addition = self.sense == Sense::Addition;
        match cc {
            C::o => Cond::Vs,
            C::no => Cond::Vc,
            C::e => Cond::Eq,
            C::ne => Cond::Ne,
            C::s => Cond::Mi,
            C::ns => Cond::Pl,
            C::l => Cond::Lt,
            C::ge => Cond::Ge,
            C::le => Cond::Le,
            C::g => Cond::Gt,
            C::b if addition => Cond::Hs,
            C::b => Cond::Lo,
            C::ae if addition => Cond::Lo,
            C::ae => Cond::Hs,
            C::be | C::a => {
                self.canonical_carry();
                if cc == C::be { Cond::Ls } else { Cond::Hi }
            }
            C::p | C::np | C::None => unreachable!("filtered by `supported`"),
        }
    }

    fn op(&mut self, op: Op, next: u32) -> Flow {
        match op {
            Op::Nop => {}
            Op::Mov(Src::Reg(dst), source) => {
                self.clobber(Operand::Guest(dst));
                match source {
                    Src::Reg(source) if biased(dst) => {
                        self.emit(add_ext(X, host(dst), WINDOW, host(source), Uxtw, 0));
                    }
                    Src::Reg(source) => self.emit(mov(W, host(dst), host(source))),
                    Src::Imm(value) if biased(dst) => {
                        let value = self.imm(value, X16);
                        self.emit(add_ext(X, host(dst), WINDOW, value, Uxtw, 0));
                    }
                    Src::Imm(value) => self.mov_imm(host(dst), value),
                    Src::Mem(mem) => {
                        let addr = self.address(mem);
                        self.load(addr, Self::result(dst));
                        self.commit(dst);
                    }
                }
            }
            Op::Mov(Src::Mem(mem), source) => {
                let value = match source {
                    // The low halves of x23 and x24 are ESP and EBP.
                    Src::Reg(source) => host(source),
                    Src::Imm(value) => self.imm(value, X6),
                    Src::Mem(_) => unreachable!(),
                };
                let addr = self.address(mem);
                self.store(addr, value);
            }
            Op::Mov(Src::Imm(_), _) => unreachable!(),
            Op::Lea(dst, mem) => {
                self.clobber(Operand::Guest(dst));
                let address = self.effective(mem);
                if biased(dst) {
                    self.emit(add_ext(X, host(dst), WINDOW, address, Uxtw, 0));
                } else if address != host(dst) {
                    self.emit(mov(W, host(dst), address));
                }
            }
            Op::Alu(alu, Src::Reg(dst), source) => self.alu_register(alu, dst, source),
            Op::Alu(alu, Src::Mem(mem), source) => self.alu_memory(alu, mem, source),
            Op::Alu(_, Src::Imm(_), _) => unreachable!(),
            Op::Step(increment, index) => {
                let (result, source) = (Self::result(index), host(index));
                let step = |rd| {
                    if increment {
                        adds_imm(W, rd, source, 1, false)
                    } else {
                        subs_imm(W, rd, source, 1, false)
                    }
                };
                // INC and DEC keep CF: restore C around the flag-setting step.
                if self.features.flagm {
                    self.emit_all(&[cset(W, X17, Cond::Hs), step(result), rmif(X17, 63, 2)]);
                } else {
                    self.emit_all(&[
                        mrs_nzcv(X17),
                        step(result),
                        mrs_nzcv(X6),
                        lsr_imm(X, X17, X17, 29),
                        bfi(X, X6, X17, 29, 1),
                        msr_nzcv(X6),
                    ]);
                }
                self.commit(index);
                self.recipe = if increment {
                    Recipe::AddResult(Operand::Guest(index), Operand::Imm(1))
                } else {
                    Recipe::SubResult(Operand::Guest(index), Operand::Imm(1))
                };
            }
            Op::Neg(index) => {
                self.emit(subs(W, Self::result(index), Reg::ZR, host(index), Lsl, 0));
                self.commit(index);
                self.sense = Sense::Subtraction;
                self.recipe = Recipe::Neg(Operand::Guest(index));
            }
            Op::Not(index) => {
                self.clobber(Operand::Guest(index));
                self.emit(mvn(W, Self::result(index), host(index)));
                self.commit(index);
            }
            Op::Extend(dst, source, signed) => self.extend(dst, source, signed),
            Op::Cdq => {
                self.clobber(Operand::Guest(2));
                self.emit(sbfx(W, host(2), host(0), 31, 1));
            }
            Op::Push(source) => {
                self.clobber(Operand::Guest(ESP));
                let value = match source {
                    Src::Reg(ESP) => {
                        self.emit(mov(W, X6, host(ESP)));
                        X6
                    }
                    Src::Reg(index) => host(index),
                    Src::Imm(value) => self.imm(value, X6),
                    Src::Mem(mem) => {
                        let addr = self.address(mem);
                        self.load(addr, X6);
                        X6
                    }
                };
                self.push(value);
            }
            Op::Pop(index) => {
                self.clobber(Operand::Guest(ESP));
                self.clobber(Operand::Guest(index));
                self.site();
                self.emit(ldr_indexed(W, Post, Self::result(index), host(ESP), 4));
                self.commit(index);
            }
            Op::Jmp(target) => return Flow::Jump(target),
            Op::Call(target) => {
                self.clobber(Operand::Guest(ESP));
                let value = self.imm(next, X6);
                self.push(value);
                return Flow::Jump(target);
            }
            Op::Jcc(cc, target) => {
                let cond = self.condition(cc);
                let label = self.asm.label();
                self.cold.push(Cold {
                    label,
                    sense: self.sense,
                    recipe: self.recipe,
                    target,
                    retired: self.retired + 1,
                });
                self.asm.b_cond(cond, label);
            }
            Op::Ret(release) => {
                self.clobber(Operand::Guest(ESP));
                self.site();
                self.emit(ldr_indexed(W, Post, X16, host(ESP), 4));
                if release != 0 {
                    self.emit(add_imm(X, host(ESP), host(ESP), u32::from(release), false));
                }
                self.end_dynamic();
                return Flow::End;
            }
            Op::JmpIndirect(source) => {
                self.target_to_w16(source);
                self.end_dynamic();
                return Flow::End;
            }
            Op::CallIndirect(source) => {
                self.clobber(Operand::Guest(ESP));
                self.target_to_w16(source);
                let value = self.imm(next, X6);
                self.push(value);
                self.end_dynamic();
                return Flow::End;
            }
            Op::Setcc(cc, byte) => {
                let (index, lsb) = if byte < 4 { (byte, 0) } else { (byte - 4, 8) };
                let cond = self.condition(cc);
                self.clobber(Operand::Guest(index));
                self.emit_all(&[cset(W, X17, cond), bfi(W, host(index), X17, lsb, 8)]);
            }
            Op::Cmov(cc, dst, source) => {
                let value = match source {
                    Src::Reg(index) => host(index),
                    Src::Mem(mem) => {
                        // The load happens whatever the condition.
                        let addr = self.address(mem);
                        self.load(addr, X6);
                        X6
                    }
                    Src::Imm(_) => unreachable!(),
                };
                let cond = self.condition(cc);
                self.clobber(Operand::Guest(dst));
                self.emit(csel(W, host(dst), value, host(dst), cond));
            }
        }
        Flow::Next
    }

    fn push(&mut self, value: Reg) {
        self.site();
        self.emit(str_indexed(W, Pre, value, host(ESP), -4));
    }

    fn target_to_w16(&mut self, source: Src) {
        match source {
            Src::Reg(index) => self.emit(mov(W, X16, host(index))),
            Src::Mem(mem) => {
                let addr = self.address(mem);
                self.load(addr, X16);
            }
            Src::Imm(_) => unreachable!(),
        }
    }

    fn end_dynamic(&mut self) {
        self.retired += 1;
        self.leave(Exit::Dispatch, Target::Dynamic);
    }

    fn extend(&mut self, dst: u8, source: Narrow8or16, signed: bool) {
        let field = |rd, rn, lsb, width| {
            if signed {
                sbfx(W, rd, rn, lsb, width)
            } else {
                ubfx(W, rd, rn, lsb, width)
            }
        };
        let result = Self::result(dst);
        match source {
            Narrow8or16::Byte(byte) => {
                let (index, lsb) = if byte < 4 { (byte, 0) } else { (byte - 4, 8) };
                self.clobber(Operand::Guest(dst));
                self.emit(field(result, host(index), lsb, 8));
            }
            Narrow8or16::Word(index) => {
                self.clobber(Operand::Guest(dst));
                self.emit(field(result, host(index), 0, 16));
            }
            Narrow8or16::Mem(mem, half) => {
                self.clobber(Operand::Guest(dst));
                let address = self.effective(mem);
                let narrow = match (half, signed) {
                    (false, false) => Narrow::Byte,
                    (false, true) => Narrow::SignedByte,
                    (true, false) => Narrow::Half,
                    (true, true) => Narrow::SignedHalf,
                };
                self.site();
                self.emit(ldr_narrow(narrow, result, WINDOW, address));
            }
        }
        self.commit(dst);
    }

    /// `alu reg, source`.
    fn alu_register(&mut self, alu: Alu, dst: u8, source: Src) {
        let left = host(dst);
        let result = Self::result(dst);
        // The right operand as a register and as a recipe operand.
        let (right, operand) = match source {
            Src::Reg(index) => (Some(host(index)), Operand::Guest(index)),
            Src::Imm(value) => (None, Operand::Imm(value)),
            Src::Mem(mem) => {
                let t = self.take_temp();
                let addr = self.address(mem);
                self.load(addr, temp(t));
                (Some(temp(t)), Operand::Temp(t))
            }
        };
        let imm = match source {
            Src::Imm(value) => value,
            _ => 0,
        };
        let right = |t: &mut Self| right.unwrap_or_else(|| t.imm(imm, X17));
        let guest = Operand::Guest(dst);
        match alu {
            Alu::Add | Alu::Sub | Alu::Cmp => {
                let subtract = alu != Alu::Add;
                let rd = if alu == Alu::Cmp { Reg::ZR } else { result };
                let negated = imm.wrapping_neg();
                let word = if matches!(source, Src::Imm(_)) && imm < 4096 {
                    if subtract {
                        subs_imm(W, rd, left, imm, false)
                    } else {
                        adds_imm(W, rd, left, imm, false)
                    }
                } else if matches!(source, Src::Imm(_)) && imm != 0 && negated < 4096 {
                    // x + (2^32 - n) carries exactly when x - n does not
                    // borrow, so the sense is unchanged.
                    if subtract {
                        adds_imm(W, rd, left, negated, false)
                    } else {
                        subs_imm(W, rd, left, negated, false)
                    }
                } else {
                    let right = right(self);
                    if subtract {
                        subs(W, rd, left, right, Lsl, 0)
                    } else {
                        adds(W, rd, left, right, Lsl, 0)
                    }
                };
                self.emit(word);
                self.sense = if subtract {
                    Sense::Subtraction
                } else {
                    Sense::Addition
                };
                // The result overwrites a right operand that is `dst` itself.
                self.recipe = match alu {
                    Alu::Add if operand == guest => Recipe::Doubled(guest),
                    Alu::Sub if operand == guest => Recipe::Logic(guest),
                    Alu::Add => Recipe::AddResult(guest, operand),
                    Alu::Sub => Recipe::SubResult(guest, operand),
                    _ => Recipe::Sub(guest, operand),
                };
                if alu != Alu::Cmp {
                    self.commit(dst);
                }
            }
            Alu::And | Alu::Test => {
                let rd = if alu == Alu::Test { Reg::ZR } else { result };
                let word = match source {
                    Src::Imm(value) if bitmask(W, u64::from(value)).is_some() => {
                        ands_imm(W, rd, left, u64::from(value))
                    }
                    _ => {
                        let right = right(self);
                        ands(W, rd, left, right, Lsl, 0)
                    }
                };
                self.emit(word);
                self.sense = Sense::Addition;
                if alu == Alu::And {
                    self.recipe = Recipe::Logic(guest);
                    self.commit(dst);
                } else {
                    self.recipe = Recipe::And(guest, operand);
                }
            }
            Alu::Or | Alu::Xor => {
                let word = match source {
                    Src::Imm(value) if bitmask(W, u64::from(value)).is_some() => {
                        if alu == Alu::Or {
                            orr_imm(W, result, left, u64::from(value))
                        } else {
                            eor_imm(W, result, left, u64::from(value))
                        }
                    }
                    _ => {
                        let right = right(self);
                        if alu == Alu::Or {
                            orr(W, result, left, right, Lsl, 0)
                        } else {
                            eor(W, result, left, right, Lsl, 0)
                        }
                    }
                };
                // The write and the flags are separate host instructions with
                // no site between them.
                self.emit_all(&[word, tst(W, result, result, Lsl, 0)]);
                self.sense = Sense::Addition;
                self.recipe = Recipe::Logic(guest);
                self.commit(dst);
            }
        }
    }

    /// `alu [mem], source`: load, compute, store, then the flags (§5.2).
    fn alu_memory(&mut self, alu: Alu, mem: Mem, source: Src) {
        let t = self.take_temp();
        // The address first: it may use w17, and the result goes to w6.
        let addr = self.address(mem);
        let (right, operand) = match source {
            Src::Reg(index) => (host(index), Operand::Guest(index)),
            Src::Imm(value) => (self.imm(value, X17), Operand::Imm(value)),
            Src::Mem(_) => unreachable!(),
        };
        let left = temp(t);
        self.load(addr, left);
        let stored = match alu {
            Alu::Add => Some(add(W, X6, left, right, Lsl, 0)),
            Alu::Sub => Some(sub(W, X6, left, right, Lsl, 0)),
            Alu::And => Some(and(W, X6, left, right, Lsl, 0)),
            Alu::Or => Some(orr(W, X6, left, right, Lsl, 0)),
            Alu::Xor => Some(eor(W, X6, left, right, Lsl, 0)),
            Alu::Cmp | Alu::Test => None,
        };
        if let Some(word) = stored {
            self.emit(word);
            self.store(addr, X6);
        }
        let left_operand = Operand::Temp(t);
        let (word, sense, recipe) = match alu {
            Alu::Add => (
                cmn(W, left, right, Lsl, 0),
                Sense::Addition,
                Recipe::Add(left_operand, operand),
            ),
            Alu::Sub | Alu::Cmp => (
                cmp(W, left, right, Lsl, 0),
                Sense::Subtraction,
                Recipe::Sub(left_operand, operand),
            ),
            Alu::And | Alu::Test => (
                tst(W, left, right, Lsl, 0),
                Sense::Addition,
                Recipe::And(left_operand, operand),
            ),
            Alu::Or => (
                tst(W, X6, X6, Lsl, 0),
                Sense::Addition,
                Recipe::Or(left_operand, operand),
            ),
            Alu::Xor => (
                tst(W, X6, X6, Lsl, 0),
                Sense::Addition,
                Recipe::Xor(left_operand, operand),
            ),
        };
        self.emit(word);
        self.sense = sense;
        self.recipe = recipe;
    }

    fn finish(
        mut self,
        at: usize,
        exit: usize,
        code: Vec<(u32, u32)>,
        retired: u32,
        refuse: Label,
    ) -> Unit {
        // Not enough countdown for the whole unit: leave before it starts.
        self.asm.bind(refuse);
        // Not linkable: a branch here would bypass the safepoint.
        let entry = self.entry;
        self.exit(Exit::Dispatch, Target::Static(entry), false);
        for cold in std::mem::take(&mut self.cold) {
            self.asm.bind(cold.label);
            self.sense = cold.sense;
            self.recipe = cold.recipe;
            self.retired = cold.retired;
            self.leave(Exit::Dispatch, Target::Static(cold.target));
        }
        let exits = std::mem::take(&mut self.exits);
        let mut words = self.asm.finish();
        words[0] = sub_imm(X, X16, X9, retired, false);
        for index in exits {
            let from = at + 4 * index;
            words[index] = b((exit as i64 - from as i64) as i32);
        }
        Unit {
            words,
            sites: self.sites,
            retired,
            code,
        }
    }
}

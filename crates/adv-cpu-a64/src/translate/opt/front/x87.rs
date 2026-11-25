use super::{
    ConditionCode, FlagKind, FpOp, FpUnary, Front, IntOp, Location, Mem, Op, Part, Recipe, Reject,
    Source, Type, Unit, Value, decode, model,
};
use crate::translate::front::forms::X87 as Instruction;
use crate::translate::front::tags::{Code, Register};
use adv_cpu_core::floating::fast::{Fast, Source as Operand};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ValueX87 {
    Fcomi { index: u8, ordered: bool, pop: bool },
    Fcmov { index: u8, cc: ConditionCode },
    Ftst,
}

/// Recognizes `Op::Value` x87 instructions (`fcomi*`/`fucomi*`/`fcmov*`/`ftst`)
/// using the trace's prepared `ValueInstruction` form and register summary.
pub(super) fn value_x87(unit: &Unit, index: usize) -> Option<ValueX87> {
    let decode::Op::Value(h) = unit.trace.insts.get(index)?.op else {
        return None;
    };
    let form = unit
        .trace
        .helpers
        .get(usize::from(h))?
        .value
        .as_ref()?
        .decoded_form();
    if form == Code::Ftst as u16 {
        return Some(ValueX87::Ftst);
    }
    let st_index = || -> Option<u8> {
        let summary = unit.trace.x86.get(index)?.as_deref()?;
        let mut other = None;
        for used in &summary.registers {
            let r = used.register;
            if r >= Register::ST1 && r <= Register::ST7 {
                other = Some((r as u32 - Register::ST0 as u32) as u8);
            }
        }
        Some(other.unwrap_or(0))
    };
    Some(if form == Code::Fcomi_st0_sti as u16 {
        ValueX87::Fcomi {
            index: st_index()?,
            ordered: true,
            pop: false,
        }
    } else if form == Code::Fcomip_st0_sti as u16 {
        ValueX87::Fcomi {
            index: st_index()?,
            ordered: true,
            pop: true,
        }
    } else if form == Code::Fucomi_st0_sti as u16 {
        ValueX87::Fcomi {
            index: st_index()?,
            ordered: false,
            pop: false,
        }
    } else if form == Code::Fucomip_st0_sti as u16 {
        ValueX87::Fcomi {
            index: st_index()?,
            ordered: false,
            pop: true,
        }
    } else if form == Code::Fcmovb_st0_sti as u16 {
        ValueX87::Fcmov {
            index: st_index()?,
            cc: ConditionCode::b,
        }
    } else if form == Code::Fcmovnb_st0_sti as u16 {
        ValueX87::Fcmov {
            index: st_index()?,
            cc: ConditionCode::ae,
        }
    } else if form == Code::Fcmove_st0_sti as u16 {
        ValueX87::Fcmov {
            index: st_index()?,
            cc: ConditionCode::e,
        }
    } else if form == Code::Fcmovne_st0_sti as u16 {
        ValueX87::Fcmov {
            index: st_index()?,
            cc: ConditionCode::ne,
        }
    } else if form == Code::Fcmovbe_st0_sti as u16 {
        ValueX87::Fcmov {
            index: st_index()?,
            cc: ConditionCode::be,
        }
    } else if form == Code::Fcmovnbe_st0_sti as u16 {
        ValueX87::Fcmov {
            index: st_index()?,
            cc: ConditionCode::a,
        }
    } else if form == Code::Fcmovu_st0_sti as u16 {
        ValueX87::Fcmov {
            index: st_index()?,
            cc: ConditionCode::p,
        }
    } else if form == Code::Fcmovnu_st0_sti as u16 {
        ValueX87::Fcmov {
            index: st_index()?,
            cc: ConditionCode::np,
        }
    } else {
        return None;
    })
}

fn is_unordered_compare(unit: &Unit, helper: u8) -> bool {
    let Some(form) = unit
        .trace
        .helpers
        .get(usize::from(helper))
        .and_then(|h| h.value.as_ref())
        .map(|v| v.decoded_form())
    else {
        return false;
    };
    form == Code::Fucom_st0_sti as u16
        || form == Code::Fucomp_st0_sti as u16
        || form == Code::Fucompp as u16
}

impl Front<'_> {
    pub(super) fn arguments(&mut self, to: usize) -> Result<Vec<Value>, Reject> {
        let live = &self.live.entry[to];
        let mut args: Vec<Value> = (0..8)
            .filter(|&r| live.reg(r))
            .map(|r| self.state.regs[r])
            .chain([self.state.flags])
            .chain(
                (0..8)
                    .filter(|&r| live.reg(8 + r))
                    .map(|r| self.state.xmm[r]),
            )
            .chain(
                (0..8)
                    .filter(|&r| live.reg(16 + r))
                    .map(|r| self.state.mmx[r]),
            )
            .chain(self.state.x87.into_iter().flatten())
            .collect();
        for start in self.held[to].clone() {
            let ty = if self.float_cell(to, start) {
                Type::F64
            } else {
                Source::Frame {
                    offset: start,
                    width: self.cells[&start],
                }
                .ty()
            };
            let value = self.cell_value(start, ty);
            args.push(value);
        }
        for &k in self.models[to].floats.clone().iter() {
            if !self.float_cell(to, k) {
                args.push(self.state.floats[&k]);
            }
        }
        let ip = self.location(self.state.ip);
        let opcode = self.location(self.state.opcode);
        args.extend([ip, opcode]);
        if let model::C1::Round(op, single) = self.models[to].c1 {
            let synthetic = |op: FpOp, single: bool| {
                let eps = if single {
                    f64::from(f32::EPSILON)
                } else {
                    f64::EPSILON
                };
                match op {
                    FpOp::Add => (1.0, 0.75 * eps),
                    FpOp::Sub => (1.0, -0.75 * eps),
                    FpOp::Mul => (1.5, 1.0 + eps),
                    FpOp::Div => (1.0, 10.0),
                }
            };
            let pair = match self.state.c1 {
                Some(Location::Recipe(Recipe::C1 {
                    op: from,
                    single: narrow,
                    a,
                    b,
                })) if from == op && single == narrow => [a, b],
                Some(Location::Constant(c)) => {
                    let (a, b_up) = synthetic(op, single);
                    let b = if c != 0 { b_up } else { 1.0 };
                    debug_assert_eq!(super::super::eval::rounded_up(op, a, b, single), c != 0);
                    [
                        self.constant(Type::F64, u128::from(a.to_bits())),
                        self.constant(Type::F64, u128::from(b.to_bits())),
                    ]
                }
                Some(Location::At(cond)) => {
                    let (a, b_up) = synthetic(op, single);
                    debug_assert!(super::super::eval::rounded_up(op, a, b_up, single));
                    debug_assert!(!super::super::eval::rounded_up(op, a, 1.0, single));
                    let a = self.constant(Type::F64, u128::from(a.to_bits()));
                    let b_true = self.constant(Type::F64, u128::from(b_up.to_bits()));
                    let b_false = self.constant(Type::F64, u128::from(1.0f64.to_bits()));
                    let b = self.value(Op::Select {
                        cond,
                        a: b_true,
                        b: b_false,
                    });
                    [a, b]
                }
                _ => return Err(Reject::StaticX87(to)),
            };
            args.extend(pair);
        }
        if self.models[to].c1 == model::C1::Value {
            let loc = self.state.c1.ok_or(Reject::StaticX87(to))?;
            let val = if self.models[to].has_cc {
                self.pack_status_value(loc)
            } else {
                self.c1_value(loc)
            };
            args.push(val);
        }
        if self.assumed.mmx && self.models[to].mmx_meta.1 != u32::MAX {
            args.push(self.state.mmx_meta);
        }
        Ok(args)
    }
    fn location(&mut self, loc: Location<Value>) -> Value {
        match loc {
            Location::Constant(n) => self.imm(n as u32),
            Location::At(v) => v,
            _ => unreachable!(),
        }
    }
    pub(super) fn c1_value(&mut self, loc: Location<Value>) -> Value {
        match loc {
            Location::Constant(n) => self.imm(n as u32),
            Location::At(v) => v,
            Location::Recipe(Recipe::C1 { op, a, b, single }) => self.value(Op::X87RoundC1 {
                op,
                a,
                b,
                pc: if single { 0 } else { 2 },
                rounding: 0,
            }),
            _ => unreachable!(),
        }
    }
    pub(super) fn x87(&mut self, x: Instruction) -> Result<(), Reject> {
        let mut metadata = true;
        if !matches!(
            x.fast,
            Fast::Compare { .. } | Fast::StoreStatus | Fast::StoreControl | Fast::LoadControl
        ) || x.flag_compare().is_some()
        {
            self.state.x87_last_cmp = None;
        }
        match x.fast {
            Fast::Load(src @ (Operand::F64 | Operand::F32 | Operand::I32 | Operand::St(_))) => {
                let value = self.x87_source(src, x.mem)?;
                self.x87_push(value)?;
                self.state.c1 = Some(Location::Constant(0));
            }
            Fast::Constant(one) => {
                let value = self.constant(
                    Type::F64,
                    u128::from(if one { 1.0f64.to_bits() } else { 0 }),
                );
                self.x87_push(value)?;
                self.state.c1 = Some(Location::Constant(0));
            }
            Fast::Store {
                width: 8,
                integer: false,
                pop,
            } => {
                let value = self.st(0)?;
                let mem = x.mem.ok_or(Reject::Unsupported(self.index))?;
                self.write(mem, 8, value)?;
                if let Some(offset) = self.frame_offset(mem) {
                    self.state.floats.insert(offset, value);
                }
                if pop {
                    self.x87_pop();
                }
                self.state.c1 = Some(Location::Constant(0));
            }
            Fast::Store {
                width,
                integer,
                pop,
            } => {
                let rounding = (self.state.control >> 10 & 3) as u8;
                if self.state.control & 0x3f != 0x3f {
                    return Err(Reject::Mode(self.index));
                }
                let value = self.st(0)?;
                let stored = self.value(Op::X87Store {
                    value,
                    width,
                    rounding,
                    integer,
                    proved: false,
                });
                let mem = x.mem.ok_or(Reject::Unsupported(self.index))?;
                self.write(mem, width, stored)?;
                if pop {
                    self.x87_pop();
                }
                self.state.c1 = Some(if rounding == 3 {
                    Location::Constant(0)
                } else {
                    Location::At(self.value(Op::X87StoreC1 {
                        value,
                        stored,
                        integer,
                    }))
                });
            }
            Fast::StoreSt { index, pop } => {
                let value = self.st(0)?;
                let slot = (self.state.top + index) & 7;
                self.state.x87[slot as usize] = Some(value);
                if pop {
                    self.x87_pop();
                }
                self.state.c1 = Some(Location::Constant(0));
            }
            Fast::Exchange(index) => {
                self.st(0)?;
                self.st(index)?;
                let slot = (self.state.top + index) & 7;
                self.state.x87.swap(self.state.top as usize, slot as usize);
                self.state.c1 = Some(Location::Constant(0));
            }
            Fast::Negate | Fast::Absolute => {
                let value = self.st(0)?;
                let result = self.value(Op::FpUnary(
                    if x.fast == Fast::Negate {
                        FpUnary::Neg
                    } else {
                        FpUnary::Abs
                    },
                    value,
                ));
                self.state.x87[self.state.top as usize] = Some(result);
                self.state.c1 = Some(Location::Constant(0));
            }
            Fast::Arith {
                op,
                dest,
                source: src @ (Operand::F64 | Operand::F32 | Operand::St(_)),
                reverse,
                pop,
            } => {
                if self.state.control & 0x3f != 0x3f {
                    return Err(Reject::Mode(self.index));
                }
                let pc = ((self.state.control >> 8) & 3) as u8;
                let rounding = ((self.state.control >> 10) & 3) as u8;
                if pc == 1 {
                    return Err(Reject::Mode(self.index));
                }
                let a = self.st(dest)?;
                let b = self.x87_source(src, x.mem)?;
                let (a, b) = if reverse { (b, a) } else { (a, b) };
                let op = model::fp(op);
                let result = if rounding == 0 && pc == 0 {
                    self.value(Op::X87Single(op, a, b, 0))
                } else if rounding == 0 && pc == 2 {
                    self.value(Op::Fp(op, a, b, false))
                } else {
                    self.value(Op::X87Arith {
                        op,
                        a,
                        b,
                        pc,
                        rounding,
                        proved: false,
                    })
                };
                let slot = (self.state.top + dest) & 7;
                self.state.x87[slot as usize] = Some(result);
                self.state.c1 = Some(if pc == 3 {
                    Location::Constant(0)
                } else if rounding == 0 {
                    Location::Recipe(Recipe::C1 {
                        op,
                        a,
                        b,
                        single: pc == 0,
                    })
                } else if rounding == 3 {
                    Location::Constant(0)
                } else {
                    Location::At(self.value(Op::X87RoundC1 {
                        op,
                        a,
                        b,
                        pc,
                        rounding,
                    }))
                });
                if pop {
                    self.x87_pop();
                }
            }
            Fast::Compare {
                source: src @ (Operand::F64 | Operand::F32 | Operand::St(_)),
                pops,
            } => {
                if self.state.control & 0x3f != 0x3f {
                    return Err(Reject::Mode(self.index));
                }
                let a = self.st(0)?;
                let b = self.x87_source(src, x.mem)?;
                if let Some(quiet) = x.flag_compare() {
                    // `fcomi`/`fcomip`/`fucomi`/`fucomip`: write EFLAGS (ZF/PF/CF),
                    // preserving x87 C0–C3 and C1.
                    self.state.flags = self.value(Op::Flags {
                        kind: if quiet {
                            FlagKind::UnorderedCompare
                        } else {
                            FlagKind::OrderedCompare
                        },
                        width: 10,
                        a,
                        b,
                        prior: Some(self.state.flags),
                    });
                    for _ in 0..pops {
                        self.st(0)?;
                        self.x87_pop();
                    }
                    self.state.x87_last_cmp = None;
                } else {
                    let ordered = !is_unordered_compare(self.unit, x.helper);
                    let cc = self.value(Op::X87Compare { a, b, ordered });
                    self.state.x87_cc = Some(cc);
                    self.state.c1 = Some(Location::Constant(0));
                    for _ in 0..pops {
                        self.st(0)?;
                        self.x87_pop();
                    }
                    self.state.x87_last_cmp = Some((a, b, ordered, self.state.top));
                }
            }
            Fast::StoreStatus => {
                metadata = false;
                let base_bits =
                    (u32::from(self.at.x87.status) & !0x7f00) | (u32::from(self.state.top) << 11);
                let cc = match self.state.x87_cc {
                    Some(v) => {
                        let mask = self.imm(0x4500);
                        self.int(IntOp::And, v, mask)
                    }
                    None => self.imm(u32::from(self.at.x87.status & 0x4500)),
                };
                let base = self.imm(base_bits);
                let mut status = self.int(IntOp::Or, base, cc);
                if let Some(c1_loc) = self.state.c1 {
                    let c1_raw = self.c1_value(c1_loc);
                    let one = self.imm(1);
                    let c1_bit = self.int(IntOp::And, c1_raw, one);
                    let nine = self.imm(9);
                    let shifted = self.int(IntOp::Shl, c1_bit, nine);
                    status = self.int(IntOp::Or, status, shifted);
                }
                if let Some(mem) = x.mem {
                    self.write(mem, 2, status)?;
                } else {
                    self.put_part(Part::Reg(0), 2, status)?;
                    // Record that AX/AH now holds this compare's status word for `sahf` folding.
                    self.state.ax_x87_cmp = self.state.x87_last_cmp;
                }
            }
            Fast::Round => {
                if self.state.control & 0x3f != 0x3f {
                    return Err(Reject::Mode(self.index));
                }
                let rounding = ((self.state.control >> 10) & 3) as u8;
                let a = self.st(0)?;
                let result = self.value(Op::X87Round {
                    value: a,
                    rounding,
                    denormal: !self.assumed.denormal,
                    proved: false,
                });
                self.state.x87[self.state.top as usize] = Some(result);
                self.state.c1 = Some(if rounding == 3 {
                    Location::Constant(0)
                } else {
                    let abs_a = self.value(Op::FpUnary(FpUnary::Abs, a));
                    let abs_r = self.value(Op::FpUnary(FpUnary::Abs, result));
                    let cmp = self.value(Op::Flags {
                        kind: FlagKind::UnorderedCompare,
                        width: 8,
                        a: abs_a,
                        b: abs_r,
                        prior: None,
                    });
                    Location::At(self.value(Op::Test {
                        flags: cmp,
                        cond: ConditionCode::b,
                    }))
                });
            }
            Fast::Sqrt => {
                if self.state.control & 0x3f != 0x3f {
                    return Err(Reject::Mode(self.index));
                }
                let pc = ((self.state.control >> 8) & 3) as u8;
                let rounding = ((self.state.control >> 10) & 3) as u8;
                if pc == 1 {
                    return Err(Reject::Mode(self.index));
                }
                let a = self.st(0)?;
                let result = self.value(Op::X87Sqrt {
                    value: a,
                    pc,
                    rounding,
                    proved: false,
                });
                self.state.x87[self.state.top as usize] = Some(result);
                self.state.c1 = Some(if pc == 3 {
                    Location::Constant(0)
                } else {
                    Location::At(self.value(Op::X87SqrtC1 { input: a, result }))
                });
            }
            Fast::Examine => {
                let a = self.st(0)?;
                let exam = self.value(Op::X87Exam(a));
                let cc_mask = self.imm(0x4500);
                let cc = self.int(IntOp::And, exam, cc_mask);
                let nine = self.imm(9);
                let shifted = self.int(IntOp::Shr, exam, nine);
                let one = self.imm(1);
                let c1 = self.int(IntOp::And, shifted, one);
                self.state.x87_cc = Some(cc);
                self.state.c1 = Some(Location::At(c1));
                self.state.x87_last_cmp = None;
            }
            Fast::StoreControl => {
                metadata = false;
                let value = self.imm(u32::from(self.state.control));
                self.write(x.mem.ok_or(Reject::Unsupported(self.index))?, 2, value)?;
            }
            Fast::LoadControl => {
                metadata = false;
                let value = self.models[self.index]
                    .load(
                        x.mem.ok_or(Reject::Unsupported(self.index))?,
                        2,
                        &self.offsets[self.index],
                    )
                    .ok_or(Reject::Mode(self.index))? as u16;
                if value & 0x3f != 0x3f || self.at.x87.status & 0x8000 != 0 {
                    return Err(Reject::Mode(self.index));
                }
                self.state.control = value;
            }
            _ => return Err(Reject::Unsupported(self.index)),
        }
        if metadata {
            self.x87_metadata(x.opcode);
        }
        self.pack_x87_status();
        Ok(())
    }

    /// Packs dynamic `x87_cc` (`C3|C2|C0`) and `c1` into `self.state.c1` with bit 16 set
    /// so records and exits carry both without extra `Recipe` variants.
    fn pack_x87_status(&mut self) {
        if self.state.x87_cc.is_some()
            && let Some(c1_loc) = self.state.c1
        {
            let packed = self.pack_status_value(c1_loc);
            self.state.c1 = Some(Location::At(packed));
        }
    }

    fn pack_status_value(&mut self, c1_loc: Location<Value>) -> Value {
        let cc = match self.state.x87_cc {
            Some(v) => {
                let mask = self.imm(0x4500);
                self.int(IntOp::And, v, mask)
            }
            None => self.imm(u32::from(self.at.x87.status & 0x4500)),
        };
        let c1_raw = self.c1_value(c1_loc);
        let one = self.imm(1);
        let c1_bit = self.int(IntOp::And, c1_raw, one);
        let tag = self.imm(1 << 16);
        let with_tag = self.int(IntOp::Or, cc, tag);
        self.int(IntOp::Or, with_tag, c1_bit)
    }

    pub(super) fn x87_value(&mut self, vx: ValueX87) -> Result<(), Reject> {
        if self.state.control & 0x3f != 0x3f {
            return Err(Reject::Mode(self.index));
        }
        if !matches!(vx, ValueX87::Ftst) {
            self.state.x87_last_cmp = None;
        }
        match vx {
            ValueX87::Fcomi {
                index,
                ordered,
                pop,
            } => {
                let a = self.st(0)?;
                let b = self.st(index)?;
                self.state.flags = self.value(Op::Flags {
                    kind: if ordered {
                        FlagKind::OrderedCompare
                    } else {
                        FlagKind::UnorderedCompare
                    },
                    width: 10,
                    a,
                    b,
                    prior: Some(self.state.flags),
                });
                if pop {
                    self.x87_pop();
                }
            }
            ValueX87::Fcmov { index, cc } => {
                let dst = self.st(0)?;
                let src = self.st(index)?;
                let cond = self.value(Op::Test {
                    flags: self.state.flags,
                    cond: cc,
                });
                let result = self.value(Op::Select {
                    cond,
                    a: src,
                    b: dst,
                });
                self.state.x87[self.state.top as usize] = Some(result);
                self.state.c1 = Some(Location::Constant(0));
            }
            ValueX87::Ftst => {
                let a = self.st(0)?;
                let zero = self.constant(Type::F64, 0);
                let cc = self.value(Op::X87Compare {
                    a,
                    b: zero,
                    ordered: true,
                });
                self.state.x87_cc = Some(cc);
                self.state.c1 = Some(Location::Constant(0));
                self.state.x87_last_cmp = Some((a, zero, true, self.state.top));
            }
        }
        self.x87_metadata(None);
        self.pack_x87_status();
        Ok(())
    }
    fn st(&self, index: u8) -> Result<Value, Reject> {
        self.state.x87[((self.state.top + index) & 7) as usize].ok_or(Reject::StaticX87(self.index))
    }
    fn x87_push(&mut self, value: Value) -> Result<(), Reject> {
        let top = self.state.top.wrapping_sub(1) & 7;
        if self.state.x87[top as usize].is_some() {
            return Err(Reject::StaticX87(self.index));
        }
        self.state.top = top;
        self.state.x87[top as usize] = Some(value);
        Ok(())
    }
    fn x87_pop(&mut self) {
        self.state.x87[self.state.top as usize] = None;
        self.state.top = (self.state.top + 1) & 7;
    }
    pub(super) fn fild64(&mut self, mem: Mem, opcode: Option<u16>) -> Result<(), Reject> {
        self.state.x87_last_cmp = None;
        let value = self.integer_load(mem, 8)?;
        self.x87_push(value)?;
        self.state.c1 = Some(Location::Constant(0));
        self.x87_metadata(opcode);
        self.pack_x87_status();
        Ok(())
    }
    fn integer_load(&mut self, mem: Mem, width: u8) -> Result<Value, Reject> {
        let integer = self.read(mem, width, if width == 8 { Type::I64 } else { Type::I32 })?;
        Ok(self.value(Op::FromInt(integer)))
    }
    fn x87_source(&mut self, source: Operand, mem: Option<Mem>) -> Result<Value, Reject> {
        match source {
            Operand::St(index) => self.st(index),
            Operand::F64 => {
                let mem = mem.ok_or(Reject::Unsupported(self.index))?;
                if let Some(offset) = self.frame_offset(mem)
                    && let Some(&value) = self.state.floats.get(&offset)
                {
                    return Ok(value);
                }
                let bits = self.read(mem, 8, Type::I64)?;
                Ok(self.value(Op::X87Load {
                    bits,
                    denormal: !self.assumed.denormal,
                    single: false,
                    proved: false,
                }))
            }
            Operand::F32 => {
                let mem = mem.ok_or(Reject::Unsupported(self.index))?;
                let bits = self.read(mem, 4, Type::I32)?;
                Ok(self.value(Op::X87Load {
                    bits,
                    denormal: !self.assumed.denormal,
                    single: true,
                    proved: false,
                }))
            }
            Operand::I32 => self.integer_load(mem.ok_or(Reject::Unsupported(self.index))?, 4),
            _ => Err(Reject::Unsupported(self.index)),
        }
    }
    fn x87_metadata(&mut self, opcode: Option<u16>) {
        self.state.ip = Location::Constant(u64::from(self.unit.trace.insts[self.index].pc));
        if let Some(opcode) = opcode {
            self.state.opcode = Location::Constant(u64::from(opcode));
        }
    }
}

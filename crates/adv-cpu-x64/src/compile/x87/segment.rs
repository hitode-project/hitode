//! A success-only straight-line lane. Rejection materializes the precise prefix
//! and resumes at canonical single-instruction entries, never replaying a load
//! or store. No resident state crosses a CFG edge, helper, or environment
//! restore. FLDCW updates control facts without changing the resident stack.
//!
//! Compilers interleave integer address and counter updates with x87 code, so a
//! lane may also carry plain integer instructions. Their guards and shared/file
//! paths reject before the instruction commits; the recovery re-executes it on
//! the canonical path with the x87 prefix materialized.
use super::*;

/// Numeric x87 operations per lane, and all instructions per lane.
const LIMIT: usize = 8;
const SPAN: usize = 24;

#[derive(Default)]
pub(in crate::compile) struct Segments {
    pub(in crate::compile) starts: BTreeMap<u32, Vec<Instruction>>,
    /// x87 members after the first: canonical restart entries use the helper.
    pub(in crate::compile) interiors: BTreeSet<u32>,
    /// Integer members: only recovery and restarts reach their canonical code.
    pub(in crate::compile) integers: BTreeSet<u32>,
}

/// Integer forms whose only side exits are access guards and whose writes all
/// follow those guards, so a rejected attempt has committed nothing.
fn lane_integer(i: &Instruction) -> bool {
    matches!(
        i.mnemonic(),
        M::Mov
            | M::Movzx
            | M::Movsx
            | M::Lea
            | M::Add
            | M::Sub
            | M::And
            | M::Or
            | M::Xor
            | M::Cmp
            | M::Test
            | M::Inc
            | M::Dec
            | M::Not
            | M::Neg
            | M::Nop
    ) && supported(i)
        && crate::strings::operation(i).is_none()
        && !mmx::supported(i)
        && !scalar::supported(i)
        && !simd::supported(i)
}

pub(in crate::compile) fn segments(
    instructions: &BTreeMap<u32, Instruction>,
    explicit_entries: &[u32],
) -> Segments {
    #[cfg(test)]
    if testing::SINGLE_ONLY.get() {
        return Segments::default();
    }
    if !enabled()
        || std::env::var_os("ADV_JIT_X87_SINGLE_ONLY").is_some_and(|v| v == "1")
        || instructions.values().any(mmx::supported)
    {
        return Segments::default();
    }
    let mut boundaries: BTreeSet<_> = explicit_entries.iter().copied().collect();
    for i in instructions.values() {
        if i.flow_control() != FlowControl::Next {
            boundaries.insert(i.next_ip32());
        }
        if i.op0_kind() == OpKind::NearBranch32 {
            boundaries.insert(i.near_branch32());
        }
    }
    let linear = |i: &Instruction| {
        i.next_ip32() > i.ip32()
            && i.memory_base().size() != 2
            && i.memory_index().size() != 2
            // A second decode may enter in the middle of a linear instruction.
            // Keep its overlapping bytes out of any resident segment.
            && boundaries.range((std::ops::Bound::Excluded(i.ip32()), std::ops::Bound::Excluded(i.next_ip32()))).next().is_none()
    };
    let floating = |i: &Instruction| {
        linear(i)
            && crate::floating::fast::classify(i).is_some_and(|fast| {
                !matches!(
                    fast,
                    Fast::StoreControl
                        | Fast::StoreStatus
                        | Fast::Round
                        | Fast::Scale
                        | Fast::Examine
                )
            })
    };
    let integer =
        |i: &Instruction| linear(i) && i.flow_control() == FlowControl::Next && lane_integer(i);
    let mut result = Segments::default();
    for (&start, i) in instructions {
        if result.interiors.contains(&start) || !floating(i) {
            continue;
        }
        let mut run = vec![*i];
        let numeric = |i: &Instruction| {
            floating(i)
                && !matches!(
                    crate::floating::fast::classify(i),
                    Some(Fast::LoadControl | Fast::Wait)
                )
        };
        let mut count = usize::from(numeric(i));
        while count < LIMIT && run.len() < SPAN {
            let next = run.last().unwrap().next_ip32();
            if boundaries.contains(&next)
                || result.interiors.contains(&next)
                || result.starts.contains_key(&next)
            {
                break;
            }
            let Some(i) = instructions
                .get(&next)
                .filter(|i| floating(i) || integer(i))
            else {
                break;
            };
            count += usize::from(numeric(i));
            run.push(*i);
        }
        // Trailing integer instructions gain nothing from residency.
        while run.last().is_some_and(|i| !floating(i)) {
            run.pop();
        }
        if count > 1 {
            for i in &run[1..] {
                if floating(i) {
                    result.interiors.insert(i.ip32());
                } else {
                    result.integers.insert(i.ip32());
                }
            }
            result.starts.insert(start, run);
        }
    }
    result
}

#[derive(Clone, Copy, PartialEq)]
enum Recovery {
    Budget,
    Floating,
    Integer,
}

#[derive(Clone, Copy)]
enum Narrow {
    Unknown,
    /// An exact 24-bit value regardless of the control word.
    Always,
    /// Narrow if the current admitted control word selects PC=24.
    Admitted,
}

impl Narrow {
    fn known(self) -> bool {
        !matches!(self, Self::Unknown)
    }

    fn forget_control(self) -> Self {
        match self {
            Self::Admitted => Self::Unknown,
            other => other,
        }
    }
}

#[derive(Clone, Copy)]
struct Resident {
    value: Value,
    range: i32,
    /// A 24-bit proof, either unconditional or tied to the current control word.
    narrow: Narrow,
}

#[derive(Clone, Copy)]
struct Control {
    value: Value,
    /// Some(single) proves masked exceptions and round-to-nearest at PC=24/53.
    arithmetic: Option<(Value, Value)>,
}

#[derive(Clone)]
struct Prefix {
    base_top: Value,
    top: u8,
    tags: Value,
    status: Value,
    control: Control,
    values: [Option<Resident>; 8],
    dirty: [bool; 8],
    empty: [Option<bool>; 8],
    tag_changes: [Option<Value>; 8],
    metadata: Option<(u32, Option<u16>)>,
    dp: Option<Value>,
}

impl Prefix {
    fn slot(&self, index: u8) -> usize {
        usize::from((self.top + index) & 7)
    }
}

impl Emitter<'_> {
    pub(in crate::compile) fn native_x87_segment(&mut self, instructions: &[Instruction]) {
        debug_assert!(self.x87_native && self.mmx_mode.is_none());
        for i in instructions
            .iter()
            .filter(|i| self.x87.contains_key(&i.ip32()))
        {
            let effects = crate::floating::x87_effects(i).unwrap();
            debug_assert!(!effects.read_flags && !effects.write_flags && effects.write_gpr == 0);
            self.x87_cold_reads |= effects.read_gpr;
        }
        let status = self.x87_word(offset_of!(X87State, status));
        let top = self.b.ins().ushr_imm(status, 11);
        let base_top = self.b.ins().band_imm(top, 7);
        let mut prefix = Prefix {
            base_top,
            top: 0,
            tags: self.x87_word(offset_of!(X87State, tag)),
            status,
            control: Control {
                value: self.x87_word(offset_of!(X87State, control)),
                arithmetic: None,
            },
            values: [None; 8],
            dirty: [false; 8],
            empty: [None; 8],
            tag_changes: [None; 8],
            metadata: None,
            dp: None,
        };
        let mut recoveries = Vec::with_capacity(instructions.len() * 2);
        for (index, i) in instructions.iter().enumerate() {
            self.at(i.ip32());
            // Mirror the canonical code's per-block fuel accounting.
            if index != 0 && self.blocks.contains_key(&i.ip32()) {
                let exhausted = self.b.create_block();
                self.b.set_cold_block(exhausted);
                recoveries.push((exhausted, prefix.clone(), *i, Recovery::Budget));
                let budget = self.b.use_var(self.fuel);
                let zero = self.b.ins().icmp_imm(IntCC::Equal, budget, 0);
                self.x87_reject(zero, exhausted);
                let remaining = self.b.ins().iadd_imm(budget, -1);
                self.b.def_var(self.fuel, remaining);
            }
            if !self.x87.contains_key(&i.ip32()) {
                let rejected = self.b.create_block();
                self.b.set_cold_block(rejected);
                recoveries.push((rejected, prefix.clone(), *i, Recovery::Integer));
                self.lane = Some(rejected);
                let ended = self.instruction(i);
                self.lane = None;
                debug_assert!(!ended);
                continue;
            }
            let slow = self.b.create_block();
            self.b.set_cold_block(slow);
            recoveries.push((slow, prefix.clone(), *i, Recovery::Floating));
            if index == 0 {
                let mode = self.b.ins().load(
                    types::I32,
                    MemFlags::trusted(),
                    self.state,
                    offset_of!(CpuState, mmx_mode) as i32,
                );
                let bad = self.b.ins().icmp_imm(IntCC::NotEqual, mode, 0);
                self.x87_reject(bad, slow);
                let pending = self.b.ins().band_imm(prefix.status, 0x80);
                let bad = self.b.ins().icmp_imm(IntCC::NotEqual, pending, 0);
                self.x87_reject(bad, slow);
            }
            let fast = crate::floating::fast::classify(i).unwrap();
            // Entry rejects ES/MMX, and every admitted lane operation keeps ES
            // clear (including FLDCW). WAIT preserves the entire resident prefix.
            if fast == Fast::Wait {
                continue;
            }
            if fast != Fast::LoadControl {
                prefix.status = self.b.ins().band_imm(prefix.status, !C1);
            }
            match fast {
                Fast::Load(source) => {
                    let value = self.resident_source(&mut prefix, i, source, 1023, slow);
                    let target = prefix.slot(7);
                    self.resident_empty(&mut prefix, target, true, slow);
                    prefix.top = target as u8;
                    self.resident_write(&mut prefix, target, value);
                }
                Fast::Constant(one) => {
                    let target = prefix.slot(7);
                    self.resident_empty(&mut prefix, target, true, slow);
                    prefix.top = target as u8;
                    let value = self.b.ins().f64const(if one { 1.0 } else { 0.0 });
                    let constant = Resident {
                        value,
                        range: 0,
                        narrow: Narrow::Always,
                    };
                    self.resident_write(&mut prefix, target, constant);
                }
                Fast::Store {
                    width,
                    integer,
                    pop,
                } => {
                    // Retain the original address/access-before-stack guard order.
                    let (at, pointer) = self.x87_memory(i, width, true, slow);
                    let slot = prefix.slot(0);
                    let value = self.resident_double(&mut prefix, slot, 1023, slow).value;
                    let (bits, rounded) = if integer {
                        let nearest = if prefix.control.arithmetic.is_some() {
                            self.b.ins().iconst(types::I8, 1)
                        } else {
                            let rc = self.b.ins().band_imm(prefix.control.value, 0xc00);
                            let nearest = self.b.ins().icmp_imm(IntCC::Equal, rc, 0);
                            let truncate = self.b.ins().icmp_imm(IntCC::Equal, rc, 0xc00);
                            let valid = self.b.ins().bor(nearest, truncate);
                            self.x87_require(valid, slow);
                            nearest
                        };
                        let rn = self.b.ins().nearest(value);
                        let rz = self.b.ins().trunc(value);
                        let rounded = self.b.ins().select(nearest, rn, rz);
                        let limit = if width == 4 {
                            2f64.powi(31)
                        } else {
                            2f64.powi(63)
                        };
                        let lower = self.b.ins().f64const(-limit);
                        let upper = self.b.ins().f64const(limit);
                        let low = self.b.ins().fcmp(FloatCC::LessThan, rounded, lower);
                        let high = self
                            .b
                            .ins()
                            .fcmp(FloatCC::GreaterThanOrEqual, rounded, upper);
                        let bad = self.b.ins().bor(low, high);
                        self.x87_reject(bad, slow);
                        (
                            self.b
                                .ins()
                                .fcvt_to_sint_sat(integer_type(width as usize), rounded),
                            rounded,
                        )
                    } else if width == 8 {
                        (
                            self.b.ins().bitcast(types::I64, MemFlags::new(), value),
                            value,
                        )
                    } else {
                        if prefix.control.arithmetic.is_none() {
                            let rc = self.b.ins().band_imm(prefix.control.value, 0xc00);
                            let bad = self.b.ins().icmp_imm(IntCC::NotEqual, rc, 0);
                            self.x87_reject(bad, slow);
                        }
                        let narrow = self.b.ins().fdemote(types::F32, value);
                        let bits = self.b.ins().bitcast(types::I32, MemFlags::new(), narrow);
                        let exponent = self.b.ins().ushr_imm(bits, 23);
                        let exponent = self.b.ins().band_imm(exponent, 255);
                        let exponent = self.b.ins().iadd_imm(exponent, -1);
                        let normal = self
                            .b
                            .ins()
                            .icmp_imm(IntCC::UnsignedLessThan, exponent, 254);
                        let zero = self.b.ins().f64const(0.0);
                        let was_zero = self.b.ins().fcmp(FloatCC::Equal, value, zero);
                        let valid = self.b.ins().bor(normal, was_zero);
                        self.x87_require(valid, slow);
                        (bits, self.b.ins().fpromote(types::F64, narrow))
                    };
                    let inexact = self.b.ins().fcmp(FloatCC::NotEqual, rounded, value);
                    if prefix.control.arithmetic.is_none() {
                        let unmasked = self.b.ins().band_imm(prefix.control.value, PE);
                        let unmasked = self.b.ins().icmp_imm(IntCC::Equal, unmasked, 0);
                        let bad = self.b.ins().band(inexact, unmasked);
                        self.x87_reject(bad, slow);
                    }
                    let a = self.b.ins().fabs(rounded);
                    let b = self.b.ins().fabs(value);
                    let up = self.b.ins().fcmp(FloatCC::GreaterThan, a, b);
                    prefix.status = self.x87_round_flags(prefix.status, inexact, up);
                    // Every current-instruction guard precedes this guest write.
                    self.b.ins().store(MemFlags::new(), bits, pointer, 0);
                    prefix.dp = Some(at);
                    if pop {
                        self.resident_pop(&mut prefix);
                    }
                }
                Fast::StoreSt { index, pop } => {
                    let slot = prefix.slot(0);
                    let value = self.resident_double(&mut prefix, slot, 1023, slow);
                    let target = prefix.slot(index);
                    self.resident_write(&mut prefix, target, value);
                    if pop {
                        self.resident_pop(&mut prefix);
                    }
                }
                Fast::Arith {
                    op,
                    dest,
                    source,
                    reverse,
                    pop,
                } => {
                    let precision = match prefix.control.arithmetic {
                        Some(precision) => precision,
                        None => {
                            let precision = self.x87_admit_arithmetic(prefix.control.value, slow);
                            prefix.control.arithmetic = Some(precision);
                            precision
                        }
                    };
                    let target = prefix.slot(dest);
                    let a = self.resident_double(&mut prefix, target, RANGE, slow);
                    let b = self.resident_source(&mut prefix, i, source, RANGE, slow);
                    let (a, b) = if reverse { (b, a) } else { (a, b) };
                    let (value, inexact, up) = self.x87_rounded_arithmetic(
                        op,
                        (a.value, a.narrow.known()),
                        (b.value, b.narrow.known()),
                        precision,
                        slow,
                    );
                    // Both input checks dominate this continuation. The proof
                    // is conditional on this control word, not an unconditional
                    // 24-bit representation when PC=53 admitted wider inputs.
                    for slot in [
                        Some(target),
                        match source {
                            Source::St(index) => Some(prefix.slot(index)),
                            _ => None,
                        },
                    ]
                    .into_iter()
                    .flatten()
                    {
                        let value = prefix.values[slot].as_mut().unwrap();
                        if !value.narrow.known() {
                            value.narrow = Narrow::Admitted;
                        }
                    }
                    prefix.status = self.x87_round_flags(prefix.status, inexact, up);
                    // Inputs are bounded, so the result is normal/zero and finite.
                    // It can exceed +-400 and must be guarded before the next op.
                    // A PC=24 lane rounded it to 24 bits; PC=53 never asks.
                    let result = Resident {
                        value,
                        range: 1023,
                        narrow: Narrow::Admitted,
                    };
                    self.resident_write(&mut prefix, target, result);
                    if pop {
                        self.resident_pop(&mut prefix);
                    }
                }
                Fast::Compare { source, pops } => {
                    let slot = prefix.slot(0);
                    let a = self.resident_double(&mut prefix, slot, 1023, slow).value;
                    let b = self
                        .resident_source(&mut prefix, i, source, 1023, slow)
                        .value;
                    let equal = self.b.ins().fcmp(FloatCC::Equal, a, b);
                    let less = self.b.ins().fcmp(FloatCC::LessThan, a, b);
                    let equal = self.b.ins().uextend(types::I32, equal);
                    let less = self.b.ins().uextend(types::I32, less);
                    let equal = self.b.ins().ishl_imm(equal, 14);
                    let less = self.b.ins().ishl_imm(less, 8);
                    prefix.status = self.b.ins().band_imm(prefix.status, !0x4700);
                    prefix.status = self.b.ins().bor(prefix.status, equal);
                    prefix.status = self.b.ins().bor(prefix.status, less);
                    for _ in 0..pops {
                        self.resident_pop(&mut prefix);
                    }
                }
                Fast::Exchange(index) => {
                    let slot = prefix.slot(0);
                    let target = prefix.slot(index);
                    let a = self.resident_double(&mut prefix, slot, 1023, slow);
                    let b = self.resident_double(&mut prefix, target, 1023, slow);
                    self.resident_write(&mut prefix, slot, b);
                    self.resident_write(&mut prefix, target, a);
                }
                Fast::Negate | Fast::Absolute => {
                    let slot = prefix.slot(0);
                    let mut value = self.resident_double(&mut prefix, slot, 1023, slow);
                    value.value = if fast == Fast::Negate {
                        self.b.ins().fneg(value.value)
                    } else {
                        self.b.ins().fabs(value.value)
                    };
                    self.resident_write(&mut prefix, slot, value);
                }
                Fast::LoadControl => {
                    let (_, pointer) = self.x87_memory(i, 2, false, slow);
                    let control = self.b.ins().load(types::I16, MemFlags::new(), pointer, 0);
                    let control = self.b.ins().uextend(types::I32, control);
                    let unmasked = self.b.ins().bnot(control);
                    let pending = self.b.ins().band(prefix.status, unmasked);
                    let pending = self.b.ins().band_imm(pending, 0x3f);
                    let bad = self.b.ins().icmp_imm(IntCC::NotEqual, pending, 0);
                    self.x87_reject(bad, slow);
                    // Commit immediately: all later recovery paths must observe
                    // this control word, while a fault above retains the old one.
                    self.x87_store_word(offset_of!(X87State, control), control);
                    prefix.control = Control {
                        value: control,
                        arithmetic: None,
                    };
                    for resident in prefix.values.iter_mut().flatten() {
                        resident.narrow = resident.narrow.forget_control();
                    }
                    prefix.status = self.b.ins().band_imm(prefix.status, !0x8080);
                    // FLDCW leaves C1, FIP/FDP/FOP and TOP/tag unchanged.
                    continue;
                }
                Fast::Wait
                | Fast::StoreControl
                | Fast::StoreStatus
                | Fast::Round
                | Fast::Scale
                | Fast::Examine => unreachable!(),
            }
            prefix.metadata = Some((i.ip32(), self.x87[&i.ip32()].1));
        }
        self.materialize_x87_prefix(&prefix);
        let done = self.b.create_block();
        self.b.ins().jump(done, &[]);
        self.mark_source(super::diagnostics::x87_recovery_source());
        self.resident_recoveries(recoveries, instructions);
        self.b.switch_to_block(done);
        self.at(instructions.last().unwrap().ip32());
    }

    fn resident_recoveries(
        &mut self,
        recoveries: Vec<(ir::Block, Prefix, Instruction, Recovery)>,
        instructions: &[Instruction],
    ) {
        let dirty_slots: Vec<_> = (0..8)
            .filter(|&slot| recoveries.iter().any(|(_, p, _, _)| p.dirty[slot]))
            .collect();
        let tag_slots: Vec<_> = (0..8)
            .filter(|&slot| {
                recoveries
                    .iter()
                    .any(|(_, p, _, _)| p.tag_changes[slot].is_some())
            })
            .collect();
        // Initial TOP/tag dominate all guards; neither changes in CpuState
        // until a recovery or the success tail. Do not pass them through phis.
        let base_top = recoveries[0].1.base_top;
        let mut tags = recoveries[0].1.tags;
        let restore = self.b.create_block();
        self.b.set_cold_block(restore);
        self.b.append_block_param(restore, types::I32); // status
        self.b.append_block_param(restore, types::I32); // dirty/tag masks and TOP
        self.b.append_block_param(restore, types::I64); // resume PC and next PC
        self.b.append_block_param(restore, self.ptr_ty); // operation
        for _ in &dirty_slots {
            self.b.append_block_param(restore, types::F64);
        }
        for _ in &tag_slots {
            self.b.append_block_param(restore, types::I32);
        }
        let integers: Vec<_> = recoveries
            .iter()
            .filter(|(_, _, _, kind)| *kind == Recovery::Integer)
            .map(|(_, _, i, _)| i.ip32())
            .collect();
        for (block, p, i, kind) in recoveries {
            self.b.switch_to_block(block);
            if kind == Recovery::Integer && self.blocks.contains_key(&i.ip32()) {
                // The canonical block entry charges this instruction's fuel again.
                let fuel = self.b.use_var(self.fuel);
                let fuel = self.b.ins().iadd_imm(fuel, 1);
                self.b.def_var(self.fuel, fuel);
            }
            let mask = (0..8).fold(u32::from(p.top) << 16, |mask, slot| {
                mask | (u32::from(p.dirty[slot]) << slot)
                    | (u32::from(p.tag_changes[slot].is_some()) << (slot + 8))
            });
            let mask = self.constant(mask);
            self.materialize_x87_metadata(&p);
            let pcs = u64::from(i.ip32()) | (u64::from(i.next_ip32()) << 32);
            let pcs = self.b.ins().iconst(types::I64, pcs as i64);
            // Null and one are internal budget/integer sentinels, never
            // operation pointers.
            let operation = match kind {
                Recovery::Budget => self.b.ins().iconst(self.ptr_ty, 0),
                Recovery::Integer => self.b.ins().iconst(self.ptr_ty, 1),
                Recovery::Floating => self.b.ins().func_addr(self.ptr_ty, self.x87[&i.ip32()].0),
            };
            let mut args: Vec<ir::BlockArg> =
                vec![p.status.into(), mask.into(), pcs.into(), operation.into()];
            for &slot in &dirty_slots {
                let value = if p.dirty[slot] {
                    p.values[slot].unwrap().value
                } else {
                    self.b.ins().f64const(0.0)
                };
                args.push(value.into());
            }
            for &slot in &tag_slots {
                let tag = p.tag_changes[slot].unwrap_or_else(|| self.constant(0));
                args.push(tag.into());
            }
            self.b.ins().jump(restore, &args);
        }
        self.b.switch_to_block(restore);
        let args = self.b.block_params(restore).to_vec();
        for (index, &slot) in dirty_slots.iter().enumerate() {
            let dirty = self.b.ins().band_imm(args[1], 1 << slot);
            let dirty = self.b.ins().icmp_imm(IntCC::NotEqual, dirty, 0);
            let write = self.b.create_block();
            let after = self.b.create_block();
            self.b.set_cold_block(write);
            self.b.set_cold_block(after);
            self.b.ins().brif(dirty, write, &[], after, &[]);
            self.b.switch_to_block(write);
            let physical = self.x87_slot(base_top, slot as u8);
            self.x87_write(physical, args[4 + index]);
            self.b.ins().jump(after, &[]);
            self.b.switch_to_block(after);
        }
        for (index, &slot) in tag_slots.iter().enumerate() {
            let changed = self.b.ins().band_imm(args[1], 1 << (slot + 8));
            let changed = self.b.ins().icmp_imm(IntCC::NotEqual, changed, 0);
            let physical = self.x87_slot(base_top, slot as u8);
            let updated = self.x87_set_tag(tags, physical, args[4 + dirty_slots.len() + index]);
            tags = self.b.ins().select(changed, updated, tags);
        }
        self.x87_store_word(offset_of!(X87State, tag), tags);
        let top = self.b.ins().ushr_imm(args[1], 16);
        let top = self.b.ins().iadd(base_top, top);
        let top = self.b.ins().band_imm(top, 7);
        let top = self.b.ins().ishl_imm(top, 11);
        let status = self.b.ins().band_imm(args[0], !0x3800);
        let status = self.b.ins().bor(status, top);
        self.x87_store_word(offset_of!(X87State, status), status);
        let pc = self.b.ins().ireduce(types::I32, args[2]);
        let next = self.b.ins().ushr_imm(args[2], 32);
        let next = self.b.ins().ireduce(types::I32, next);
        let budget = self.b.ins().icmp_imm(IntCC::Equal, args[3], 0);
        let exhausted = self.b.create_block();
        let helper = self.b.create_block();
        self.b.set_cold_block(exhausted);
        self.b.set_cold_block(helper);
        self.b.ins().brif(budget, exhausted, &[], helper, &[]);
        self.b.switch_to_block(exhausted);
        self.exit(Exit::Budget, pc);
        self.b.switch_to_block(helper);
        if !integers.is_empty() {
            let integer = self.b.ins().icmp_imm(IntCC::Equal, args[3], 1);
            let resume = self.b.create_block();
            let floating = self.b.create_block();
            self.b.set_cold_block(resume);
            self.b.set_cold_block(floating);
            self.b.ins().brif(integer, resume, &[], floating, &[]);
            self.b.switch_to_block(resume);
            let mut switch = Switch::new();
            let tails: Vec<_> = integers
                .iter()
                .map(|&pc| {
                    let block = self.b.create_block();
                    self.b.set_cold_block(block);
                    switch.set_entry(u128::from(pc), block);
                    (pc, block)
                })
                .collect();
            let unknown = self.b.create_block();
            self.b.set_cold_block(unknown);
            switch.emit(&mut self.b, pc, unknown);
            self.b.switch_to_block(unknown);
            self.exit(Exit::Fallback, pc);
            for (pc, block) in tails {
                self.b.switch_to_block(block);
                self.canonical_integer_tail(pc, instructions);
            }
            self.b.switch_to_block(floating);
        }
        self.canonical_x87_recovery(pc, next, args[3]);
    }

    /// Re-executes lane members from `pc` on the canonical path up to the next
    /// block entry. Every x87 member is an entry, so only integers run here.
    fn canonical_integer_tail(&mut self, mut pc: u32, instructions: &[Instruction]) {
        while !self.blocks.contains_key(&pc) {
            let i = instructions
                .iter()
                .find(|i| i.ip32() == pc)
                .expect("lane members end at a block entry");
            debug_assert!(!self.x87.contains_key(&pc));
            self.at(pc);
            let ended = self.instruction(i);
            debug_assert!(!ended);
            pc = i.next_ip32();
        }
        self.direct(pc);
    }

    fn resident_empty(&mut self, p: &mut Prefix, slot: usize, empty: bool, slow: ir::Block) {
        if let Some(known) = p.empty[slot] {
            if known != empty {
                let bad = self.constant(1);
                self.x87_reject(bad, slow);
            }
            return;
        }
        #[cfg(test)]
        testing::emitted(|counts| counts.tag_guards += 1);
        let physical = self.x87_slot(p.base_top, slot as u8);
        let tag = self.x87_tag(p.tags, physical);
        let bad = self
            .b
            .ins()
            .icmp_imm(if empty { IntCC::NotEqual } else { IntCC::Equal }, tag, 3);
        self.x87_reject(bad, slow);
        p.empty[slot] = Some(empty);
    }

    fn resident_double(
        &mut self,
        p: &mut Prefix,
        slot: usize,
        range: i32,
        slow: ir::Block,
    ) -> Resident {
        if let Some(mut resident) = p.values[slot] {
            self.resident_empty(p, slot, false, slow);
            if resident.range > range {
                let bits = self
                    .b
                    .ins()
                    .bitcast(types::I64, MemFlags::new(), resident.value);
                let magnitude = self.b.ins().ishl_imm(bits, 1);
                let zero = self.b.ins().icmp_imm(IntCC::Equal, magnitude, 0);
                let exponent = self.b.ins().ushr_imm(bits, 52);
                let exponent = self.b.ins().band_imm(exponent, 0x7ff);
                let relative = self.b.ins().iadd_imm(exponent, -i64::from(1023 - range));
                let bounded = self.b.ins().icmp_imm(
                    IntCC::UnsignedLessThanOrEqual,
                    relative,
                    i64::from(2 * range),
                );
                let valid = self.b.ins().bor(zero, bounded);
                self.x87_require(valid, slow);
                resident.range = range;
                p.values[slot] = Some(resident);
            }
            resident
        } else {
            self.resident_empty(p, slot, false, slow);
            let physical = self.x87_slot(p.base_top, slot as u8);
            // resident_empty already established the tag; decode only once.
            let value = self.x87_decode_double(physical, range, slow);
            let resident = Resident {
                value,
                range,
                narrow: Narrow::Unknown,
            };
            p.values[slot] = Some(resident);
            p.empty[slot] = Some(false);
            resident
        }
    }

    fn resident_source(
        &mut self,
        p: &mut Prefix,
        i: &Instruction,
        source: Source,
        range: i32,
        slow: ir::Block,
    ) -> Resident {
        if let Source::St(index) = source {
            return self.resident_double(p, p.slot(index), range, slow);
        }
        let (value, at) = self.x87_source(i, source, p.base_top, p.tags, range, slow);
        p.dp = at;
        Resident {
            value,
            range: match source {
                Source::F32 => 149,
                Source::I16 => 15,
                Source::I32 => 31,
                _ => range,
            },
            narrow: if matches!(source, Source::F32 | Source::I16) {
                Narrow::Always
            } else {
                Narrow::Unknown
            },
        }
    }

    fn resident_write(&mut self, p: &mut Prefix, slot: usize, resident: Resident) {
        p.values[slot] = Some(resident);
        p.dirty[slot] = true;
        p.empty[slot] = Some(false);
        let zero = self.b.ins().f64const(0.0);
        let zero = self.b.ins().fcmp(FloatCC::Equal, resident.value, zero);
        p.tag_changes[slot] = Some(self.b.ins().uextend(types::I32, zero));
    }

    fn resident_pop(&mut self, p: &mut Prefix) {
        let slot = p.slot(0);
        p.empty[slot] = Some(true);
        p.tag_changes[slot] = Some(self.constant(3));
        p.top = (p.top + 1) & 7;
    }

    fn materialize_x87_prefix(&mut self, p: &Prefix) {
        let mut tags = p.tags;
        for slot in 0..8 {
            if p.dirty[slot] || p.tag_changes[slot].is_some() {
                let physical = self.x87_slot(p.base_top, slot as u8);
                if p.dirty[slot] {
                    self.x87_write(physical, p.values[slot].unwrap().value);
                }
                if let Some(tag) = p.tag_changes[slot] {
                    tags = self.x87_set_tag(tags, physical, tag);
                }
            }
        }
        self.x87_store_word(offset_of!(X87State, tag), tags);
        let top = self.x87_slot(p.base_top, p.top);
        let top = self.b.ins().ishl_imm(top, 11);
        let status = self.b.ins().band_imm(p.status, !0x3800);
        let status = self.b.ins().bor(status, top);
        self.x87_store_word(offset_of!(X87State, status), status);
        self.materialize_x87_metadata(p);
    }

    fn materialize_x87_metadata(&mut self, p: &Prefix) {
        if let Some((pc, opcode)) = p.metadata {
            let pc = self.constant(pc);
            self.b.ins().store(
                MemFlags::trusted(),
                pc,
                self.state,
                (FP + offset_of!(X87State, ip)) as i32,
            );
            if let Some(opcode) = opcode {
                let opcode = self.constant(u32::from(opcode));
                self.x87_store_word(offset_of!(X87State, opcode), opcode);
            }
        }
        if let Some(dp) = p.dp {
            self.b.ins().store(
                MemFlags::trusted(),
                dp,
                self.state,
                (FP + offset_of!(X87State, dp)) as i32,
            );
        }
    }
}

//! Phase 4.1: Intra-iteration cross-channel SLP with scope-accurate pricing.
//!
//! Packs isomorphic, mutually independent proved floating-point channel chains
//! within a basic block into `.4s` (4-wide binary32) or `.2d` (2-wide binary64)
//! SIMD operations (`Op::VecFp`, `Op::VecFma`, `Op::VecFpToInt`), hoisting
//! loop-invariant operand packs into `Block(0)` while keeping every guest
//! `Op::Store` in its exact program order with its original `RecordId`.
//!
//! Each candidate group is priced against the post-Phase 1–2 scalar sequence
//! (including broadcasts, lane packs, destructive `FMLA` moves, and lane
//! extractions) and committed only when it strictly lowers per-iteration host
//! instruction words and amortizes any once-per-entry `Block(0)` pack setup.

use std::collections::{BTreeMap, BTreeSet};

use super::super::{
    End, FlagKind, FmaOp, FpOp, Func, IntOp, Location, Op, Recipe, SINGLE_A, SINGLE_B, SINGLE_C,
    SINGLE_EXACT, SINGLE_RESULT, Type, Value, VecOp, VecToInt, VecUnary,
};
use crate::translate::front::tags::ConditionCode;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum StoreKind {
    Int32(u8), // width <= 4, rounding
    F32,       // !integer, width == 4
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ChannelKey {
    /// Two-op `Op::X87Single` chain (`op1` -> `op2`) + fused `X87Store`.
    Single2(FpOp, FpOp, StoreKind),
    /// Single `Op::Fma { single: true }` (`Add` or `RevSub`) + fused `X87Store`.
    SingleFma(bool, StoreKind),
    /// Standalone `Op::X87Single` + fused `X87Store`.
    Single1(FpOp, StoreKind),
    /// Two-op proved `Op::Fp` chain (`op1` -> `op2`) + fused `X87Store` (`width <= 4`).
    Double2(FpOp, FpOp, u8),
    /// Proved `Op::Fma { single: false }` (`Add` or `RevSub`) + fused `X87Store` (`width <= 4`).
    DoubleFma(bool, u8),
}

impl ChannelKey {
    fn is_single(self) -> bool {
        matches!(
            self,
            Self::Single2(..) | Self::SingleFma(..) | Self::Single1(..)
        )
    }

    fn width(self) -> usize {
        if self.is_single() { 4 } else { 2 }
    }
}

#[derive(Clone)]
struct Channel {
    key: ChannelKey,
    /// Instruction indices inside the block belonging to this channel (ascending).
    indices: Vec<usize>,
    /// Intermediate `op1` result for 2-op chains (`Single2` / `Double2`), if any.
    mid_val: Option<Value>,
    /// Final FP/FMA result consumed by `X87Store`.
    prod_val: Value,
    /// Final SSA value produced by the channel (the `X87Store` output).
    output: Value,
    /// Input operands in canonical slot order:
    /// - `*2`: `[a, b, c]` (`op2(op1(a, b), c)`)
    /// - `*Fma`: `[a, b, c]` (`c ± a * b`)
    /// - `Single1`: `[a, b]` (`op1(a, b)`)
    inputs: Vec<Value>,
    /// Exact scalar host word cost of the replaced ops in this channel.
    scalar_words: u32,
}

fn count_inst_uses(func: &Func) -> Vec<u32> {
    let mut uses = vec![0u32; func.types.len()];
    for block in &func.blocks {
        for &(_, op) in &block.insts {
            for v in op.uses() {
                uses[v.0 as usize] = uses[v.0 as usize].saturating_add(1);
            }
        }
        for t in block.end.targets() {
            for &v in &t.args {
                uses[v.0 as usize] = uses[v.0 as usize].saturating_add(1);
            }
        }
        if let End::Branch { cond, .. } = block.end {
            uses[cond.0 as usize] = uses[cond.0 as usize].saturating_add(1);
        }
    }
    uses
}

fn inline_exit_records(func: &Func) -> Vec<bool> {
    let mut inline = vec![false; func.records.len()];
    for block in &func.blocks {
        if let Some(head) = block.head {
            inline[head.record.0 as usize] = true;
        }
        if let End::Exit(id) = block.end {
            inline[id.0 as usize] = true;
        }
    }
    inline
}

/// Checks that every reference to `prod` in `func.records` lives in a cold
/// (non-inline-exit) record's `x87.stack` as `Location::At(prod)`, so it can
/// be rewritten to `Recipe::VecLane` with zero hot-path cost.
fn prod_records_rewritable(func: &Func, prod: Value, inline: &[bool]) -> bool {
    for (idx, rec) in func.records.iter().enumerate() {
        if !rec.uses().any(|v| v == prod) {
            continue;
        }
        if inline[idx] {
            return false;
        }
        if rec
            .regs
            .iter()
            .chain([&rec.eflags, &rec.mmx_meta])
            .chain(&rec.xmm)
            .chain(&rec.mmx)
            .flatten()
            .chain(rec.frame.iter().map(|(_, _, loc)| loc))
            .any(|loc| loc_uses(*loc, prod))
            || rec.flag_groups.iter().any(|g| g.uses().any(|v| v == prod))
        {
            return false;
        }
        if let Some(x87) = &rec.x87 {
            if [&x87.c1, &x87.ip, &x87.opcode]
                .into_iter()
                .flatten()
                .any(|loc| loc_uses(*loc, prod))
            {
                return false;
            }
            for loc in x87.stack.iter().flatten() {
                if loc_uses(*loc, prod) && *loc != Location::At(prod) {
                    return false;
                }
            }
        }
    }
    true
}

/// Checks that every reference to `mid` (`op1` result) in `func.records` lives
/// in a cold record's `x87.stack` as `Location::At(mid)` or in `x87.c1` as
/// `Recipe::C1 { op: op2, a, b, single }` pairing `mid` with `c`.
fn mid_records_rewritable(
    func: &Func,
    mid: Value,
    op2: FpOp,
    c: Value,
    is_single: bool,
    inline: &[bool],
) -> bool {
    for (idx, rec) in func.records.iter().enumerate() {
        if !rec.uses().any(|v| v == mid) {
            continue;
        }
        if inline[idx] {
            return false;
        }
        if rec
            .regs
            .iter()
            .chain([&rec.eflags, &rec.mmx_meta])
            .chain(&rec.xmm)
            .chain(&rec.mmx)
            .flatten()
            .chain(rec.frame.iter().map(|(_, _, loc)| loc))
            .any(|loc| loc_uses(*loc, mid))
            || rec.flag_groups.iter().any(|g| g.uses().any(|v| v == mid))
        {
            return false;
        }
        if let Some(x87) = &rec.x87 {
            if [&x87.ip, &x87.opcode]
                .into_iter()
                .flatten()
                .any(|loc| loc_uses(*loc, mid))
            {
                return false;
            }
            for loc in x87.stack.iter().flatten() {
                if loc_uses(*loc, mid) && *loc != Location::At(mid) {
                    return false;
                }
            }
            if let Some(loc) = x87.c1
                && loc_uses(loc, mid)
            {
                let ok = matches!(
                    loc,
                    Location::Recipe(Recipe::C1 { op, a, b, single })
                        if op == op2
                            && single == is_single
                            && ((a == mid && b == c) || (a == c && b == mid))
                );
                if !ok {
                    return false;
                }
            }
        }
    }
    true
}

fn loc_uses(loc: Location<Value>, target: Value) -> bool {
    match loc {
        Location::Constant(_) => false,
        Location::At(v)
        | Location::Recipe(Recipe::Eflags(v))
        | Location::Recipe(Recipe::VecLane { vec: v, .. }) => v == target,
        Location::Recipe(Recipe::C1 { a, b, .. } | Recipe::C1Vec { a, b, .. }) => {
            a == target || b == target
        }
        Location::Recipe(Recipe::C1Fma { a, b, c, .. }) => {
            a == target || b == target || c == target
        }
    }
}

fn scalar_x87_single_words(proved: u8) -> u32 {
    if proved & SINGLE_EXACT != 0 { 1 } else { 4 }
}

fn scalar_fma_words(single: bool, proved: u8) -> u32 {
    if !single || proved & SINGLE_EXACT != 0 {
        1
    } else {
        5
    }
}

fn scalar_store_words(kind: StoreKind) -> u32 {
    match kind {
        StoreKind::Int32(_) => 1,
        StoreKind::F32 => 2,
    }
}

fn single_proved(proved: u8) -> bool {
    const REQ: u8 = SINGLE_A | SINGLE_B | SINGLE_RESULT;
    proved & SINGLE_EXACT != 0 || proved & REQ == REQ
}

fn single_fma_proved(proved: u8) -> bool {
    const REQ: u8 = SINGLE_A | SINGLE_B | SINGLE_C | SINGLE_RESULT;
    proved & SINGLE_EXACT != 0 || proved & REQ == REQ
}

/// Extract non-overlapping proved channel candidates within `block_idx`.
fn collect_channels(
    func: &Func,
    block_idx: usize,
    inst_uses: &[u32],
    inline_records: &[bool],
) -> Vec<Channel> {
    let insts = &func.blocks[block_idx].insts;
    let mut def_pos = BTreeMap::<Value, usize>::new();
    for (idx, &(res, _)) in insts.iter().enumerate() {
        if let Some(v) = res {
            def_pos.insert(v, idx);
        }
    }

    let mut channels = Vec::new();
    let mut claimed = vec![false; insts.len()];

    for (store_idx, &(res, op)) in insts.iter().enumerate() {
        let Some(output) = res else { continue };
        let Op::X87Store {
            value: prod,
            width,
            rounding,
            integer,
            proved: true,
        } = op
        else {
            continue;
        };
        let store_kind = if integer && width <= 4 {
            StoreKind::Int32(rounding)
        } else if !integer && width == 4 {
            StoreKind::F32
        } else {
            continue;
        };
        if inst_uses[prod.0 as usize] != 1 || !prod_records_rewritable(func, prod, inline_records) {
            continue;
        }
        let Some(&prod_idx) = def_pos.get(&prod) else {
            continue;
        };
        if claimed[prod_idx] || claimed[store_idx] {
            continue;
        }

        let prod_op = insts[prod_idx].1;
        let candidate = match prod_op {
            Op::X87Single(op2, lhs, rhs, p2) if single_proved(p2) => {
                // Prefer matching a two-op X87Single chain when one operand is
                // a single-use proved X87Single in the same block.
                let two_op = [(lhs, rhs, false), (rhs, lhs, true)].into_iter().find_map(
                    |(m, c, swapped)| {
                        if swapped && !matches!(op2, FpOp::Add | FpOp::Mul) {
                            return None;
                        }
                        if inst_uses[m.0 as usize] != 1
                            || !mid_records_rewritable(func, m, op2, c, true, inline_records)
                        {
                            return None;
                        }
                        let &m_idx = def_pos.get(&m)?;
                        if claimed[m_idx] {
                            return None;
                        }
                        if let Op::X87Single(op1, a, b, p1) = insts[m_idx].1
                            && single_proved(p1)
                        {
                            Some((m_idx, m, op1, a, b, c, p1))
                        } else {
                            None
                        }
                    },
                );
                if let Some((m_idx, m, op1, a, b, c, p1)) = two_op {
                    Some(Channel {
                        key: ChannelKey::Single2(op1, op2, store_kind),
                        indices: vec![m_idx, prod_idx, store_idx],
                        mid_val: Some(m),
                        prod_val: prod,
                        output,
                        inputs: vec![a, b, c],
                        scalar_words: scalar_x87_single_words(p1)
                            + scalar_x87_single_words(p2)
                            + scalar_store_words(store_kind),
                    })
                } else {
                    Some(Channel {
                        key: ChannelKey::Single1(op2, store_kind),
                        indices: vec![prod_idx, store_idx],
                        mid_val: None,
                        prod_val: prod,
                        output,
                        inputs: vec![lhs, rhs],
                        scalar_words: scalar_x87_single_words(p2) + scalar_store_words(store_kind),
                    })
                }
            }
            Op::Fma {
                op: fma_op @ (FmaOp::Add | FmaOp::RevSub),
                a,
                b,
                c,
                single: true,
                proved,
            } if single_fma_proved(proved) => Some(Channel {
                key: ChannelKey::SingleFma(fma_op == FmaOp::RevSub, store_kind),
                indices: vec![prod_idx, store_idx],
                mid_val: None,
                prod_val: prod,
                output,
                inputs: vec![a, b, c],
                scalar_words: scalar_fma_words(true, proved) + scalar_store_words(store_kind),
            }),
            Op::Fp(op2, lhs, rhs, true) if matches!(store_kind, StoreKind::Int32(_)) => {
                let StoreKind::Int32(rc) = store_kind else {
                    unreachable!()
                };
                [(lhs, rhs, false), (rhs, lhs, true)]
                    .into_iter()
                    .find_map(|(m, c, swapped)| {
                        if swapped && !matches!(op2, FpOp::Add | FpOp::Mul) {
                            return None;
                        }
                        if inst_uses[m.0 as usize] != 1
                            || !mid_records_rewritable(func, m, op2, c, false, inline_records)
                        {
                            return None;
                        }
                        let &m_idx = def_pos.get(&m)?;
                        if claimed[m_idx] {
                            return None;
                        }
                        if let Op::Fp(op1, a, b, true) = insts[m_idx].1 {
                            Some(Channel {
                                key: ChannelKey::Double2(op1, op2, rc),
                                indices: vec![m_idx, prod_idx, store_idx],
                                mid_val: Some(m),
                                prod_val: prod,
                                output,
                                inputs: vec![a, b, c],
                                scalar_words: 3, // op1 + op2 + X87Store
                            })
                        } else {
                            None
                        }
                    })
            }
            Op::Fma {
                op: fma_op @ (FmaOp::Add | FmaOp::RevSub),
                a,
                b,
                c,
                single: false,
                proved,
            } if proved & SINGLE_RESULT != 0 && matches!(store_kind, StoreKind::Int32(_)) => {
                let StoreKind::Int32(rc) = store_kind else {
                    unreachable!()
                };
                Some(Channel {
                    key: ChannelKey::DoubleFma(fma_op == FmaOp::RevSub, rc),
                    indices: vec![prod_idx, store_idx],
                    mid_val: None,
                    prod_val: prod,
                    output,
                    inputs: vec![a, b, c],
                    scalar_words: 2, // FMADD D + FCVT*S
                })
            }
            _ => None,
        };

        if let Some(ch) = candidate {
            for &idx in &ch.indices {
                claimed[idx] = true;
            }
            channels.push(ch);
        }
    }

    channels
}

struct GroupPlan {
    key: ChannelKey,
    channels: Vec<Channel>,
    insert_at: usize,
}

/// Returns whether every value in `vals` is loop-invariant (defined in `Block(0)`
/// or a pure constant).
fn all_in_block0(vals: &[Value], def_block: &[Option<usize>], const_f64: &[Option<u64>]) -> bool {
    vals.iter()
        .all(|v| def_block[v.0 as usize] == Some(0) || const_f64[v.0 as usize].is_some())
}

/// Checks whether `v` is available strictly before `pos` in `block_idx` (or is a
/// pure `F64` constant that can be materialized anywhere).
fn available_before(
    v: Value,
    block_idx: usize,
    pos: usize,
    def_block: &[Option<usize>],
    def_pos_in_block: &BTreeMap<Value, usize>,
    const_f64: &[Option<u64>],
) -> bool {
    if const_f64[v.0 as usize].is_some() {
        return true;
    }
    match def_block[v.0 as usize] {
        Some(b) if b != block_idx => true,
        Some(_) => def_pos_in_block.get(&v).is_none_or(|&p| p < pos),
        None => true,
    }
}

struct PriceCtx<'a> {
    func: &'a Func,
    block_idx: usize,
    def_block: &'a [Option<usize>],
    const_f64: &'a [Option<u64>],
    loop_uses: &'a [u32],
    def_op: &'a [Option<Op>],
    def_pos_in_block: &'a BTreeMap<Value, usize>,
    last_store_before: &'a [Option<usize>],
}

#[derive(Clone, Copy)]
struct MemLane {
    addr: Value,
    base: Value,
    offset: i32,
    pc: u32,
    load_idx: usize,
}

fn decompose_addr(def_op: &[Option<Op>], mut addr: Value) -> (Value, i32) {
    let mut offset = 0i32;
    for _ in 0..4 {
        match def_op.get(addr.0 as usize).copied().flatten() {
            Some(Op::Int(op @ (IntOp::Add | IntOp::Sub), a, b)) => {
                if let Some(Op::Constant(Type::I32, c)) =
                    def_op.get(b.0 as usize).copied().flatten()
                {
                    let delta = c as u32 as i32;
                    offset = offset.wrapping_add(if op == IntOp::Add {
                        delta
                    } else {
                        delta.wrapping_neg()
                    });
                    addr = a;
                    continue;
                }
                break;
            }
            _ => break,
        }
    }
    (addr, offset)
}

/// If `v` is a proved `Op::X87Load` of an unchecked same-block `Op::Load` at
/// width `elem` (4 for single, 8 for double) with no intervening store before
/// `insert_at`, returns its decomposed memory lane.
fn mem_lane_of(
    v: Value,
    is_single: bool,
    insert_at: usize,
    def_op: &[Option<Op>],
    def_pos_in_block: &BTreeMap<Value, usize>,
    last_store_before: &[Option<usize>],
) -> Option<MemLane> {
    let Op::X87Load {
        mut bits,
        single,
        proved: true,
        ..
    } = def_op.get(v.0 as usize).copied().flatten()?
    else {
        return None;
    };
    if single != is_single {
        return None;
    }
    while let Some(Op::Bits {
        to: Type::I32 | Type::I64,
        value,
    }) = def_op.get(bits.0 as usize).copied().flatten()
    {
        bits = value;
    }
    let expected_w = if is_single { 4 } else { 8 };
    let Op::Load {
        width,
        addr,
        checked: false,
        scope: None,
        clock: false,
        pc,
        ..
    } = def_op.get(bits.0 as usize).copied().flatten()?
    else {
        return None;
    };
    if width != expected_w {
        return None;
    }
    let &load_idx = def_pos_in_block.get(&bits)?;
    if load_idx >= insert_at {
        return None;
    }
    if last_store_before
        .get(insert_at)
        .copied()
        .flatten()
        .is_some_and(|st| st > load_idx)
    {
        return None;
    }
    let (base, offset) = decompose_addr(def_op, addr);
    Some(MemLane {
        addr,
        base,
        offset,
        pc,
        load_idx,
    })
}

/// Checks whether `lanes` are strided memory loads with stride `k * elem` (`k in 1..=4`).
/// Returns `Some((base, first_offset, stride_channels, first_addr, pc))`.
fn detect_strided_slot(
    lanes: &[Value],
    is_single: bool,
    insert_at: usize,
    def_op: &[Option<Op>],
    def_pos_in_block: &BTreeMap<Value, usize>,
    last_store_before: &[Option<usize>],
) -> Option<(Value, i32, u8, Value, u32)> {
    if lanes.len() < 2 {
        return None;
    }
    let mut mems = Vec::with_capacity(lanes.len());
    for &v in lanes {
        mems.push(mem_lane_of(
            v,
            is_single,
            insert_at,
            def_op,
            def_pos_in_block,
            last_store_before,
        )?);
    }
    let base = mems[0].base;
    let first_off = mems[0].offset;
    let elem = if is_single { 4i32 } else { 8i32 };
    let stride = mems[1].offset.checked_sub(first_off)?;
    if stride <= 0 || stride % elem != 0 {
        return None;
    }
    let channels = stride / elem;
    if !(1..=4).contains(&channels) {
        return None;
    }
    for (i, m) in mems.iter().enumerate() {
        if m.base != base || m.offset != first_off + (i as i32) * stride {
            return None;
        }
    }
    Some((base, first_off, channels as u8, mems[0].addr, mems[0].pc))
}

/// Pricing for one operand slot (2 or 4 lanes) of a packed group.
/// Returns `(body_words, header_words, extra_live_vregs)`.
fn price_slot(
    lanes: &[Value],
    is_single: bool,
    insert_at: usize,
    ctx: &PriceCtx<'_>,
    shared_bcasts: &mut BTreeSet<(bool, Value)>,
) -> (u32, u32, i32) {
    let invariant = ctx.block_idx != 0 && all_in_block0(lanes, ctx.def_block, ctx.const_f64);
    let net_vregs = |hoisted: bool| -> i32 {
        if !hoisted {
            return 0;
        }
        // Replacing `k` loop-body single-use scalar F64 invariants with 1 V128
        // pack frees `k - 1` vector registers across the loop; if lanes retain
        // other loop-body uses, the hoisted pack adds +1 live V128.
        let mut unique = BTreeSet::new();
        let mut freed = 0i32;
        for &v in lanes {
            if unique.insert(v)
                && ctx.const_f64[v.0 as usize].is_none()
                && ctx.loop_uses[v.0 as usize] <= 1
            {
                freed += 1;
            }
        }
        1 - freed
    };
    if lanes.iter().all(|v| ctx.const_f64[v.0 as usize].is_some()) {
        // Packs directly into a single `Op::Constant(Type::V128, bits)`.
        let w = 2;
        return if invariant {
            (0, w, net_vregs(true))
        } else {
            (w, 0, 0)
        };
    }
    let all_same = lanes.iter().all(|&v| v == lanes[0]);
    if all_same {
        let key = (is_single, lanes[0]);
        if !shared_bcasts.insert(key) {
            return (0, 0, 0);
        }
        if !invariant
            && mem_lane_of(
                lanes[0],
                is_single,
                insert_at,
                ctx.def_op,
                ctx.def_pos_in_block,
                ctx.last_store_before,
            )
            .is_some()
        {
            // Single `LD1R` broadcast load.
            return (1, 0, 0);
        }
        // Single: NarrowScalar (1) + Dup32 (1) = 2; Double: Dup64 (1).
        let w = if is_single { 2 } else { 1 };
        if invariant {
            (0, w, net_vregs(true))
        } else {
            (w, 0, 0)
        }
    } else if !invariant
        && ctx.func.resident
        && !crate::translate::opt::back::is_compact_window()
        && let Some((_, _, channels, ..)) = detect_strided_slot(
            lanes,
            is_single,
            insert_at,
            ctx.def_op,
            ctx.def_pos_in_block,
            ctx.last_store_before,
        )
    {
        // Contiguous `LDR Q` (channels == 1) or AoS `LD2/3/4` (channels in 2..=4).
        let _ = channels;
        (1, 0, 0)
    } else if is_single {
        // 4× NarrowScalar (4) + 3× Insert32 (3..6, conservatively 4) = 8 words.
        let w = 8;
        if invariant {
            (0, w, net_vregs(true))
        } else {
            (w, 0, 0)
        }
    } else {
        // Insert64(true, false) (vmov + vins = 2 words).
        let w = 2;
        if invariant {
            (0, w, net_vregs(true))
        } else {
            (w, 0, 0)
        }
    }
}

fn price_group(
    key: ChannelKey,
    channels: &[Channel],
    insert_at: usize,
    ctx: &PriceCtx<'_>,
    live_vregs: &mut i32,
    shared_bcasts: &mut BTreeSet<(bool, Value)>,
) -> (u32, u32, u32) {
    let scalar_words: u32 = channels.iter().map(|c| c.scalar_words).sum();
    let is_single = key.is_single();
    let n_slots = channels[0].inputs.len();
    let mut vec_body_words = 0u32;
    let mut header_words = 0u32;

    for s in 0..n_slots {
        let lanes: Vec<Value> = channels.iter().map(|c| c.inputs[s]).collect();
        let (bw, hw, dv) = price_slot(&lanes, is_single, insert_at, ctx, shared_bcasts);
        vec_body_words += bw;
        header_words += hw;
        *live_vregs += dv;
        // Charge a conservative reload penalty if hoisting pushes live vector
        // registers past the 14-register comfort margin (out of v0..v15).
        if dv > 0 && *live_vregs > 14 {
            vec_body_words += 2;
        }
    }

    // Core vector arithmetic + conversion + lane extractions.
    let (arith_words, conv_extract_words) = match key {
        ChannelKey::Single2(_, _, StoreKind::Int32(_)) => (2, 1 + 4),
        ChannelKey::Single2(_, _, StoreKind::F32) => (2, 4),
        ChannelKey::SingleFma(_, StoreKind::Int32(_)) => (2, 1 + 4),
        ChannelKey::SingleFma(_, StoreKind::F32) => (2, 4),
        ChannelKey::Single1(_, StoreKind::Int32(_)) => (1, 1 + 4),
        ChannelKey::Single1(_, StoreKind::F32) => (1, 4),
        ChannelKey::Double2(..) => (2, 1 + 2),
        ChannelKey::DoubleFma(..) => (2, 1 + 2),
    };
    vec_body_words += arith_words + conv_extract_words;

    (scalar_words, vec_body_words, header_words)
}

/// Statically provable minimum trip count for a simple single-block loop
/// entered via an unconditional `End::Jump` predecessor. Returns `1` whenever
/// the bound is dynamic/unknown, so header setup is never amortized against
/// unverified trip counts.
fn known_min_trips(func: &Func, block_idx: usize) -> u32 {
    if block_idx == 0 {
        return 1;
    }
    let block = &func.blocks[block_idx];
    let End::Branch { cond, taken, fall } = &block.end else {
        return 1;
    };
    // Support both direct self-backedges (`taken -> block_idx`) and 1-hop
    // resident checkpoint trampolines (`taken -> tramp -> block_idx`).
    let (back, tramp_block) =
        if taken.block.0 as usize == block_idx && fall.block.0 as usize != block_idx {
            (taken, None)
        } else if fall.block.0 as usize != block_idx
            && let tb = taken.block.0 as usize
            && let End::Jump(t) = &func.blocks[tb].end
            && t.block.0 as usize == block_idx
        {
            (t, Some(tb))
        } else {
            return 1;
        };

    let mut entry = None;
    for (bi, b) in func.blocks.iter().enumerate() {
        if bi == block_idx || Some(bi) == tramp_block {
            continue;
        }
        for t in b.end.targets() {
            if t.block.0 as usize == block_idx {
                if entry.is_some() || !matches!(b.end, End::Jump(_)) {
                    return 1;
                }
                entry = Some(t);
            }
        }
    }
    let Some(entry) = entry else {
        return 1;
    };

    let mut const_i32 = BTreeMap::<Value, u32>::new();
    let mut def_op = BTreeMap::<Value, Op>::new();
    for b in &func.blocks {
        for &(res, op) in &b.insts {
            if let Some(v) = res {
                def_op.insert(v, op);
                if let Op::Constant(Type::I32, bits) = op {
                    const_i32.insert(v, bits as u32);
                }
            }
        }
    }

    let Some(Op::Test { flags, cond: cc }) = def_op.get(cond).copied() else {
        return 1;
    };
    let Some(Op::Flags { kind, a, b, .. }) = def_op.get(&flags).copied() else {
        return 1;
    };

    for (idx, &param) in block.params.iter().enumerate() {
        let Some(&init) = entry.args.get(idx).and_then(|v| const_i32.get(v)) else {
            continue;
        };
        let Some(&next) = back.args.get(idx) else {
            continue;
        };
        if kind == FlagKind::Decrement && cc == ConditionCode::ne && a == param {
            if let Some(Op::Int(IntOp::Sub, lhs, rhs)) = def_op.get(&next).copied()
                && lhs == param
                && const_i32.get(&rhs) == Some(&1)
                && init > 0
            {
                return init;
            }
        } else if kind == FlagKind::Sub
            && cc == ConditionCode::l
            && a == next
            && let Some(&bound) = const_i32.get(&b)
            && let Some(Op::Int(IntOp::Add, lhs, rhs)) = def_op.get(&next).copied()
            && lhs == param
            && let Some(&step) = const_i32.get(&rhs)
            && step > 0
            && (init as i32) < (bound as i32)
        {
            let diff = (i64::from(bound as i32) - i64::from(init as i32)) as u64;
            let trips = diff.div_ceil(u64::from(step));
            return u32::try_from(trips).unwrap_or(u32::MAX).max(1);
        }
    }
    1
}

fn slp_profitable(
    scalar_words: u32,
    vec_body_words: u32,
    header_words: u32,
    min_trips: u32,
) -> bool {
    if vec_body_words >= scalar_words {
        return false;
    }
    let saved = scalar_words - vec_body_words;
    header_words < saved.saturating_mul(min_trips)
}

fn block_has_slp_candidates(insts: &[(Option<Value>, Op)]) -> bool {
    let mut count = 0usize;
    for &(res, op) in insts {
        if res.is_some()
            && matches!(
                op,
                Op::X87Store {
                    width,
                    integer,
                    proved: true,
                    ..
                } if (integer && width <= 4) || (!integer && width == 4)
            )
        {
            count += 1;
            if count >= 2 {
                return true;
            }
        }
    }
    false
}

struct FunctionTables {
    def_block: Vec<Option<usize>>,
    const_f64: Vec<Option<u64>>,
    inst_uses: Vec<u32>,
    inline_records: Vec<bool>,
}

/// Run intra-iteration cross-channel SLP across all blocks of `func`.
pub(in crate::translate::opt) fn run(func: &mut Func) -> bool {
    let mut any_changed = false;
    let mut tables: Option<FunctionTables> = None;

    for block_idx in 0..func.blocks.len() {
        if !block_has_slp_candidates(&func.blocks[block_idx].insts) {
            continue;
        }
        let cached = tables.get_or_insert_with(|| {
            let n = func.types.len();
            let mut def_block = vec![None; n];
            let mut const_f64 = vec![None; n];
            for (bi, block) in func.blocks.iter().enumerate() {
                for &p in &block.params {
                    def_block[p.0 as usize] = Some(bi);
                }
                for &(res, op) in &block.insts {
                    if let Some(v) = res {
                        def_block[v.0 as usize] = Some(bi);
                        if let Op::Constant(Type::F64, bits) = op {
                            const_f64[v.0 as usize] = Some(bits as u64);
                        }
                    }
                }
            }
            let inst_uses = count_inst_uses(func);
            let inline_records = inline_exit_records(func);
            FunctionTables {
                def_block,
                const_f64,
                inst_uses,
                inline_records,
            }
        });
        let channels = collect_channels(func, block_idx, &cached.inst_uses, &cached.inline_records);
        if channels.len() < 2 {
            continue;
        }
        let n = func.types.len();
        let def_block: &[Option<usize>] = &cached.def_block;
        let const_f64: &[Option<u64>] = &cached.const_f64;

        let mut def_pos_in_block = BTreeMap::<Value, usize>::new();
        let mut last_store_before = vec![None; func.blocks[block_idx].insts.len() + 1];
        let mut cur_store = None;
        for (idx, &(res, op)) in func.blocks[block_idx].insts.iter().enumerate() {
            last_store_before[idx] = cur_store;
            if matches!(
                op,
                Op::Store { .. }
                    | Op::FrameStore { .. }
                    | Op::ScopeWritten(_)
                    | Op::PageCross { .. }
                    | Op::FrameDisjoint { .. }
            ) {
                cur_store = Some(idx);
            }
            if let Some(v) = res {
                def_pos_in_block.insert(v, idx);
            }
        }
        last_store_before[func.blocks[block_idx].insts.len()] = cur_store;
        let mut def_op = vec![None; n];
        for b in &func.blocks {
            for &(res, op) in &b.insts {
                if let Some(v) = res {
                    def_op[v.0 as usize] = Some(op);
                }
            }
        }

        // Group isomorphic channels by `ChannelKey`, preserving guest order.
        let mut by_key = BTreeMap::<ChannelKey, Vec<Channel>>::new();
        for ch in channels {
            by_key.entry(ch.key).or_default().push(ch);
        }

        // First collect valid independent chunks of `key.width()` channels,
        // then price the combined set of groups sharing broadcasts in this block.
        let mut tentative: Vec<(ChannelKey, Vec<Channel>, usize)> = Vec::new();
        for (key, list) in by_key {
            let width = key.width();
            let mut usable: Vec<Channel> = Vec::new();
            for ch in list {
                usable.push(ch);
                if usable.len() == width {
                    let insert_at = usable.iter().map(|c| c.indices[0]).min().unwrap();
                    let ok = usable.iter().all(|c| {
                        c.inputs.iter().all(|&v| {
                            available_before(
                                v,
                                block_idx,
                                insert_at,
                                def_block,
                                &def_pos_in_block,
                                const_f64,
                            )
                        })
                    });
                    if ok {
                        tentative.push((key, std::mem::take(&mut usable), insert_at));
                    } else {
                        // Drop the earliest channel and keep scanning.
                        usable.remove(0);
                    }
                }
            }
        }
        if tentative.is_empty() {
            continue;
        }

        // Estimate live vector/FP registers entering `block_idx` plus proven
        // minimum trip count so header setup is priced against real trips and
        // register-spill pressure rather than an unverified fixed multiplier.
        let min_trips = known_min_trips(func, block_idx);
        // Count non-Block(0) uses so Block(0) entry DomainGuards don't make
        // single-use loop invariants look multi-use inside the loop body.
        let mut loop_uses = vec![0u32; n];
        for (bi, b) in func.blocks.iter().enumerate() {
            if bi == 0 && block_idx != 0 {
                continue;
            }
            for &(_, op) in &b.insts {
                for v in op.uses() {
                    loop_uses[v.0 as usize] = loop_uses[v.0 as usize].saturating_add(1);
                }
            }
            for t in b.end.targets() {
                for &v in &t.args {
                    loop_uses[v.0 as usize] = loop_uses[v.0 as usize].saturating_add(1);
                }
            }
        }
        let mut live_vregs: i32 = func.blocks[0]
            .params
            .iter()
            .copied()
            .chain(func.blocks[0].insts.iter().filter_map(|&(res, _)| res))
            .filter(|&v| {
                matches!(func.ty(v), Type::F64 | Type::V128)
                    && const_f64[v.0 as usize].is_none()
                    && loop_uses[v.0 as usize] > 0
            })
            .count() as i32;

        // Price each group with shared broadcasts across all accepted groups in
        // this block, keeping only strictly profitable groups. Also handle the
        // case where a shared broadcast (e.g. Dup64 on a loop induction value)
        // only breaks even when amortized across >= 2 groups of the same key.
        let price_ctx = PriceCtx {
            func,
            block_idx,
            def_block,
            const_f64,
            loop_uses: &loop_uses,
            def_op: &def_op,
            def_pos_in_block: &def_pos_in_block,
            last_store_before: &last_store_before,
        };
        let mut shared_bcasts = BTreeSet::<(bool, Value)>::new();
        let mut accepted: Vec<GroupPlan> = Vec::new();
        let mut deferred = BTreeMap::<ChannelKey, Vec<(Vec<Channel>, usize)>>::new();
        for (key, chs, insert_at) in tentative {
            let mut trial_bcasts = shared_bcasts.clone();
            let mut trial_vregs = live_vregs;
            let (scalar_words, vec_body_words, header_words) = price_group(
                key,
                &chs,
                insert_at,
                &price_ctx,
                &mut trial_vregs,
                &mut trial_bcasts,
            );
            if slp_profitable(scalar_words, vec_body_words, header_words, min_trips) {
                shared_bcasts = trial_bcasts;
                live_vregs = trial_vregs;
                accepted.push(GroupPlan {
                    key,
                    channels: chs,
                    insert_at,
                });
            } else {
                deferred.entry(key).or_default().push((chs, insert_at));
            }
        }
        for (key, groups) in deferred {
            if groups.len() < 2 {
                continue;
            }
            let mut trial_bcasts = shared_bcasts.clone();
            let mut trial_vregs = live_vregs;
            let mut plans = Vec::with_capacity(groups.len());
            let (mut tot_scalar, mut tot_body, mut tot_header) = (0u32, 0u32, 0u32);
            for (chs, insert_at) in groups {
                let (sw, bw, hw) = price_group(
                    key,
                    &chs,
                    insert_at,
                    &price_ctx,
                    &mut trial_vregs,
                    &mut trial_bcasts,
                );
                tot_scalar += sw;
                tot_body += bw;
                tot_header += hw;
                plans.push(GroupPlan {
                    key,
                    channels: chs,
                    insert_at,
                });
            }
            if slp_profitable(tot_scalar, tot_body, tot_header, min_trips) {
                shared_bcasts = trial_bcasts;
                live_vregs = trial_vregs;
                accepted.extend(plans);
            }
        }

        if accepted.is_empty() {
            continue;
        }

        // Emit header packs into Block(0) and body vector ops at each group's `insert_at`.
        let mut header_insts: Vec<(Option<Value>, Op)> = Vec::new();
        let mut body_inserts: BTreeMap<usize, Vec<(Option<Value>, Op)>> = BTreeMap::new();
        let mut dead_indices = BTreeSet::<usize>::new();
        let mut header_bcasts = BTreeMap::<(bool, Value), Value>::new();
        let mut body_bcasts = BTreeMap::<(bool, Value), Value>::new();

        let alloc = |func: &mut Func, ty: Type| -> Value {
            let v = Value(func.types.len() as u32);
            func.types.push(ty);
            v
        };

        let materialize = |func: &mut Func,
                           target: &mut Vec<(Option<Value>, Op)>,
                           v: Value,
                           const_f64: &[Option<u64>]|
         -> Value {
            if let Some(bits) = const_f64[v.0 as usize] {
                let c = alloc(func, Type::F64);
                target.push((Some(c), Op::Constant(Type::F64, u128::from(bits))));
                c
            } else {
                v
            }
        };

        let emit_narrow = |func: &mut Func,
                           target: &mut Vec<(Option<Value>, Op)>,
                           v: Value,
                           const_f64: &[Option<u64>]|
         -> Value {
            let v = materialize(func, target, v, const_f64);
            let bits = alloc(func, Type::V128);
            target.push((
                Some(bits),
                Op::Bits {
                    to: Type::V128,
                    value: v,
                },
            ));
            let narrow = alloc(func, Type::V128);
            target.push((Some(narrow), Op::VecUnary(VecUnary::NarrowScalar, bits)));
            narrow
        };

        let build_slot = |func: &mut Func,
                          lanes: &[Value],
                          is_single: bool,
                          block_idx: usize,
                          def_block: &[Option<usize>],
                          const_f64: &[Option<u64>],
                          header_insts: &mut Vec<(Option<Value>, Op)>,
                          body_out: &mut Vec<(Option<Value>, Op)>,
                          header_bcasts: &mut BTreeMap<(bool, Value), Value>,
                          body_bcasts: &mut BTreeMap<(bool, Value), Value>|
         -> Value {
            let invariant = block_idx != 0 && all_in_block0(lanes, def_block, const_f64);
            if lanes.iter().all(|v| const_f64[v.0 as usize].is_some()) {
                let mut packed = 0u128;
                if is_single {
                    for (i, v) in lanes.iter().enumerate() {
                        let bits = const_f64[v.0 as usize].unwrap();
                        let f = (f64::from_bits(bits) as f32).to_bits();
                        packed |= u128::from(f) << (i * 32);
                    }
                } else {
                    for (i, v) in lanes.iter().enumerate() {
                        let bits = const_f64[v.0 as usize].unwrap();
                        packed |= u128::from(bits) << (i * 64);
                    }
                }
                let target = if invariant { header_insts } else { body_out };
                let c = alloc(func, Type::V128);
                target.push((Some(c), Op::Constant(Type::V128, packed)));
                return c;
            }
            let all_same = lanes.iter().all(|&v| v == lanes[0]);
            if all_same {
                let key = (is_single, lanes[0]);
                if invariant {
                    if let Some(&v) = header_bcasts.get(&key) {
                        return v;
                    }
                    let dup = if is_single {
                        let narrow = emit_narrow(func, header_insts, lanes[0], const_f64);
                        let d = alloc(func, Type::V128);
                        header_insts.push((Some(d), Op::VecUnary(VecUnary::Dup32, narrow)));
                        d
                    } else {
                        let v0 = materialize(func, header_insts, lanes[0], const_f64);
                        let bits = alloc(func, Type::V128);
                        header_insts.push((
                            Some(bits),
                            Op::Bits {
                                to: Type::V128,
                                value: v0,
                            },
                        ));
                        let d = alloc(func, Type::V128);
                        header_insts.push((Some(d), Op::VecUnary(VecUnary::Dup64, bits)));
                        d
                    };
                    header_bcasts.insert(key, dup);
                    return dup;
                } else {
                    if let Some(&v) = body_bcasts.get(&key) {
                        return v;
                    }
                    let dup = if is_single {
                        let narrow = emit_narrow(func, body_out, lanes[0], const_f64);
                        let d = alloc(func, Type::V128);
                        body_out.push((Some(d), Op::VecUnary(VecUnary::Dup32, narrow)));
                        d
                    } else {
                        let v0 = materialize(func, body_out, lanes[0], const_f64);
                        let bits = alloc(func, Type::V128);
                        body_out.push((
                            Some(bits),
                            Op::Bits {
                                to: Type::V128,
                                value: v0,
                            },
                        ));
                        let d = alloc(func, Type::V128);
                        body_out.push((Some(d), Op::VecUnary(VecUnary::Dup64, bits)));
                        d
                    };
                    body_bcasts.insert(key, dup);
                    return dup;
                }
            }

            let target = if invariant { header_insts } else { body_out };
            if is_single {
                let mut acc = emit_narrow(func, target, lanes[0], const_f64);
                for (lane, &val) in lanes.iter().enumerate().skip(1) {
                    let nv = emit_narrow(func, target, val, const_f64);
                    let next = alloc(func, Type::V128);
                    target.push((Some(next), Op::Vec(VecOp::Insert32(lane as u8), acc, nv)));
                    acc = next;
                }
                acc
            } else {
                let v0 = materialize(func, target, lanes[0], const_f64);
                let b0 = alloc(func, Type::V128);
                target.push((
                    Some(b0),
                    Op::Bits {
                        to: Type::V128,
                        value: v0,
                    },
                ));
                let v1 = materialize(func, target, lanes[1], const_f64);
                let b1 = alloc(func, Type::V128);
                target.push((
                    Some(b1),
                    Op::Bits {
                        to: Type::V128,
                        value: v1,
                    },
                ));
                let packed = alloc(func, Type::V128);
                target.push((Some(packed), Op::Vec(VecOp::Insert64(true, false), b0, b1)));
                packed
            }
        };

        // Sort accepted groups by `insert_at` so shared body broadcasts dominate
        // all later groups in the same block!
        accepted.sort_by_key(|g| g.insert_at);
        let allow_wide_mem = func.resident && !crate::translate::opt::back::is_compact_window();
        for group in accepted {
            let is_single = group.key.is_single();
            let double = !is_single;
            let n_slots = group.channels[0].inputs.len();
            let body_out = body_inserts.entry(group.insert_at).or_default();

            let mut vec_inputs = vec![None; n_slots];
            let slot_lanes: Vec<Vec<Value>> = (0..n_slots)
                .map(|s| group.channels.iter().map(|c| c.inputs[s]).collect())
                .collect();

            // Detect 2/3/4-channel AoS strided loads (`LD2`/`LD3`/`LD4`), contiguous
            // 16-byte vector loads (`LDR Q`), and broadcast-from-memory (`LD1R`).
            if allow_wide_mem {
                let elem = if is_single { 4i32 } else { 8i32 };
                let strided: Vec<Option<(Value, i32, u8, Value, u32)>> = slot_lanes
                    .iter()
                    .map(|lanes| {
                        let invariant =
                            block_idx != 0 && all_in_block0(lanes, def_block, const_f64);
                        if invariant || lanes.iter().all(|&v| v == lanes[0]) {
                            return None;
                        }
                        detect_strided_slot(
                            lanes,
                            is_single,
                            group.insert_at,
                            &def_op,
                            &def_pos_in_block,
                            &last_store_before,
                        )
                    })
                    .collect();

                // First match AoS groups sharing `(base, channels in 2..=4)` at
                // offsets `base_off + ch * elem`.
                for s in 0..n_slots {
                    if vec_inputs[s].is_some() {
                        continue;
                    }
                    let Some((base, off0, channels, addr0, pc)) = strided[s] else {
                        continue;
                    };
                    if channels == 1 {
                        let v = alloc(func, Type::V128);
                        body_out.push((
                            Some(v),
                            Op::Load {
                                ty: Type::V128,
                                width: 16,
                                addr: addr0,
                                checked: false,
                                pc,
                                scope: None,
                                clock: false,
                            },
                        ));
                        vec_inputs[s] = Some(v);
                    } else if (2..=4).contains(&channels) {
                        // Look for sibling slots belonging to the same AoS struct.
                        let mut members: Vec<(usize, i32, Value, u32)> = vec![(s, off0, addr0, pc)];
                        for other in s + 1..n_slots {
                            if vec_inputs[other].is_none()
                                && let Some((ob, ooff, och, oaddr, opc)) = strided[other]
                                && ob == base
                                && och == channels
                                && (ooff - off0).abs() < i32::from(channels) * elem
                                && (ooff - off0) % elem == 0
                            {
                                members.push((other, ooff, oaddr, opc));
                            }
                        }
                        members.sort_by_key(|&(_, off, ..)| off);
                        let min_off = members[0].1;
                        let first_addr = members[0].2;
                        let first_pc = members[0].3;
                        let mut unique_offs: Vec<i32> =
                            members.iter().map(|&(_, off, ..)| off).collect();
                        unique_offs.dedup();
                        // Require every channel `0..channels` to be referenced by the guest
                        // so `LD2`/`LD3`/`LD4` never over-reads unreferenced adjacent bytes.
                        let all_channels_covered = unique_offs.len() == usize::from(channels)
                            && unique_offs
                                .iter()
                                .enumerate()
                                .all(|(ch, &off)| off == min_off + (ch as i32) * elem);
                        if all_channels_covered {
                            // Emit consecutive `Op::VecDeintLoad` for channels 0..channels
                            // so `opt/back.rs` fuses them into a single `LD2`/`LD3`/`LD4`.
                            let mut ch_vals = Vec::with_capacity(usize::from(channels));
                            for ch in 0..channels {
                                let v = alloc(func, Type::V128);
                                body_out.push((
                                    Some(v),
                                    Op::VecDeintLoad {
                                        addr: first_addr,
                                        channels,
                                        channel: ch,
                                        double,
                                        pc: first_pc,
                                    },
                                ));
                                ch_vals.push(v);
                            }
                            for (slot_idx, off, ..) in members {
                                let ch = ((off - min_off) / elem) as usize;
                                vec_inputs[slot_idx] = Some(ch_vals[ch]);
                            }
                        }
                    }
                }
            }

            // Broadcast-from-memory (`LD1R`) for non-invariant all-same memory slots.
            for s in 0..n_slots {
                if vec_inputs[s].is_some() {
                    continue;
                }
                let lanes = &slot_lanes[s];
                let invariant = block_idx != 0 && all_in_block0(lanes, def_block, const_f64);
                if !invariant
                    && lanes.iter().all(|&v| v == lanes[0])
                    && let Some(ml) = mem_lane_of(
                        lanes[0],
                        is_single,
                        group.insert_at,
                        &def_op,
                        &def_pos_in_block,
                        &last_store_before,
                    )
                {
                    let key = (is_single, lanes[0]);
                    if let Some(&cached) = body_bcasts.get(&key) {
                        vec_inputs[s] = Some(cached);
                    } else {
                        let v = alloc(func, Type::V128);
                        body_out.push((
                            Some(v),
                            Op::VecLoadBcast {
                                addr: ml.addr,
                                double,
                                pc: ml.pc,
                            },
                        ));
                        body_bcasts.insert(key, v);
                        vec_inputs[s] = Some(v);
                    }
                }
            }

            let vec_inputs: Vec<Value> = (0..n_slots)
                .map(|s| {
                    if let Some(v) = vec_inputs[s] {
                        v
                    } else {
                        build_slot(
                            func,
                            &slot_lanes[s],
                            is_single,
                            block_idx,
                            def_block,
                            const_f64,
                            &mut header_insts,
                            body_out,
                            &mut header_bcasts,
                            &mut body_bcasts,
                        )
                    }
                })
                .collect();

            let (mid_vec, vec_res, store_kind) = match group.key {
                ChannelKey::Single2(op1, op2, sk) => {
                    let t = alloc(func, Type::V128);
                    body_out.push((
                        Some(t),
                        Op::VecFp {
                            op: op1,
                            double: false,
                            a: vec_inputs[0],
                            b: vec_inputs[1],
                        },
                    ));
                    let r = alloc(func, Type::V128);
                    body_out.push((
                        Some(r),
                        Op::VecFp {
                            op: op2,
                            double: false,
                            a: t,
                            b: vec_inputs[2],
                        },
                    ));
                    (Some(t), r, sk)
                }
                ChannelKey::SingleFma(sub, sk) => {
                    let r = alloc(func, Type::V128);
                    body_out.push((
                        Some(r),
                        Op::VecFma {
                            sub,
                            double: false,
                            a: vec_inputs[0],
                            b: vec_inputs[1],
                            c: vec_inputs[2],
                        },
                    ));
                    (None, r, sk)
                }
                ChannelKey::Single1(op1, sk) => {
                    let r = alloc(func, Type::V128);
                    body_out.push((
                        Some(r),
                        Op::VecFp {
                            op: op1,
                            double: false,
                            a: vec_inputs[0],
                            b: vec_inputs[1],
                        },
                    ));
                    (None, r, sk)
                }
                ChannelKey::Double2(op1, op2, rc) => {
                    let t = alloc(func, Type::V128);
                    body_out.push((
                        Some(t),
                        Op::VecFp {
                            op: op1,
                            double: true,
                            a: vec_inputs[0],
                            b: vec_inputs[1],
                        },
                    ));
                    let r = alloc(func, Type::V128);
                    body_out.push((
                        Some(r),
                        Op::VecFp {
                            op: op2,
                            double: true,
                            a: t,
                            b: vec_inputs[2],
                        },
                    ));
                    (Some(t), r, StoreKind::Int32(rc))
                }
                ChannelKey::DoubleFma(sub, rc) => {
                    let r = alloc(func, Type::V128);
                    body_out.push((
                        Some(r),
                        Op::VecFma {
                            sub,
                            double: true,
                            a: vec_inputs[0],
                            b: vec_inputs[1],
                            c: vec_inputs[2],
                        },
                    ));
                    (None, r, StoreKind::Int32(rc))
                }
            };

            let extract_src = match store_kind {
                StoreKind::Int32(rounding) => {
                    let ints = alloc(func, Type::V128);
                    body_out.push((
                        Some(ints),
                        Op::VecFpToInt {
                            value: vec_res,
                            double,
                            rounding,
                        },
                    ));
                    ints
                }
                StoreKind::F32 => vec_res,
            };

            for (lane, ch) in group.channels.iter().enumerate() {
                let s_lane = if double { (lane * 2) as u8 } else { lane as u8 };
                // Reuse the exact original `ch.output` SSA value so all downstream
                // guest `Op::Store`s stay untouched in exact guest order!
                body_out.push((
                    Some(ch.output),
                    Op::VecToInt(VecToInt::Extract32(s_lane), extract_src),
                ));
                for &idx in &ch.indices {
                    dead_indices.insert(idx);
                }

                // Rewrite cold recovery records referencing replaced `prod_val` or
                // `mid_val` to lazy vector-lane recipes (zero hot-path cost).
                let lane_u8 = lane as u8;
                for rec in &mut func.records {
                    let Some(x87) = &mut rec.x87 else { continue };
                    for slot in x87.stack.iter_mut().flatten() {
                        if *slot == Location::At(ch.prod_val) {
                            *slot = Location::Recipe(Recipe::VecLane {
                                vec: vec_res,
                                lane: lane_u8,
                                double,
                            });
                        } else if let Some(mid) = ch.mid_val
                            && let Some(t) = mid_vec
                            && *slot == Location::At(mid)
                        {
                            *slot = Location::Recipe(Recipe::VecLane {
                                vec: t,
                                lane: lane_u8,
                                double,
                            });
                        }
                    }
                    if let Some(mid) = ch.mid_val
                        && let Some(t) = mid_vec
                        && let Some(Location::Recipe(Recipe::C1 { op, a, b, single })) = x87.c1
                    {
                        if a == mid {
                            x87.c1 = Some(Location::Recipe(Recipe::C1Vec {
                                op,
                                a: t,
                                b: vec_inputs[2],
                                lane: lane_u8,
                                single,
                            }));
                        } else if b == mid {
                            x87.c1 = Some(Location::Recipe(Recipe::C1Vec {
                                op,
                                a: vec_inputs[2],
                                b: t,
                                lane: lane_u8,
                                single,
                            }));
                        }
                    }
                }
            }
        }

        // Splice emitted body instructions into `func.blocks[block_idx]` while
        // removing replaced scalar ops and keeping every guest `Op::Store` intact.
        let old = std::mem::take(&mut func.blocks[block_idx].insts);
        let mut new_insts = Vec::with_capacity(old.len() + 16);
        for (idx, inst) in old.into_iter().enumerate() {
            if let Some(emitted) = body_inserts.remove(&idx) {
                new_insts.extend(emitted);
            }
            if !dead_indices.contains(&idx) {
                new_insts.push(inst);
            }
        }
        func.blocks[block_idx].insts = new_insts;

        // Append hoisted loop-invariant packs at the end of `Block(0)` (after all
        // `Block(0)` defs/guards/loads, right before its terminator).
        if !header_insts.is_empty() {
            func.blocks[0].insts.extend(header_insts);
        }
        tables = None;
        any_changed = true;
    }

    any_changed
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: A64-OPT-2
    #[test]
    fn phase4_slp_skips_pure_integer_blocks_without_scanning_func() {
        use crate::translate::opt::{
            ir::{Head, Source, Target},
            recovery::IrRecord,
            verify,
        };

        const OPS: usize = 1024;
        const BLOCKS: usize = 128;
        let mut func = Func {
            kept: 0x00ff_fffe,
            entry: vec![Source::Reg(0), Source::Eflags],
            ..Func::default()
        };
        let mut block = func.block(None);
        let mut value = func.param(block, Type::I32);
        let flags = func.param(block, Type::Flags);
        let mut record = IrRecord {
            eip: 0,
            refund: 0,
            regs: [None; 8],
            eflags: Some(Location::At(flags)),
            flag_groups: vec![],
            x87: None,
            xmm: [None; 8],
            mmx: [None; 8],
            mmx_meta: None,
            frame: vec![],
        };
        record.regs[0] = Some(Location::At(value));
        let entry = func.record(record.clone());
        func.blocks[0].head = Some(Head { record: entry });
        for index in 0..BLOCKS {
            if index != 0 {
                let next = func.block(None);
                func.end(
                    block,
                    End::Jump(Target {
                        block: next,
                        args: vec![],
                        charge: 0,
                    }),
                );
                block = next;
            }
            for _ in 0..OPS / BLOCKS {
                value = func.push(block, Op::Int(IntOp::Add, value, value)).unwrap();
            }
        }
        record.regs[0] = Some(Location::At(value));
        let exit = func.record(record);
        func.end(block, End::Exit(exit));
        assert_eq!(verify::verify(&func), Ok(()));
        assert!(!run(&mut func));
    }
}

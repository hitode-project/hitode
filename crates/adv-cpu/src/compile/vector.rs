//! Integer vectorization of versioned loops. A general kernel runs `LANES`
//! consecutive iterations of a qualifying loop as one batch: the values the
//! iterations compute live in host vector lanes, one lane per iteration,
//! while the pointers and the counter stay scalar with the first lane's
//! values. A batch starts only when the counter and the fuel cover all of it.
//!
//! Every lane must follow the same path. At a branch where the lanes
//! disagree, or one would leave the loop, the batch restarts as scalar code:
//! one iteration runs in the scalar clone before the next batch. No batch
//! writes memory before a data-dependent branch, so restarting it is exact.
//!
//! Lanes run instruction by instruction rather than iteration by iteration,
//! so no lane may touch bytes another lane writes. Operands through the same
//! registers are checked here; the others when the kernel starts, from their
//! distance. No value can flow from one iteration into the next: each
//! register an iteration computes is written in it before it is read. A
//! register written on one path keeps its value on the other, which a batch
//! whose lanes agree on the path preserves in its last lane: the last lane
//! is the state after the batch.
//!
//! Bit-preserving DWORD copies pack four iterations densely and reconstruct
//! their scalar state at the batch-loop exit using the same admission proofs.
use super::*;
use iced_x86::{InstructionInfoFactory, OpAccess, RflagsBits};

/// Iterations per batch. Each lane of an MMX register is one 64-bit half of
/// a host vector; each lane of a general register is an even 32-bit element
/// of an `I32X4`, which `movd` to and from MMX keeps in place.
pub(super) const LANES: i64 = 2;

/// A bit-preserving DWORD load/store pair. Its four packed lanes have no
/// per-lane arithmetic or branches; the last scalar value is rebuilt on exit.
#[derive(Debug, Clone, Copy)]
struct CopyBatch {
    load: u32,
    store: u32,
    register: R,
}

const COPY_LANES: i64 = 4;

#[derive(Debug, Clone)]
pub(super) struct Batch {
    copy: Option<CopyBatch>,
    /// General and MMX registers the iterations compute.
    pub general: u8,
    pub mmx: u8,
    /// First of the pointer and counter steps closing each iteration, which
    /// a batch runs once with its last lane's values.
    pub tail: u32,
    /// Whether the tail leaves exact flags after a batch. Otherwise flags the
    /// body sets and the tail keeps hold no lane's values: a scalar
    /// iteration, which sets them before any exit, follows every batch.
    pub exact: bool,
    /// Block starts of the batch body.
    pub starts: BTreeSet<u32>,
    /// Operand groups through different registers, one of them written,
    /// whose lanes the kernel keeps apart when it starts.
    pub checks: Vec<(loops::Group, loops::Group)>,
}

impl Batch {
    pub(super) fn copies(&self) -> bool {
        self.copy.is_some()
    }
    fn width(&self) -> i64 {
        if self.copy.is_some() {
            COPY_LANES
        } else {
            LANES
        }
    }
}

fn copy_batch(plan: &loops::Plan, body: &[&Instruction], tail: u32) -> Option<CopyBatch> {
    let operations: Vec<_> = body
        .iter()
        .copied()
        .filter(|i| i.ip32() < tail && i.mnemonic() != M::Nop)
        .collect();
    let [load, store] = operations.as_slice() else {
        return None;
    };
    let register = load.op0_register();
    let plain = load.mnemonic() == M::Mov && register.is_gpr32();
    let packed = load.mnemonic() == M::Movd && mmx::register(register).is_some();
    if !(plain || packed)
        || load.op0_kind() != OpKind::Register
        || load.op1_kind() != OpKind::Memory
        || store.mnemonic() != load.mnemonic()
        || store.op0_kind() != OpKind::Memory
        || store.op1_kind() != OpKind::Register
        || store.op1_register() != register
        || load.memory_size().size() != 4
        || store.memory_size().size() != 4
    {
        return None;
    }
    for pc in [load.ip32(), store.ip32()] {
        let (_, access, _, _) = plan.accesses.iter().find(|(at, ..)| *at == pc)?;
        if access.step != 4 {
            return None;
        }
    }
    // Cleanup replays ONLY the register-only affine tail's last iteration.
    // No hidden memory effects or control flow may be deferred there.
    if body.iter().filter(|i| i.ip32() >= tail).any(|i| {
        !plan.updates.contains(&i.ip32())
            || i.flow_control() != FlowControl::Next
            || i.op0_kind() != OpKind::Register
    }) {
        return None;
    }
    Some(CopyBatch {
        load: load.ip32(),
        store: store.ip32(),
        register,
    })
}

/// A kernel's batch blocks and lane values while it is emitted.
pub(super) struct Lanes {
    /// Splats the scalar state into lanes, then runs a batch.
    entry: ir::Block,
    blocks: BTreeMap<u32, ir::Block>,
    /// Restarts the current batch as scalar code.
    rewind: ir::Block,
    general: [Option<Variable>; 8],
    mmx: [Option<Variable>; 8],
    /// Whether the operand groups of different lanes stay apart.
    ok: Value,
    strides: BTreeMap<u32, i64>,
    /// The state a rewind restores: values at the batch start.
    start: Vec<(Variable, Value)>,
    /// Per lane, zero exactly when the zero flag is set.
    zero: Option<Value>,
    /// Whether the emitter is inside a batch, where computed registers live
    /// in lanes and an exit first takes the last lane's values.
    active: bool,
}

fn bits(mask: u8) -> impl Iterator<Item = usize> {
    (0..8).filter(move |index| mask & 1 << index != 0)
}

fn high_byte(register: R) -> bool {
    matches!(register, R::AH | R::CH | R::DH | R::BH)
}

/// Integer forms a batch computes per lane.
fn general_supported(i: &Instruction) -> bool {
    let register = |n: u32| {
        i.op_kind(n) == OpKind::Register && reg_index(i.op_register(n).full_register32()).is_some()
    };
    let wide = |n: u32| register(n) && i.op_register(n).size() == 4;
    let memory =
        |n: u32| i.op_kind(n) == OpKind::Memory && matches!(i.memory_size().size(), 1 | 2 | 4);
    let immediate = |n: u32| {
        matches!(
            i.op_kind(n),
            OpKind::Immediate8
                | OpKind::Immediate16
                | OpKind::Immediate32
                | OpKind::Immediate8to16
                | OpKind::Immediate8to32
        )
    };
    let count = i.op_count();
    match i.mnemonic() {
        M::Nop => true,
        M::Mov | M::Add | M::Sub | M::And | M::Or | M::Xor | M::Cmp | M::Test => {
            count == 2
                && (register(0) && (register(1) || memory(1) || immediate(1))
                    || memory(0) && (register(1) || immediate(1)))
        }
        M::Movzx | M::Movsx => {
            count == 2 && register(0) && i.op0_register().size() >= 2 && (register(1) || memory(1))
        }
        M::Inc | M::Dec | M::Not | M::Neg => count == 1 && register(0),
        M::Shl | M::Shr | M::Sar | M::Rol | M::Ror => {
            count == 2 && wide(0) && i.op1_kind() == OpKind::Immediate8 && i.immediate8() & 31 != 0
        }
        M::Lea => {
            count == 2
                && wide(0)
                && [i.memory_base(), i.memory_index()]
                    .iter()
                    .all(|&r| r == R::None || reg_index(r).is_some())
        }
        M::Imul => {
            let source = wide(1) || memory(1) && i.memory_size().size() == 4;
            wide(0) && source && (count == 2 || count == 3 && immediate(2))
        }
        _ => false,
    }
}

/// MMX forms a batch computes per lane: the ones whose lanes stay in their
/// own halves, with constant shift counts.
fn mmx_supported(i: &Instruction) -> bool {
    mmx::supported(i)
        && !matches!(
            i.mnemonic(),
            M::Emms | M::Pextrw | M::Pinsrw | M::Pmovmskb | M::Psadbw | M::Pmuludq
        )
        && !(matches!(
            i.mnemonic(),
            M::Psrlw | M::Psrld | M::Psrlq | M::Psllw | M::Pslld | M::Psllq | M::Psraw | M::Psrad
        ) && i.op1_kind() != OpKind::Immediate8)
}

/// Whether the zero flag a conditional branch reads comes from a lane
/// instruction of its own block.
fn zero_source(body: &[&Instruction], branch: usize, first: u32, updates: &BTreeSet<u32>) -> bool {
    for i in body[..branch].iter().rev() {
        if i.ip32() < first {
            return false;
        }
        if i.rflags_modified() & RflagsBits::ZF != 0 {
            return !updates.contains(&i.ip32())
                && matches!(
                    i.mnemonic(),
                    M::Add
                        | M::Sub
                        | M::And
                        | M::Or
                        | M::Xor
                        | M::Cmp
                        | M::Test
                        | M::Inc
                        | M::Dec
                        | M::Neg
                        | M::Shl
                        | M::Shr
                        | M::Sar
                );
        }
    }
    false
}

pub(super) fn batch(
    plan: &loops::Plan,
    instructions: &BTreeMap<u32, Instruction>,
) -> Result<Batch, String> {
    // A bound reloaded into a vector temporary needs explicit lane/scalar
    // comparison handling. Keep the proven scalar kernel until that exists.
    if matches!(plan.bound, loops::Bound::Memory(_)) {
        return Err("memory bound".into());
    }
    if !plan.windows.is_empty() {
        return Err("windows".into());
    }
    if !plan.slots.is_empty() {
        return Err("slots".into());
    }
    let body: Vec<&Instruction> = instructions
        .range(plan.header..plan.latch)
        .map(|(_, i)| i)
        .collect();
    let end = instructions.get(&plan.latch).ok_or("latch")?.next_ip32();
    let mut tail = plan.latch;
    for i in body.iter().rev() {
        if !plan.updates.contains(&i.ip32()) {
            break;
        }
        tail = i.ip32();
    }
    let mut factory = InstructionInfoFactory::new();
    let (mut general, mut mmx) = (0u8, 0u8);
    let (mut body_flags, mut tail_flags) = (0, 0);
    for i in &body {
        if i.ip32() >= tail {
            tail_flags |= i.rflags_modified();
            continue;
        }
        body_flags |= i.rflags_modified();
        if plan.updates.contains(&i.ip32()) {
            continue;
        }
        for used in factory.info(i).used_registers() {
            let register = used.register();
            let write = matches!(
                used.access(),
                OpAccess::Write
                    | OpAccess::CondWrite
                    | OpAccess::ReadWrite
                    | OpAccess::ReadCondWrite
            );
            if let Some(index) = reg_index(register.full_register32()) {
                general |= u8::from(write) << index;
            } else if let Some(index) = mmx::register(register) {
                mmx |= u8::from(write) << index;
            } else if !register.is_segment_register() {
                return Err(format!("register {register:?}").to_lowercase());
            }
        }
    }
    if general & (plan.inductions | 1 << 4) != 0 {
        return Err("induction written".into());
    }
    // The tail runs with the last lane's pointers, so its flags are the
    // batch's, unless the body sets flags the tail keeps.
    let arithmetic = RflagsBits::OF
        | RflagsBits::SF
        | RflagsBits::ZF
        | RflagsBits::AF
        | RflagsBits::CF
        | RflagsBits::PF;
    let kept = body_flags & !tail_flags & arithmetic;
    // Budget, interrupt and fault boundaries expose flags even if the next
    // guest instruction overwrites them. Keep a scalar final iteration when
    // the vector body has flags the affine tail cannot reconstruct.
    let exact = kept == 0;

    // Registers and flags written on every path so far, and whether a store
    // may have run.
    #[derive(Clone, Copy)]
    struct State {
        general: u8,
        mmx: u8,
        flags: u32,
        stored: bool,
    }
    let meet = |a: State, b: State| State {
        general: a.general & b.general,
        mmx: a.mmx & b.mmx,
        flags: a.flags & b.flags,
        stored: a.stored | b.stored,
    };
    // Without exact flags, every exit is preceded by its own iteration's.
    let leaves = |state: State| exact || kept & !state.flags == 0;
    let mut starts = plan.starts.clone();
    starts.insert(tail);
    let accesses: BTreeMap<u32, bool> = plan
        .accesses
        .iter()
        .map(|&(pc, _, _, write)| (pc, write))
        .collect();
    let mut incoming: BTreeMap<u32, State> = BTreeMap::new();
    let mut current = Some(State {
        general: 0,
        mmx: 0,
        flags: 0,
        stored: false,
    });
    for (n, i) in body.iter().enumerate() {
        let pc = i.ip32();
        let state = match (current, incoming.remove(&pc)) {
            (Some(a), Some(b)) => meet(a, b),
            (a, b) => a.or(b).ok_or("unreachable")?,
        };
        let mut merge = |target: u32, state: State| {
            let merged = incoming
                .get(&target)
                .map_or(state, |&other| meet(other, state));
            incoming.insert(target, merged);
        };
        let mut state = state;
        state.flags |= i.rflags_modified();
        current = Some(state);
        if plan.updates.contains(&pc) {
            continue;
        }
        match i.flow_control() {
            FlowControl::Next => {}
            FlowControl::ConditionalBranch => {
                let first = *starts.range(..=pc).next_back().ok_or("block")?;
                if !matches!(i.mnemonic(), M::Je | M::Jne) {
                    return Err(format!("branch {}", loops::name(i)));
                }
                if state.stored {
                    return Err("branch after store".into());
                }
                if !zero_source(&body, n, first, &plan.updates) {
                    return Err("branch flags".into());
                }
                if i.near_branch32() < end {
                    merge(i.near_branch32(), state);
                } else if !leaves(state) {
                    return Err("exit flags".into());
                }
                continue;
            }
            FlowControl::UnconditionalBranch if i.near_branch32() < end => {
                merge(i.near_branch32(), state);
                current = None;
                continue;
            }
            _ => return Err(format!("flow {}", loops::name(i))),
        }
        if !general_supported(i) && !mmx_supported(i) {
            return Err(format!("instruction {}", loops::name(i)));
        }
        let info = factory.info(i);
        if !info.used_memory().is_empty() && !matches!(i.mnemonic(), M::Lea | M::Nop) {
            state.stored |= *accesses.get(&pc).ok_or("unproven access")?;
        }
        let mut written = (0u8, 0u8);
        for used in info.used_registers() {
            let register = used.register();
            let (read, write) = match used.access() {
                OpAccess::Read | OpAccess::CondRead => (true, false),
                OpAccess::Write => (false, true),
                OpAccess::ReadWrite => (true, true),
                _ if register.is_segment_register() => continue,
                _ => return Err("conditional access".into()),
            };
            if let Some(index) = reg_index(register.full_register32()) {
                // A narrow write keeps the rest of the register.
                let read = read || write && register.size() < 4;
                if read && general & !state.general & 1 << index != 0 {
                    return Err(format!("carried {:?}", register.full_register32()).to_lowercase());
                }
                written.0 |= u8::from(write) << index;
            } else if let Some(index) = mmx::register(register) {
                if read && mmx & !state.mmx & 1 << index != 0 {
                    return Err(format!("carried {register:?}").to_lowercase());
                }
                written.1 |= u8::from(write) << index;
            }
        }
        state.general |= written.0;
        state.mmx |= written.1;
        current = Some(state);
    }
    if !leaves(current.ok_or("unreachable")?) {
        return Err("exit flags".into());
    }

    let copy = copy_batch(plan, &body, tail);
    let width = if copy.is_some() { COPY_LANES } else { LANES };
    // Lanes of operands through the same registers keep fixed distances.
    let meets = |a: &loops::Access, b: &loops::Access| {
        (0..width).any(|x| {
            (0..width).any(|y| {
                x != y
                    && a.low + x * a.step < b.high + y * b.step
                    && b.low + y * b.step < a.high + x * a.step
            })
        })
    };
    for (n, (pc, a, read, write)) in plan.accesses.iter().enumerate() {
        for (other, b, _, other_write) in &plan.accesses[n..] {
            // One instruction's lanes store in order; its own reads come first.
            let conflict = if pc == other {
                *read && *write
            } else {
                *write || *other_write
            };
            if conflict && a.base == b.base && a.index == b.index && meets(a, b) {
                return Err("lane overlap".into());
            }
        }
    }
    let mut checks = Vec::new();
    for (n, a) in plan.groups.iter().enumerate() {
        for b in &plan.groups[n + 1..] {
            if (a.write || b.write) && (a.base != b.base || a.index != b.index) {
                checks.push((a.clone(), b.clone()));
            }
        }
    }
    Ok(Batch {
        copy,
        general,
        mmx,
        tail,
        exact,
        starts,
        checks,
    })
}

impl Emitter<'_> {
    pub(super) fn lanes_active(&self) -> bool {
        self.lanes.as_ref().is_some_and(|lanes| lanes.active)
    }
    fn lanes(&mut self) -> &mut Lanes {
        self.lanes.as_mut().expect("inside a batch")
    }
    pub(super) fn lane_mmx(&mut self, register: usize) -> Variable {
        self.lanes().mmx[register].unwrap()
    }

    /// At a kernel's entry: lane copies of the registers, the check that
    /// lanes keep their operands apart, and the batch blocks.
    pub(super) fn begin_lanes(&mut self, index: usize) {
        let Some(batch) = &self.batches[index] else {
            return;
        };
        let starts = batch.starts.clone();
        let checks = batch.checks.clone();
        let inductions = self.plans[index].inductions;
        let mut general = [None; 8];
        for (register, lanes) in general.iter_mut().enumerate() {
            if inductions & 1 << register == 0 {
                let var = self.b.declare_var(types::I32X4);
                let value = self.b.use_var(self.regs[register]);
                let value = self.b.ins().splat(types::I32X4, value);
                self.b.def_var(var, value);
                *lanes = Some(var);
            }
        }
        let mut mmx = [None; 8];
        for (register, lanes) in mmx.iter_mut().enumerate() {
            if let Some(scalar) = self.mmx[register] {
                let var = self.b.declare_var(types::I8X16);
                let value = self.b.use_var(scalar);
                let value = self.mmx_shuffle(value, value, (0..8).chain(0..8).collect());
                self.b.def_var(var, value);
                *lanes = Some(var);
            }
        }
        let ok = self.lanes_apart(index, &checks);
        let strides = self.plans[index]
            .accesses
            .iter()
            .map(|(pc, access, ..)| (*pc, access.step))
            .collect();
        let entry = self.b.create_block();
        let rewind = self.b.create_block();
        self.b.set_cold_block(rewind);
        let blocks = starts
            .iter()
            .map(|&pc| (pc, self.b.create_block()))
            .collect();
        self.lanes = Some(Lanes {
            entry,
            blocks,
            rewind,
            general,
            mmx,
            ok,
            strides,
            start: Vec::new(),
            zero: None,
            active: false,
        });
    }

    /// Whether no lane's bytes of one operand group meet another lane's
    /// bytes of a group through different registers, over the iterations
    /// the kernel may run. Groups stepping alike keep their distance.
    fn lanes_apart(&mut self, index: usize, checks: &[(loops::Group, loops::Group)]) -> Value {
        let width = self.batches[index].as_ref().unwrap().width();
        let copy = self.batches[index].as_ref().unwrap().copy.is_some();
        let count = self.remaining(index);
        let more = self.b.ins().iadd_imm(count, -1);
        let mut ok = self.b.ins().iconst(types::I8, 1);
        for (a, b) in checks {
            let origin_a = self.group_origin(a);
            let origin_b = self.group_origin(b);
            let apart = if copy {
                // Four contiguous DWORDs conflict across lanes precisely
                // when their byte distance is nonzero and less than 16.
                // Equal addresses and a whole-batch distance are safe, even
                // if later batches read bytes earlier batches wrote.
                let origin_a = self.b.ins().iadd_imm(origin_a, a.low);
                let origin_b = self.b.ins().iadd_imm(origin_b, b.low);
                let distance = self.b.ins().isub(origin_b, origin_a);
                let same = self.b.ins().icmp_imm(IntCC::Equal, distance, 0);
                let shifted = self.b.ins().iadd_imm(distance, width * 4 - 1);
                let far = self.b.ins().icmp_imm(
                    IntCC::UnsignedGreaterThanOrEqual,
                    shifted,
                    width * 8 - 1,
                );
                self.b.ins().bor(same, far)
            } else if a.step == b.step {
                let distance = self.b.ins().isub(origin_b, origin_a);
                let mut apart = self.b.ins().iconst(types::I8, 1);
                for lanes in (1 - width)..width {
                    if lanes == 0 {
                        continue;
                    }
                    let shift = lanes * a.step;
                    let above = self.b.ins().icmp_imm(
                        IntCC::SignedGreaterThan,
                        distance,
                        a.low - b.high - shift,
                    );
                    let below = self.b.ins().icmp_imm(
                        IntCC::SignedLessThan,
                        distance,
                        a.high - b.low - shift,
                    );
                    let meet = self.b.ins().band(above, below);
                    let clear = self.b.ins().bxor_imm(meet, 1);
                    apart = self.b.ins().band(apart, clear);
                }
                apart
            } else {
                let (start_a, end_a) = self.group_span(a, origin_a, more);
                let (start_b, end_b) = self.group_span(b, origin_b, more);
                let below = self
                    .b
                    .ins()
                    .icmp(IntCC::SignedLessThanOrEqual, end_a, start_b);
                let above = self
                    .b
                    .ins()
                    .icmp(IntCC::SignedLessThanOrEqual, end_b, start_a);
                self.b.ins().bor(below, above)
            };
            ok = self.b.ins().band(ok, apart);
        }
        ok
    }
    /// A group's register sum, wrapped like the loop proof's addresses.
    fn group_origin(&mut self, group: &loops::Group) -> Value {
        let mut address = self.constant(0);
        if let Some(base) = group.base {
            let value = self.b.use_var(self.regs[base]);
            address = self.b.ins().iadd(address, value);
        }
        if let Some((index, scale)) = group.index {
            let value = self.b.use_var(self.regs[index]);
            let scaled = self.b.ins().imul_imm(value, i64::from(scale));
            address = self.b.ins().iadd(address, scaled);
        }
        self.b.ins().uextend(types::I64, address)
    }
    fn group_span(&mut self, group: &loops::Group, origin: Value, more: Value) -> (Value, Value) {
        let mut start = self.b.ins().iadd_imm(origin, group.low);
        let mut end = self.b.ins().iadd_imm(origin, group.high);
        let span = self.b.ins().imul_imm(more, group.step.abs());
        if group.step >= 0 {
            end = self.b.ins().iadd(end, span);
        } else {
            start = self.b.ins().isub(start, span);
        }
        (start, end)
    }

    /// Whether the counter has a whole batch left before its stop value,
    /// and the scalar iteration after it unless the batch has exact flags.
    fn batch_room(&mut self, index: usize) -> Value {
        let batch = self.batches[index].as_ref().unwrap();
        let (exact, width) = (batch.exact, batch.width());
        let plan = &self.plans[index];
        let (counter, magnitude) = (plan.counter, plan.steps[plan.counter].abs());
        let (_, stop, _) = self.kernel.expect("batches run in kernels");
        let count = self.b.use_var(self.regs[counter]);
        let stop = self.b.use_var(stop);
        // The distance is a whole number of steps, represented in I32.
        // Do not truncate a wide stride's required distance into that type:
        // a two-lane batch with stride 2^31 would otherwise require zero.
        let required = (width + i64::from(!exact)) * magnitude;
        if required > i64::from(u32::MAX) {
            return self.b.ins().iconst(types::I8, 0);
        }
        let left = self.counter_distance(index, count, stop);
        self.b
            .ins()
            .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, left, required)
    }
    /// Continues loop `index` at its header: as a batch when one fits, else
    /// as one scalar iteration.
    pub(super) fn resume_clone(&mut self, index: usize) {
        let fast = self.fast_blocks[index][&self.plans[index].header];
        let Some((ok, entry)) = self.lanes.as_ref().map(|lanes| (lanes.ok, lanes.entry)) else {
            self.b.ins().jump(fast, &[]);
            return;
        };
        let room = self.batch_room(index);
        let run = self.b.ins().band(room, ok);
        self.b.ins().brif(run, entry, &[], fast, &[]);
    }

    /// The scalar state after the batch so far: its last lane's registers.
    pub(super) fn materialize_lanes(&mut self) {
        let index = self.kernel.expect("batches run in kernels").0;
        let batch = self.batches[index].as_ref().unwrap();
        let (general, mmx) = (batch.general, batch.mmx);
        for register in bits(general) {
            let var = self.lanes().general[register].unwrap();
            let value = self.b.use_var(var);
            let value = self.b.ins().extractlane(value, (2 * (LANES - 1)) as u8);
            self.b.def_var(self.regs[register], value);
        }
        for register in bits(mmx) {
            let var = self.lanes().mmx[register].unwrap();
            let value = self.b.use_var(var);
            let last = (8 * (LANES - 1)) as u8;
            let value = self.mmx_shuffle(
                value,
                value,
                (last..last + 8).chain(last..last + 8).collect(),
            );
            self.b.def_var(self.mmx[register].unwrap(), value);
        }
    }

    /// Emits the batch body of kernel `index`, after its scalar clone.
    pub(super) fn emit_lanes(&mut self, index: usize, instructions: &BTreeMap<u32, Instruction>) {
        if let Some(copy) = self.batches[index].as_ref().and_then(|batch| batch.copy) {
            self.emit_copy_batch(index, copy, instructions);
            return;
        }
        let Some(lanes) = &self.lanes else {
            return;
        };
        let (entry, rewind, blocks) = (lanes.entry, lanes.rewind, lanes.blocks.clone());
        let batch = self.batches[index].as_ref().unwrap();
        let (general, mmx, tail) = (batch.general, batch.mmx, batch.tail);
        let plan = &self.plans[index];
        let (header, latch, inductions, steps) =
            (plan.header, plan.latch, plan.inductions, plan.steps);
        let updates = plan.updates.clone();
        let end = instructions[&latch].next_ip32();
        self.fast = Some(index);

        self.b.switch_to_block(entry);
        self.mmx_effects = None;
        for register in bits(general) {
            let value = self.b.use_var(self.regs[register]);
            let value = self.b.ins().splat(types::I32X4, value);
            let var = self.lanes().general[register].unwrap();
            self.b.def_var(var, value);
        }
        for register in bits(mmx) {
            let value = self.b.use_var(self.mmx[register].unwrap());
            let value = self.mmx_shuffle(value, value, (0..8).chain(0..8).collect());
            let var = self.lanes().mmx[register].unwrap();
            self.b.def_var(var, value);
        }
        self.b.ins().jump(blocks[&header], &[]);

        for (&start, &block) in &blocks {
            self.b.switch_to_block(block);
            self.pc = start;
            self.begin_mmx_block();
            self.lanes().active = true;
            self.lanes().zero = None;
            if start == header {
                let mut vars: Vec<Variable> = self.lazy.to_vec();
                vars.push(self.flags);
                vars.extend(self.written);
                vars.extend(self.mmx_mode);
                vars.extend(bits(inductions).map(|register| self.regs[register]));
                let lanes = self.lanes();
                vars.extend(bits(general).map(|register| lanes.general[register].unwrap()));
                vars.extend(bits(mmx).map(|register| lanes.mmx[register].unwrap()));
                let start = vars
                    .into_iter()
                    .map(|var| (var, self.b.use_var(var)))
                    .collect();
                self.lanes().start = start;
            }
            if self.written.is_some() {
                let mask = nest_emit::block_writes(
                    instructions,
                    start,
                    &self.batches[index].as_ref().unwrap().starts,
                );
                self.record_writes(mask);
            }
            if start == tail {
                // The tail steps from the last lane's values.
                for register in bits(inductions) {
                    if steps[register] != 0 {
                        let value = self.b.use_var(self.regs[register]);
                        let value = self.b.ins().iadd_imm(value, (LANES - 1) * steps[register]);
                        self.b.def_var(self.regs[register], value);
                    }
                }
            }
            let mut pc = start;
            loop {
                if pc != start && blocks.contains_key(&pc) {
                    self.flush_mmx_effects();
                    self.b.ins().jump(blocks[&pc], &[]);
                    break;
                }
                if pc == latch {
                    self.lanes_latch(index, &instructions[&latch]);
                    break;
                }
                let i = &instructions[&pc];
                self.at(pc);
                if updates.contains(&pc) {
                    self.instruction(i);
                    if i.rflags_modified() & RflagsBits::ZF != 0 {
                        self.lanes().zero = None;
                    }
                } else {
                    match i.flow_control() {
                        FlowControl::ConditionalBranch => {
                            self.lanes_branch(i, end, &blocks, rewind);
                            break;
                        }
                        FlowControl::UnconditionalBranch => {
                            self.flush_mmx_effects();
                            self.b.ins().jump(blocks[&i.near_branch32()], &[]);
                            break;
                        }
                        _ => {
                            if mmx::supported(i) {
                                self.mmx_instruction(i);
                            } else {
                                self.lane_instruction(i);
                            }
                        }
                    }
                }
                pc = i.next_ip32();
            }
        }

        // A rewind restores the batch start, as the scalar clone's state.
        self.b.switch_to_block(rewind);
        self.mmx_effects = None;
        for (var, value) in std::mem::take(&mut self.lanes().start) {
            self.b.def_var(var, value);
        }
        self.materialize_lanes();
        self.lanes().active = false;
        let fast = self.fast_blocks[index][&header];
        self.b.ins().jump(fast, &[]);
    }

    /// A copy uses the same admission/range/alias proofs as arithmetic batches,
    /// but keeps only native pointers and a funded batch counter on its backedge.
    /// Scalar tail semantics, flags and the last MOV/MOVD value are reconstructed
    /// on the common exit, before either an interrupt or scalar continuation.
    fn emit_copy_batch(
        &mut self,
        index: usize,
        copy: CopyBatch,
        instructions: &BTreeMap<u32, Instruction>,
    ) {
        let entry = self.lanes.as_ref().unwrap().entry;
        let plan = &self.plans[index];
        let (header, latch, counter, inductions, steps) = (
            plan.header,
            plan.latch,
            plan.counter,
            plan.inductions,
            plan.steps,
        );
        let tail = self.batches[index].as_ref().unwrap().tail;
        self.fast = Some(index);
        self.b.switch_to_block(entry);
        self.lanes().active = false;
        self.mmx_effects = None;
        self.at(copy.load);
        let (source, offset) = self.proven(4).unwrap();
        let source = self.b.ins().iadd_imm(source, i64::from(offset));
        self.at(copy.store);
        let (destination, offset) = self.proven(4).unwrap();
        let destination = self.b.ins().iadd_imm(destination, i64::from(offset));
        let count = self.b.use_var(self.regs[counter]);
        let (_, stop, _) = self.kernel.unwrap();
        let stop = self.b.use_var(stop);
        let available = self.counter_steps(index, count, stop);
        let batches = self.b.ins().ushr_imm(available, 2);
        let body = self.b.create_block();
        self.b.append_block_param(body, self.ptr_ty);
        self.b.append_block_param(body, self.ptr_ty);
        self.b.append_block_param(body, types::I32);
        let cleanup = self.b.create_block();
        self.b.append_block_param(cleanup, types::I8);
        self.b.set_cold_block(cleanup);
        self.b
            .ins()
            .jump(body, &[source.into(), destination.into(), batches.into()]);
        self.b.switch_to_block(body);
        let source = self.b.block_params(body)[0];
        let destination = self.b.block_params(body)[1];
        let remaining = self.b.block_params(body)[2];
        let value = self.b.ins().load(types::I32X4, MemFlags::new(), source, 0);
        self.b.ins().store(MemFlags::new(), value, destination, 0);
        let remaining = self.b.ins().iadd_imm(remaining, -1);
        let again = self.b.create_block();
        let no_interrupt = self.b.ins().iconst(types::I8, 0);
        self.b
            .ins()
            .brif(remaining, again, &[], cleanup, &[no_interrupt.into()]);
        self.b.switch_to_block(again);
        let pending = self
            .b
            .ins()
            .atomic_load(types::I8, MemFlags::trusted(), self.interrupt);
        let advance = self.b.create_block();
        self.b
            .ins()
            .brif(pending, cleanup, &[pending.into()], advance, &[]);
        self.b.switch_to_block(advance);
        let source = self.b.ins().iadd_imm(source, COPY_LANES * 4);
        let destination = self.b.ins().iadd_imm(destination, COPY_LANES * 4);
        self.b
            .ins()
            .jump(body, &[source.into(), destination.into(), remaining.into()]);

        self.b.switch_to_block(cleanup);
        if self.written.is_some() {
            let mask = instructions
                .range(header..=latch)
                .fold(0, |mask, (_, i)| mask | nest::written(i));
            self.record_writes(mask);
        }
        let pending = self.b.block_params(cleanup)[0];
        let completed = self.b.ins().isub(batches, remaining);
        let completed = self.b.ins().ishl_imm(completed, 2);
        let preceding = self.b.ins().iadd_imm(completed, -1);
        for register in bits(inductions) {
            let original = self.b.use_var(self.regs[register]);
            let delta = self.b.ins().imul_imm(preceding, steps[register]);
            let last = self.b.ins().iadd(original, delta);
            self.b.def_var(self.regs[register], last);
        }
        self.begin_mmx_block();
        let last = self.b.ins().extractlane(value, (COPY_LANES - 1) as u8);
        if let Some(register) = mmx::register(copy.register) {
            let last = self.b.ins().uextend(types::I64, last);
            let last = self.b.ins().scalar_to_vector(types::I64X2, last);
            let last = self.mmx_cast(last, types::I8X16);
            self.b.def_var(self.mmx[register].unwrap(), last);
            self.mmx_written(Some(register));
        } else {
            self.b
                .def_var(self.regs[reg_index(copy.register).unwrap()], last);
        }
        for (&pc, instruction) in instructions.range(tail..latch) {
            self.at(pc);
            self.instruction(instruction);
        }
        let interrupted = self.b.create_block();
        self.b.set_cold_block(interrupted);
        let finished = self.b.create_block();
        self.b.ins().brif(pending, interrupted, &[], finished, &[]);
        self.b.switch_to_block(interrupted);
        self.exit_const(Exit::Interrupted, header);
        self.b.switch_to_block(finished);
        self.at(latch);
        self.instruction(&instructions[&latch]);
    }

    /// The batch's back edge: another batch while one fits, else the scalar
    /// latch with the last lane's state. Without exact flags, the funded
    /// iteration left runs as scalar code before the kernel can leave.
    fn lanes_latch(&mut self, index: usize, latch: &Instruction) {
        let exact = self.batches[index].as_ref().unwrap().exact;
        let header = self.plans[index].header;
        let fast = self.fast_blocks[index][&header];
        self.flush_mmx_effects();
        let room = self.batch_room(index);
        let again = self.b.create_block();
        let leave = self.b.create_block();
        self.b.ins().brif(room, again, &[], leave, &[]);
        self.b.switch_to_block(again);
        if exact {
            let target = self.constant(header);
            self.interrupt_at(target, false);
        } else {
            let pending = self
                .b
                .ins()
                .atomic_load(types::I8, MemFlags::trusted(), self.interrupt);
            let interrupted = self.b.create_block();
            self.b.set_cold_block(interrupted);
            let continued = self.b.create_block();
            self.b.ins().brif(pending, interrupted, &[], continued, &[]);
            self.b.switch_to_block(interrupted);
            self.materialize_lanes();
            self.b.ins().jump(fast, &[]);
            self.b.switch_to_block(continued);
        }
        self.refresh_clone();
        let block = self.lanes().blocks[&header];
        self.b.ins().jump(block, &[]);
        self.b.switch_to_block(leave);
        self.materialize_lanes();
        self.lanes().active = false;
        if exact {
            self.at(latch.ip32());
            self.instruction(latch);
        } else {
            self.b.ins().jump(fast, &[]);
        }
    }

    /// A data-dependent branch: taken or not by every lane, else a rewind.
    /// A lane leaving the loop also rewinds, so the scalar code leaves.
    fn lanes_branch(
        &mut self,
        i: &Instruction,
        end: u32,
        blocks: &BTreeMap<u32, ir::Block>,
        rewind: ir::Block,
    ) {
        let zero = self
            .lanes()
            .zero
            .expect("a lane instruction sets the zero flag");
        let condition = if i.mnemonic() == M::Je {
            IntCC::Equal
        } else {
            IntCC::NotEqual
        };
        let taken: Vec<Value> = (0..LANES)
            .map(|lane| {
                let value = self.b.ins().extractlane(zero, (2 * lane) as u8);
                self.b.ins().icmp_imm(condition, value, 0)
            })
            .collect();
        self.flush_mmx_effects();
        let next = blocks[&i.next_ip32()];
        let target = i.near_branch32();
        if target < end {
            let mut split = self.b.ins().iconst(types::I8, 0);
            for &lane in &taken[1..] {
                let differs = self.b.ins().bxor(taken[0], lane);
                split = self.b.ins().bor(split, differs);
            }
            let uniform = self.b.create_block();
            self.b.ins().brif(split, rewind, &[], uniform, &[]);
            self.b.switch_to_block(uniform);
            self.b.ins().brif(taken[0], blocks[&target], &[], next, &[]);
        } else {
            let mut any = taken[0];
            for &lane in &taken[1..] {
                any = self.b.ins().bor(any, lane);
            }
            self.b.ins().brif(any, rewind, &[], next, &[]);
        }
    }

    fn lane_constant(&mut self, value: u32) -> Value {
        let value = self.constant(value);
        self.b.ins().splat(types::I32X4, value)
    }
    pub(super) fn lane_vector(&mut self, words: [u32; 4]) -> Value {
        let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
        let constant = self
            .b
            .func
            .dfg
            .constants
            .insert(ir::ConstantData::from(bytes));
        self.b.ins().vconst(types::I32X4, constant)
    }
    /// A general register's lanes, zero-extended from its width. The
    /// pointers and the counter hold the first lane's value, which every
    /// further lane advances by one iteration's step.
    pub(super) fn lane_register(&mut self, register: R) -> Value {
        let index = reg_index(register.full_register32()).unwrap();
        let plan = &self.plans[self.fast.unwrap()];
        let full = if plan.inductions & 1 << index != 0 {
            let step = plan.steps[index] as u32;
            let mut offsets = [0; 4];
            for lane in 0..LANES as usize {
                offsets[2 * lane] = step.wrapping_mul(lane as u32);
            }
            let value = self.b.use_var(self.regs[index]);
            let value = self.b.ins().splat(types::I32X4, value);
            let offsets = self.lane_vector(offsets);
            self.b.ins().iadd(value, offsets)
        } else {
            let var = self.lanes().general[index].unwrap();
            self.b.use_var(var)
        };
        match register.size() {
            4 => full,
            2 => {
                let mask = self.lane_constant(0xffff);
                self.b.ins().band(full, mask)
            }
            _ => {
                let value = if high_byte(register) {
                    self.b.ins().ushr_imm(full, 8)
                } else {
                    full
                };
                let mask = self.lane_constant(0xff);
                self.b.ins().band(value, mask)
            }
        }
    }
    /// Writes the low bytes of `value`'s lanes to `register`, keeping the
    /// rest of a narrow register's full one.
    fn lane_set(&mut self, register: R, value: Value) {
        let index = reg_index(register.full_register32()).unwrap();
        let var = self.lanes().general[index].unwrap();
        let (mask, value) = match register.size() {
            4 => {
                self.b.def_var(var, value);
                return;
            }
            2 => (0xffff, value),
            _ if high_byte(register) => (0xff00, self.b.ins().ishl_imm(value, 8)),
            _ => (0xff, value),
        };
        let old = self.b.use_var(var);
        let mask = self.lane_constant(mask);
        let merged = self.b.ins().bitselect(mask, value, old);
        self.b.def_var(var, merged);
    }
    /// The current instruction's proven operand in every lane: an `I64X2`
    /// for quadwords, else `I32X4` even lanes zero-extended from `width`.
    pub(super) fn lane_load(&mut self, width: u32) -> Value {
        let (address, offset) = self
            .proven(width as usize)
            .expect("batches prove every operand");
        let pc = self.pc;
        let step = self.lanes().strides[&pc];
        let flags = MemFlags::new();
        if width == 8 {
            if step == 8 {
                return self.b.ins().load(types::I64X2, flags, address, offset);
            }
            let first = self.b.ins().load(types::I64, flags, address, offset);
            let next = self.b.ins().iadd_imm(address, step);
            let second = self.b.ins().load(types::I64, flags, next, offset);
            let value = self.b.ins().scalar_to_vector(types::I64X2, first);
            return self.b.ins().insertlane(value, second, 1);
        }
        if width == 4 && step == 4 {
            let value = self.b.ins().uload32x2(flags, address, offset);
            return self.mmx_cast(value, types::I32X4);
        }
        let mut value = None;
        for lane in 0..LANES {
            let address = self.b.ins().iadd_imm(address, lane * step);
            let element = match width {
                4 => self.b.ins().load(types::I32, flags, address, offset),
                2 => self.b.ins().uload16(types::I32, flags, address, offset),
                _ => self.b.ins().uload8(types::I32, flags, address, offset),
            };
            value = Some(match value {
                None => self.b.ins().scalar_to_vector(types::I32X4, element),
                Some(vector) => self.b.ins().insertlane(vector, element, (2 * lane) as u8),
            });
        }
        value.unwrap()
    }
    /// Stores every lane of `value`, laid out as `lane_load` returns it, to
    /// the current instruction's proven operand, first lane first.
    pub(super) fn lane_store(&mut self, width: u32, value: Value) {
        let (address, offset) = self
            .proven(width as usize)
            .expect("batches prove every operand");
        let pc = self.pc;
        let step = self.lanes().strides[&pc];
        let flags = MemFlags::new();
        if width == 8 {
            let value = self.mmx_cast(value, types::I64X2);
            if step == 8 {
                self.b.ins().store(flags, value, address, offset);
                return;
            }
            for lane in 0..LANES {
                let element = self.b.ins().extractlane(value, lane as u8);
                let address = self.b.ins().iadd_imm(address, lane * step);
                self.b.ins().store(flags, element, address, offset);
            }
            return;
        }
        if width == 4 && step == 4 {
            let value = self.mmx_cast(value, types::I8X16);
            // The even doublewords, one AArch64 UZP1.
            let packed = self.mmx_shuffle(
                value,
                value,
                (0..4).chain(8..12).chain(16..20).chain(24..28).collect(),
            );
            let packed = self.mmx_cast(packed, types::I64X2);
            let bits = self.b.ins().extractlane(packed, 0);
            self.b.ins().store(flags, bits, address, offset);
            return;
        }
        let value = self.mmx_cast(value, types::I32X4);
        for lane in 0..LANES {
            let element = self.b.ins().extractlane(value, (2 * lane) as u8);
            let address = self.b.ins().iadd_imm(address, lane * step);
            match width {
                4 => self.b.ins().store(flags, element, address, offset),
                2 => self.b.ins().istore16(flags, element, address, offset),
                _ => self.b.ins().istore8(flags, element, address, offset),
            };
        }
    }
    fn lane_operand(&mut self, i: &Instruction, n: u32, width: u32) -> Value {
        match i.op_kind(n) {
            OpKind::Register => self.lane_register(i.op_register(n)),
            OpKind::Memory => self.lane_load(i.memory_size().size() as u32),
            _ => {
                let mask = if width == 4 {
                    u32::MAX
                } else {
                    (1 << (8 * width)) - 1
                };
                self.lane_constant(i.immediate(n) as u32 & mask)
            }
        }
    }
    pub(super) fn lane_write(&mut self, i: &Instruction, value: Value, width: u32) {
        if i.op0_kind() == OpKind::Memory {
            self.lane_store(width, value);
        } else {
            self.lane_set(i.op0_register(), value);
        }
    }
    fn lane_narrow(&mut self, value: Value, width: u32) -> Value {
        if width == 4 {
            return value;
        }
        let mask = self.lane_constant((1 << (8 * width)) - 1);
        self.b.ins().band(value, mask)
    }

    fn lane_instruction(&mut self, i: &Instruction) {
        let width = if i.op0_kind() == OpKind::Memory {
            i.memory_size().size() as u32
        } else {
            i.op0_register().size() as u32
        };
        match i.mnemonic() {
            M::Nop => {}
            M::Mov => {
                let value = self.lane_operand(i, 1, width);
                self.lane_write(i, value, width);
            }
            M::Movzx | M::Movsx => {
                let source = if i.op1_kind() == OpKind::Register {
                    i.op1_register().size()
                } else {
                    i.memory_size().size()
                } as u32;
                let mut value = self.lane_operand(i, 1, source);
                if i.mnemonic() == M::Movsx {
                    let shift = i64::from(32 - 8 * source);
                    value = self.b.ins().ishl_imm(value, shift);
                    value = self.b.ins().sshr_imm(value, shift);
                }
                self.lane_set(i.op0_register(), value);
            }
            M::Add | M::Sub | M::And | M::Or | M::Xor | M::Cmp | M::Test => {
                let a = self.lane_operand(i, 0, width);
                let b = self.lane_operand(i, 1, width);
                let result = match i.mnemonic() {
                    M::Add => self.b.ins().iadd(a, b),
                    M::Sub | M::Cmp => self.b.ins().isub(a, b),
                    M::And | M::Test => self.b.ins().band(a, b),
                    M::Or => self.b.ins().bor(a, b),
                    _ => self.b.ins().bxor(a, b),
                };
                let zero = self.lane_narrow(result, width);
                self.lanes().zero = Some(zero);
                if !matches!(i.mnemonic(), M::Cmp | M::Test) {
                    self.lane_write(i, result, width);
                }
            }
            M::Inc | M::Dec | M::Neg | M::Not => {
                let a = self.lane_operand(i, 0, width);
                let result = match i.mnemonic() {
                    M::Inc => {
                        let one = self.lane_constant(1);
                        self.b.ins().iadd(a, one)
                    }
                    M::Dec => {
                        let one = self.lane_constant(1);
                        self.b.ins().isub(a, one)
                    }
                    M::Neg => self.b.ins().ineg(a),
                    _ => self.b.ins().bnot(a),
                };
                if i.mnemonic() != M::Not {
                    let zero = self.lane_narrow(result, width);
                    self.lanes().zero = Some(zero);
                }
                self.lane_write(i, result, width);
            }
            M::Shl | M::Shr | M::Sar | M::Rol | M::Ror => {
                let count = i64::from(i.immediate8() & 31);
                let a = self.lane_register(i.op0_register());
                let result = match i.mnemonic() {
                    M::Shl => self.b.ins().ishl_imm(a, count),
                    M::Shr => self.b.ins().ushr_imm(a, count),
                    M::Sar => self.b.ins().sshr_imm(a, count),
                    M::Rol => {
                        let high = self.b.ins().ishl_imm(a, count);
                        let low = self.b.ins().ushr_imm(a, 32 - count);
                        self.b.ins().bor(high, low)
                    }
                    _ => {
                        let low = self.b.ins().ushr_imm(a, count);
                        let high = self.b.ins().ishl_imm(a, 32 - count);
                        self.b.ins().bor(high, low)
                    }
                };
                // Rotations leave the zero flag.
                if matches!(i.mnemonic(), M::Shl | M::Shr | M::Sar) {
                    self.lanes().zero = Some(result);
                }
                self.lane_set(i.op0_register(), result);
            }
            M::Lea => {
                let mut sum = self.lane_constant(i.memory_displacement32());
                if i.memory_base() != R::None {
                    let base = self.lane_register(i.memory_base());
                    sum = self.b.ins().iadd(sum, base);
                }
                if i.memory_index() != R::None {
                    let index = self.lane_register(i.memory_index());
                    let shift = i64::from(i.memory_index_scale().trailing_zeros());
                    let index = self.b.ins().ishl_imm(index, shift);
                    sum = self.b.ins().iadd(sum, index);
                }
                self.lane_set(i.op0_register(), sum);
            }
            M::Imul => {
                let (a, b) = if i.op_count() == 3 {
                    let a = self.lane_operand(i, 1, 4);
                    (a, self.lane_constant(i.immediate(2) as u32))
                } else {
                    let a = self.lane_register(i.op0_register());
                    (a, self.lane_operand(i, 1, 4))
                };
                let result = self.b.ins().imul(a, b);
                self.lanes().zero = None;
                self.lane_set(i.op0_register(), result);
            }
            _ => unreachable!("batch() admits {:?}", i.mnemonic()),
        }
    }
}

//! Shared SSA lowering for the two recovery routes.
use super::*;
use cranelift_codegen::ir::{
    self, AbiParam, InstBuilder, MemFlags, Value, condcodes::IntCC, types,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};

pub(super) struct Code {
    module: Option<JITModule>,
    pub entry: *const u8,
    pub bytes: usize,
    pub machine: Vec<u8>,
    pub blocks: Vec<u32>,
    pub sites: Vec<backend::Site>,
    pub recovery: u32,
    pub vcode: String,
    pub ir_count: usize,
    pub exits: usize,
    pub recovery_uses: usize,
    pub spill_stores: usize,
    pub compile_us: u128,
    recoverable: bool,
    pub graph: Graph,
    pub proven: BTreeSet<u32>,
}
impl Drop for Code {
    fn drop(&mut self) {
        unsafe { self.module.take().unwrap().free_memory() };
    }
}
impl Code {
    pub unsafe fn call(
        &self,
        frame: &mut Frame,
        data: *mut u8,
        perms: *const u8,
        stop: &std::sync::atomic::AtomicBool,
        budget: u32,
    ) -> u32 {
        assert!(self.recoverable, "counterfactual code must never execute");
        let function: unsafe extern "C" fn(
            *mut Frame,
            *mut u8,
            *const u8,
            *const std::sync::atomic::AtomicBool,
            u32,
            u32,
        ) -> u32 = unsafe { std::mem::transmute(self.entry) };
        unsafe { function(frame, data, perms, stop, budget, 0) }
    }
}

const RECOVERY_SOURCE: u32 = 0x7f00_0001;

pub(super) fn compile(
    graph: &Graph,
    route: Route,
    base: u32,
    len: usize,
    proven: &BTreeSet<u32>,
) -> Code {
    compile_inner(graph, route, base, len, proven, true)
}

pub(super) fn counterfactual(graph: &Graph, base: u32, len: usize, proven: &BTreeSet<u32>) -> Code {
    compile_inner(graph, Route::Proof, base, len, proven, false)
}

fn compile_inner(
    graph: &Graph,
    route: Route,
    base: u32,
    len: usize,
    proven: &BTreeSet<u32>,
    recoverable: bool,
) -> Code {
    let start = std::time::Instant::now();
    #[allow(unused_mut)]
    let mut builder = JITBuilder::with_flags(
        &[
            ("opt_level", "speed"),
            ("preserve_frame_pointers", "true"),
            ("machine_code_cfg_info", "true"),
        ],
        cranelift_module::default_libcall_names(),
    )
    .unwrap();
    let mut module = JITModule::new(builder);
    let mut context = module.make_context();
    let ptr = module.target_config().pointer_type();
    context
        .func
        .signature
        .params
        .extend([AbiParam::new(ptr); 4]);
    context
        .func
        .signature
        .params
        .extend([AbiParam::new(types::I32); 2]);
    context
        .func
        .signature
        .returns
        .push(AbiParam::new(types::I32));
    let id = module
        .declare_function("gir_region", Linkage::Local, &context.func.signature)
        .unwrap();
    let mut fc = FunctionBuilderContext::new();
    let (slot, exits, recovery_uses, spill_stores) = build(
        &mut context.func,
        &mut fc,
        graph,
        route,
        base,
        len,
        proven,
        recoverable,
    );
    let ir_count = context
        .func
        .layout
        .blocks()
        .map(|b| context.func.layout.block_insts(b).count())
        .sum();
    context.set_disasm(true);
    module.define_function(id, &mut context).unwrap();
    let machine = context.compiled_code().unwrap();
    let recovery = machine
        .buffer
        .get_srclocs_sorted()
        .iter()
        .filter(|r| r.loc.bits() == RECOVERY_SOURCE)
        .map(|r| r.start)
        .min()
        .unwrap();
    let mut sites = Vec::new();
    if route == Route::Backend {
        let offset = machine.sized_stackslot_offsets[slot];
        for trap in machine
            .buffer
            .traps()
            .iter()
            .filter(|t| t.code == ir::TrapCode::user(1).unwrap())
        {
            sites.push(backend::Site {
                offset: trap.offset,
                slots: std::array::from_fn(|n| offset + n as u32 * 4),
            });
        }
        // Unreachable entries and redundant loads may disappear. Register all
        // surviving machine traps, rather than equating an IR site count with
        // a machine instruction count after optimization.
    }
    let bytes = machine.code_buffer().len();
    let machine_bytes = machine.code_buffer().to_vec();
    let blocks = machine.bb_starts.clone();
    let vcode = machine.vcode.clone().unwrap();
    module.finalize_definitions().unwrap();
    let entry = module.get_finalized_function(id);
    let compile_us = start.elapsed().as_micros();
    Code {
        module: Some(module),
        entry,
        bytes,
        machine: machine_bytes,
        blocks,
        sites,
        recovery,
        vcode,
        ir_count,
        exits,
        recovery_uses,
        spill_stores,
        compile_us,
        recoverable,
        graph: graph.clone(),
        proven: proven.clone(),
    }
}

struct Emitter<'a> {
    b: FunctionBuilder<'a>,
    blocks: BTreeMap<u32, ir::Block>,
    returns: &'a BTreeSet<u32>,
    state: State<Value>,
    committed: Value,
    data: Value,
    perms: Value,
    stop: Value,
    budget: Value,
    exit: ir::Block,
    slot: ir::StackSlot,
    route: Route,
    base: u32,
    len: usize,
    pc: u32,
    proven: &'a BTreeSet<u32>,
    exits: usize,
    recovery_uses: usize,
    spill_stores: usize,
    recoverable: bool,
}

fn build<'a>(
    function: &'a mut ir::Function,
    context: &'a mut FunctionBuilderContext,
    graph: &'a Graph,
    route: Route,
    base: u32,
    len: usize,
    proven: &'a BTreeSet<u32>,
    recoverable: bool,
) -> (ir::StackSlot, usize, usize, usize) {
    let mut b = FunctionBuilder::new(function, context);
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    let blocks: BTreeMap<_, _> = graph
        .instructions
        .keys()
        .map(|pc| {
            let block = b.create_block();
            for _ in 0..10 {
                b.append_block_param(block, types::I32);
            }
            (*pc, block)
        })
        .collect();
    let exit = b.create_block();
    b.set_cold_block(exit);
    for _ in 0..13 {
        b.append_block_param(exit, types::I32);
    }
    let hardware = b.create_block();
    b.set_cold_block(hardware);
    b.switch_to_block(entry);
    let args = b.block_params(entry).to_vec();
    let [frame, data, perms, stop, budget, bailout]: [Value; 6] = args.try_into().unwrap();
    let state = State {
        gpr: std::array::from_fn(|n| {
            b.ins()
                .load(types::I32, MemFlags::trusted(), frame, n as i32 * 4)
        }),
        flags: b.ins().load(types::I32, MemFlags::trusted(), frame, 32),
    };
    let committed = b.ins().iconst(types::I32, 0);
    let slot = b.create_sized_stack_slot(ir::StackSlotData::new(
        ir::StackSlotKind::ExplicitSlot,
        48,
        3,
    ));
    let state_args: Vec<ir::BlockArg> = state
        .gpr
        .into_iter()
        .chain([state.flags, committed])
        .map(Into::into)
        .collect();
    b.ins()
        .brif(bailout, hardware, &[], blocks[&graph.start], &state_args);
    b.switch_to_block(hardware);
    b.set_srcloc(ir::SourceLoc::new(RECOVERY_SOURCE));
    let status = b.ins().iconst(types::I32, i64::from(HARDWARE));
    b.ins().return_(&[status]);
    let mut e = Emitter {
        b,
        blocks,
        returns: &graph.returns,
        state,
        committed,
        data,
        perms,
        stop,
        budget,
        exit,
        slot,
        route,
        base,
        len,
        pc: graph.start,
        proven,
        exits: 0,
        recovery_uses: 0,
        spill_stores: 0,
        recoverable,
    };
    for (&pc, i) in &graph.instructions {
        e.pc = pc;
        e.b.switch_to_block(e.blocks[&pc]);
        e.b.set_srcloc(ir::SourceLoc::new(16 + (pc - graph.start)));
        let args = e.b.block_params(e.blocks[&pc]);
        e.state = State {
            gpr: args[..8].try_into().unwrap(),
            flags: args[8],
        };
        e.committed = args[9];
        let stopped =
            e.b.ins()
                .atomic_load(types::I8, MemFlags::trusted(), e.stop);
        e.guard(stopped, INTERRUPT, None);
        let exhausted =
            e.b.ins()
                .icmp(IntCC::UnsignedGreaterThanOrEqual, e.committed, e.budget);
        e.guard(exhausted, BUDGET, None);
        e.instruction(i);
    }
    e.b.switch_to_block(exit);
    e.b.set_srcloc(ir::SourceLoc::new(8));
    let params = e.b.block_params(exit).to_vec();
    for (n, &value) in params[..12].iter().enumerate() {
        e.b.ins()
            .store(MemFlags::trusted(), value, frame, n as i32 * 4);
    }
    e.b.ins().return_(&[params[12]]);
    e.b.seal_all_blocks();
    let result = (slot, e.exits, e.recovery_uses, e.spill_stores);
    e.b.finalize();
    result
}

impl Emitter<'_> {
    fn constant(&mut self, x: u32) -> Value {
        self.b.ins().iconst(types::I32, i64::from(x))
    }
    fn recovery(&mut self, pc: u32, address: Option<Value>) -> Recovery<Value> {
        let state = if self.recoverable {
            self.state
        } else {
            let zero = self.constant(0);
            State {
                gpr: [zero; 8],
                flags: zero,
            }
        };
        Recovery {
            state,
            pc: self.constant(pc),
            committed: self.committed,
            address: address.unwrap_or_else(|| self.constant(0)),
        }
    }
    fn words(s: Recovery<Value>) -> [Value; 12] {
        let [a, b, c, d, e, f, g, h] = s.state.gpr;
        [
            a,
            b,
            c,
            d,
            e,
            f,
            g,
            h,
            s.state.flags,
            s.pc,
            s.committed,
            s.address,
        ]
    }
    fn exit_args(&mut self, pc: u32, reason: u32, address: Option<Value>) -> Vec<ir::BlockArg> {
        let mut recovery = self.recovery(pc, address);
        if reason == DONE {
            recovery.state = self.state;
        }
        let reason = self.constant(reason);
        self.recovery_uses += 9;
        Self::words(recovery)
            .into_iter()
            .chain([reason])
            .map(Into::into)
            .collect()
    }
    fn guard(&mut self, bad: Value, reason: u32, address: Option<Value>) {
        let args = self.exit_args(self.pc, reason, address);
        let ok = self.b.create_block();
        self.b.ins().brif(bad, self.exit, &args, ok, &[]);
        self.b.switch_to_block(ok);
        self.exits += 1;
    }
    fn transfer(&mut self, target: u32) {
        if let Some(&block) = self.blocks.get(&target) {
            let args: Vec<ir::BlockArg> = self
                .state
                .gpr
                .into_iter()
                .chain([self.state.flags, self.committed])
                .map(Into::into)
                .collect();
            self.b.ins().jump(block, &args);
        } else {
            let args = self.exit_args(target, DONE, None);
            self.b.ins().jump(self.exit, &args);
        }
    }
    fn address(&mut self, i: &Instruction) -> Value {
        let mut value = self.constant(i.memory_displacement32());
        if let Some(n) = reg(i.memory_base()) {
            value = self.b.ins().iadd(value, self.state.gpr[n]);
        }
        if let Some(n) = reg(i.memory_index()) {
            let part = self
                .b
                .ins()
                .imul_imm(self.state.gpr[n], i64::from(i.memory_index_scale()));
            value = self.b.ins().iadd(value, part);
        }
        value
    }
    fn pointer(&mut self, address: Value, write: bool) -> Value {
        let offset = self.b.ins().iadd_imm(address, -i64::from(self.base));
        if self.route == Route::Backend || !self.proven.contains(&self.pc) {
            let bad =
                self.b
                    .ins()
                    .icmp_imm(IntCC::UnsignedGreaterThan, offset, (self.len - 4) as i64);
            self.guard(bad, CHECK, Some(address));
            if self.route == Route::Proof {
                let end = self.b.ins().iadd_imm(offset, 3);
                for position in [offset, end] {
                    let page = self.b.ins().ushr_imm(position, 12);
                    let page = self.b.ins().uextend(types::I64, page);
                    let at = self.b.ins().iadd(self.perms, page);
                    let permission =
                        self.b
                            .ins()
                            .load(types::I8, MemFlags::trusted().with_readonly(), at, 0);
                    let required = if write {
                        crate::READ | crate::WRITE
                    } else {
                        crate::READ
                    };
                    let masked = self.b.ins().band_imm(
                        permission,
                        i64::from(required | !(crate::READ | crate::WRITE)),
                    );
                    let bad = self
                        .b
                        .ins()
                        .icmp_imm(IntCC::NotEqual, masked, i64::from(required));
                    self.guard(bad, CHECK, Some(address));
                }
            } else if write {
                // Aligned private dword stores cannot partially commit across
                // host pages. Unknown/unaligned stores leave before execution.
                let misaligned = self.b.ins().band_imm(address, 3);
                self.guard(misaligned, CHECK, Some(address));
            }
        }
        let offset = self.b.ins().uextend(types::I64, offset);
        self.b.ins().iadd(self.data, offset)
    }
    fn preserve(&mut self, address: Value) {
        if self.route != Route::Backend {
            return;
        }
        let snapshot = self.recovery(self.pc, Some(address));
        for (n, value) in Self::words(snapshot).into_iter().enumerate() {
            self.b.ins().stack_store(value, self.slot, n as i32 * 4);
        }
        self.spill_stores += 12;
        self.recovery_uses += 9;
    }
    fn load(&mut self, address: Value) -> Value {
        let pointer = self.pointer(address, false);
        self.preserve(address);
        let flags = if self.route == Route::Backend {
            MemFlags::new().with_trap_code(Some(ir::TrapCode::user(1).unwrap()))
        } else {
            MemFlags::trusted()
        };
        self.b.ins().load(types::I32, flags, pointer, 0)
    }
    fn store(&mut self, address: Value, value: Value) {
        let pointer = self.pointer(address, true);
        self.preserve(address);
        let flags = if self.route == Route::Backend {
            MemFlags::new().with_trap_code(Some(ir::TrapCode::user(1).unwrap()))
        } else {
            MemFlags::trusted()
        };
        self.b.ins().store(flags, value, pointer, 0);
    }
    fn read(&mut self, i: &Instruction, n: u32) -> Value {
        match i.op_kind(n) {
            OpKind::Register => self.state.gpr[reg(i.op_register(n)).unwrap()],
            OpKind::Memory => {
                let address = self.address(i);
                self.load(address)
            }
            OpKind::Immediate32 => self.constant(i.immediate32()),
            OpKind::Immediate8to32 => self.constant(i.immediate8to32() as u32),
            _ => panic!("unvalidated operand"),
        }
    }
    fn write(&mut self, i: &Instruction, value: Value) {
        if i.op0_kind() == OpKind::Memory {
            let at = self.address(i);
            self.store(at, value);
        } else {
            self.state.gpr[reg(i.op0_register()).unwrap()] = value;
        }
    }
    fn flags(
        &mut self,
        left: Value,
        right: Value,
        result: Value,
        subtract: bool,
        logical: bool,
        preserve_carry: bool,
    ) -> Value {
        let z = self.b.ins().icmp_imm(IntCC::Equal, result, 0);
        let z = self.b.ins().uextend(types::I32, z);
        let z = self.b.ins().ishl_imm(z, 6);
        let s = self.b.ins().ushr_imm(result, 24);
        let s = self.b.ins().band_imm(s, 0x80);
        let low = self.b.ins().band_imm(result, 255);
        let parity = self.b.ins().popcnt(low);
        let parity = self.b.ins().bxor_imm(parity, 1);
        let parity = self.b.ins().band_imm(parity, 1);
        let parity = self.b.ins().ishl_imm(parity, 2);
        let mut flags = self.b.ins().bor(z, s);
        flags = self.b.ins().bor(flags, parity);
        let mask = if logical {
            0x8c5
        } else if preserve_carry {
            0x8d4
        } else {
            0x8d5
        };
        let old = self
            .b
            .ins()
            .band_imm(self.state.flags, !mask as i64 & 0xffff_ffff);
        flags = self.b.ins().bor(flags, old);
        if !logical {
            let cross = self.b.ins().bxor(left, right);
            let aux = self.b.ins().bxor(cross, result);
            let aux = self.b.ins().band_imm(aux, 0x10);
            flags = self.b.ins().bor(flags, aux);
            let sign = self.b.ins().bxor(left, result);
            let cross = if subtract {
                cross
            } else {
                self.b.ins().bnot(cross)
            };
            let overflow = self.b.ins().band(cross, sign);
            let overflow = self.b.ins().ushr_imm(overflow, 20);
            let overflow = self.b.ins().band_imm(overflow, 0x800);
            flags = self.b.ins().bor(flags, overflow);
            if !preserve_carry {
                let carry = if subtract {
                    self.b.ins().icmp(IntCC::UnsignedLessThan, left, right)
                } else {
                    self.b.ins().icmp(IntCC::UnsignedLessThan, result, left)
                };
                let carry = self.b.ins().uextend(types::I32, carry);
                flags = self.b.ins().bor(flags, carry);
            }
        }
        flags
    }
    fn condition(&mut self, c: iced_x86::ConditionCode) -> Value {
        use iced_x86::ConditionCode as C;
        let f = self.state.flags;
        let bit = |b: &mut FunctionBuilder<'_>, shift| {
            let v = b.ins().ushr_imm(f, shift);
            b.ins().band_imm(v, 1)
        };
        let value = match c {
            C::o | C::no => bit(&mut self.b, 11),
            C::b | C::ae => bit(&mut self.b, 0),
            C::e | C::ne => bit(&mut self.b, 6),
            C::be | C::a => {
                let a = bit(&mut self.b, 0);
                let b = bit(&mut self.b, 6);
                self.b.ins().bor(a, b)
            }
            C::s | C::ns => bit(&mut self.b, 7),
            C::p | C::np => bit(&mut self.b, 2),
            C::l | C::ge | C::le | C::g => {
                let a = bit(&mut self.b, 7);
                let b = bit(&mut self.b, 11);
                let value = self.b.ins().bxor(a, b);
                if matches!(c, C::le | C::g) {
                    let z = bit(&mut self.b, 6);
                    self.b.ins().bor(value, z)
                } else {
                    value
                }
            }
            _ => unreachable!(),
        };
        if matches!(
            c,
            C::no | C::ae | C::ne | C::a | C::ns | C::np | C::ge | C::g
        ) {
            self.b.ins().bxor_imm(value, 1)
        } else {
            value
        }
    }
    fn instruction(&mut self, i: &Instruction) {
        match i.mnemonic() {
            M::Mov => {
                let value = self.read(i, 1);
                self.write(i, value);
            }
            M::Lea => {
                let value = self.address(i);
                self.write(i, value);
            }
            M::Not => {
                let value = self.read(i, 0);
                let value = self.b.ins().bnot(value);
                self.write(i, value);
            }
            M::Add | M::Sub | M::Cmp | M::Inc | M::Dec | M::And | M::Or | M::Xor | M::Test => {
                let left = self.read(i, 0);
                let unary = matches!(i.mnemonic(), M::Inc | M::Dec);
                let right = if unary {
                    self.constant(1)
                } else {
                    self.read(i, 1)
                };
                let subtract = matches!(i.mnemonic(), M::Sub | M::Cmp | M::Dec);
                let logical = matches!(i.mnemonic(), M::And | M::Or | M::Xor | M::Test);
                let result = match i.mnemonic() {
                    M::And | M::Test => self.b.ins().band(left, right),
                    M::Or => self.b.ins().bor(left, right),
                    M::Xor => self.b.ins().bxor(left, right),
                    _ if subtract => self.b.ins().isub(left, right),
                    _ => self.b.ins().iadd(left, right),
                };
                let flags = self.flags(left, right, result, subtract, logical, unary);
                if !matches!(i.mnemonic(), M::Cmp | M::Test) {
                    self.write(i, result);
                }
                self.state.flags = flags;
            }
            M::Push | M::Call => {
                let value = if i.mnemonic() == M::Call {
                    self.constant(i.next_ip32())
                } else {
                    self.read(i, 0)
                };
                let sp = self.b.ins().iadd_imm(self.state.gpr[4], -4);
                self.store(sp, value);
                self.state.gpr[4] = sp;
            }
            M::Pop => {
                let value = self.load(self.state.gpr[4]);
                self.state.gpr[reg(i.op0_register()).unwrap()] = value;
                self.state.gpr[4] = self.b.ins().iadd_imm(self.state.gpr[4], 4);
            }
            M::Ret => {
                let target = self.load(self.state.gpr[4]);
                self.state.gpr[4] = self.b.ins().iadd_imm(self.state.gpr[4], 4);
                self.committed = self.b.ins().iadd_imm(self.committed, 1);
                for &pc in self.returns {
                    let matches = self.b.ins().icmp_imm(IntCC::Equal, target, i64::from(pc));
                    let yes = self.b.create_block();
                    let no = self.b.create_block();
                    self.b.ins().brif(matches, yes, &[], no, &[]);
                    self.b.switch_to_block(yes);
                    self.transfer(pc);
                    self.b.switch_to_block(no);
                }
                let mut args = self.exit_args(self.pc, DONE, None);
                args[9] = target.into();
                self.b.ins().jump(self.exit, &args);
                return;
            }
            _ => {}
        }
        self.committed = self.b.ins().iadd_imm(self.committed, 1);
        match i.flow_control() {
            FlowControl::Call | FlowControl::UnconditionalBranch => {
                self.transfer(i.near_branch32())
            }
            FlowControl::ConditionalBranch => {
                let condition = self.condition(i.condition_code());
                let yes = self.b.create_block();
                let no = self.b.create_block();
                self.b.ins().brif(condition, yes, &[], no, &[]);
                self.b.switch_to_block(yes);
                self.transfer(i.near_branch32());
                self.b.switch_to_block(no);
                self.transfer(i.next_ip32());
            }
            _ => self.transfer(i.next_ip32()),
        }
    }
}

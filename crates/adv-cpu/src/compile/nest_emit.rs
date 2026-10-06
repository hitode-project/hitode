//! Region-side sampling and the C-to-Tail bridge for proven row kernels.
use super::*;

#[derive(Clone)]
pub(super) struct Hook {
    pub nest: nest::Nest,
    pub descriptor: usize,
    pub valid: Variable,
    pub previous: [Variable; 8],
}

pub(super) fn descriptor(
    nest: &nest::Nest,
    plan: &loops::Plan,
    memory_base: u32,
    memory_len: usize,
    thunk: usize,
    target: usize,
) -> crate::rows::Descriptor {
    use crate::rows::{Descriptor, RowGroup};
    debug_assert_eq!(nest.invariant & nest.inductions, 0);
    debug_assert_ne!(nest.invariant & (1 << plan.counter), 0);
    Descriptor {
        groups: plan
            .groups
            .iter()
            .map(|g| RowGroup {
                base: g.base,
                index: g.index,
                step: g.step,
                low: g.low,
                high: g.high,
                read: g.read,
                write: g.write,
            })
            .collect(),
        glue_reads: nest
            .glue_reads
            .iter()
            .map(|read| {
                let operand = &read.operand;
                RowGroup {
                    base: operand.base,
                    index: operand.index,
                    step: 0,
                    low: i64::from(operand.displacement),
                    high: i64::from(operand.displacement) + i64::from(operand.width),
                    read: true,
                    write: false,
                }
            })
            .collect(),
        memory_base,
        memory_len,
        outer_header: nest.header,
        outer_latch: nest.latch,
        inner_header: plan.header,
        inner_exit: nest.exit,
        inner_counter: plan.counter,
        counter_size: plan.counter_size,
        outer_counter: nest.counter,
        inductions: nest.inductions,
        invariant: nest.invariant,
        inner_steps: plan.steps,
        tracked: nest.tracked,
        inner_cost: nest.inner_cost,
        glue_cost: nest.glue_cost,
        thunk,
        target,
    }
}

pub(super) fn block_writes(
    instructions: &BTreeMap<u32, Instruction>,
    start: u32,
    starts: &BTreeSet<u32>,
) -> u32 {
    let mut mask = 0;
    for (&pc, instruction) in instructions.range(start..) {
        if pc != start && starts.contains(&pc) {
            break;
        }
        mask |= nest::written(instruction);
        if instruction.flow_control() != FlowControl::Next {
            break;
        }
    }
    mask
}

pub(super) fn thunk(
    module: &mut JITModule,
    fb: &mut FunctionBuilderContext,
    tail: &ir::Signature,
) -> Result<(cranelift_module::FuncId, cranelift_codegen::Context), String> {
    let ptr = module.target_config().pointer_type();
    let mut ctx = module.make_context();
    ctx.func.signature = module.make_signature();
    ctx.func.signature.params.extend([AbiParam::new(ptr); 3]);
    ctx.func.signature.returns.push(AbiParam::new(types::I32));
    let id = module
        .declare_function("guest_row_entry", Linkage::Local, &ctx.func.signature)
        .map_err(|e| e.to_string())?;
    let callee = ctx.func.import_signature(tail.clone());
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, fb);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let args = b.block_params(entry).to_vec();
        let call = b.ins().call_indirect(callee, args[2], &args[..2]);
        let result = b.inst_results(call)[0];
        b.ins().return_(&[result]);
        b.seal_all_blocks();
        b.finalize();
    }
    module
        .define_function(id, &mut ctx)
        .map_err(|e| format!("{e:?}"))?;
    Ok((id, ctx))
}

impl Emitter<'_> {
    pub(super) fn record_writes(&mut self, mask: u32) {
        if mask == 0 {
            return;
        }
        if let Some(written) = self.written {
            let current = self.b.use_var(written);
            let value = self.b.ins().bor_imm(current, i64::from(mask));
            self.b.def_var(written, value);
        }
    }

    pub(super) fn reset_rows(&mut self) {
        if self.row_hooks.is_empty() {
            return;
        }
        let zero = self.constant(0);
        for hook in &self.row_hooks {
            self.b.def_var(hook.valid, zero);
        }
    }

    /// Sampling belongs to the edge after the prologue, before the inner
    /// header charges its ordinary block. Private kernels charge that block.
    pub(super) fn row_edge(&mut self, target: u32) -> bool {
        if self.kernel.is_some() || self.row_hooks.is_empty() {
            return false;
        }
        for hook in self.row_hooks.clone() {
            let inside = |pc| (hook.nest.header..=hook.nest.latch).contains(&pc);
            if inside(target) && !inside(self.pc) {
                let zero = self.constant(0);
                self.b.def_var(hook.valid, zero);
            }
            let inner = &self.plans[hook.nest.inner];
            if target == inner.header && self.pc != inner.latch {
                self.row_call(hook, target);
                return true;
            }
        }
        false
    }

    fn row_call(&mut self, hook: Hook, header: u32) {
        // Both the declined and successful paths publish this MMX prefix.
        self.flush_mmx_effects();
        let effects = self.mmx_effects.take();
        let current: [Value; 8] = std::array::from_fn(|r| self.b.use_var(self.regs[r]));
        let valid = self.b.use_var(hook.valid);
        let ready = self.b.ins().icmp_imm(IntCC::Equal, valid, 1);
        let pool = self.b.ins().load(
            self.ptr_ty,
            MemFlags::trusted(),
            self.context,
            offset_of!(NativeContext, rows) as i32,
        );
        let enabled = self.b.ins().icmp_imm(IntCC::NotEqual, pool, 0);
        let ready = self.b.ins().band(ready, enabled);
        let validate = self.b.create_block();
        let attempt = self.b.create_block();
        let sample = self.b.create_block();
        self.b.ins().brif(ready, validate, &[], sample, &[]);
        self.b.switch_to_block(validate);
        // A first serial row can mutate a glue parameter before moving away
        // from it. Future rectangles alone cannot validate that old sample.
        let mut stable = self.b.ins().iconst(types::I8, 1);
        for r in 0..8 {
            if hook.nest.invariant & (1 << r) != 0 {
                let previous = self.b.use_var(hook.previous[r]);
                let same = self.b.ins().icmp(IntCC::Equal, current[r], previous);
                stable = self.b.ins().band(stable, same);
            }
        }
        let changed = self.b.create_block();
        self.b.ins().brif(stable, attempt, &[], changed, &[]);
        self.b.switch_to_block(changed);
        let rejected = self.constant(2);
        self.b.def_var(hook.valid, rejected);
        self.b.ins().jump(self.blocks[&header], &[]);
        self.b.switch_to_block(attempt);
        let slot = self.b.create_sized_stack_slot(ir::StackSlotData::new(
            ir::StackSlotKind::ExplicitSlot,
            32,
            2,
        ));
        let deltas: [Value; 8] = std::array::from_fn(|r| {
            if hook.nest.inductions & (1 << r) == 0 {
                return self.constant(0);
            }
            let previous = self.b.use_var(hook.previous[r]);
            self.b.ins().isub(current[r], previous)
        });
        for (r, &delta) in deltas.iter().enumerate() {
            self.b.ins().stack_store(delta, slot, (r * 4) as i32);
        }
        let pc = self.constant(header);
        self.save_state(pc);
        let descriptor = self.b.ins().iconst(self.ptr_ty, hook.descriptor as i64);
        let deltas_ptr = self.b.ins().stack_addr(self.ptr_ty, slot, 0);
        let call = self.b.ins().call(
            self.nest_ref,
            &[self.context, self.mem, descriptor, deltas_ptr],
        );
        let rows = self.b.inst_results(call)[0];
        self.reload_state();
        // reload_state's recipe describes only this branch. Other branches
        // enter the canonical guest block with its own MMX recipe.
        self.mmx_effects = None;
        let done = self.b.create_block();
        let declined = self.b.create_block();
        self.b.ins().brif(rows, done, &[], declined, &[]);
        self.b.switch_to_block(done);
        let previous_rows = self.b.ins().iadd_imm(rows, -1);
        for r in 0..8 {
            let distance = self.b.ins().imul(previous_rows, deltas[r]);
            let previous = self.b.ins().iadd(current[r], distance);
            self.b.def_var(hook.previous[r], previous);
        }
        let one = self.constant(1);
        self.b.def_var(hook.valid, one);
        self.b.ins().jump(self.blocks[&hook.nest.exit], &[]);
        self.b.switch_to_block(declined);
        let rejected = self.constant(2);
        self.b.def_var(hook.valid, rejected);
        for (r, value) in current.iter().enumerate() {
            self.b.def_var(hook.previous[r], *value);
        }
        self.b.ins().jump(self.blocks[&header], &[]);
        self.b.switch_to_block(sample);
        for (r, value) in current.iter().enumerate() {
            self.b.def_var(hook.previous[r], *value);
        }
        // A rejected proof waits until an outside edge or dispatch re-enters.
        let rejected = self.b.ins().icmp_imm(IntCC::Equal, valid, 2);
        let one = self.constant(1);
        let next = self.b.ins().select(rejected, valid, one);
        self.b.def_var(hook.valid, next);
        self.b.ins().jump(self.blocks[&header], &[]);
        self.mmx_effects = effects;
    }
}

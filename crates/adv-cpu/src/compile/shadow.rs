//! Inline prediction metadata. All guest stack accesses have already completed
//! through the checked lane before these infallible, owner-private operations.
use super::*;
use crate::linking::shadow::{CAPACITY, Hint, Stack};

impl Emitter<'_> {
    fn shadow_stack(&mut self) -> Value {
        self.b.ins().load(
            self.ptr_ty,
            MemFlags::trusted(),
            self.context,
            offset_of!(NativeContext, shadow) as i32,
        )
    }

    fn shadow_increment(&mut self, stack: Value, offset: usize) {
        let old = self
            .b
            .ins()
            .load(types::I64, MemFlags::trusted(), stack, offset as i32);
        let next = self.b.ins().iadd_imm(old, 1);
        self.b
            .ins()
            .store(MemFlags::trusted(), next, stack, offset as i32);
    }

    fn shadow_address(&mut self, stack: Value, index: Value) -> Value {
        let offset = self.b.ins().uextend(self.ptr_ty, index);
        let offset = self
            .b
            .ins()
            .imul_imm(offset, std::mem::size_of::<Hint>() as i64);
        let address = self.b.ins().iadd(stack, offset);
        self.b
            .ins()
            .iadd_imm(address, offset_of!(Stack, entries) as i64)
    }

    pub(super) fn shadow_clear(&mut self) {
        if self.shadow_ref.is_none() {
            return;
        }
        let stack = self.shadow_stack();
        let zero = self.constant(0);
        self.b.ins().store(
            MemFlags::trusted(),
            zero,
            stack,
            offset_of!(Stack, top) as i32,
        );
    }

    pub(super) fn shadow_push(&mut self, sp: Value, pc: u32) {
        let Some(caller) = self.shadow_ref else {
            return;
        };
        // Pure local single-continuation calls retain the original return_hint
        // fast path. They neither add nor consume the enclosing cross-call hint.
        if self.local_return_hint == Some(pc) {
            return;
        }
        let stack = self.shadow_stack();
        let top = self.b.ins().load(
            types::I32,
            MemFlags::trusted(),
            stack,
            offset_of!(Stack, top) as i32,
        );
        let full =
            self.b
                .ins()
                .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, top, i64::from(CAPACITY));
        let overflow = self.b.create_block();
        self.b.set_cold_block(overflow);
        let push = self.b.create_block();
        self.b.append_block_param(push, types::I32);
        self.b.ins().brif(full, overflow, &[], push, &[top.into()]);
        self.b.switch_to_block(overflow);
        self.shadow_increment(stack, offset_of!(Stack, overflows));
        let zero = self.constant(0);
        self.b.ins().jump(push, &[zero.into()]);
        self.b.switch_to_block(push);
        let top = self.b.block_params(push)[0];
        let address = self.shadow_address(stack, top);
        let target = self.constant(pc);
        let entry = if self.shadow_entries.contains(&pc) {
            self.b.ins().func_addr(self.ptr_ty, caller)
        } else {
            // A sliced CALL's continuation may belong to a different body.
            // Keep the frame balanced but let RET use the current slot table.
            self.b.ins().iconst(self.ptr_ty, 0)
        };
        for (value, offset) in [
            (sp, offset_of!(Hint, sp)),
            (target, offset_of!(Hint, pc)),
            (entry, offset_of!(Hint, entry)),
        ] {
            self.b
                .ins()
                .store(MemFlags::trusted(), value, address, offset as i32);
        }
        let top = self.b.ins().iadd_imm(top, 1);
        self.b.ins().store(
            MemFlags::trusted(),
            top,
            stack,
            offset_of!(Stack, top) as i32,
        );
    }

    pub(super) fn shadow_prediction(&mut self, sp: Value, pc: Value) -> Option<Value> {
        self.shadow_ref?;
        let stack = self.shadow_stack();
        let top = self.b.ins().load(
            types::I32,
            MemFlags::trusted(),
            stack,
            offset_of!(Stack, top) as i32,
        );
        let empty = self.b.ins().icmp_imm(IntCC::Equal, top, 0);
        let miss = self.b.create_block();
        self.b.set_cold_block(miss);
        let inspect = self.b.create_block();
        let matched = self.b.create_block();
        let merge = self.b.create_block();
        self.b.append_block_param(merge, self.ptr_ty);
        self.b.ins().brif(empty, miss, &[], inspect, &[]);
        self.b.switch_to_block(inspect);
        let index = self.b.ins().iadd_imm(top, -1);
        let address = self.shadow_address(stack, index);
        let saved_sp = self.b.ins().load(
            types::I32,
            MemFlags::trusted(),
            address,
            offset_of!(Hint, sp) as i32,
        );
        let saved_pc = self.b.ins().load(
            types::I32,
            MemFlags::trusted(),
            address,
            offset_of!(Hint, pc) as i32,
        );
        let sp_ok = self.b.ins().icmp(IntCC::Equal, saved_sp, sp);
        let pc_ok = self.b.ins().icmp(IntCC::Equal, saved_pc, pc);
        let valid = self.b.ins().band(sp_ok, pc_ok);
        self.b.ins().brif(valid, matched, &[], miss, &[]);
        self.b.switch_to_block(matched);
        self.b.ins().store(
            MemFlags::trusted(),
            index,
            stack,
            offset_of!(Stack, top) as i32,
        );
        self.shadow_increment(stack, offset_of!(Stack, matches));
        // Do not even load a native address until both architectural checks
        // succeeded. Its owner is immutably borrowed for this entire chain.
        let entry = self.b.ins().load(
            self.ptr_ty,
            MemFlags::trusted(),
            address,
            offset_of!(Hint, entry) as i32,
        );
        self.b.ins().jump(merge, &[entry.into()]);
        self.b.switch_to_block(miss);
        let zero = self.constant(0);
        self.b.ins().store(
            MemFlags::trusted(),
            zero,
            stack,
            offset_of!(Stack, top) as i32,
        );
        self.shadow_increment(stack, offset_of!(Stack, misses));
        let absent = self.b.ins().iconst(self.ptr_ty, 0);
        self.b.ins().jump(merge, &[absent.into()]);
        self.b.switch_to_block(merge);
        Some(self.b.block_params(merge)[0])
    }

    pub(super) fn shadow_hit(&mut self) {
        let stack = self.shadow_stack();
        self.shadow_increment(stack, offset_of!(Stack, hits));
    }
}

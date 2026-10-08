//! One state and control contract for native regions and loop kernels.
//!
//! Two pointer Tail arguments carry the context and guest memory window.
//! Guest state is canonical at region boundaries; hot exits publish only
//! their write footprint. Region bodies and loop kernels share this ABI.
use super::*;

pub(super) fn signature(module: &JITModule) -> ir::Signature {
    let ptr = module.target_config().pointer_type();
    let mut signature = module.make_signature();
    signature.call_conv = cranelift_codegen::isa::CallConv::Tail;
    signature.params.extend([AbiParam::new(ptr); 2]);
    signature.returns.push(AbiParam::new(types::I32));
    signature
}

fn requested(b: &mut FunctionBuilder<'_>, interrupt: Value) -> Value {
    let pending = b
        .ins()
        .atomic_load(types::I8, MemFlags::trusted(), interrupt);
    b.ins().icmp_imm(IntCC::NotEqual, pending, 0)
}

/// Rust entries start from canonical state. Linked entries have already polled
/// their outgoing edge, so repeating this check in every Tail body is wasteful.
pub(super) fn guard_entry(b: &mut FunctionBuilder<'_>, ptr: ir::Type, context: Value) {
    let state = b.ins().load(
        ptr,
        MemFlags::trusted(),
        context,
        offset_of!(NativeContext, state) as i32,
    );
    let pc = b.ins().load(
        types::I32,
        MemFlags::trusted(),
        state,
        offset_of!(CpuState, eip) as i32,
    );
    let fuel = b.ins().load(
        types::I64,
        MemFlags::trusted(),
        state,
        offset_of!(CpuState, fuel) as i32,
    );
    let interrupt = b.ins().load(
        ptr,
        MemFlags::trusted(),
        context,
        offset_of!(NativeContext, interrupt) as i32,
    );
    let pending = requested(b, interrupt);
    let stop = b.ins().load(
        types::I32,
        MemFlags::trusted(),
        context,
        offset_of!(NativeContext, stop) as i32,
    );
    let work = b.ins().icmp_imm(IntCC::NotEqual, fuel, 0);
    let ordinary = b.ins().icmp(IntCC::NotEqual, pc, stop);
    let pending = b.ins().band(pending, work);
    let pending = b.ins().band(pending, ordinary);
    let interrupted = b.create_block();
    b.set_cold_block(interrupted);
    let continued = b.create_block();
    b.ins().brif(pending, interrupted, &[], continued, &[]);
    b.switch_to_block(interrupted);
    let reason = b.ins().iconst(types::I32, Exit::Interrupted as i64);
    b.ins().return_(&[reason]);
    b.switch_to_block(continued);
}

impl Emitter<'_> {
    pub(super) fn tail_to(&mut self, address: Value) {
        self.b
            .ins()
            .return_call_indirect(self.tail_ref, address, &[self.context, self.mem]);
    }
}

/// A cold return entered both by a checked guard and by a registered fault.
/// Its only input is an explicit stack slot, never a live guest or host GPR.
#[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
#[derive(Clone, Copy)]
pub(super) struct RecoveryExit {
    block: ir::Block,
    result: ir::StackSlot,
}

#[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
impl RecoveryExit {
    const MARKER: u32 = 2;

    pub fn new(b: &mut FunctionBuilder<'_>) -> Self {
        let block = b.create_block();
        b.set_cold_block(block);
        let result = b.create_sized_stack_slot(ir::StackSlotData::new(
            ir::StackSlotKind::ExplicitSlot,
            4,
            2,
        ));
        Self { block, result }
    }

    pub fn jump(self, b: &mut FunctionBuilder<'_>) {
        let reason = b.ins().iconst(types::I32, Exit::ReadFault as i64);
        b.ins().stack_store(reason, self.result, 0);
        b.ins().jump(self.block, &[]);
    }

    pub fn publish(self, b: &mut FunctionBuilder<'_>, ptr: ir::Type, context: Value) {
        let address = b.ins().stack_addr(ptr, self.result, 0);
        let cell = b.ins().load(
            ptr,
            MemFlags::trusted(),
            context,
            offset_of!(NativeContext, recovery_slot) as i32,
        );
        // Only a registered hardware-read session reaches this publication.
        // The backend owns the stack address, including outgoing-call space.
        b.ins().store(MemFlags::trusted(), address, cell, 0);
    }

    pub fn emit(self, b: &mut FunctionBuilder<'_>, ptr: ir::Type) {
        b.switch_to_block(self.block);
        b.set_srcloc(ir::SourceLoc::default());
        let address = b.ins().stack_addr(ptr, self.result, 0);
        // Address expressions can be hoisted or shared with the hot path.
        // Mark only the non-hoistable memory effect, never its operands.
        b.set_srcloc(ir::SourceLoc::new(Self::MARKER));
        // Atomic loads cannot be forwarded from a normal predecessor's store.
        // The signal path writes this private slot before resuming this block.
        let reason = b
            .ins()
            .atomic_load(types::I32, MemFlags::trusted(), address);
        b.set_srcloc(ir::SourceLoc::default());
        b.ins().return_(&[reason]);
    }

    pub fn location(self, code: &cranelift_codegen::CompiledCode) -> Option<usize> {
        let marker = code
            .buffer
            .get_srclocs_sorted()
            .iter()
            .filter(|range| range.loc.bits() == Self::MARKER)
            .map(|range| range.start)
            .min()?;
        // The marker is the sole memory effect in an otherwise empty return
        // block. Enter the backend's block start, including address formation
        // and register-allocation edits, instead of inspecting epilogue bytes.
        let start = code
            .bb_starts
            .iter()
            .copied()
            .filter(|offset| *offset <= marker)
            .max()?;
        Some(start as usize)
    }
}

/// ABI used only by generated code. The C entry trampoline passes this context
/// and the guest memory base to a Tail-convention region. No Rust stack grows
/// when regions call one another. The table and code allocations outlive a chain.
#[repr(C)]
pub(crate) struct NativeContext {
    pub state: *mut CpuState,
    /// Borrowed row executor; null in guest-parallel and child row contexts.
    pub rows: *const crate::rows::RowPool,
    /// Actual architectural register writes of a dedicated row kernel.
    pub written: u32,
    /// Pinned native page metadata: exact permissions plus two-page grants.
    pub perms: *const u8,
    pub bridge: *mut std::ffi::c_void,
    /// Pinned AtomicBool, read by generated byte-width atomic loads only.
    pub interrupt: *const std::sync::atomic::AtomicBool,
    pub slots: *const Slot,
    pub mask: u32,
    pub remaining: u32,
    pub stop: u32,
    /// Only the initial borrowed owner has a hardware-fault session. Tail
    /// transfers clear this flag before entering another checked region.
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    pub fault_reads: u8,
    /// Private session cell receiving the IR-owned recovery stack address.
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    pub recovery_slot: *const std::cell::Cell<usize>,
    pub linked_entries: u64,
    /// Exclusive metadata borrowed for this chain. Cleared before/after native
    /// execution, so it cannot retain a target across publication or retirement.
    pub shadow: *mut crate::linking::shadow::Stack,
}

impl Emitter<'_> {
    pub(super) fn chain_or_return(
        &mut self,
        pc: Value,
        target_hash: Option<Value>,
        reason: Option<Value>,
        prediction: Option<Value>,
    ) {
        let returned = self.b.create_block();
        if let Some(reason) = reason {
            let candidate = self.b.create_block();
            let dispatch = self
                .b
                .ins()
                .icmp_imm(IntCC::Equal, reason, Exit::Dispatch as i64);
            self.b.ins().brif(dispatch, candidate, &[], returned, &[]);
            self.b.switch_to_block(candidate);
        }
        let slots = self.b.ins().load(
            self.ptr_ty,
            MemFlags::trusted(),
            self.context,
            offset_of!(NativeContext, slots) as i32,
        );
        let remaining = self.b.ins().load(
            types::I32,
            MemFlags::trusted(),
            self.context,
            offset_of!(NativeContext, remaining) as i32,
        );
        let stop = self.b.ins().load(
            types::I32,
            MemFlags::trusted(),
            self.context,
            offset_of!(NativeContext, stop) as i32,
        );
        let fuel = self.b.use_var(self.fuel);
        let none = self.b.ins().icmp_imm(IntCC::Equal, slots, 0);
        let limit = self.b.ins().icmp_imm(IntCC::Equal, remaining, 0);
        let is_stop = self.b.ins().icmp(IntCC::Equal, pc, stop);
        let empty = self.b.ins().icmp_imm(IntCC::Equal, fuel, 0);
        let bad = self.b.ins().bor(none, limit);
        let bad = self.b.ins().bor(bad, is_stop);
        let bad = self.b.ins().bor(bad, empty);
        let prepare = self.b.create_block();
        self.b.ins().brif(bad, returned, &[], prepare, &[]);
        self.b.switch_to_block(prepare);
        // Both hot footers and cold exits have published canonical state.
        let pending = requested(&mut self.b, self.interrupt);
        let interrupted = self.b.create_block();
        self.b.set_cold_block(interrupted);
        let search_ready = self.b.create_block();
        self.b
            .ins()
            .brif(pending, interrupted, &[], search_ready, &[]);
        self.b.switch_to_block(interrupted);
        let interrupted_reason = self.constant(Exit::Interrupted as u32);
        self.b.ins().return_(&[interrupted_reason]);
        self.b.switch_to_block(search_ready);
        let hit = self.b.create_block();
        self.b.append_block_param(hit, self.ptr_ty);
        if let Some(prediction) = prediction {
            let valid = self.b.ins().icmp_imm(IntCC::NotEqual, prediction, 0);
            let predicted = self.b.create_block();
            let search_only = self.b.create_block();
            self.b.ins().brif(valid, predicted, &[], search_only, &[]);
            self.b.switch_to_block(predicted);
            self.shadow_hit();
            self.b.ins().jump(hit, &[prediction.into()]);
            self.b.switch_to_block(search_only);
        }
        let mask = self.b.ins().load(
            types::I32,
            MemFlags::trusted(),
            self.context,
            offset_of!(NativeContext, mask) as i32,
        );
        let hashed = if let Some(hash) = target_hash {
            hash
        } else {
            let high = self.b.ins().ushr_imm(pc, 16);
            let mixed = self.b.ins().bxor(pc, high);
            let mixed = self.b.ins().imul_imm(mixed, 0x9e37_79b1_u32 as i64);
            let high = self.b.ins().ushr_imm(mixed, 16);
            self.b.ins().bxor(mixed, high)
        };
        let index = self.b.ins().band(hashed, mask);
        let search = self.b.create_block();
        self.b.append_block_param(search, types::I32);
        self.b.append_block_param(search, types::I32);
        let zero = self.constant(0);
        self.b.ins().jump(search, &[index.into(), zero.into()]);
        self.b.switch_to_block(search);
        let index = self.b.block_params(search)[0];
        let probes = self.b.block_params(search)[1];
        let offset = self.b.ins().uextend(self.ptr_ty, index);
        let offset = self
            .b
            .ins()
            .imul_imm(offset, std::mem::size_of::<Slot>() as i64);
        let address = self.b.ins().iadd(slots, offset);
        let entry_address = self
            .b
            .ins()
            .iadd_imm(address, offset_of!(Slot, entry) as i64);
        // The whole table is immutable for this borrowed chain. Publication
        // and retirement happen between calls, not concurrently with this load.
        let entry = self.b.ins().load(
            self.ptr_ty,
            MemFlags::trusted().with_readonly(),
            entry_address,
            0,
        );
        let absent = self.b.ins().icmp_imm(IntCC::Equal, entry, 0);
        let occupied = self.b.create_block();
        self.b.ins().brif(absent, returned, &[], occupied, &[]);
        self.b.switch_to_block(occupied);
        let key = self.b.ins().load(
            types::I32,
            MemFlags::trusted(),
            address,
            offset_of!(Slot, pc) as i32,
        );
        let found = self.b.ins().icmp(IntCC::Equal, pc, key);
        let advance = self.b.create_block();
        self.b.ins().brif(found, hit, &[entry.into()], advance, &[]);
        self.b.switch_to_block(advance);
        let next = self.b.ins().iadd_imm(index, 1);
        let next = self.b.ins().band(next, mask);
        let probes = self.b.ins().iadd_imm(probes, 1);
        let exhausted = self.b.ins().icmp_imm(
            IntCC::UnsignedGreaterThanOrEqual,
            probes,
            crate::linking::PROBES as i64,
        );
        self.b.ins().brif(
            exhausted,
            returned,
            &[],
            search,
            &[next.into(), probes.into()],
        );
        self.b.switch_to_block(hit);
        let entry = self.b.block_params(hit)[0];
        let remaining = self.b.ins().iadd_imm(remaining, -1);
        self.b.ins().store(
            MemFlags::trusted(),
            remaining,
            self.context,
            offset_of!(NativeContext, remaining) as i32,
        );
        let linked = self.b.ins().load(
            types::I64,
            MemFlags::trusted(),
            self.context,
            offset_of!(NativeContext, linked_entries) as i32,
        );
        let linked = self.b.ins().iadd_imm(linked, 1);
        self.b.ins().store(
            MemFlags::trusted(),
            linked,
            self.context,
            offset_of!(NativeContext, linked_entries) as i32,
        );
        #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
        if self.fault_mode.is_some() {
            let disabled = self.b.ins().iconst(types::I8, 0);
            self.b.ins().store(
                MemFlags::trusted(),
                disabled,
                self.context,
                offset_of!(NativeContext, fault_reads) as i32,
            );
        }
        self.tail_to(entry);
        self.b.switch_to_block(returned);
        let reason = reason.unwrap_or_else(|| self.constant(Exit::Dispatch as u32));
        self.b.ins().return_(&[reason]);
    }
    pub(super) fn interrupt_at(&mut self, pc: Value, respect_stops: bool) {
        let mut pending = requested(&mut self.b, self.interrupt);
        // A clone checks interrupts after funding its next iteration.
        if self.fast.is_none() {
            let fuel = self.b.use_var(self.fuel);
            let work = self.b.ins().icmp_imm(IntCC::NotEqual, fuel, 0);
            pending = self.b.ins().band(pending, work);
        }
        if respect_stops {
            let stop = self.b.ins().load(
                types::I32,
                MemFlags::trusted(),
                self.context,
                offset_of!(NativeContext, stop) as i32,
            );
            let ordinary = self.b.ins().icmp(IntCC::NotEqual, pc, stop);
            pending = self.b.ins().band(pending, ordinary);
        }
        let interrupted = self.b.create_block();
        self.b.set_cold_block(interrupted);
        let continued = self.b.create_block();
        self.b.ins().brif(pending, interrupted, &[], continued, &[]);
        self.b.switch_to_block(interrupted);
        self.exit(Exit::Interrupted, pc);
        self.b.switch_to_block(continued);
    }
}

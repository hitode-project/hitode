//! One register-passed state contract for native regions and loop kernels.
//!
//! Cranelift Tail has six integer argument registers on both supported hosts.
//! Two vector arguments carry all guest GPRs; EFLAGS occupies the fifth integer
//! register. CpuState is canonical only at a Rust/helper boundary.
use super::*;

pub(super) const PC: usize = 2;
pub(super) const FUEL: usize = 3;
pub(super) const GPRS: usize = 4;
pub(super) const FLAGS: usize = 6;

pub(super) fn signature(module: &JITModule) -> ir::Signature {
    let ptr = module.target_config().pointer_type();
    let mut signature = module.make_signature();
    signature.call_conv = cranelift_codegen::isa::CallConv::Tail;
    signature.params.extend([AbiParam::new(ptr); 2]);
    signature.params.push(AbiParam::new(types::I32));
    signature.params.push(AbiParam::new(types::I64));
    signature.params.extend([AbiParam::new(types::I32X4); 2]);
    signature.params.push(AbiParam::new(types::I32));
    signature.returns.push(AbiParam::new(types::I32));
    signature
}

/// The C entry and row thunk are the only callers starting from canonical state.
pub(super) fn load(
    b: &mut FunctionBuilder<'_>,
    ptr: ir::Type,
    context: Value,
    memory: Value,
) -> Vec<Value> {
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
    let low = b.ins().load(types::I32X4, MemFlags::trusted(), state, 0);
    let high = b.ins().load(types::I32X4, MemFlags::trusted(), state, 16);
    let flags = b.ins().load(
        types::I32,
        MemFlags::trusted(),
        state,
        offset_of!(CpuState, eflags) as i32,
    );
    vec![context, memory, pc, fuel, low, high, flags]
}

impl Emitter<'_> {
    pub(super) fn gpr_arguments(&mut self) -> [Value; 2] {
        let mut vectors = self.abi_state;
        let written = self.write_summary.map_or(0xff, |summary| summary.gpr);
        for index in 0..8 {
            if written & (1 << index) != 0 {
                let value = self.b.use_var(self.regs[index]);
                vectors[index / 4] =
                    self.b
                        .ins()
                        .insertlane(vectors[index / 4], value, (index % 4) as u8);
            }
        }
        vectors
    }

    pub(super) fn tail_arguments(&mut self, pc: Value) -> Vec<Value> {
        let fuel = self.b.use_var(self.fuel);
        let vectors = self.gpr_arguments();
        let flags = self.materialize_flags();
        vec![
            self.context,
            self.mem,
            pc,
            fuel,
            vectors[0],
            vectors[1],
            flags,
        ]
    }

    pub(super) fn tail_to(&mut self, address: Value, pc: Value) {
        let args = self.tail_arguments(pc);
        self.b
            .ins()
            .return_call_indirect(self.tail_ref, address, &args);
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
        b.set_srcloc(ir::SourceLoc::new(Self::MARKER));
        let address = b.ins().stack_addr(ptr, self.result, 0);
        // Atomic loads cannot be forwarded from a normal predecessor's store.
        // The signal path writes this private slot before resuming this block.
        let reason = b
            .ins()
            .atomic_load(types::I32, MemFlags::trusted(), address);
        b.ins().return_(&[reason]);
        b.set_srcloc(ir::SourceLoc::default());
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

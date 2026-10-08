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

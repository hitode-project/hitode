//! Explicit checkpointed scalar lane; every other instruction stays checked.
use super::*;

pub(super) fn candidate(i: &Instruction) -> bool {
    if i.has_lock_prefix() || !matches!(i.memory_size().size(), 1 | 2 | 4) {
        return false;
    }
    let load = matches!(i.mnemonic(), M::Mov | M::Movzx)
        && i.op0_kind() == OpKind::Register
        && reg_index(i.op0_register()).is_some()
        && i.op0_register().size() == 4
        && i.op1_kind() == OpKind::Memory;
    let store = i.mnemonic() == M::Mov
        && i.op0_kind() == OpKind::Memory
        && matches!(
            i.op1_kind(),
            OpKind::Register | OpKind::Immediate8 | OpKind::Immediate16 | OpKind::Immediate32
        )
        && (i.op1_kind() != OpKind::Register
            || reg_index(i.op1_register().full_register32()).is_some());
    load || store
}

pub(super) fn declare(
    module: &mut JITModule,
) -> Result<(cranelift_module::FuncId, cranelift_module::FuncId), String> {
    let ptr = module.target_config().pointer_type();
    let mut signature = module.make_signature();
    signature.params.extend([AbiParam::new(ptr); 2]);
    signature.params.extend([AbiParam::new(types::I32); 4]);
    signature.returns.push(AbiParam::new(types::I64));
    let access = module
        .declare_function("adv_direct_scalar", Linkage::Import, &signature)
        .map_err(|e| e.to_string())?;
    let mut signature = module.make_signature();
    signature.params.push(AbiParam::new(ptr));
    let checked = module
        .declare_function("adv_direct_checked", Linkage::Import, &signature)
        .map_err(|e| e.to_string())?;
    Ok((access, checked))
}

impl Emitter<'_> {
    pub(super) fn direct_mov(&mut self, i: &Instruction) {
        let write = i.op0_kind() == OpKind::Memory;
        let width = i.memory_size().size();
        let destination = if write {
            0
        } else {
            reg_index(i.op0_register()).unwrap() as u32
        };
        let operand = self.constant(width as u32 | if write { 0x100 } else { destination << 16 });
        let address = self.address(i);
        let value = if write {
            self.read(i, 1)
        } else {
            self.constant(0)
        };
        let pc = self.constant(i.ip32());
        // Calls, SIMD, lazy flags and budget are all canonical at a recovery
        // point. Resident x87 lanes never enter here; their cold path does.
        self.save_state(pc);
        let next_pc = self.constant(i.next_ip32());
        let call = self.b.ins().call(
            self.direct_refs.unwrap().0,
            &[self.bridge, self.state, address, value, next_pc, operand],
        );
        let result = self.b.inst_results(call)[0];
        let status = self.b.ins().ushr_imm(result, 32);
        let done = self.b.create_block();
        if !write {
            self.b.append_block_param(done, types::I32);
        }
        let native = self.b.create_block();
        let checked = self.b.create_block();
        let failed = self.b.create_block();
        let mut switch = Switch::new();
        switch.set_entry(0, native);
        switch.set_entry(1, checked);
        switch.emit(&mut self.b, status, failed);
        self.b.switch_to_block(failed);
        // B already restored the precise checkpoint. Saving old SSA state here
        // would overwrite the recovered fault address and committed prefix.
        let read = self.b.ins().icmp_imm(IntCC::Equal, status, 2);
        let write_fault = self.b.ins().icmp_imm(IntCC::Equal, status, 3);
        let fallback = self.constant(Exit::Fallback as u32);
        let read_reason = self.constant(Exit::ReadFault as u32);
        let write_reason = self.constant(Exit::WriteFault as u32);
        let reason = self.b.ins().select(write_fault, write_reason, fallback);
        let reason = self.b.ins().select(read, read_reason, reason);
        self.b.ins().return_(&[reason]);
        self.b.switch_to_block(native);
        if write {
            self.b.ins().jump(done, &[]);
        } else {
            let value = self.b.ins().ireduce(types::I32, result);
            self.b.ins().jump(done, &[value.into()]);
        }
        self.b.switch_to_block(checked);
        if write {
            let value = if width == 4 {
                value
            } else {
                self.b.ins().ireduce(integer_type(width), value)
            };
            self.direct_store(address, value);
            self.b.ins().jump(done, &[]);
        } else {
            let value = self.read(i, 1);
            self.b.ins().jump(done, &[value.into()]);
        }
        self.b.switch_to_block(done);
        self.b.seal_block(done);
        if !write {
            let value = self.b.block_params(done)[0];
            self.write(i, None, value);
        }
    }
}

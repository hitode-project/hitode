use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::mem::offset_of;

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{self, AbiParam, InstBuilder, MemFlags, Value, types};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Switch, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module, default_libcall_names};
use iced_x86::{
    Decoder, DecoderError, DecoderOptions, FlowControl, Instruction, Mnemonic as M, OpKind,
    Register as R,
};

use crate::{CompileStrategy, CpuState, EXEC, Exit, Memory, READ, Region, WRITE};
mod mmx;
mod scalar;
mod simd;

type Entry = unsafe extern "C" fn(*mut CpuState, *mut u8, *const u8, *mut std::ffi::c_void) -> u32;

pub(crate) struct Compiled {
    // Keeps executable pages alive until the entry pointer can no longer be called.
    module: Option<JITModule>,
    entry: Entry,
    pub pages: Vec<(usize, u64)>,
    pub entries: BTreeSet<u32>,
    pub code_size: usize,
    #[cfg(all(test, target_arch = "x86_64"))]
    disasm: String,
}

impl Drop for Compiled {
    fn drop(&mut self) {
        // SAFETY: the entry pointer is private, run borrows self, and no generated
        // code can still be executing while this exclusive drop runs.
        unsafe { self.module.take().unwrap().free_memory() };
    }
}

impl Compiled {
    pub fn valid(&self, memory: &Memory) -> bool {
        self.pages
            .iter()
            .all(|&(page, version)| memory.versions[page] == version)
    }
    pub fn run(
        &self,
        state: &mut CpuState,
        memory: &mut Memory,
        bridge: &mut crate::host::Bridge<'_>,
    ) -> u32 {
        bridge.memory = memory;
        // SAFETY: generated function has this ABI; exclusive borrows keep its pointers
        // stable. Every guest access is range/permission checked by generated code.
        unsafe {
            (self.entry)(
                state,
                memory.ptr.as_ptr(),
                memory.perms.as_ptr(),
                (bridge as *mut crate::host::Bridge<'_>).cast(),
            )
        }
    }
}

fn record_tail_dependencies(
    region: &Region,
    memory: &Memory,
    bytes: &[u8],
    offset: usize,
    decoder: &Decoder<'_>,
    pages: &mut BTreeSet<usize>,
) {
    if !region.complete_tail {
        return;
    }
    let start = (region.start - memory.base) as usize;
    let read_end = start + offset + decoder.position();
    pages.extend(start / 4096..=(read_end - 1) / 4096);
    if decoder.last_error() == DecoderError::NoMoreBytes {
        // The next page's mapping/EXEC permission can make a previously
        // truncated instruction compilable, even if none of its bytes
        // could be read. Keep that negative dependency in the cache.
        if let Some(address) = u64::from(region.start)
            .checked_add(bytes.len() as u64)
            .and_then(|address| u32::try_from(address).ok())
            && let Ok(range) = memory.range(address, 1)
        {
            pages.insert(range.start / 4096);
        }
    }
}

pub(crate) fn compile(region: &Region, memory: &Memory) -> Result<Compiled, String> {
    if region.len == 0 || region.len > 16 * 1024 || !memory.executable(region.start, region.len) {
        return Err("region must contain 1..16384 executable bytes".into());
    }
    let mut bytes = Cow::Borrowed(memory.read(region.start, region.len)?);
    if region.complete_tail {
        // Only instruction starts belong to the nominal region. A final x86
        // instruction can consume at most 14 more executable bytes beyond it.
        for offset in region.len..region.len + 14 {
            let Some(address) = u64::from(region.start)
                .checked_add(offset as u64)
                .and_then(|address| u32::try_from(address).ok())
            else {
                break;
            };
            if !memory.executable(address, 1) {
                break;
            }
            bytes.to_mut().push(memory.read(address, 1)?[0]);
        }
    }
    let first_page = (region.start - memory.base) as usize / 4096;
    let mut pages: BTreeSet<usize> =
        (first_page..=((region.start - memory.base) as usize + region.len - 1) / 4096).collect();
    let mut instructions = BTreeMap::new();
    if !region.reachable {
        let mut decoder =
            Decoder::with_ip(32, &bytes, u64::from(region.start), DecoderOptions::NONE);
        while decoder.can_decode() && decoder.ip() < u64::from(region.start) + region.len as u64 {
            let instruction = decoder.decode();
            record_tail_dependencies(region, memory, &bytes, 0, &decoder, &mut pages);
            instructions.insert(instruction.ip32(), instruction);
        }
    }
    // Entry points can follow padding/data or overlap an earlier linear decode.
    // Decode direct targets independently, as the CPU would at the actual target PC.
    let mut pending: Vec<u32> = region.entries.clone();
    if region.reachable && pending.is_empty() {
        pending.push(region.start);
    }
    if !region.reachable {
        pending.extend(
            instructions
                .values()
                .filter(|i| i.op0_kind() == OpKind::NearBranch32)
                .map(Instruction::near_branch32),
        );
    }
    while let Some(target) = pending.pop() {
        if instructions.contains_key(&target) {
            continue;
        }
        let Some(offset) = target
            .checked_sub(region.start)
            .map(|n| n as usize)
            .filter(|&n| n < region.len)
        else {
            continue;
        };
        let mut decoder = Decoder::with_ip(
            32,
            &bytes[offset..],
            u64::from(target),
            DecoderOptions::NONE,
        );
        while decoder.can_decode()
            && decoder.ip() < u64::from(region.start) + region.len as u64
            && !instructions.contains_key(&(decoder.ip() as u32))
        {
            let i = decoder.decode();
            record_tail_dependencies(region, memory, &bytes, offset, &decoder, &mut pages);
            if i.op0_kind() == OpKind::NearBranch32 {
                pending.push(i.near_branch32());
            }
            instructions.insert(i.ip32(), i);
            if region.reachable
                && (i.is_invalid()
                    || matches!(
                        i.flow_control(),
                        FlowControl::UnconditionalBranch
                            | FlowControl::IndirectBranch
                            | FlowControl::Return
                            | FlowControl::Exception
                    ))
            {
                break;
            }
        }
    }
    let mut entries = BTreeSet::from([region.start]);
    for instruction in instructions.values() {
        if crate::strings::operation(instruction).is_some() {
            entries.insert(instruction.ip32());
            entries.insert(instruction.next_ip32());
        }
        if instruction.flow_control() != FlowControl::Next {
            entries.insert(instruction.next_ip32());
        }
        if matches!(instruction.op0_kind(), OpKind::NearBranch32) {
            entries.insert(instruction.near_branch32());
        }
    }
    for &entry in &region.entries {
        if !instructions.contains_key(&entry) {
            return Err(format!("entry {entry:#x} is not an instruction boundary"));
        }
        entries.insert(entry);
    }
    entries.retain(|pc| instructions.contains_key(pc));
    let returns: BTreeSet<_> = instructions
        .values()
        .filter(|i| {
            matches!(
                i.flow_control(),
                FlowControl::Call | FlowControl::IndirectCall
            )
        })
        .map(Instruction::next_ip32)
        .filter(|pc| entries.contains(pc))
        .collect();
    let return_hint = if returns.len() == 1 {
        returns.first().copied()
    } else {
        None
    };

    let opt_level = match region.compile_strategy {
        CompileStrategy::Speed => "speed",
        CompileStrategy::Fast => "none",
    };
    let mut builder = JITBuilder::with_flags(&[("opt_level", opt_level)], default_libcall_names())
        .map_err(|e| e.to_string())?;
    builder.symbol("adv_host_call", crate::host::host_call as *const u8);
    builder.symbol("adv_repeat", crate::strings::repeat as *const u8);
    let mut module = JITModule::new(builder);
    let ptr_ty = module.target_config().pointer_type();
    let mut ctx = module.make_context();
    #[cfg(all(test, target_arch = "x86_64"))]
    ctx.set_disasm(true);
    ctx.func.signature = module.make_signature();
    ctx.func.signature.params.extend([AbiParam::new(ptr_ty); 4]);
    ctx.func.signature.returns.push(AbiParam::new(types::I32));
    let id = module
        .declare_function("guest_region", Linkage::Local, &ctx.func.signature)
        .map_err(|e| e.to_string())?;
    let mut fb_ctx = FunctionBuilderContext::new();
    let mut host_signature = module.make_signature();
    host_signature.params.extend([AbiParam::new(ptr_ty); 2]);
    host_signature.returns.push(AbiParam::new(types::I32));
    let host_id = module
        .declare_function("adv_host_call", Linkage::Import, &host_signature)
        .map_err(|e| e.to_string())?;
    let host_ref = module.declare_func_in_func(host_id, &mut ctx.func);
    let mut repeat_signature = host_signature.clone();
    repeat_signature.params.push(AbiParam::new(types::I32));
    let repeat_id = module
        .declare_function("adv_repeat", Linkage::Import, &repeat_signature)
        .map_err(|e| e.to_string())?;
    let repeat_ref = module.declare_func_in_func(repeat_id, &mut ctx.func);
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
        let prologue = b.create_block();
        b.append_block_params_for_function_params(prologue);
        b.switch_to_block(prologue);
        b.seal_block(prologue);
        let params = b.block_params(prologue).to_vec();
        let regs: [Variable; 8] = std::array::from_fn(|_| b.declare_var(types::I32));
        let mut xmm = [None; 8];
        for instruction in instructions.values().filter(|i| supported(i)) {
            for operand in 0..instruction.op_count() {
                if let Some(index) = simd::register(instruction.op_register(operand))
                    && xmm[index].is_none()
                {
                    let var = b.declare_var(types::I8X16);
                    let value = b.ins().load(
                        types::I8X16,
                        MemFlags::new(),
                        params[0],
                        (offset_of!(CpuState, xmm) + index * 16) as i32,
                    );
                    b.def_var(var, value);
                    xmm[index] = Some(var);
                }
            }
        }
        let mut mmx = [None; 8];
        let has_mmx = instructions
            .values()
            .any(|i| supported(i) && mmx::supported(i));
        for instruction in instructions
            .values()
            .filter(|i| supported(i) && mmx::supported(i))
        {
            for operand in 0..instruction.op_count() {
                if let Some(index) = mmx::register(instruction.op_register(operand))
                    && mmx[index].is_none()
                {
                    let var = b.declare_var(types::I8X16);
                    let bits = b.ins().load(
                        types::I64,
                        MemFlags::new(),
                        params[0],
                        (offset_of!(CpuState, mmx) + index * 8) as i32,
                    );
                    let vector = b.ins().scalar_to_vector(types::I64X2, bits);
                    let value = b.ins().bitcast(
                        types::I8X16,
                        MemFlags::new().with_endianness(ir::Endianness::Little),
                        vector,
                    );
                    b.def_var(var, value);
                    mmx[index] = Some(var);
                }
            }
        }
        let mmx_mode = has_mmx.then(|| {
            let var = b.declare_var(types::I32);
            let value = b.ins().load(
                types::I32,
                MemFlags::new(),
                params[0],
                offset_of!(CpuState, mmx_mode) as i32,
            );
            b.def_var(var, value);
            var
        });
        let flags = b.declare_var(types::I32);
        let lazy: [Variable; 7] = std::array::from_fn(|_| b.declare_var(types::I32));
        let zero = b.ins().iconst(types::I32, 0);
        for var in lazy {
            b.def_var(var, zero);
        }
        let fuel = b.declare_var(types::I64);
        for (i, reg) in regs.iter().enumerate() {
            let v = b
                .ins()
                .load(types::I32, MemFlags::trusted(), params[0], (i * 4) as i32);
            b.def_var(*reg, v);
        }
        let f = b.ins().load(
            types::I32,
            MemFlags::trusted(),
            params[0],
            offset_of!(CpuState, eflags) as i32,
        );
        b.def_var(flags, f);
        let carry = b.ins().band_imm(f, 1);
        b.def_var(lazy[4], carry);
        let f = b.ins().load(
            types::I64,
            MemFlags::trusted(),
            params[0],
            offset_of!(CpuState, fuel) as i32,
        );
        b.def_var(fuel, f);
        let pc = b.ins().load(
            types::I32,
            MemFlags::trusted(),
            params[0],
            offset_of!(CpuState, eip) as i32,
        );
        let blocks: BTreeMap<_, _> = entries.iter().map(|&pc| (pc, b.create_block())).collect();
        let dispatch = b.create_block();
        b.append_block_param(dispatch, types::I32);
        let shared_exit = b.create_block();
        b.set_cold_block(shared_exit);
        b.append_block_param(shared_exit, types::I32); // Exit reason.
        b.append_block_param(shared_exit, types::I32); // Exact guest PC.
        b.ins().jump(dispatch, &[pc.into()]);
        let mut e = Emitter {
            b,
            regs,
            xmm,
            mmx,
            mmx_mode,
            flags,
            lazy,
            fuel,
            state: params[0],
            mem: params[1],
            perms: params[2],
            ptr_ty,
            base: memory.base,
            len: memory.len,
            blocks,
            dispatch,
            shared_exit,
            pc: region.start,
            return_hint,
            bridge: params[3],
            host_ref,
            repeat_ref,
        };
        for (&start, &block) in &e.blocks.clone() {
            e.b.switch_to_block(block);
            e.pc = start;
            let budget = e.b.use_var(e.fuel);
            let zero = e.b.ins().icmp_imm(IntCC::Equal, budget, 0);
            e.guard(zero, Exit::Budget, None);
            let remaining = e.b.ins().iadd_imm(budget, -1);
            e.b.def_var(e.fuel, remaining);
            let mut pc = start;
            loop {
                if pc != start && e.blocks.contains_key(&pc) {
                    e.direct(pc);
                    break;
                }
                let Some(instruction) = instructions.get(&pc) else {
                    e.exit_const(Exit::Dispatch, pc);
                    break;
                };
                e.pc = pc;
                if !supported(instruction) {
                    e.exit_const(Exit::Fallback, pc);
                    break;
                }
                if e.instruction(instruction) {
                    break;
                }
                pc = instruction.next_ip32();
            }
        }
        e.b.switch_to_block(dispatch);
        let target = e.b.block_params(dispatch)[0];
        let outside = e.b.create_block();
        e.b.set_cold_block(outside);
        let mut switch = Switch::new();
        for (&pc, &block) in &e.blocks {
            switch.set_entry(u128::from(pc), block);
        }
        switch.emit(&mut e.b, target, outside);
        e.b.switch_to_block(outside);
        e.exit(Exit::Dispatch, target);
        // SSA variables merge each predecessor's state here. Keep one writeback
        // and one lazy-flags materialization instead of duplicating them at every
        // memory/budget guard. Fault addresses are still stored at the fault site.
        e.b.switch_to_block(shared_exit);
        let reason = e.b.block_params(shared_exit)[0];
        let pc = e.b.block_params(shared_exit)[1];
        e.save_state(pc);
        e.b.ins().return_(&[reason]);
        e.b.seal_all_blocks();
        e.b.finalize();
    }
    module
        .define_function(id, &mut ctx)
        .map_err(|e| format!("{e:?}"))?;
    let code_size = ctx
        .compiled_code()
        .map_or(0, |code| code.code_buffer().len());
    module.finalize_definitions().map_err(|e| e.to_string())?;
    // SAFETY: exact signature declared above, finalized memory is owned by module.
    let entry =
        unsafe { std::mem::transmute::<*const u8, Entry>(module.get_finalized_function(id)) };
    // Unsupported entry instructions belong to the cold executor. This also prevents
    // a one-unit scheduler slice from repeatedly entering native code without progress.
    entries.retain(|pc| supported(&instructions[pc]));
    Ok(Compiled {
        module: Some(module),
        entry,
        pages: pages
            .into_iter()
            .map(|page| (page, memory.versions[page]))
            .collect(),
        entries,
        code_size,
        #[cfg(all(test, target_arch = "x86_64"))]
        disasm: ctx
            .compiled_code()
            .and_then(|code| code.vcode.clone())
            .unwrap_or_default(),
    })
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;
    use iced_x86::code_asm::*;

    #[test]
    fn divide_uses_distinct_host_widths_without_a_second_remainder_division() {
        for clear_high in [true, false] {
            let mut a = CodeAssembler::new(32).unwrap();
            if clear_high {
                a.xor(edx, edx).unwrap();
            }
            a.div(ecx).unwrap();
            a.ret().unwrap();
            let bytes = a.assemble(0x10000).unwrap();
            let mut machine = crate::Machine::new(0x10000, 4096).unwrap();
            machine.write(0x10000, &bytes).unwrap();
            machine.protect(0x10000, 4096, READ | EXEC).unwrap();
            let compiled = compile(&Region::new(0x10000, bytes.len()), machine.memory()).unwrap();
            let divides: Vec<_> = compiled
                .disasm
                .lines()
                .filter(|line| line.contains("div"))
                .collect();
            println!("EDX cleared {clear_high}: {divides:?}");
            assert!(compiled.disasm.contains("divl"), "{}", compiled.disasm);
            assert_eq!(
                divides.iter().filter(|line| line.contains("divl")).count(),
                1
            );
            assert!(divides.iter().filter(|line| line.contains("divq")).count() <= 1);
            if !clear_high {
                assert!(compiled.disasm.contains("divq"));
            }
        }
    }
}

fn reg_index(reg: R) -> Option<usize> {
    Some(match reg {
        R::EAX => 0,
        R::ECX => 1,
        R::EDX => 2,
        R::EBX => 3,
        R::ESP => 4,
        R::EBP => 5,
        R::ESI => 6,
        R::EDI => 7,
        _ => return None,
    })
}

fn supported(i: &Instruction) -> bool {
    if crate::strings::operation(i).is_some() {
        return true;
    }
    if i.is_invalid()
        || i.has_lock_prefix()
        || i.has_rep_prefix()
        || i.has_repne_prefix()
        || !matches!(i.segment_prefix(), R::None | R::DS | R::SS | R::ES | R::FS)
    {
        return false;
    }
    if i.mnemonic() == M::Int && i.immediate8() == 0x2e {
        return true;
    }
    if !matches!(
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
            | M::Push
            | M::Pop
            | M::Nop
            | M::Jmp
            | M::Call
            | M::Ret
            | M::Je
            | M::Jne
            | M::Ja
            | M::Jae
            | M::Jb
            | M::Jbe
            | M::Jg
            | M::Jge
            | M::Jl
            | M::Jle
            | M::Jo
            | M::Jno
            | M::Js
            | M::Jns
            | M::Jp
            | M::Jnp
    ) {
        return simd::supported(i) || scalar::supported(i) || mmx::supported(i);
    }
    for n in 0..i.op_count() {
        match i.op_kind(n) {
            OpKind::Register
                if reg_index(i.op_register(n)).is_some()
                    || (matches!(
                        i.mnemonic(),
                        M::Mov | M::Movzx | M::Movsx | M::Cmp | M::Test
                    ) && (i.op_register(n).is_gpr8() || i.op_register(n).is_gpr16())) => {}
            OpKind::Memory => {
                if i.memory_size().size() != 4
                    && i.mnemonic() != M::Lea
                    && !(matches!(
                        i.mnemonic(),
                        M::Mov | M::Movzx | M::Movsx | M::Cmp | M::Test
                    ) && matches!(i.memory_size().size(), 1 | 2))
                {
                    return false;
                }
                if [i.memory_base(), i.memory_index()]
                    .iter()
                    .any(|&r| r != R::None && reg_index(r).is_none())
                {
                    return false;
                }
            }
            OpKind::Immediate32 | OpKind::Immediate8to32 | OpKind::NearBranch32 => {}
            OpKind::Immediate16 if matches!(i.mnemonic(), M::Ret | M::Mov | M::Cmp | M::Test) => {}
            OpKind::Immediate8 if matches!(i.mnemonic(), M::Mov | M::Cmp | M::Test) => {}
            OpKind::Immediate8to16 if matches!(i.mnemonic(), M::Cmp | M::Test) => {}
            _ => return false,
        }
    }
    if matches!(i.mnemonic(), M::Movzx | M::Movsx) && reg_index(i.op0_register()).is_none() {
        return false;
    }
    // POP r/m has special ESP-relative addressing; leave that form to the fallback.
    if i.mnemonic() == M::Pop && i.op0_kind() != OpKind::Register {
        return false;
    }
    // Far and 16-bit CALL/RET/PUSH/POP must not inherit our 32-bit stack semantics.
    match i.mnemonic() {
        M::Ret => matches!(
            i.code(),
            iced_x86::Code::Retnd | iced_x86::Code::Retnd_imm16
        ),
        M::Nop => i.op_count() == 0,
        _ => true,
    }
}

struct Emitter<'a> {
    b: FunctionBuilder<'a>,
    regs: [Variable; 8],
    xmm: [Option<Variable>; 8],
    mmx: [Option<Variable>; 8],
    mmx_mode: Option<Variable>,
    flags: Variable,
    fuel: Variable,
    // Last flag-producing instruction: lhs, rhs, result, kind (0=raw,1=add,2=sub,3=logic).
    // Carry has its own operands/kind because INC/DEC preserve it.
    lazy: [Variable; 7],
    state: Value,
    mem: Value,
    perms: Value,
    ptr_ty: ir::Type,
    base: u32,
    len: usize,
    blocks: BTreeMap<u32, ir::Block>,
    dispatch: ir::Block,
    shared_exit: ir::Block,
    pc: u32,
    return_hint: Option<u32>,
    bridge: Value,
    host_ref: ir::FuncRef,
    repeat_ref: ir::FuncRef,
}

impl Emitter<'_> {
    fn constant(&mut self, value: u32) -> Value {
        self.b.ins().iconst(types::I32, i64::from(value))
    }
    fn exit_const(&mut self, reason: Exit, pc: u32) {
        let pc = self.constant(pc);
        self.exit(reason, pc);
    }
    fn exit(&mut self, reason: Exit, pc: Value) {
        let reason = self.constant(reason as u32);
        self.b
            .ins()
            .jump(self.shared_exit, &[reason.into(), pc.into()]);
    }
    fn save_state(&mut self, pc: Value) {
        for (index, var) in self.mmx.iter().enumerate() {
            if let Some(var) = var {
                let value = self.b.use_var(*var);
                let value = self.b.ins().bitcast(
                    types::I64X2,
                    MemFlags::new().with_endianness(ir::Endianness::Little),
                    value,
                );
                let bits = self.b.ins().extractlane(value, 0);
                self.b.ins().store(
                    MemFlags::new(),
                    bits,
                    self.state,
                    (offset_of!(CpuState, mmx) + index * 8) as i32,
                );
            }
        }
        if let Some(var) = self.mmx_mode {
            let value = self.b.use_var(var);
            self.b.ins().store(
                MemFlags::new(),
                value,
                self.state,
                offset_of!(CpuState, mmx_mode) as i32,
            );
        }
        for (index, var) in self.xmm.iter().enumerate() {
            if let Some(var) = var {
                let value = self.b.use_var(*var);
                self.b.ins().store(
                    MemFlags::new(),
                    value,
                    self.state,
                    (offset_of!(CpuState, xmm) + index * 16) as i32,
                );
            }
        }
        for (i, reg) in self.regs.iter().enumerate() {
            let v = self.b.use_var(*reg);
            self.b
                .ins()
                .store(MemFlags::trusted(), v, self.state, (i * 4) as i32);
        }
        let flags = self.materialize_flags();
        self.b.ins().store(
            MemFlags::trusted(),
            flags,
            self.state,
            offset_of!(CpuState, eflags) as i32,
        );
        let fuel = self.b.use_var(self.fuel);
        self.b.ins().store(
            MemFlags::trusted(),
            fuel,
            self.state,
            offset_of!(CpuState, fuel) as i32,
        );
        self.b.ins().store(
            MemFlags::trusted(),
            pc,
            self.state,
            offset_of!(CpuState, eip) as i32,
        );
    }
    fn reload_state(&mut self) {
        for (index, var) in self.mmx.iter().enumerate() {
            if let Some(var) = var {
                let bits = self.b.ins().load(
                    types::I64,
                    MemFlags::new(),
                    self.state,
                    (offset_of!(CpuState, mmx) + index * 8) as i32,
                );
                let vector = self.b.ins().scalar_to_vector(types::I64X2, bits);
                let value = self.b.ins().bitcast(
                    types::I8X16,
                    MemFlags::new().with_endianness(ir::Endianness::Little),
                    vector,
                );
                self.b.def_var(*var, value);
            }
        }
        if let Some(var) = self.mmx_mode {
            let value = self.b.ins().load(
                types::I32,
                MemFlags::new(),
                self.state,
                offset_of!(CpuState, mmx_mode) as i32,
            );
            self.b.def_var(var, value);
        }
        for (i, var) in self.regs.iter().enumerate() {
            let v = self
                .b
                .ins()
                .load(types::I32, MemFlags::trusted(), self.state, (i * 4) as i32);
            self.b.def_var(*var, v);
        }
        for (i, var) in self.xmm.iter().enumerate() {
            if let Some(var) = var {
                let v = self.b.ins().load(
                    types::I8X16,
                    MemFlags::new(),
                    self.state,
                    (offset_of!(CpuState, xmm) + i * 16) as i32,
                );
                self.b.def_var(*var, v);
            }
        }
        let flags = self.b.ins().load(
            types::I32,
            MemFlags::trusted(),
            self.state,
            offset_of!(CpuState, eflags) as i32,
        );
        self.b.def_var(self.flags, flags);
        let zero = self.constant(0);
        for var in self.lazy {
            self.b.def_var(var, zero);
        }
        let carry = self.b.ins().band_imm(flags, 1);
        self.b.def_var(self.lazy[4], carry);
        let fuel = self.b.ins().load(
            types::I64,
            MemFlags::trusted(),
            self.state,
            offset_of!(CpuState, fuel) as i32,
        );
        self.b.def_var(self.fuel, fuel);
    }
    fn host_call(&mut self, next: u32) {
        let pc = self.constant(next);
        self.save_state(pc);
        let call = self.b.ins().call(self.host_ref, &[self.bridge, self.state]);
        let result = self.b.inst_results(call)[0];
        self.reload_state();
        let done = self.b.ins().icmp_imm(IntCC::Equal, result, 0);
        let resume = self.b.create_block();
        let park = self.b.create_block();
        self.b.ins().brif(done, resume, &[], park, &[]);
        self.b.switch_to_block(park);
        let pc = self.b.ins().load(
            types::I32,
            MemFlags::trusted(),
            self.state,
            offset_of!(CpuState, eip) as i32,
        );
        self.exit(Exit::Host, pc);
        self.b.switch_to_block(resume);
        self.direct(next);
    }
    fn guard(&mut self, bad: Value, reason: Exit, addr: Option<Value>) {
        let fail = self.b.create_block();
        self.b.set_cold_block(fail);
        let ok = self.b.create_block();
        self.b.ins().brif(bad, fail, &[], ok, &[]);
        self.b.switch_to_block(fail);
        if let Some(addr) = addr {
            self.b.ins().store(
                MemFlags::trusted(),
                addr,
                self.state,
                offset_of!(CpuState, fault_addr) as i32,
            );
        }
        self.exit_const(reason, self.pc);
        self.b.switch_to_block(ok);
    }
    fn direct(&mut self, pc: u32) {
        if let Some(&block) = self.blocks.get(&pc) {
            self.b.ins().jump(block, &[]);
        } else {
            self.exit_const(Exit::Dispatch, pc);
        }
    }
    fn indirect(&mut self, target: Value) {
        self.b.ins().jump(self.dispatch, &[target.into()]);
    }
    fn address(&mut self, i: &Instruction) -> Value {
        let mut addr = self.constant(i.memory_displacement32());
        if let Some(reg) = reg_index(i.memory_base()) {
            let value = self.b.use_var(self.regs[reg]);
            addr = self.b.ins().iadd(addr, value);
        }
        if let Some(reg) = reg_index(i.memory_index()) {
            let value = self.b.use_var(self.regs[reg]);
            let scaled = self
                .b
                .ins()
                .imul_imm(value, i64::from(i.memory_index_scale()));
            addr = self.b.ins().iadd(addr, scaled);
        }
        if i.segment_prefix() == R::FS && i.mnemonic() != M::Lea {
            let fs = self.b.ins().load(
                types::I32,
                MemFlags::trusted(),
                self.state,
                offset_of!(CpuState, fs_base) as i32,
            );
            addr = self.b.ins().iadd(addr, fs);
        }
        addr
    }
    fn pointer(&mut self, addr: Value, write: bool, size: usize) -> Value {
        let offset = self.b.ins().iadd_imm(addr, -i64::from(self.base));
        let bad =
            self.b
                .ins()
                .icmp_imm(IntCC::UnsignedGreaterThan, offset, (self.len - size) as i64);
        let reason = if write {
            Exit::WriteFault
        } else {
            Exit::ReadFault
        };
        self.guard(bad, reason, Some(addr));
        let end = self.b.ins().iadd_imm(offset, size as i64 - 1);
        let mut combined = None;
        for position in [offset, end] {
            let page = self.b.ins().ushr_imm(position, 12);
            let page = self.b.ins().uextend(self.ptr_ty, page);
            let at = self.b.ins().iadd(self.perms, page);
            let p = self.b.ins().load(types::I8, MemFlags::trusted(), at, 0);
            let mask = self
                .b
                .ins()
                .band_imm(p, i64::from(if write { WRITE } else { READ }));
            let bad = self.b.ins().icmp_imm(IntCC::Equal, mask, 0);
            self.guard(bad, reason, Some(addr));
            combined = Some(if let Some(old) = combined {
                self.b.ins().bor(old, p)
            } else {
                p
            });
        }
        if write {
            let code = self.b.ins().band_imm(combined.unwrap(), i64::from(EXEC));
            let code = self.b.ins().icmp_imm(IntCC::NotEqual, code, 0);
            // Stop before the write. Unicorn executes it and the runtime invalidates
            // native regions before they can be entered again.
            self.guard(code, Exit::Fallback, None);
        }
        let offset = self.b.ins().uextend(self.ptr_ty, offset);
        self.b.ins().iadd(self.mem, offset)
    }
    fn read(&mut self, i: &Instruction, n: u32) -> Value {
        match i.op_kind(n) {
            OpKind::Register => {
                let reg = i.op_register(n);
                let full = self
                    .b
                    .use_var(self.regs[reg_index(reg.full_register32()).unwrap()]);
                if reg.size() == 4 {
                    full
                } else {
                    let value = if matches!(reg, R::AH | R::CH | R::DH | R::BH) {
                        self.b.ins().ushr_imm(full, 8)
                    } else {
                        full
                    };
                    self.b
                        .ins()
                        .band_imm(value, if reg.size() == 1 { 255 } else { 65535 })
                }
            }
            OpKind::Memory => {
                let addr = self.address(i);
                let size = i.memory_size().size();
                let ptr = self.pointer(addr, false, size);
                let value = self
                    .b
                    .ins()
                    .load(integer_type(size), MemFlags::new(), ptr, 0);
                if size == 4 {
                    value
                } else {
                    self.b.ins().uextend(types::I32, value)
                }
            }
            OpKind::Immediate32 => self.constant(i.immediate32()),
            OpKind::Immediate16 => self.constant(u32::from(i.immediate16())),
            OpKind::Immediate8 => self.constant(u32::from(i.immediate8())),
            OpKind::Immediate8to32 => self.constant(i.immediate8to32() as u32),
            OpKind::Immediate8to16 => self.constant(u32::from(i.immediate8to16() as u16)),
            OpKind::NearBranch32 => self.constant(i.near_branch32()),
            _ => unreachable!(),
        }
    }
    fn destination(&mut self, i: &Instruction) -> Option<Value> {
        if i.op0_kind() == OpKind::Memory {
            let addr = self.address(i);
            Some(self.pointer(addr, true, i.memory_size().size()))
        } else {
            None
        }
    }
    fn write(&mut self, i: &Instruction, ptr: Option<Value>, value: Value) {
        if let Some(ptr) = ptr {
            let size = i.memory_size().size();
            let value = if size == 4 {
                value
            } else {
                self.b.ins().ireduce(integer_type(size), value)
            };
            self.b.ins().store(MemFlags::new(), value, ptr, 0);
        } else {
            let reg = i.op0_register();
            let var = self.regs[reg_index(reg.full_register32()).unwrap()];
            let value = if reg.size() == 4 {
                value
            } else {
                let high = matches!(reg, R::AH | R::CH | R::DH | R::BH);
                let mask = if reg.size() == 1 { 255 } else { 65535 };
                let value = self.b.ins().band_imm(value, mask);
                let value = if high {
                    self.b.ins().ishl_imm(value, 8)
                } else {
                    value
                };
                let old = self.b.use_var(var);
                let old = self
                    .b
                    .ins()
                    .band_imm(old, !(mask << if high { 8 } else { 0 }));
                self.b.ins().bor(old, value)
            };
            self.b.def_var(var, value);
        }
    }
    fn flags(
        &mut self,
        a: Value,
        b: Value,
        result: Value,
        subtract: bool,
        logical: bool,
        keep_cf: bool,
    ) {
        let kind = self.constant(if logical {
            3
        } else if subtract {
            2
        } else {
            1
        });
        for (var, value) in self.lazy[..4].iter().zip([a, b, result, kind]) {
            self.b.def_var(*var, value);
        }
        if !keep_cf {
            let (left, right, kind) = if logical {
                let zero = self.constant(0);
                (zero, zero, zero)
            } else {
                let kind = self.constant(1);
                if subtract {
                    (a, b, kind)
                } else {
                    (result, a, kind)
                }
            };
            for (var, value) in self.lazy[4..].iter().zip([left, right, kind]) {
                self.b.def_var(*var, value);
            }
        }
    }
    /// Compute only the requested flag. Full EFLAGS is reconstructed on exits.
    fn flag(&mut self, bit: u32) -> Value {
        if bit == 0 {
            let a = self.b.use_var(self.lazy[4]);
            let b = self.b.use_var(self.lazy[5]);
            let kind = self.b.use_var(self.lazy[6]);
            let raw = self.b.ins().icmp_imm(IntCC::Equal, kind, 0);
            let c = self.b.ins().icmp(IntCC::UnsignedLessThan, a, b);
            let c = self.b.ins().uextend(types::I32, c);
            return self.b.ins().select(raw, a, c);
        }
        let old = self.b.use_var(self.flags);
        let old = self.b.ins().ushr_imm(old, i64::from(bit));
        let old = self.b.ins().band_imm(old, 1);
        let a = self.b.use_var(self.lazy[0]);
        let b = self.b.use_var(self.lazy[1]);
        let result = self.b.use_var(self.lazy[2]);
        let kind = self.b.use_var(self.lazy[3]);
        let value = match bit {
            6 => {
                let z = self.b.ins().icmp_imm(IntCC::Equal, result, 0);
                self.b.ins().uextend(types::I32, z)
            }
            7 => self.b.ins().ushr_imm(result, 31),
            2 => {
                let byte = self.b.ins().band_imm(result, 255);
                let count = self.b.ins().popcnt(byte);
                let count = self.b.ins().band_imm(count, 1);
                self.b.ins().bxor_imm(count, 1)
            }
            4 | 11 => {
                let ab = self.b.ins().bxor(a, b);
                let value = if bit == 4 {
                    let xor = self.b.ins().bxor(ab, result);
                    self.b.ins().ushr_imm(xor, 4)
                } else {
                    let add = self.b.ins().icmp_imm(IntCC::Equal, kind, 1);
                    let not = self.b.ins().bnot(ab);
                    let first = self.b.ins().select(add, not, ab);
                    let ar = self.b.ins().bxor(a, result);
                    let of = self.b.ins().band(first, ar);
                    self.b.ins().ushr_imm(of, 31)
                };
                let value = self.b.ins().band_imm(value, 1);
                let logical = self.b.ins().icmp_imm(IntCC::Equal, kind, 3);
                let zero = self.constant(0);
                self.b.ins().select(logical, zero, value)
            }
            _ => unreachable!(),
        };
        let raw = self.b.ins().icmp_imm(IntCC::Equal, kind, 0);
        self.b.ins().select(raw, old, value)
    }
    fn materialize_flags(&mut self) -> Value {
        let old = self.b.use_var(self.flags);
        let mut flags = self.b.ins().band_imm(old, !0x8d5_i64);
        for bit in [0, 2, 4, 6, 7, 11] {
            let value = self.flag(bit);
            let value = self.b.ins().ishl_imm(value, i64::from(bit));
            flags = self.b.ins().bor(flags, value);
        }
        flags
    }
    fn condition(&mut self, m: M) -> Value {
        // Emit just what the condition needs; in particular JZ/JNZ needs no parity,
        // carry or overflow work in an interpreter's loop backedge.
        let (v, is_zero) = match m {
            M::Je | M::Jne => (self.flag(6), m == M::Jne),
            M::Jb | M::Jae => (self.flag(0), m == M::Jae),
            M::Jbe | M::Ja => {
                let c = self.flag(0);
                let z = self.flag(6);
                (self.b.ins().bor(c, z), m == M::Ja)
            }
            M::Jl | M::Jge | M::Jle | M::Jg => {
                let s = self.flag(7);
                let o = self.flag(11);
                let mut so = self.b.ins().bxor(s, o);
                if matches!(m, M::Jle | M::Jg) {
                    let z = self.flag(6);
                    so = self.b.ins().bor(so, z);
                }
                (so, matches!(m, M::Jge | M::Jg))
            }
            M::Jo | M::Jno => (self.flag(11), m == M::Jno),
            M::Js | M::Jns => (self.flag(7), m == M::Jns),
            M::Jp | M::Jnp => (self.flag(2), m == M::Jnp),
            _ => unreachable!(),
        };
        self.b.ins().icmp_imm(
            if is_zero {
                IntCC::Equal
            } else {
                IntCC::NotEqual
            },
            v,
            0,
        )
    }
    fn instruction(&mut self, i: &Instruction) -> bool {
        if let Some(operation) = crate::strings::operation(i) {
            let pc = self.constant(i.ip32());
            self.save_state(pc);
            let operation = self.constant(operation);
            let call = self
                .b
                .ins()
                .call(self.repeat_ref, &[self.bridge, self.state, operation]);
            let result = self.b.inst_results(call)[0];
            self.reload_state();
            let done = self.b.create_block();
            let retry = self.b.create_block();
            let fallback = self.b.create_block();
            let budget = self.b.create_block();
            let mut switch = Switch::new();
            switch.set_entry(0, done);
            switch.set_entry(u128::from(crate::strings::MORE), retry);
            switch.set_entry(u128::from(crate::strings::BUDGET), budget);
            switch.emit(&mut self.b, result, fallback);
            self.b.switch_to_block(done);
            self.direct(i.next_ip32());
            self.b.switch_to_block(retry);
            self.direct(i.ip32());
            self.b.switch_to_block(fallback);
            self.exit_const(Exit::Fallback, i.ip32());
            self.b.switch_to_block(budget);
            self.exit_const(Exit::Budget, i.ip32());
            return true;
        }
        if matches!(i.mnemonic(), M::Cmp | M::Test) {
            let size = if i.op0_kind() == OpKind::Memory {
                i.memory_size().size()
            } else {
                i.op0_register().size()
            };
            if size < 4 {
                self.narrow_compare(i, size);
                return false;
            }
        }
        if mmx::supported(i) {
            self.mmx_instruction(i);
            return false;
        }
        if scalar::supported(i) {
            self.scalar_instruction(i);
            return false;
        }
        if simd::supported(i) {
            self.simd_instruction(i);
            return false;
        }
        match i.mnemonic() {
            M::Int => {
                self.host_call(i.next_ip32());
                return true;
            }
            M::Nop => {}
            M::Mov | M::Lea | M::Movzx | M::Movsx => {
                let mut v = if i.mnemonic() == M::Lea {
                    self.address(i)
                } else {
                    self.read(i, 1)
                };
                if i.mnemonic() == M::Movsx {
                    let size = if i.op1_kind() == OpKind::Memory {
                        i.memory_size().size()
                    } else {
                        i.op1_register().size()
                    };
                    let narrow = self.b.ins().ireduce(integer_type(size), v);
                    v = self.b.ins().sextend(types::I32, narrow);
                }
                let ptr = self.destination(i);
                self.write(i, ptr, v);
            }
            M::Add
            | M::Sub
            | M::And
            | M::Or
            | M::Xor
            | M::Cmp
            | M::Test
            | M::Inc
            | M::Dec
            | M::Not
            | M::Neg => {
                let a = self.read(i, 0);
                let b = if i.op_count() == 2 {
                    self.read(i, 1)
                } else {
                    self.constant(1)
                };
                // Validate the destination before changing flags or registers.
                let ptr = if matches!(i.mnemonic(), M::Cmp | M::Test) {
                    None
                } else {
                    self.destination(i)
                };
                let result = match i.mnemonic() {
                    M::Add | M::Inc => self.b.ins().iadd(a, b),
                    M::Sub | M::Cmp | M::Dec => self.b.ins().isub(a, b),
                    M::And | M::Test => self.b.ins().band(a, b),
                    M::Or => self.b.ins().bor(a, b),
                    M::Xor => self.b.ins().bxor(a, b),
                    M::Not => self.b.ins().bnot(a),
                    M::Neg => self.b.ins().ineg(a),
                    _ => unreachable!(),
                };
                if i.mnemonic() == M::Neg {
                    let zero = self.constant(0);
                    self.flags(zero, a, result, true, false, false);
                } else if i.mnemonic() != M::Not {
                    self.flags(
                        a,
                        b,
                        result,
                        matches!(i.mnemonic(), M::Sub | M::Cmp | M::Dec),
                        matches!(i.mnemonic(), M::And | M::Or | M::Xor | M::Test),
                        matches!(i.mnemonic(), M::Inc | M::Dec),
                    );
                }
                if !matches!(i.mnemonic(), M::Cmp | M::Test) {
                    self.write(i, ptr, result);
                }
            }
            M::Push | M::Call => {
                let v = self.read(i, 0);
                let sp = self.b.use_var(self.regs[4]);
                let sp = self.b.ins().iadd_imm(sp, -4);
                let ptr = self.pointer(sp, true, 4);
                let pushed = if i.mnemonic() == M::Call {
                    self.constant(i.next_ip32())
                } else {
                    v
                };
                self.b.ins().store(MemFlags::new(), pushed, ptr, 0);
                self.b.def_var(self.regs[4], sp);
                if i.mnemonic() == M::Call {
                    if i.op0_kind() == OpKind::NearBranch32 {
                        self.direct(i.near_branch32());
                    } else {
                        self.indirect(v);
                    }
                    return true;
                }
            }
            M::Pop | M::Ret => {
                let sp = self.b.use_var(self.regs[4]);
                let ptr = self.pointer(sp, false, 4);
                let value = self.b.ins().load(types::I32, MemFlags::new(), ptr, 0);
                let adjustment = if i.mnemonic() == M::Ret && i.op_count() != 0 {
                    i64::from(i.immediate16())
                } else {
                    0
                };
                let sp = self.b.ins().iadd_imm(sp, 4 + adjustment);
                self.b.def_var(self.regs[4], sp);
                if i.mnemonic() == M::Ret {
                    if let Some(target) = self.return_hint {
                        // A small interpreter helper often has one caller in the region.
                        // Check the actual guest return address before taking this edge.
                        let hit = self
                            .b
                            .ins()
                            .icmp_imm(IntCC::Equal, value, i64::from(target));
                        let yes = self.b.create_block();
                        let miss = self.b.create_block();
                        self.b.ins().brif(hit, yes, &[], miss, &[]);
                        self.b.switch_to_block(yes);
                        self.direct(target);
                        self.b.switch_to_block(miss);
                        self.indirect(value);
                    } else {
                        self.indirect(value);
                    }
                    return true;
                }
                self.write(i, None, value);
            }
            M::Jmp => {
                if i.op0_kind() == OpKind::NearBranch32 {
                    self.direct(i.near_branch32());
                } else {
                    let target = self.read(i, 0);
                    self.indirect(target);
                }
                return true;
            }
            branch => {
                let condition = self.condition(branch);
                let yes = self.b.create_block();
                let no = self.b.create_block();
                self.b.ins().brif(condition, yes, &[], no, &[]);
                self.b.switch_to_block(yes);
                self.direct(i.near_branch32());
                self.b.switch_to_block(no);
                self.direct(i.next_ip32());
                return true;
            }
        }
        false
    }
}

fn integer_type(size: usize) -> ir::Type {
    match size {
        1 => types::I8,
        2 => types::I16,
        4 => types::I32,
        _ => unreachable!(),
    }
}

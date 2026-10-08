//! Narrow Cranelift access thunks with a per-access frozen recovery lease.
//! Keeping the lease inside the thunk call lets ordinary JIT helpers and traps
//! change mappings after it returns, without weakening A's freeze contract.
use crate::CpuState;
use crate::execution_abi::{NativeRecoveryPoint, RecoveryAccess, RecoveryValue, StoreCommitProof};
use crate::fault_recovery::{CodeRegion, RecoveryKind};
use cranelift_codegen::ir::{self, AbiParam, InstBuilder, MemFlags, types};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module, default_libcall_names};
use std::cell::Cell;

pub(crate) const CHECKED: u64 = 1 << 32;
pub(crate) const READ_FAULT: u64 = 2 << 32;
pub(crate) const WRITE_FAULT: u64 = 3 << 32;
pub(crate) const CODE_WRITE: u64 = 4 << 32;

type Entry = unsafe extern "C" fn(*mut u8, u32) -> u32;
struct Thunk {
    entry: Entry,
    len: usize,
    site: usize,
    resume: usize,
}

pub(crate) struct Engine {
    module: Option<JITModule>,
    thunks: Vec<Thunk>,
    pub recoveries: Cell<u64>,
    pub violations: Cell<u64>,
    pub checked: Cell<u64>,
    pub code_writes: Cell<u64>,
    pub accesses: Cell<u64>,
}

impl Drop for Engine {
    fn drop(&mut self) {
        // No thunk pointer escapes the owner; its sessions borrow this engine.
        unsafe {
            self.module.take().unwrap().free_memory();
        }
    }
}

impl Engine {
    pub fn new() -> Result<Self, String> {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        let _write = crate::jit_memory_macos::WriteScope::new();
        let builder = JITBuilder::with_flags(&[("opt_level", "speed")], default_libcall_names())
            .map_err(|e| e.to_string())?;
        #[cfg(all(target_os = "android", target_arch = "aarch64"))]
        let builder = {
            let mut builder = builder;
            builder.memory_provider(Box::new(crate::jit_memory_android::AndroidMemory::default()));
            builder
        };
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        let builder = {
            let mut builder = builder;
            builder.memory_provider(Box::new(crate::jit_memory_macos::MacMemory::default()));
            builder
        };
        let mut module = JITModule::new(builder);
        let mut descriptions = Vec::new();
        for write in [false, true] {
            for width in [1, 2, 4] {
                let mut ctx = module.make_context();
                let ptr = module.target_config().pointer_type();
                ctx.func.signature = module.make_signature();
                ctx.func
                    .signature
                    .params
                    .extend([AbiParam::new(ptr), AbiParam::new(types::I32)]);
                ctx.func.signature.returns.push(AbiParam::new(types::I32));
                let id = module
                    .declare_function(
                        &format!("scalar_{}_{}", write as u8, width),
                        Linkage::Local,
                        &ctx.func.signature,
                    )
                    .map_err(|e| e.to_string())?;
                let mut frontend = FunctionBuilderContext::new();
                let mut b = FunctionBuilder::new(&mut ctx.func, &mut frontend);
                let entry = b.create_block();
                b.append_block_params_for_function_params(entry);
                b.switch_to_block(entry);
                let address = b.block_params(entry)[0];
                let value = b.block_params(entry)[1];
                let ty = match width {
                    1 => types::I8,
                    2 => types::I16,
                    _ => types::I32,
                };
                let flags = MemFlags::new().with_trap_code(Some(ir::TrapCode::user(1).unwrap()));
                let result = if write {
                    let value = if width == 4 {
                        value
                    } else {
                        b.ins().ireduce(ty, value)
                    };
                    b.ins().store(flags, value, address, 0);
                    b.ins().iconst(types::I32, 0)
                } else {
                    let value = b.ins().load(ty, flags, address, 0);
                    if width == 4 {
                        value
                    } else {
                        b.ins().uextend(types::I32, value)
                    }
                };
                b.ins().return_(&[result]);
                b.seal_all_blocks();
                b.finalize();
                module
                    .define_function(id, &mut ctx)
                    .map_err(|e| format!("{e:?}"))?;
                let code = ctx.compiled_code().unwrap();
                let traps = code.buffer.traps();
                if traps.len() != 1 {
                    return Err("scalar thunk must contain exactly one faulting access".into());
                }
                let site = traps[0].offset as usize;
                let resume = validate_instruction(code.code_buffer(), site, width, write)?;
                descriptions.push((id, code.code_buffer().len(), site, resume));
            }
        }
        module.finalize_definitions().map_err(|e| e.to_string())?;
        let thunks = descriptions
            .into_iter()
            .map(|(id, len, site, resume)| {
                let pointer = module.get_finalized_function(id);
                Thunk {
                    entry: unsafe { std::mem::transmute::<*const u8, Entry>(pointer) },
                    len,
                    site,
                    resume,
                }
            })
            .collect();
        Ok(Self {
            module: Some(module),
            thunks,
            recoveries: Cell::new(0),
            violations: Cell::new(0),
            checked: Cell::new(0),
            code_writes: Cell::new(0),
            accesses: Cell::new(0),
        })
    }
}

/// Confirm the precise machine access, not a source-line/debug-label range.
/// The generated function has no calls or guest side effects after this access;
/// on a fault its otherwise unused return value is replaced by recovery below.
fn validate_instruction(
    code: &[u8],
    offset: usize,
    width: usize,
    write: bool,
) -> Result<usize, String> {
    #[cfg(target_arch = "x86_64")]
    {
        use iced_x86::{Decoder, DecoderOptions, Mnemonic, OpKind};
        let i = Decoder::new(64, &code[offset..], DecoderOptions::NONE).decode();
        if matches!(i.mnemonic(), Mnemonic::Mov | Mnemonic::Movzx)
            && i.op_kind(if write { 0 } else { 1 }) == OpKind::Memory
            && i.memory_size().size() == width
            && !i.has_lock_prefix()
        {
            return Ok(offset + i.len());
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        let word = u32::from_le_bytes(code[offset..offset + 4].try_into().unwrap());
        // Plain unsigned-offset LDR[B/H]/STR[B/H], W register, offset zero.
        // No pairs, exclusive accesses, SIMD, writeback or sign extension.
        let expected = match width {
            1 => 0x39000000,
            2 => 0x79000000,
            _ => 0xb9000000,
        } | if write { 0 } else { 0x00400000 };
        if word & 0xfffffc00 == expected {
            return Ok(offset + 4);
        }
    }
    Err("unsupported machine instruction at scalar recovery site".into())
}

/// Called only by generated MOV code after materializing its complete prefix.
/// A source/value constant in this session describes this invocation, never a
/// reread of guest memory. The shared ABI v1 remains unchanged.
pub(crate) unsafe extern "C" fn access(
    bridge: *mut u8,
    state: *mut CpuState,
    address: u32,
    value: u32,
    next_pc: u32,
    operand: u32,
) -> u64 {
    let bridge = unsafe { &mut *bridge.cast::<crate::trap::Bridge<'_>>() };
    let memory = unsafe { &*bridge.memory };
    let engine = memory.direct_engine.as_ref().expect("direct scalar owner");
    let width = (operand & 0xff) as usize;
    let write = operand & 0x100 != 0;
    let offset = address.checked_sub(memory.base).map(|v| v as usize);
    // Shape checks are not READ/WRITE permission checks. Any private in-range
    // violation reaches the OS window and B's precise fault classification.
    let eligible = offset.is_some_and(|offset| {
        offset
            .checked_add(width)
            .is_some_and(|end| end <= memory.len)
            && (address as usize & 4095) + width <= 4096
            && (!write || (address as usize).is_multiple_of(width))
            && memory.perms[offset / 4096] & crate::memory::SHARED == 0
    });
    if !eligible {
        engine.checked.set(engine.checked.get() + 1);
        return CHECKED;
    }
    let offset = offset.unwrap();
    let index = match width {
        1 => 0,
        2 => 1,
        4 => 2,
        _ => unreachable!(),
    } + if write { 3 } else { 0 };
    let thunk = &engine.thunks[index];
    let base = thunk.entry as usize;
    // Cranelift's heap-backed code can carry an Android allocation tag. The
    // kernel reports an untagged instruction PC, so both exact recovery PCs
    // must use that representation. Keep the entry and allocator-owned pointer
    // unchanged: data pointers still need their original tag on deallocation.
    #[cfg(all(target_os = "android", target_arch = "aarch64"))]
    let base = base & 0x00ff_ffff_ffff_ffff;
    let state = unsafe { &mut *state };
    let site = NativeRecoveryPoint {
        host_pc: base + thunk.site,
        recovery_pc: base + thunk.resume,
        guest_pc: state.eip,
        next_pc,
        width: width as u8,
        access: if write {
            RecoveryAccess::Write
        } else {
            RecoveryAccess::Read
        },
        proof: if write {
            StoreCommitProof::AlignedPrivateScalar
        } else {
            StoreCommitProof::LoadBeforeCommit
        },
        registers: std::array::from_fn(|index| RecoveryValue::GuestRegister(index as u8)),
        flags: RecoveryValue::GuestFlags,
        address: RecoveryValue::Constant(address),
        value: RecoveryValue::Constant(value),
        destination: (!write).then_some((operand >> 16) as u8),
        committed_units: 0,
    };
    let window = memory.direct_window();
    // SAFETY: one plain, naturally aligned private scalar store (or scalar
    // load), exact PC, immutable table and pinned code/backing. The return path
    // after the access has no guest effects. Recovery finishes before the
    // session ends and before the outer JIT frame is allowed to continue.
    let execution = unsafe {
        CodeRegion::new(engine, base, thunk.len, std::slice::from_ref(&site)).and_then(|region| {
            crate::fault_recovery::with_session(state, &[region], window, memory.base, |_| {
                let result = (thunk.entry)(window.ptr.as_ptr().add(offset), value);
                let recovered = crate::fault_recovery::recover_current();
                (result, recovered)
            })
        })
    };
    let execution = match execution {
        Ok(execution) => execution,
        Err(error) => {
            // Installation/publication failed before entry, or an invariant
            // failed after entry. Do not retry an operation of unknown status.
            bridge.error = Some(error);
            return if write { WRITE_FAULT } else { READ_FAULT };
        }
    };
    engine.accesses.set(engine.accesses.get() + 1);
    match execution.result {
        (result, None) => u64::from(result),
        (_, Some(outcome)) => match outcome.kind {
            RecoveryKind::Completed => {
                engine.recoveries.set(engine.recoveries.get() + 1);
                u64::from(if write { 0 } else { outcome.value })
            }
            RecoveryKind::AccessViolation => {
                engine.violations.set(engine.violations.get() + 1);
                if write { WRITE_FAULT } else { READ_FAULT }
            }
            RecoveryKind::CodeWriteProtection => {
                engine.code_writes.set(engine.code_writes.get() + 1);
                CODE_WRITE
            }
        },
    }
}

pub(crate) unsafe extern "C" fn checked(bridge: *mut u8) {
    let bridge = unsafe { &*bridge.cast::<crate::trap::Bridge<'_>>() };
    let memory = unsafe { &*bridge.memory };
    if let Some(engine) = &memory.direct_engine {
        engine.checked.set(engine.checked.get() + 1);
    }
}

#[cfg(test)]
mod tests;

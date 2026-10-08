//! Cranelift lowering of verified GIR on a private serial mapping lease.
use super::{ir::*, recovery, verify};
use crate::memory::gir::{Lease, LeaseCache};
use cranelift_codegen::ir::{
    self as cl, AbiParam, InstBuilder, MemFlags, Value as V, condcodes::IntCC as CC, types,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub const COMPLETE: u32 = 0;
pub const BUDGET: u32 = 1;
pub const READ_CHECK: u32 = 2;
pub const WRITE_CHECK: u32 = 3;
pub const GUARD: u32 = 4;
pub const INDIRECT: u32 = 5;
pub const UNSUPPORTED: u32 = 6;
pub const INTERRUPTED: u32 = 7;
pub const REVOKED: u32 = 8;
pub const STOP: u32 = 9;
pub const CALL: u32 = 10;

#[derive(Clone, Debug)]
pub struct RangeProof {
    pub memory: MemoryId,
    pub address: u32,
    pub width: usize,
    pub write: bool,
}

impl RangeProof {
    fn constants(r: &Region) -> Vec<Self> {
        r.memory
            .iter()
            .enumerate()
            .filter_map(|(n, m)| {
                let ValueKind::Constant(address) = r.values[m.address.0].kind else {
                    return None;
                };
                Some(Self {
                    memory: MemoryId(n),
                    address: address as u32,
                    width: (m.width.bits() / 8) as usize,
                    write: matches!(m.access, Access::Write { .. }),
                })
            })
            .collect()
    }
}
#[repr(C)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    pub gpr: [u32; 8],
    pub flags: u32,
    pub pc: u32,
    pub committed: u64,
    pub address: u32,
    pub status: u32,
    pub lease_epoch: u64,
    pub last_indirect_branch: crate::IndirectBranch,
    pub tail_entries: u64,
    pub tail_calls: u64,
}
impl Frame {
    pub fn from_cpu(c: &crate::CpuState) -> Self {
        Self {
            gpr: c.regs,
            flags: c.eflags,
            pc: c.eip,
            last_indirect_branch: c.last_indirect_branch,
            ..Self::default()
        }
    }
    pub fn restore(&self, c: &mut crate::CpuState) {
        c.regs = self.gpr;
        c.eflags = self.flags;
        c.eip = self.pc;
        c.last_indirect_branch = self.last_indirect_branch;
    }
}
pub struct Code {
    module: Option<JITModule>,
    entry: *const u8,
    tail: Option<*const u8>,
    link_count: usize,
    base: u32,
    len: usize,
    pub machine: Vec<u8>,
    pub code_bytes: usize,
    pub blocks: Vec<u32>,
    pub vcode: String,
    pub cross: Option<(Vec<u8>, Vec<u32>, String)>,
    pub cold_edges: usize,
    pub cold_blocks: usize,
    pub flags_materialized: usize,
    pub compile_us: u128,
    pub proofs: Vec<RangeProof>,
    // Code, cold recovery recipes and dependency identities have one owner.
    pub recovery: recovery::Plan,
    pub table: RecoveryTable,
}

/// One immutable invocation shared by every tail in an owner-held family.
/// No guest state lives here: the eleven-argument Tail signature carries it.
#[repr(C)]
struct Invocation {
    frame: *mut Frame,
    data: *mut u8,
    perms: *const u8,
    budget: u64,
    interrupt: *const AtomicBool,
    revoke: *const AtomicBool,
    stop: u64,
    links: *const usize,
}

#[cfg(test)]
#[test]
fn invocation_layout_matches_the_tail_bridge() {
    use std::mem::{offset_of, size_of};
    assert_eq!(size_of::<Invocation>(), 64);
    assert_eq!(offset_of!(Invocation, frame), 0);
    assert_eq!(offset_of!(Invocation, data), 8);
    assert_eq!(offset_of!(Invocation, perms), 16);
    assert_eq!(offset_of!(Invocation, budget), 24);
    assert_eq!(offset_of!(Invocation, interrupt), 32);
    assert_eq!(offset_of!(Invocation, revoke), 40);
    assert_eq!(offset_of!(Invocation, stop), 48);
    assert_eq!(offset_of!(Invocation, links), 56);
}

pub(super) fn tail_signature() -> cl::Signature {
    let mut sig = cl::Signature::new(cranelift_codegen::isa::CallConv::Tail);
    sig.params.extend([AbiParam::new(types::I64); 11]);
    sig.returns.push(AbiParam::new(types::I32));
    sig
}

/// B-route v1 metadata describes completed cold checkpoints. It is not a
/// hardware-fault table: arbitrary hot PCs have no published recovery source.
pub struct RecoveryTable {
    pub version: u32,
    pub code_id: u64,
    pub code_version: u32,
    pub returns: Vec<(u32, usize)>,
    pub snapshots: Vec<Snapshot>,
    pub values: Vec<Value>,
    pub checkpoints: Vec<recovery::Dag>,
    pub commits: Vec<CommitPoint>,
    pub accesses: Vec<MemoryOp>,
    pub dependencies: Vec<Dependency>,
    pub page_dependencies: Vec<(usize, u64)>,
    pub lease_epoch_offset: u32,
    /// Every return checkpoint contains GPR/EFLAGS/PC/retirement/EA/reason.
    /// Offsets are frame-relative, valid only after that cold block's stores.
    pub checkpoint_offsets: [u32; 16],
}
impl Drop for Code {
    fn drop(&mut self) {
        unsafe { self.module.take().unwrap().free_memory() };
    }
}
impl Code {
    /// Diagnostic convenience wrapper; production reuses its lease table.
    pub fn run(
        &self,
        frame: &mut Frame,
        memory: &mut crate::Memory,
        budget: u64,
    ) -> Result<u32, String> {
        let mut cache = LeaseCache::default();
        let mut lease = cache.acquire(memory)?;
        self.run_lease(
            frame,
            &mut lease,
            budget,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
            u64::MAX,
        )
    }

    pub fn proven(&self, lease: &Lease<'_>) -> bool {
        lease.window() == (self.base, self.len)
            && self
                .proofs
                .iter()
                .all(|p| lease.allows(p.address, p.width, p.write))
    }

    pub fn run_lease(
        &self,
        frame: &mut Frame,
        lease: &mut Lease<'_>,
        budget: u64,
        interrupt: &AtomicBool,
        revoke: &AtomicBool,
        stop: u64,
    ) -> Result<u32, String> {
        self.run_linked(frame, lease, budget, interrupt, revoke, stop, &[])
    }

    pub fn tail_entry(&self) -> usize {
        self.tail.expect("state-argument body") as usize
    }

    pub fn run_linked(
        &self,
        frame: &mut Frame,
        lease: &mut Lease<'_>,
        budget: u64,
        interrupt: &AtomicBool,
        revoke: &AtomicBool,
        stop: u64,
        links: &[usize],
    ) -> Result<u32, String> {
        if !self.proven(lease) {
            return Err("GIR entry mapping proof failed".into());
        }
        frame.lease_epoch = lease.epoch;
        if self.tail.is_some() {
            if links.len() < self.link_count {
                return Err("GIR tail family is incomplete".into());
            }
            let invocation = Invocation {
                frame,
                data: lease.data(),
                perms: lease.permissions(),
                budget: frame.committed.saturating_add(budget),
                interrupt,
                revoke,
                stop,
                links: links.as_ptr(),
            };
            // SAFETY: the family owns all immutable entries until this lease
            // returns; the C wrapper converts to the uniform Tail convention.
            let f: unsafe extern "C" fn(*const Invocation) -> u32 =
                unsafe { std::mem::transmute(self.entry) };
            return Ok(unsafe { f(&invocation) });
        }
        let f: unsafe extern "C" fn(
            *mut Frame,
            *mut u8,
            *const u8,
            u64,
            &AtomicBool,
            &AtomicBool,
            u64,
        ) -> u32 = unsafe { std::mem::transmute(self.entry) };
        // SAFETY: generated checks cover the full range and both endpoint
        // pages; width <= 4, mappings stay frozen, and no helpers can reenter.
        Ok(unsafe {
            f(
                frame,
                lease.data(),
                lease.permissions(),
                frame.committed.saturating_add(budget),
                interrupt,
                revoke,
                stop,
            )
        })
    }
}
pub fn compile(r: &Region, base: u32, len: usize, cross: bool) -> Result<Code, String> {
    compile_with_sharing(r, base, len, cross, recovery::Sharing::Compatible)
}
pub fn compile_with_sharing(
    r: &Region,
    base: u32,
    len: usize,
    cross: bool,
    sharing: recovery::Sharing,
) -> Result<Code, String> {
    compile_variant(r, base, len, cross, sharing, false, None, 0)
}

pub fn compile_proven(r: &Region, base: u32, len: usize) -> Result<Code, String> {
    compile_variant(
        r,
        base,
        len,
        false,
        recovery::Sharing::Compatible,
        true,
        None,
        0,
    )
}

pub fn compile_linked(
    r: &Region,
    base: u32,
    len: usize,
    links: &BTreeMap<u32, usize>,
    entry_pc: u32,
) -> Result<Code, String> {
    compile_variant(
        r,
        base,
        len,
        false,
        recovery::Sharing::Compatible,
        false,
        Some(links),
        entry_pc,
    )
}

pub fn compile_proven_tail(
    r: &Region,
    base: u32,
    len: usize,
    entry_pc: u32,
) -> Result<Code, String> {
    compile_variant(
        r,
        base,
        len,
        false,
        recovery::Sharing::Compatible,
        true,
        Some(&BTreeMap::new()),
        entry_pc,
    )
}

fn compile_variant(
    r: &Region,
    base: u32,
    len: usize,
    cross: bool,
    sharing: recovery::Sharing,
    proven: bool,
    links: Option<&BTreeMap<u32, usize>>,
    entry_pc: u32,
) -> Result<Code, String> {
    verify::verify(r).map_err(|e| e.to_string())?;
    let plan = recovery::Plan::build(r, sharing);
    plan.verify(r).map_err(|e| e.to_string())?;
    let proofs = if proven {
        RangeProof::constants(r)
    } else {
        vec![]
    };
    let began = std::time::Instant::now();
    #[allow(unused_mut)]
    let mut jit = JITBuilder::with_flags(
        &[
            ("opt_level", "speed"),
            ("preserve_frame_pointers", "true"),
            ("machine_code_cfg_info", "true"),
        ],
        cranelift_module::default_libcall_names(),
    )
    .map_err(|e| e.to_string())?;
    #[cfg(all(target_os = "android", target_arch = "aarch64"))]
    jit.memory_provider(Box::new(crate::jit_memory_android::AndroidMemory::default()));
    let mut module = JITModule::new(jit);
    let mut context = module.make_context();
    context
        .func
        .signature
        .params
        .extend([AbiParam::new(types::I64); 7]);
    context
        .func
        .signature
        .returns
        .push(AbiParam::new(types::I32));
    if links.is_some() {
        context.func.signature = tail_signature();
    }
    let id = module
        .declare_function("formal_gir", Linkage::Local, &context.func.signature)
        .map_err(|e| e.to_string())?;
    let mut fc = FunctionBuilderContext::new();
    let (cold_edges, cold_blocks, flags_materialized, checkpoints) = build(
        &mut context.func,
        &mut fc,
        r,
        &plan,
        base,
        len,
        sharing,
        &proofs,
        links,
        entry_pc,
    );
    let cross_function = cross.then(|| context.func.clone());
    context.set_disasm(true);
    module
        .define_function(id, &mut context)
        .map_err(|e| format!("{e:?}"))?;
    let code = context.compiled_code().unwrap();
    static NEXT_CODE: AtomicU64 = AtomicU64::new(1);
    let table = RecoveryTable {
        version: 1,
        code_id: NEXT_CODE.fetch_add(1, Ordering::Relaxed),
        code_version: r.semantic_version,
        returns: code
            .buffer
            .get_srclocs_sorted()
            .iter()
            .filter(|s| s.loc.bits() > 0 && s.loc.bits() <= cold_blocks as u32)
            .map(|s| (s.start, s.loc.bits() as usize - 1))
            .collect(),
        snapshots: r.snapshots.clone(),
        values: r.values.clone(),
        checkpoints,
        commits: r.commits.clone(),
        accesses: r.memory.clone(),
        dependencies: r.dependencies.clone(),
        page_dependencies: vec![],
        lease_epoch_offset: 56,
        checkpoint_offsets: [0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 48, 52, 64, 68, 72],
    };
    let (machine, blocks, vcode) = (
        code.code_buffer().to_vec(),
        code.bb_starts.clone(),
        code.vcode.clone().unwrap(),
    );
    module.finalize_definitions().map_err(|e| e.to_string())?;
    let body_entry = module.get_finalized_function(id);
    let mut code_bytes = machine.len();
    let entry = if links.is_some() {
        let mut wrapper = module.make_context();
        wrapper
            .func
            .signature
            .params
            .push(AbiParam::new(types::I64));
        wrapper
            .func
            .signature
            .returns
            .push(AbiParam::new(types::I32));
        let wrapper_id = module
            .declare_function("gir_entry", Linkage::Local, &wrapper.func.signature)
            .map_err(|e| e.to_string())?;
        let sig = wrapper.func.import_signature(tail_signature());
        {
            let mut b = FunctionBuilder::new(&mut wrapper.func, &mut fc);
            let block = b.create_block();
            b.append_block_params_for_function_params(block);
            b.switch_to_block(block);
            let invocation = b.block_params(block)[0];
            let frame = b.ins().load(types::I64, MemFlags::trusted(), invocation, 0);
            let mut args = Vec::new();
            for n in 0..9 {
                let v = b.ins().load(types::I32, MemFlags::trusted(), frame, n * 4);
                args.push(b.ins().uextend(types::I64, v));
            }
            args.push(b.ins().load(types::I64, MemFlags::trusted(), frame, 40));
            args.push(invocation);
            // Different JIT allocations need not be in rel32 reach on x64.
            let address = b.ins().iconst(types::I64, body_entry as i64);
            let call = b.ins().call_indirect(sig, address, &args);
            let status = b.inst_results(call)[0];
            b.ins().return_(&[status]);
            b.seal_all_blocks();
            b.finalize();
        }
        module
            .define_function(wrapper_id, &mut wrapper)
            .map_err(|e| format!("{e:?}"))?;
        code_bytes += wrapper.compiled_code().unwrap().code_buffer().len();
        module.finalize_definitions().map_err(|e| e.to_string())?;
        module.get_finalized_function(wrapper_id)
    } else {
        body_entry
    };
    let compile_us = began.elapsed().as_micros();
    let cross = cross_function.and_then(cross_compile);
    Ok(Code {
        module: Some(module),
        entry,
        tail: links.map(|_| body_entry),
        link_count: links.and_then(|m| m.values().max()).map_or(0, |n| n + 1),
        base,
        len,
        machine,
        code_bytes,
        blocks,
        vcode,
        cross,
        cold_edges,
        cold_blocks,
        flags_materialized,
        compile_us,
        proofs,
        recovery: plan,
        table,
    })
}
#[cfg(feature = "cross-dump")]
fn cross_compile(mut f: cl::Function) -> Option<(Vec<u8>, Vec<u32>, String)> {
    use cranelift_codegen::settings::{self, Configurable};
    f.signature.call_conv = cranelift_codegen::isa::CallConv::SystemV;
    let mut flags = settings::builder();
    for (k, v) in [
        ("opt_level", "speed"),
        ("preserve_frame_pointers", "true"),
        ("machine_code_cfg_info", "true"),
    ] {
        flags.set(k, v).unwrap();
    }
    let isa = cranelift_codegen::isa::lookup_by_name("aarch64")
        .unwrap()
        .finish(settings::Flags::new(flags))
        .unwrap();
    let mut cx = cranelift_codegen::Context::for_function(f);
    cx.set_disasm(true);
    let code = cx.compile(&*isa, &mut Default::default()).unwrap();
    Some((
        code.code_buffer().to_vec(),
        code.bb_starts.clone(),
        code.vcode.clone().unwrap(),
    ))
}
#[cfg(not(feature = "cross-dump"))]
fn cross_compile(_: cl::Function) -> Option<(Vec<u8>, Vec<u32>, String)> {
    None
}
struct Emitter<'a> {
    b: FunctionBuilder<'a>,
    r: &'a Region,
    values: Vec<Option<V>>,
    blocks: Vec<cl::Block>,
    cold: Vec<(cl::Block, recovery::Dag)>,
    sharing: recovery::Sharing,
    plan: &'a recovery::Plan,
    frame: V,
    data: V,
    perms: V,
    budget: V,
    interrupt: V,
    revoke: V,
    stop: V,
    proofs: &'a [RangeProof],
    retired: V,
    base: u32,
    len: usize,
    edges: usize,
    materialized: usize,
    invocation: Option<V>,
    links: Option<&'a BTreeMap<u32, usize>>,
    tails: Vec<(cl::Block, usize, bool)>,
}
impl Emitter<'_> {
    fn c(&mut self, n: u64) -> V {
        self.b.ins().iconst(types::I64, n as i64)
    }
    fn boolean(&mut self, v: V) -> V {
        self.b.ins().uextend(types::I64, v)
    }
    fn cmp(&mut self, cc: CC, a: V, b: V) -> V {
        let v = self.b.ins().icmp(cc, a, b);
        self.boolean(v)
    }
    fn nz(&mut self, v: V) -> V {
        let q = self.b.ins().icmp_imm(CC::NotEqual, v, 0);
        self.boolean(q)
    }
    fn mask(&mut self, v: V, w: Width) -> V {
        if w == Width::B64 {
            v
        } else {
            self.b.ins().band_imm(v, w.mask() as i64)
        }
    }
    fn signed(&mut self, v: V, w: Width) -> V {
        if w == Width::B64 {
            v
        } else {
            let a = self.b.ins().ishl_imm(v, 64 - w.bits() as i64);
            self.b.ins().sshr_imm(a, 64 - w.bits() as i64)
        }
    }
    fn get(&mut self, id: ValueId) -> V {
        if let Some(v) = self.values[id.0] {
            return v;
        }
        let v = self.r.values[id.0].clone();
        match v.kind {
            ValueKind::Flags(_) | ValueKind::Pure(Pure::FlagPack { .. }) => self.full_flags(id),
            _ => self.compute(&v.kind, v.width),
        }
    }
    fn rest(&mut self, id: ValueId) -> V {
        if let Some(v) = self.values[id.0] {
            return self.b.ins().band_imm(v, i64::from(!ARITHMETIC));
        }
        match self.r.values[id.0].kind.clone() {
            ValueKind::Flags(f) => self.rest(f.preserved),
            ValueKind::Pure(Pure::FlagPack { rest, .. }) => self.get(rest),
            _ => {
                let v = self.get(id);
                self.b.ins().band_imm(v, i64::from(!ARITHMETIC))
            }
        }
    }
    fn full_flags(&mut self, id: ValueId) -> V {
        self.materialized += 1;
        let mut v = self.rest(id);
        for bit in FLAG_BITS {
            let lane = self.flag(id, bit);
            let lane = self.b.ins().ishl_imm(lane, bit.trailing_zeros() as i64);
            v = self.b.ins().bor(v, lane);
        }
        v
    }
    fn flag(&mut self, id: ValueId, bit: u32) -> V {
        if let Some(v) = self.values[id.0] {
            let v = self.b.ins().ushr_imm(v, bit.trailing_zeros() as i64);
            return self.b.ins().band_imm(v, 1);
        }
        match self.r.values[id.0].kind.clone() {
            ValueKind::Flags(f) if f.defined & bit != 0 => self.flag_recipe(&f, bit),
            ValueKind::Flags(f) => self.flag(f.preserved, bit),
            ValueKind::Pure(Pure::FlagPack { bits, .. }) => {
                self.get(bits[FLAG_BITS.iter().position(|&b| b == bit).unwrap()])
            }
            _ => {
                let v = self.get(id);
                let v = self.b.ins().ushr_imm(v, bit.trailing_zeros() as i64);
                self.b.ins().band_imm(v, 1)
            }
        }
    }
    fn flag_recipe(&mut self, f: &Flags, bit: u32) -> V {
        let a = self.get(f.lhs);
        let b = self.get(f.rhs);
        let carry = self.get(f.carry);
        let result = self.get(f.result);
        let zero = self.c(0);
        let sign = self.c(1 << (f.width.bits() - 1));
        let shift = matches!(
            f.kind,
            FlagKind::Shl | FlagKind::Shr | FlagKind::Sar | FlagKind::Rol | FlagKind::Ror
        );
        let bit_set = |this: &mut Self, v: V, mask: V| {
            let v = this.b.ins().band(v, mask);
            this.nz(v)
        };
        let mut out = match bit {
            ZF => self.cmp(CC::Equal, result, zero),
            SF => bit_set(self, result, sign),
            PF => {
                let v = self.b.ins().band_imm(result, 255);
                let v = self.b.ins().popcnt(v);
                let v = self.b.ins().band_imm(v, 1);
                self.b.ins().bxor_imm(v, 1)
            }
            AF => {
                let v = self.b.ins().bxor(a, b);
                let v = self.b.ins().bxor(v, result);
                let m = self.c(16);
                bit_set(self, v, m)
            }
            CF | OF => match f.kind {
                FlagKind::Logic => zero,
                FlagKind::Add | FlagKind::Sub if bit == CF => {
                    let rhs = self.b.ins().iadd(b, carry);
                    if f.kind == FlagKind::Sub {
                        self.cmp(CC::UnsignedLessThan, a, rhs)
                    } else {
                        let sum = self.b.ins().iadd(a, rhs);
                        let limit = self.c(f.width.mask());
                        self.cmp(CC::UnsignedGreaterThan, sum, limit)
                    }
                }
                FlagKind::Add | FlagKind::Sub => {
                    let a = self.signed(a, f.width);
                    let b = self.signed(b, f.width);
                    let wide = if f.kind == FlagKind::Sub {
                        let v = self.b.ins().isub(a, b);
                        self.b.ins().isub(v, carry)
                    } else {
                        let v = self.b.ins().iadd(a, b);
                        self.b.ins().iadd(v, carry)
                    };
                    let narrow = self.signed(result, f.width);
                    self.cmp(CC::NotEqual, wide, narrow)
                }
                FlagKind::SignedMultiply | FlagKind::UnsignedMultiply => {
                    if f.kind == FlagKind::SignedMultiply {
                        let a = self.signed(a, f.width);
                        let b = self.signed(b, f.width);
                        let wide = self.b.ins().imul(a, b);
                        let narrow = self.signed(result, f.width);
                        self.cmp(CC::NotEqual, wide, narrow)
                    } else {
                        let wide = self.b.ins().imul(a, b);
                        let limit = self.c(f.width.mask());
                        self.cmp(CC::UnsignedGreaterThan, wide, limit)
                    }
                }
                _ if bit == OF => match f.kind {
                    FlagKind::Shr => bit_set(self, a, sign),
                    FlagKind::Sar => zero,
                    FlagKind::Ror => {
                        let hi = self.b.ins().ushr_imm(result, f.width.bits() as i64 - 1);
                        let lo = self.b.ins().ushr_imm(result, f.width.bits() as i64 - 2);
                        let v = self.b.ins().bxor(hi, lo);
                        self.b.ins().band_imm(v, 1)
                    }
                    _ => {
                        let hi = bit_set(self, result, sign);
                        let cf = self.flag_recipe(f, CF);
                        self.b.ins().bxor(hi, cf)
                    }
                },
                FlagKind::Rol => self.b.ins().band_imm(result, 1),
                FlagKind::Ror => bit_set(self, result, sign),
                FlagKind::Sar => {
                    let minus = self.b.ins().iadd_imm(b, -1);
                    let shr = self.b.ins().ushr(a, minus);
                    let low = self.b.ins().band_imm(shr, 1);
                    let n = self.c(u64::from(f.width.bits()));
                    let large = self.cmp(CC::UnsignedGreaterThanOrEqual, b, n);
                    let hi = bit_set(self, a, sign);
                    self.b.ins().select(large, hi, low)
                }
                _ => {
                    let count = if f.kind == FlagKind::Shl {
                        let n = self.c(u64::from(f.width.bits()));
                        self.b.ins().isub(n, b)
                    } else {
                        self.b.ins().iadd_imm(b, -1)
                    };
                    let v = self.b.ins().ushr(a, count);
                    self.b.ins().band_imm(v, 1)
                }
            },
            _ => unreachable!(),
        };
        if shift {
            let mut preserve = self.cmp(CC::Equal, b, zero);
            if bit == OF {
                let one = self.c(1);
                preserve = self.cmp(CC::NotEqual, b, one);
            }
            if bit == CF && matches!(f.kind, FlagKind::Shl | FlagKind::Shr) {
                let width = self.c(u64::from(f.width.bits()));
                let large = self.cmp(CC::UnsignedGreaterThan, b, width);
                preserve = self.b.ins().bor(preserve, large);
            }
            let old = self.flag(f.preserved, bit);
            out = self.b.ins().select(preserve, old, out);
        }
        out
    }
    fn condition(&mut self, flags: ValueId, code: iced_x86::ConditionCode) -> V {
        use iced_x86::ConditionCode::*;
        let (value, invert) = match code {
            o | no => (self.flag(flags, OF), code == no),
            b | ae => (self.flag(flags, CF), code == ae),
            e | ne => (self.flag(flags, ZF), code == ne),
            s | ns => (self.flag(flags, SF), code == ns),
            p | np => (self.flag(flags, PF), code == np),
            be | a => {
                let c = self.flag(flags, CF);
                let z = self.flag(flags, ZF);
                (self.b.ins().bor(c, z), code == a)
            }
            l | ge | le | g => {
                let sign = self.flag(flags, SF);
                let overflow = self.flag(flags, OF);
                let mut v = self.b.ins().bxor(sign, overflow);
                if matches!(code, le | g) {
                    let z = self.flag(flags, ZF);
                    v = self.b.ins().bor(v, z);
                }
                (v, matches!(code, ge | g))
            }
            None => (self.c(1), false),
        };
        if invert {
            self.b.ins().bxor_imm(value, 1)
        } else {
            value
        }
    }
    fn compute(&mut self, kind: &ValueKind, width: Width) -> V {
        let result = match kind {
            ValueKind::Constant(n) => self.c(*n),
            ValueKind::Pure(p) => match *p {
                Pure::FlagBit { flags, bit } => self.flag(flags, bit),
                Pure::Condition { flags, code } => self.condition(flags, code),
                Pure::Nonzero(v) => {
                    let v = self.get(v);
                    self.nz(v)
                }
                Pure::Extract { value, lsb } => {
                    if self.r.values[value.0].width == Width::B32
                        && width == Width::B1
                        && FLAG_BITS.contains(&(1u32 << lsb))
                    {
                        self.flag(value, 1u32 << lsb)
                    } else {
                        let v = self.get(value);
                        self.b.ins().ushr_imm(v, lsb as i64)
                    }
                }
                Pure::Extend { value, signed } => {
                    let v = self.get(value);
                    if signed {
                        self.signed(v, self.r.values[value.0].width)
                    } else {
                        v
                    }
                }
                Pure::Insert { base, value, lsb } => {
                    let a = self.get(base);
                    let v = self.get(value);
                    let a = self
                        .b
                        .ins()
                        .band_imm(a, !(self.r.values[value.0].width.mask() << lsb) as i64);
                    let v = self.b.ins().ishl_imm(v, lsb as i64);
                    self.b.ins().bor(a, v)
                }
                Pure::Select { condition, yes, no } => {
                    let c = self.get(condition);
                    let a = self.get(yes);
                    let b = self.get(no);
                    self.b.ins().select(c, a, b)
                }
                Pure::Binary(Binary::And, a, b)
                    if self.r.values[b.0].kind == ValueKind::Constant(u64::from(!ARITHMETIC)) =>
                {
                    self.rest(a)
                }
                Pure::Binary(op, a, b) => {
                    let a = self.get(a);
                    let b = self.get(b);
                    match op {
                        Binary::Add => self.b.ins().iadd(a, b),
                        Binary::Sub => self.b.ins().isub(a, b),
                        Binary::Mul => self.b.ins().imul(a, b),
                        Binary::And => self.b.ins().band(a, b),
                        Binary::Or => self.b.ins().bor(a, b),
                        Binary::Xor => self.b.ins().bxor(a, b),
                        Binary::Shl => self.b.ins().ishl(a, b),
                        Binary::Shr => self.b.ins().ushr(a, b),
                        Binary::Sar => {
                            let a = self.signed(a, width);
                            self.b.ins().sshr(a, b)
                        }
                        Binary::Rol | Binary::Ror => {
                            let b = self.b.ins().band_imm(b, width.bits() as i64 - 1);
                            let bits = self.c(u64::from(width.bits()));
                            let opposite = self.b.ins().isub(bits, b);
                            let (left, right) = if op == Binary::Rol {
                                (b, opposite)
                            } else {
                                (opposite, b)
                            };
                            let l = self.b.ins().ishl(a, left);
                            let r = self.b.ins().ushr(a, right);
                            self.b.ins().bor(l, r)
                        }
                    }
                }
                Pure::FlagPack { .. } => unreachable!(),
            },
            _ => unreachable!(),
        };
        self.mask(result, width)
    }
}
impl Emitter<'_> {
    fn cold_target(
        &mut self,
        state: &StateMap,
        pc: V,
        address: V,
        status: u32,
    ) -> (cl::Block, Vec<cl::BlockArg>) {
        let dag = self
            .r
            .snapshots
            .iter()
            .position(|s| s.state == *state)
            .map(|n| self.plan.sites[n].dag.clone())
            .unwrap_or_else(|| recovery::Dag::build(self.r, state));
        let key = dag.key(self.r);
        let found = matches!(self.sharing, recovery::Sharing::Compatible)
            .then(|| self.cold.iter().position(|(_, d)| d.key(self.r) == key))
            .flatten();
        let block = if let Some(n) = found {
            self.cold[n].0
        } else {
            let block = self.b.create_block();
            self.b.set_cold_block(block);
            for _ in 0..dag.captures().len() + 4 {
                self.b.append_block_param(block, types::I64);
            }
            self.cold.push((block, dag.clone()));
            block
        };
        let mut args: Vec<cl::BlockArg> =
            dag.captures().iter().map(|&v| self.get(v).into()).collect();
        let status = self.c(u64::from(status));
        args.extend([pc, self.retired, address, status].map(cl::BlockArg::from));
        self.edges += 1;
        (block, args)
    }
    fn guard(&mut self, bad: V, state: &StateMap, pc: V, address: V, status: u32) {
        let (cold, args) = self.cold_target(state, pc, address, status);
        let good = self.b.create_block();
        self.b.ins().brif(bad, cold, &args, good, &[]);
        self.b.switch_to_block(good);
    }
    fn poll(&mut self, state: &StateMap, pc: V) {
        let zero = self.c(0);
        let stop = self.cmp(CC::Equal, pc, self.stop);
        self.guard(stop, state, pc, zero, STOP);
        let budget = self.cmp(CC::UnsignedGreaterThanOrEqual, self.retired, self.budget);
        self.guard(budget, state, pc, zero, BUDGET);
        // Atomic loads are observable effects, not readonly loop invariants.
        // Poll only at instruction boundaries, never after an uncommitted store.
        let interrupted = self
            .b
            .ins()
            .atomic_load(types::I8, MemFlags::trusted(), self.interrupt);
        self.guard(interrupted, state, pc, zero, INTERRUPTED);
        let revoked = self
            .b
            .ins()
            .atomic_load(types::I8, MemFlags::trusted(), self.revoke);
        self.guard(revoked, state, pc, zero, REVOKED);
    }
    fn target(&mut self, t: &Target) -> (cl::Block, Vec<cl::BlockArg>) {
        self.target_kind(t, COMPLETE)
    }
    fn target_kind(&mut self, t: &Target, complete_status: u32) -> (cl::Block, Vec<cl::BlockArg>) {
        match t {
            Target::Block(e) => {
                let mut args: Vec<_> = e.arguments.iter().map(|&v| self.get(v).into()).collect();
                args.push(self.retired.into());
                (self.blocks[e.block.0], args)
            }
            Target::Exit(e) => {
                if e.reason == ExitReason::Complete
                    && let ValueKind::Constant(pc) = self.r.values[e.pc.0].kind
                    && let Some(&slot) = self.links.and_then(|m| m.get(&(pc as u32)))
                {
                    let mut args: Vec<cl::BlockArg> =
                        e.state.gpr.iter().map(|&v| self.get(v).into()).collect();
                    args.push(self.get(e.state.flags).into());
                    args.push(self.retired.into());
                    let block = self.b.create_block();
                    for _ in 0..10 {
                        self.b.append_block_param(block, types::I64);
                    }
                    self.tails.push((block, slot, complete_status == CALL));
                    return (block, args);
                }
                let pc = self.get(e.pc);
                let address = self.c(0);
                let status = match e.reason {
                    ExitReason::Complete => complete_status,
                    ExitReason::Unsupported(_) => UNSUPPORTED,
                    ExitReason::Indirect => INDIRECT,
                    ExitReason::Guard => GUARD,
                };
                self.cold_target(&e.state, pc, address, status)
            }
        }
    }
    fn memory(&mut self, id: MemoryId) {
        let m = self.r.memory[id.0].clone();
        let snapshot = self.r.snapshots[m.snapshot.0].clone();
        let address = self.get(m.address);
        let pc = self.c(u64::from(self.r.commits[m.commit.0].pc));
        let width = (m.width.bits() / 8) as usize;
        let write = matches!(m.access, Access::Write { .. });
        let status = if write { WRITE_CHECK } else { READ_CHECK };
        // Extend before subtracting: underflow, address wrap, and the entire
        // access width are rejected before either page table or data accesses.
        let base = self.c(u64::from(self.base));
        let offset = self.b.ins().isub(address, base);
        if !self.proofs.iter().any(|p| p.memory == id) {
            let bound = self.c(self.len.saturating_sub(width) as u64);
            let mut bad = self.cmp(CC::UnsignedGreaterThan, offset, bound);
            let last = self.b.ins().iadd_imm(address, width as i64 - 1);
            let max = self.c(u64::from(u32::MAX));
            let wrap = self.cmp(CC::UnsignedGreaterThan, last, max);
            bad = self.b.ins().bor(bad, wrap);
            if self.len < width {
                bad = self.c(1);
            }
            self.guard(bad, &snapshot.state, pc, address, status);
            let last = self.b.ins().iadd_imm(offset, width as i64 - 1);
            for at in [offset, last] {
                let page = self.b.ins().ushr_imm(at, 12);
                let pointer = self.b.ins().iadd(self.perms, page);
                let p = self
                    .b
                    .ins()
                    .load(types::I8, MemFlags::trusted(), pointer, 0);
                let p = self.b.ins().uextend(types::I64, p);
                let required = if write { crate::WRITE } else { crate::READ };
                let allowed = self.b.ins().band_imm(p, i64::from(required));
                let bad = self
                    .b
                    .ins()
                    .icmp_imm(CC::NotEqual, allowed, i64::from(required));
                self.guard(bad, &snapshot.state, pc, address, status);
            }
        }
        let pointer = self.b.ins().iadd(self.data, offset);
        let ty = match m.width {
            Width::B8 => types::I8,
            Width::B16 => types::I16,
            _ => types::I32,
        };
        match m.access {
            Access::Read => {
                let v = self
                    .b
                    .ins()
                    .load(ty, MemFlags::new().with_notrap(), pointer, 0);
                let v = self.b.ins().uextend(types::I64, v);
                self.values[m.result.unwrap().0] = Some(v);
            }
            Access::Write { value, enabled } => {
                let v = self.get(value);
                let v = self.b.ins().ireduce(ty, v);
                if let Some(enabled) = enabled {
                    let yes = self.get(enabled);
                    let store = self.b.create_block();
                    let done = self.b.create_block();
                    self.b.ins().brif(yes, store, &[], done, &[]);
                    self.b.switch_to_block(store);
                    self.b
                        .ins()
                        .store(MemFlags::new().with_notrap(), v, pointer, 0);
                    self.b.ins().jump(done, &[]);
                    self.b.switch_to_block(done);
                } else {
                    self.b
                        .ins()
                        .store(MemFlags::new().with_notrap(), v, pointer, 0);
                }
            }
        }
        let old = self.get(m.input);
        self.values[m.output.0] = Some(self.b.ins().iadd_imm(old, 1));
    }
    fn cold_bodies(&mut self) {
        for (index, (block, dag)) in self.cold.clone().into_iter().enumerate() {
            self.b.switch_to_block(block);
            self.values.fill(None);
            let params = self.b.block_params(block).to_vec();
            let captures = dag.captures();
            for (&id, &v) in captures.iter().zip(&params) {
                self.values[id.0] = Some(v);
            }
            let n = captures.len();
            for (field, &root) in dag.roots.iter().enumerate() {
                let v = self.get(dag.nodes[root].value);
                let v = self.b.ins().ireduce(types::I32, v);
                self.b
                    .ins()
                    .store(MemFlags::trusted(), v, self.frame, field as i32 * 4);
            }
            for (v, offset, ty) in [
                (params[n], 36, types::I32),
                (params[n + 1], 40, types::I64),
                (params[n + 2], 48, types::I32),
                (params[n + 3], 52, types::I32),
            ] {
                let v = if ty == types::I32 {
                    self.b.ins().ireduce(ty, v)
                } else {
                    v
                };
                self.b
                    .ins()
                    .store(MemFlags::trusted(), v, self.frame, offset);
            }
            let status = self.b.ins().ireduce(types::I32, params[n + 3]);
            self.b.set_srcloc(cl::SourceLoc::new(index as u32 + 1));
            self.b.ins().return_(&[status]);
            self.b.set_srcloc(cl::SourceLoc::default());
        }
    }

    fn tail_bodies(&mut self) {
        let Some(invocation) = self.invocation else {
            return;
        };
        let sig = self.b.import_signature(tail_signature());
        for (block, slot, call) in self.tails.clone() {
            self.b.switch_to_block(block);
            let mut args = self.b.block_params(block).to_vec();
            args.push(invocation);
            let links = self
                .b
                .ins()
                .load(types::I64, MemFlags::trusted(), invocation, 56);
            let address =
                self.b
                    .ins()
                    .load(types::I64, MemFlags::trusted(), links, (slot * 8) as i32);
            let offset = std::mem::offset_of!(Frame, tail_entries) as i32;
            let count = self
                .b
                .ins()
                .load(types::I64, MemFlags::trusted(), self.frame, offset);
            let count = self.b.ins().iadd_imm(count, 1);
            self.b
                .ins()
                .store(MemFlags::trusted(), count, self.frame, offset);
            if call {
                let offset = std::mem::offset_of!(Frame, tail_calls) as i32;
                let count = self
                    .b
                    .ins()
                    .load(types::I64, MemFlags::trusted(), self.frame, offset);
                let count = self.b.ins().iadd_imm(count, 1);
                self.b
                    .ins()
                    .store(MemFlags::trusted(), count, self.frame, offset);
            }
            self.b.ins().return_call_indirect(sig, address, &args);
        }
    }
}
fn build<'a>(
    function: &'a mut cl::Function,
    context: &'a mut FunctionBuilderContext,
    r: &'a Region,
    plan: &'a recovery::Plan,
    base: u32,
    len: usize,
    sharing: recovery::Sharing,
    proofs: &'a [RangeProof],
    links: Option<&'a BTreeMap<u32, usize>>,
    entry_pc: u32,
) -> (usize, usize, usize, Vec<recovery::Dag>) {
    let mut b = FunctionBuilder::new(function, context);
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    let blocks: Vec<_> = r
        .blocks
        .iter()
        .map(|block| {
            let cl = b.create_block();
            for _ in 0..block.parameters.len() + 1 {
                b.append_block_param(cl, types::I64);
            }
            cl
        })
        .collect();
    b.switch_to_block(entry);
    let params = b.block_params(entry).to_vec();
    let invocation = links.map(|_| params[10]);
    let controls: Vec<_> = if let Some(invocation) = invocation {
        (0..7)
            .map(|n| {
                b.ins()
                    .load(types::I64, MemFlags::trusted(), invocation, n * 8)
            })
            .collect()
    } else {
        params.clone()
    };
    let [frame, data, perms, budget, interrupt, revoke, stop]: [V; 7] =
        controls.try_into().unwrap();
    let retired = if invocation.is_some() {
        params[9]
    } else {
        b.ins().load(types::I64, MemFlags::trusted(), frame, 40)
    };
    let mut e = Emitter {
        b,
        r,
        values: vec![None; r.values.len()],
        blocks,
        cold: vec![],
        sharing,
        plan,
        frame,
        data,
        perms,
        budget,
        interrupt,
        revoke,
        stop,
        proofs,
        retired,
        base,
        len,
        edges: 0,
        materialized: 0,
        invocation,
        links,
        tails: vec![],
    };
    let mut args = vec![];
    for n in 0..9 {
        let v = if invocation.is_some() {
            params[n as usize]
        } else {
            let v =
                e.b.ins()
                    .load(types::I32, MemFlags::trusted(), frame, n * 4);
            e.b.ins().uextend(types::I64, v)
        };
        args.push(v.into());
    }
    let zero = e.c(0);
    args.push(zero.into());
    args.push(retired.into());
    e.b.ins().jump(e.blocks[r.entry.0], &args);
    fn visit(r: &Region, b: BlockId, seen: &mut BTreeSet<BlockId>, post: &mut Vec<BlockId>) {
        if seen.insert(b) {
            for edge in r.blocks[b.0].terminator.edges() {
                visit(r, edge.block, seen, post);
            }
            post.push(b);
        }
    }
    let mut order = vec![];
    visit(r, r.entry, &mut BTreeSet::new(), &mut order);
    order.reverse();
    // Assign all phi identities first; non-parameter uses follow dominance.
    for &block in &order {
        for (&id, &value) in r.blocks[block.0]
            .parameters
            .iter()
            .zip(e.b.block_params(e.blocks[block.0]))
        {
            e.values[id.0] = Some(value);
        }
    }
    for block in order {
        e.b.switch_to_block(e.blocks[block.0]);
        e.retired = *e.b.block_params(e.blocks[block.0]).last().unwrap();
        let current = &r.blocks[block.0];
        if block == r.entry {
            let pc = if invocation.is_some() {
                e.c(u64::from(entry_pc))
            } else {
                let pc = e.b.ins().load(types::I32, MemFlags::trusted(), frame, 36);
                e.b.ins().uextend(types::I64, pc)
            };
            e.poll(&current.entry_state, pc);
        }
        for op in &current.operations {
            match op {
                Operation::Define(id) => {
                    if !matches!(
                        r.values[id.0].kind,
                        ValueKind::Flags(_) | ValueKind::Pure(Pure::FlagPack { .. })
                    ) {
                        let v = e.compute(&r.values[id.0].kind, r.values[id.0].width);
                        e.values[id.0] = Some(v);
                    }
                }
                Operation::Memory(id) => e.memory(*id),
                Operation::Commit {
                    point,
                    state,
                    next_pc,
                } => {
                    e.retired = e.b.ins().iadd_imm(e.retired, 1);
                    let pc = e.get(*next_pc);
                    if let Some(kind) = r.commits[point.0].indirect {
                        // This diagnostic checkpoint is independent of guest
                        // SSA state and is updated only at a committed transfer.
                        let source =
                            e.b.ins()
                                .iconst(types::I32, i64::from(r.commits[point.0].pc));
                        let target = e.b.ins().ireduce(types::I32, pc);
                        let kind = e.b.ins().iconst(types::I32, kind as i64);
                        for (value, offset) in [(source, 64), (target, 68), (kind, 72)] {
                            e.b.ins().store(MemFlags::trusted(), value, e.frame, offset);
                        }
                    }
                    e.poll(state, pc);
                }
            }
        }
        match &current.terminator {
            Terminator::Jump(t) => {
                let (b, a) = e.target(t);
                e.b.ins().jump(b, &a);
            }
            Terminator::Call { callee, .. } => {
                let (b, a) = e.target_kind(callee, CALL);
                e.b.ins().jump(b, &a);
            }
            Terminator::Exit(exit) => {
                let (b, a) = e.target(&Target::Exit(exit.clone()));
                e.b.ins().jump(b, &a);
            }
            Terminator::Branch {
                condition,
                taken,
                fallthrough,
            } => {
                let condition = e.get(*condition);
                let (t, ta) = e.target(taken);
                let (f, fa) = e.target(fallthrough);
                e.b.ins().brif(condition, t, &ta, f, &fa);
            }
            Terminator::Return {
                actual,
                expected,
                matched,
                mismatch,
            } => {
                let actual = e.get(*actual);
                let expected = e.c(u64::from(*expected));
                let good = e.cmp(CC::Equal, actual, expected);
                let (t, ta) = e.target(&Target::Block(matched.clone()));
                let (f, fa) = e.target(&Target::Exit(mismatch.clone()));
                e.b.ins().brif(good, t, &ta, f, &fa);
            }
            Terminator::Guard {
                condition,
                passed,
                failed,
                ..
            } => {
                let condition = e.get(*condition);
                let (t, ta) = e.target(&Target::Block(passed.clone()));
                let s = &r.snapshots[failed.0];
                let pc = e.c(u64::from(r.commits[s.commit.0].pc));
                let zero = e.c(0);
                let (f, fa) = e.cold_target(&s.state, pc, zero, GUARD);
                e.b.ins().brif(condition, t, &ta, f, &fa);
            }
        }
    }
    e.tail_bodies();
    e.cold_bodies();
    let stats = (
        e.edges,
        e.cold.len(),
        e.materialized,
        e.cold.iter().map(|(_, dag)| dag.clone()).collect(),
    );
    e.b.seal_all_blocks();
    e.b.finalize();
    stats
}

//! Diagnostic-only Cranelift lowering of verified GIR, with checked memory.
use super::{ir::*, recovery, verify};
use cranelift_codegen::ir::{
    self as cl, AbiParam, InstBuilder, MemFlags, Value as V, condcodes::IntCC as CC, types,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};
use std::collections::BTreeSet;

pub const COMPLETE: u32 = 0;
pub const BUDGET: u32 = 1;
pub const READ_CHECK: u32 = 2;
pub const WRITE_CHECK: u32 = 3;
pub const GUARD: u32 = 4;
pub const INDIRECT: u32 = 5;
pub const UNSUPPORTED: u32 = 6;
#[repr(C)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    pub gpr: [u32; 8],
    pub flags: u32,
    pub pc: u32,
    pub committed: u64,
    pub address: u32,
    pub status: u32,
}
impl Frame {
    pub fn from_cpu(c: &crate::CpuState) -> Self {
        Self {
            gpr: c.regs,
            flags: c.eflags,
            pc: c.eip,
            ..Self::default()
        }
    }
    pub fn restore(&self, c: &mut crate::CpuState) {
        c.regs = self.gpr;
        c.eflags = self.flags;
        c.eip = self.pc;
    }
}
pub struct Code {
    module: Option<JITModule>,
    entry: *const u8,
    base: u32,
    len: usize,
    pub machine: Vec<u8>,
    pub blocks: Vec<u32>,
    pub vcode: String,
    pub cross: Option<(Vec<u8>, Vec<u32>, String)>,
    pub cold_edges: usize,
    pub cold_blocks: usize,
    pub flags_materialized: usize,
    pub compile_us: u128,
}
impl Drop for Code {
    fn drop(&mut self) {
        unsafe { self.module.take().unwrap().free_memory() };
    }
}
impl Code {
    /// The exclusive serial borrow freezes mappings for this diagnostic call.
    /// Special/file/shared/code pages cannot enter ordinary machine accesses.
    pub fn run(
        &self,
        frame: &mut Frame,
        memory: &mut crate::Memory,
        budget: u64,
    ) -> Result<u32, String> {
        if memory.base != self.base
            || memory.len != self.len
            || memory.is_parallel()
            || memory.foreign_access_required()
        {
            return Err("diagnostic memory lease".into());
        }
        let permissions: Vec<u8> = (0..memory.len / 4096)
            .map(|p| {
                let pc = memory.base.wrapping_add((p * 4096) as u32);
                let perm = memory.perms[p];
                if !memory.mapped[p]
                    || perm & !(crate::READ | crate::WRITE) != 0
                    || memory.file_page_copy_on_write(pc).is_some()
                {
                    0
                } else {
                    perm
                }
            })
            .collect();
        let f: unsafe extern "C" fn(*mut Frame, *mut u8, *const u8, u64) -> u32 =
            unsafe { std::mem::transmute(self.entry) };
        // SAFETY: generated checks cover the full range and both endpoint
        // pages; width <= 4, mappings stay frozen, and no helpers can reenter.
        Ok(unsafe {
            f(
                frame,
                memory.ptr.as_ptr(),
                permissions.as_ptr(),
                frame.committed.saturating_add(budget),
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
    verify::verify(r).map_err(|e| e.to_string())?;
    let plan = recovery::Plan::build(r, sharing);
    plan.verify(r).map_err(|e| e.to_string())?;
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
        .extend([AbiParam::new(types::I64); 4]);
    context
        .func
        .signature
        .returns
        .push(AbiParam::new(types::I32));
    let id = module
        .declare_function("formal_gir", Linkage::Local, &context.func.signature)
        .map_err(|e| e.to_string())?;
    let mut fc = FunctionBuilderContext::new();
    let (cold_edges, cold_blocks, flags_materialized) =
        build(&mut context.func, &mut fc, r, &plan, base, len, sharing);
    let cross_function = cross.then(|| context.func.clone());
    context.set_disasm(true);
    module
        .define_function(id, &mut context)
        .map_err(|e| format!("{e:?}"))?;
    let code = context.compiled_code().unwrap();
    let (machine, blocks, vcode) = (
        code.code_buffer().to_vec(),
        code.bb_starts.clone(),
        code.vcode.clone().unwrap(),
    );
    module.finalize_definitions().map_err(|e| e.to_string())?;
    let entry = module.get_finalized_function(id);
    let compile_us = began.elapsed().as_micros();
    let cross = cross_function.and_then(cross_compile);
    Ok(Code {
        module: Some(module),
        entry,
        base,
        len,
        machine,
        blocks,
        vcode,
        cross,
        cold_edges,
        cold_blocks,
        flags_materialized,
        compile_us,
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
    retired: V,
    base: u32,
    len: usize,
    edges: usize,
    materialized: usize,
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
    fn target(&mut self, t: &Target) -> (cl::Block, Vec<cl::BlockArg>) {
        match t {
            Target::Block(e) => {
                let mut args: Vec<_> = e.arguments.iter().map(|&v| self.get(v).into()).collect();
                args.push(self.retired.into());
                (self.blocks[e.block.0], args)
            }
            Target::Exit(e) => {
                let pc = self.get(e.pc);
                let address = self.c(0);
                let status = match e.reason {
                    ExitReason::Complete => COMPLETE,
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
        for (block, dag) in self.cold.clone() {
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
            self.b.ins().return_(&[status]);
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
) -> (usize, usize, usize) {
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
    let [frame, data, perms, budget]: [V; 4] = params.try_into().unwrap();
    let retired = b.ins().load(types::I64, MemFlags::trusted(), frame, 40);
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
        retired,
        base,
        len,
        edges: 0,
        materialized: 0,
    };
    let mut args = vec![];
    for n in 0..9 {
        let v =
            e.b.ins()
                .load(types::I32, MemFlags::trusted(), frame, n * 4);
        let v = e.b.ins().uextend(types::I64, v);
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
            let pc = e.b.ins().load(types::I32, MemFlags::trusted(), frame, 36);
            let pc = e.b.ins().uextend(types::I64, pc);
            let bad = e.cmp(CC::UnsignedGreaterThanOrEqual, e.retired, e.budget);
            let zero = e.c(0);
            e.guard(bad, &current.entry_state, pc, zero, BUDGET);
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
                Operation::Commit { state, next_pc, .. } => {
                    e.retired = e.b.ins().iadd_imm(e.retired, 1);
                    let pc = e.get(*next_pc);
                    let bad = e.cmp(CC::UnsignedGreaterThanOrEqual, e.retired, e.budget);
                    let zero = e.c(0);
                    e.guard(bad, state, pc, zero, BUDGET);
                }
            }
        }
        match &current.terminator {
            Terminator::Jump(t) | Terminator::Call { callee: t, .. } => {
                let (b, a) = e.target(t);
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
    e.cold_bodies();
    let stats = (e.edges, e.cold.len(), e.materialized);
    e.b.seal_all_blocks();
    e.b.finalize();
    stats
}

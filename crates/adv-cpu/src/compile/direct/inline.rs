//! Checkpointed scalar accesses with exact-PC metadata owned by the code cache.
use super::*;
use crate::direct_memory::Engine;
use crate::fault_recovery::inline::{Registration, Site};

const ACCESS_SOURCE: u32 = 0x4000_0000;
const RETURN_SOURCE: u32 = 9;

pub(in crate::compile) struct Inline {
    recovery: Option<ir::Block>,
    sites: Vec<Draft>,
}

pub(in crate::compile) struct Draft {
    source: u32,
    guest_pc: u32,
    next_pc: u32,
    width: u8,
    write: bool,
}

impl Inline {
    pub(in crate::compile) fn new() -> Self {
        Self {
            recovery: None,
            sites: Vec::new(),
        }
    }
}

impl Emitter<'_> {
    fn engine_field(&mut self, engine: Value, offset: usize) -> Value {
        self.b.ins().load(
            self.ptr_ty,
            MemFlags::trusted().with_readonly(),
            engine,
            offset as i32,
        )
    }

    fn direct_count(&mut self, engine: Value, offset: usize) {
        let count = self
            .b
            .ins()
            .load(types::I64, MemFlags::trusted(), engine, offset as i32);
        let count = self.b.ins().iadd_imm(count, 1);
        self.b
            .ins()
            .store(MemFlags::trusted(), count, engine, offset as i32);
    }

    pub(in crate::compile) fn direct_mov(&mut self, i: &Instruction) {
        if self.inline.is_none() {
            self.legacy_direct_mov(i);
            return;
        }
        let engine = self.b.ins().load(
            self.ptr_ty,
            MemFlags::trusted().with_readonly(),
            self.context,
            offset_of!(NativeContext, direct) as i32,
        );
        self.inline_mov(i, engine);
    }

    fn inline_mov(&mut self, i: &Instruction, engine: Value) {
        let write = i.op0_kind() == OpKind::Memory;
        let width = i.memory_size().size();
        let ty = integer_type(width);
        let address = self.address(i);
        let value = write.then(|| self.read(i, 1));
        let window = self.engine_field(engine, offset_of!(Engine, window));
        let host_page = self.engine_field(engine, offset_of!(Engine, host_page_size));
        let checked = self.b.create_block();
        let shape = self.b.create_block();
        let ordinary = self.b.create_block();
        let done = self.b.create_block();
        if !write {
            self.b.append_block_param(done, types::I32);
        }
        self.b.set_cold_block(checked);

        // These are shape/provenance guards, never guest READ/WRITE checks.
        // A complete address space needs no linear bounds check. The smaller
        // synthetic Memory windows retain a guard before indexing metadata.
        let offset = self.b.ins().iadd_imm(address, -i64::from(self.base));
        let wide = self.b.ins().uextend(self.ptr_ty, offset);
        let host4k = self.b.ins().icmp_imm(IntCC::Equal, host_page, 4096);
        let in_range = if self.base == 0 && self.len == 1usize << 32 {
            host4k
        } else {
            let fits = self.b.ins().icmp_imm(
                IntCC::UnsignedLessThanOrEqual,
                wide,
                self.len.saturating_sub(width) as i64,
            );
            self.b.ins().band(host4k, fits)
        };
        self.b.ins().brif(in_range, shape, &[], checked, &[]);
        self.b.switch_to_block(shape);
        let page = self.b.ins().ushr_imm(wide, 12);
        let permission = self.b.ins().iadd(self.perms, page);
        let permission = self.b.ins().load(
            types::I8,
            MemFlags::trusted().with_readonly(),
            permission,
            0,
        );
        let shared = self
            .b
            .ins()
            .band_imm(permission, i64::from(crate::memory::SHARED));
        let private = self.b.ins().icmp_imm(IntCC::Equal, shared, 0);
        let within_page = self.b.ins().band_imm(address, 4095);
        let within_page = self.b.ins().icmp_imm(
            IntCC::UnsignedLessThanOrEqual,
            within_page,
            (4096 - width) as i64,
        );
        let mut eligible = self.b.ins().band(private, within_page);
        if write && width > 1 {
            let alignment = self.b.ins().band_imm(address, (width - 1) as i64);
            let aligned = self.b.ins().icmp_imm(IntCC::Equal, alignment, 0);
            eligible = self.b.ins().band(eligible, aligned);
        }
        self.b.ins().brif(eligible, ordinary, &[], checked, &[]);

        self.b.switch_to_block(checked);
        self.direct_count(engine, offset_of!(Engine, checked));
        if let Some(value) = value {
            let value = if width == 4 {
                value
            } else {
                self.b.ins().ireduce(ty, value)
            };
            self.direct_store(address, value);
            self.b.ins().jump(done, &[]);
        } else {
            let value = self.read(i, 1);
            self.b.ins().jump(done, &[value.into()]);
        }

        self.b.switch_to_block(ordinary);
        let pc = self.constant(i.ip32());
        self.save_state(pc);
        self.b.ins().store(
            MemFlags::trusted(),
            address,
            self.state,
            offset_of!(CpuState, fault_addr) as i32,
        );
        let recovery = match self.inline.as_ref().unwrap().recovery {
            Some(block) => block,
            None => {
                let block = self.b.create_block();
                self.b.set_cold_block(block);
                self.inline.as_mut().unwrap().recovery = Some(block);
                block
            }
        };
        let access = self.b.create_block();
        // Keep an ordinary CFG edge to the same-frame epilogue. A live direct
        // engine always owns a non-null reservation. The signal uses this edge
        // only after the complete architectural prefix has been checkpointed.
        self.b.ins().brif(window, access, &[], recovery, &[]);
        self.b.switch_to_block(access);
        self.direct_count(engine, offset_of!(Engine, accesses));
        let pointer = self.b.ins().iadd(window, wide);
        let value = value.map(|value| {
            if width == 4 {
                value
            } else {
                self.b.ins().ireduce(ty, value)
            }
        });
        let draft = self.inline.as_mut().unwrap();
        let source = ACCESS_SOURCE + draft.sites.len() as u32;
        draft.sites.push(Draft {
            source,
            guest_pc: i.ip32(),
            next_pc: i.next_ip32(),
            width: width as u8,
            write,
        });
        self.b.set_srcloc(ir::SourceLoc::new(source));
        let flags = MemFlags::new().with_trap_code(Some(ir::TrapCode::user(2).unwrap()));
        let result = if let Some(value) = value {
            self.b.ins().store(flags, value, pointer, 0);
            None
        } else {
            Some(self.b.ins().load(ty, flags, pointer, 0))
        };
        self.b.set_srcloc(self.source);
        // A successful access has no pending guest fault. This is also needed
        // by strict whole-state comparison at an ordinary region boundary.
        let zero = self.constant(0);
        self.b.ins().store(
            MemFlags::trusted(),
            zero,
            self.state,
            offset_of!(CpuState, fault_addr) as i32,
        );
        if let Some(result) = result {
            let result = if width == 4 {
                result
            } else {
                self.b.ins().uextend(types::I32, result)
            };
            self.b.ins().jump(done, &[result.into()]);
        } else {
            self.b.ins().jump(done, &[]);
        }
        self.b.switch_to_block(done);
        self.b.seal_block(done);
        if !write {
            let result = self.b.block_params(done)[0];
            self.write(i, None, result);
        }
    }

    pub(in crate::compile) fn finish_inline(&mut self) -> Option<Vec<Draft>> {
        let inline = self.inline.take()?;
        if let Some(recovery) = inline.recovery {
            self.b.switch_to_block(recovery);
            self.b.set_srcloc(ir::SourceLoc::new(RETURN_SOURCE));
            let reason = self.constant(Exit::Fallback as u32);
            self.b.ins().return_(&[reason]);
            self.b.set_srcloc(ir::SourceLoc::default());
        }
        Some(inline.sites)
    }
}

pub(in crate::compile) fn publish(
    code: &cranelift_codegen::CompiledCode,
    base: usize,
    drafts: &[Draft],
) -> Result<Option<Registration>, String> {
    if drafts.is_empty() {
        return Ok(None);
    }
    let bytes = code.code_buffer();
    let ranges = code.buffer.get_srclocs_sorted();
    let start = ranges
        .iter()
        .filter(|range| range.loc.bits() == RETURN_SOURCE)
        .map(|range| range.start as usize)
        .min()
        .ok_or("inline scalar recovery source is missing")?;
    let recovery = recovery_start(bytes, start)
        .filter(|&offset| epilogue(bytes, offset))
        .ok_or("inline scalar recovery epilogue is not supported")?;
    let base = untag(base);
    let mut sites = Vec::new();
    for trap in code
        .buffer
        .traps()
        .iter()
        .filter(|trap| trap.code == ir::TrapCode::user(2).unwrap())
    {
        let draft = drafts
            .iter()
            .find(|draft| {
                ranges.iter().any(|range| {
                    range.loc.bits() == draft.source
                        && range.start <= trap.offset
                        && trap.offset < range.end
                })
            })
            .ok_or("inline scalar fault PC has no checkpoint")?;
        if !scalar(bytes, trap.offset as usize, draft.width, draft.write) {
            return Err("inline scalar machine access is not supported".into());
        }
        sites.push(Site {
            host_pc: base + trap.offset as usize,
            recovery_pc: base + recovery,
            guest_pc: draft.guest_pc,
            next_pc: draft.next_pc,
            width: draft.width,
            write: draft.write,
        });
    }
    if sites.len() != drafts.len() {
        return Err("inline scalar fault table is incomplete".into());
    }
    sites.sort_by_key(|site| site.host_pc);
    // SAFETY: exact scalar instructions and a frame-only epilogue were checked;
    // generated guards/checkpoints prove the architectural prefix and commit
    // shape. Compiled drops the registration before freeing executable memory.
    unsafe { Registration::publish(base, bytes.len(), sites).map(Some) }
}

fn untag(pointer: usize) -> usize {
    if cfg!(all(target_os = "android", target_arch = "aarch64")) {
        pointer & 0x00ff_ffff_ffff_ffff
    } else {
        pointer
    }
}

fn scalar(code: &[u8], offset: usize, width: u8, write: bool) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        let i = Decoder::new(64, &code[offset..], DecoderOptions::NONE).decode();
        matches!(i.mnemonic(), M::Mov | M::Movzx)
            && i.op_kind(if write { 0 } else { 1 }) == OpKind::Memory
            && i.memory_size().size() == usize::from(width)
            && !i.has_lock_prefix()
    }
    #[cfg(target_arch = "aarch64")]
    {
        let Some(bytes) = code.get(offset..offset + 4) else {
            return false;
        };
        if offset % 4 != 0 {
            return false;
        }
        let word = u32::from_le_bytes(bytes.try_into().unwrap());
        let size = match width {
            1 => 0,
            2 => 1,
            4 => 2,
            _ => return false,
        };
        let common = size << 30 | if write { 0 } else { 1 << 22 };
        // Plain scalar unsigned-offset or register-offset LDR/STR. Exclude
        // SIMD, pairs, atomics, sign extension, writeback and shifted indexes.
        word & 0xffc00000 == common | 0x39000000 || word & 0xffe00c00 == common | 0x38200800
    }
}

/// Recovery changes the return register before entering this suffix. Admit
/// only stack restoration and RET; no instruction may consume a live SSA value
/// or overwrite the integer return register. Branch sharing remains bounded.
fn epilogue(code: &[u8], mut offset: usize) -> bool {
    #[cfg(target_arch = "aarch64")]
    {
        if offset % 4 != 0 {
            return false;
        }
        for _ in 0..64 {
            let Some(bytes) = code.get(offset..offset + 4) else {
                return false;
            };
            let w = u32::from_le_bytes(bytes.try_into().unwrap());
            if w == 0xd65f03c0 {
                return true;
            }
            // ADD SP,SP,#imm; integer/vector LDP from SP, post-index or offset;
            // scalar stack reloads (including D8-D15), never X0/W0.
            let add_sp = w & 0xff8003ff == 0x910003ff;
            let ldp = (w & 0xffc003e0 == 0xa8c003e0 || w & 0xffc003e0 == 0xa94003e0)
                && w & 31 != 0
                && (w >> 10) & 31 != 0;
            let fp_ldp = w & 0xffc003e0 == 0x6cc003e0 || w & 0xffc003e0 == 0x6d4003e0;
            let ldr = w & 0xffc003e0 == 0xf94003e0 && w & 31 != 0;
            let fp_ldr = w & 0xffc003e0 == 0xfd4003e0;
            if w & 0xfc000000 == 0x14000000 {
                let delta = (((w & 0x03ff_ffff) << 6) as i32 >> 4) as isize;
                let Some(target) = offset.checked_add_signed(delta) else {
                    return false;
                };
                offset = target;
                continue;
            }
            if !(add_sp || ldp || fp_ldp || ldr || fp_ldr) {
                return false;
            }
            offset += 4;
        }
        false
    }
    #[cfg(target_arch = "x86_64")]
    {
        for _ in 0..64 {
            let Some(bytes) = code.get(offset..) else {
                return false;
            };
            let i = Decoder::with_ip(64, bytes, offset as u64, DecoderOptions::NONE).decode();
            if i.is_invalid() {
                return false;
            }
            match i.mnemonic() {
                M::Ret => return true,
                M::Mov if i.op0_register() == R::RSP && i.op1_register() == R::RBP => {}
                M::Mov
                | M::Movups
                | M::Movaps
                | M::Movdqu
                | M::Movdqa
                | M::Vmovups
                | M::Vmovaps
                | M::Vmovdqu
                | M::Vmovdqa
                    if i.op0_kind() == OpKind::Register
                        && i.op1_kind() == OpKind::Memory
                        && i.memory_base() == R::RSP
                        && i.memory_index() == R::None
                        && matches!(
                            i.op0_register(),
                            R::RBX | R::RBP | R::RSI | R::RDI | R::R12 | R::R13 | R::R14 | R::R15
                        ) => {}
                M::Add
                    if i.op0_register() == R::RSP
                        && matches!(
                            i.op1_kind(),
                            OpKind::Immediate8to64 | OpKind::Immediate32to64
                        ) => {}
                M::Pop
                    if matches!(
                        i.op0_register(),
                        R::RBP | R::RBX | R::R12 | R::R13 | R::R14 | R::R15
                    ) => {}
                M::Jmp if i.op0_kind() == OpKind::NearBranch64 => {
                    offset = i.near_branch64() as usize;
                    continue;
                }
                _ => return false,
            }
            offset = i.next_ip() as usize;
        }
        false
    }
}

// Source locations cover the complete lowered return, including its ABI
// moves. Skip only pure return-value setup before validating the WHOLE frame
// restoration suffix. Searching for a later RET would lose callee saves.
fn recovery_start(code: &[u8], mut offset: usize) -> Option<usize> {
    for _ in 0..8 {
        if epilogue(code, offset) {
            return Some(offset);
        }
        #[cfg(target_arch = "aarch64")]
        {
            let w = u32::from_le_bytes(code.get(offset..offset + 4)?.try_into().ok()?);
            let mov = w & 0x7fe0ffe0 == 0x2a0003e0; // ORR Rd,ZR,Rm
            let imm = w & 0x7f800000 == 0x52800000; // MOVZ
            if !(mov || imm) || w & 31 == 31 {
                return None;
            }
            offset += 4;
        }
        #[cfg(target_arch = "x86_64")]
        {
            let i = Decoder::new(64, code.get(offset..)?, DecoderOptions::NONE).decode();
            if i.mnemonic() != M::Mov
                || !matches!(i.op0_register(), R::EAX | R::RAX)
                || !matches!(
                    i.op1_kind(),
                    OpKind::Register
                        | OpKind::Immediate32
                        | OpKind::Immediate32to64
                        | OpKind::Immediate64
                )
            {
                return None;
            }
            offset += i.len();
        }
    }
    None
}

#[cfg(all(test, target_arch = "aarch64"))]
mod tests {
    use super::*;

    fn bytes(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|word| word.to_le_bytes()).collect()
    }

    #[test]
    fn accepts_only_plain_scalar_machine_accesses() {
        for (width, opcode) in [(1, 0x39000000), (2, 0x79000000), (4, 0xb9000000)] {
            for write in [false, true] {
                let direction = if write { 0 } else { 0x00400000 };
                assert!(scalar(
                    &bytes(&[opcode | direction | 0x20]),
                    0,
                    width,
                    write
                ));
                assert!(!scalar(
                    &bytes(&[opcode | direction | 0x20]),
                    0,
                    width,
                    !write
                ));
            }
        }
        // Pairs, vector stores, atomics and post-index stores are not admitted.
        for opcode in [0xa9000440, 0x3d800020, 0xb8200020, 0xb8004420] {
            assert!(!scalar(&bytes(&[opcode]), 0, 4, true));
        }
    }

    #[test]
    fn recovery_never_skips_an_unrecognized_frame_operation() {
        let valid = bytes(&[0x52800080, 0xa8c17bfd, 0xd65f03c0]);
        assert_eq!(recovery_start(&valid, 0), Some(4));
        assert!(epilogue(&valid, 4));
        // Unknown frame mutation must not be bypassed by finding a later RET.
        let invalid = bytes(&[0x52800080, 0x910003bf, 0xd65f03c0]);
        assert_eq!(recovery_start(&invalid, 0), None);
        // Restoring X0 would overwrite the result set by the signal adapter.
        assert!(!epilogue(&bytes(&[0xf94003e0, 0xd65f03c0]), 0));
    }
}

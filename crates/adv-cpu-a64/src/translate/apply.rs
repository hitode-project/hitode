//! Operation helpers (A64-TR-4): integer instructions without native forms
//! run the interpreter's own semantics on operands in host registers. The
//! unit reads the operands, memory through its usual access paths, calls
//! `run` through the `apply` stub, which keeps guest registers, temporaries
//! and SIMD caches, then writes the result and, after that last access, the
//! flags (A64-FLAG-4).

use adv_cpu_core::integer;
use iced_x86::{Instruction, Mnemonic as M, OpKind, Register};

use crate::encode::Size::{W, X};
use crate::encode::*;

use super::decode::{Part, memory};
use super::{State, Translator, X0, X1, X2, X3, X6, X10, X11, X13, X14, host};

const ECX: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Semantic {
    Shl,
    Shr,
    Sar,
    Rol,
    Ror,
    Rcl,
    Rcr,
    Shld,
    Shrd,
}

impl Semantic {
    const ALL: [Self; 9] = [
        Self::Shl,
        Self::Shr,
        Self::Sar,
        Self::Rol,
        Self::Ror,
        Self::Rcl,
        Self::Rcr,
        Self::Shld,
        Self::Shrd,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Count {
    /// 1 to 31.
    Imm(u8),
    Cl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Apply {
    pub semantic: Semantic,
    /// 1, 2 or 4 bytes.
    pub width: u8,
    pub dst: Part,
    /// The register a double shift fills from.
    pub source: Option<u8>,
    pub count: Count,
}

/// The operation helper running `instruction`, if one does.
pub fn plan(instruction: &Instruction) -> Option<Apply> {
    let semantic = match instruction.mnemonic() {
        M::Shl | M::Sal => Semantic::Shl,
        M::Shr => Semantic::Shr,
        M::Sar => Semantic::Sar,
        M::Rol => Semantic::Rol,
        M::Ror => Semantic::Ror,
        M::Rcl => Semantic::Rcl,
        M::Rcr => Semantic::Rcr,
        M::Shld => Semantic::Shld,
        M::Shrd => Semantic::Shrd,
        _ => return None,
    };
    let gpr = |register: Register| {
        (register.is_gpr8() || register.is_gpr16() || register.is_gpr32())
            .then(|| (register.number() as u8, register.size() as u8))
    };
    let (dst, width) = match instruction.op0_kind() {
        OpKind::Register => {
            let (number, width) = gpr(instruction.op0_register())?;
            (Part::Reg(number), width)
        }
        OpKind::Memory => (
            Part::Mem(memory(instruction)?),
            instruction.memory_size().size() as u8,
        ),
        _ => return None,
    };
    let double = matches!(semantic, Semantic::Shld | Semantic::Shrd);
    let source = if double {
        let (number, size) = gpr(instruction.op1_register())?;
        if instruction.op1_kind() != OpKind::Register || size != width {
            return None;
        }
        Some(number)
    } else {
        None
    };
    let at = if double { 2 } else { 1 };
    let count = match instruction.op_kind(at) {
        // A zero count changes nothing but still checks the write: the
        // interpreter keeps that.
        OpKind::Immediate8 => match instruction.immediate8() & 31 {
            0 => return None,
            count => Count::Imm(count),
        },
        OpKind::Register if instruction.op_register(at) == Register::CL => Count::Cl,
        _ => return None,
    };
    (matches!(width, 1 | 2 | 4) && !(double && width == 1)).then_some(Apply {
        semantic,
        width,
        dst,
        source,
        count,
    })
}

impl Translator<'_> {
    pub(super) fn apply(&mut self, op: Apply) {
        self.canonicalize();
        let address = match op.dst {
            Part::Mem(mem) => self.effective(mem),
            _ => Reg::ZR,
        };
        self.narrow_read(op.dst, op.width, address, X6);
        if let Some(source) = op.source {
            self.narrow_read(Part::Reg(source), op.width, Reg::ZR, X2);
        }
        match op.count {
            Count::Imm(count) => self.mov_imm(X3, u32::from(count)),
            Count::Cl => self.emit(and_imm(W, X3, host(ECX), 31)),
        }
        self.emit(mov(W, X1, X6));
        self.mov_imm(X0, op.semantic as u32 | u32::from(op.width) << 8);
        self.far_call(self.stubs.apply);
        self.emit_all(&[mov(W, X6, X0), lsr_imm(X, X14, X0, 32)]);
        self.narrow_write(op.dst, op.width, address, X6);
        // The packed flags: NZCV in bits 0-3, AF in bit 4, x10 in bit 5.
        self.emit_all(&[
            lsl_imm(W, X13, X14, 28),
            msr_nzcv(X13),
            ubfx(W, X10, X14, 5, 1),
            and_imm(W, X11, X14, 0x10),
        ]);
        self.state = State::CANONICAL;
    }
}

/// Called by the `apply` stub: operation `op` (its `Semantic` in bits 0-7
/// and width above) of `value`, `source` and `count`, with the canonical
/// flags NZCV (subtraction sense), x10 and x11. Returns the result in the
/// low half and the flags packed as `Translator::apply` unpacks them.
pub(crate) extern "C" fn run(
    op: u32,
    value: u32,
    source: u32,
    count: u32,
    nzcv: u64,
    pf: u64,
    af: u64,
) -> u64 {
    let n = |bit: u32| nzcv >> bit & 1 != 0;
    let mut flags = (u32::from(!n(29)) * integer::CF)
        | (u32::from((pf as u8).count_ones().is_multiple_of(2)) * integer::PF)
        | (af as u32 & integer::AF)
        | (u32::from(n(30)) * integer::ZF)
        | (u32::from(n(31)) * integer::SF)
        | (u32::from(n(28)) * integer::OF);
    let semantic = Semantic::ALL[(op & 0xff) as usize];
    let width = (op >> 8) as u8;
    // A zero CL count leaves the value and flags as they were.
    let count = count & 31;
    let result = if count == 0 {
        value
    } else {
        match semantic {
            Semantic::Shld | Semantic::Shrd => integer::double_shift(
                &mut flags,
                semantic == Semantic::Shld,
                value,
                source,
                count,
                width,
            ),
            _ => {
                let kind = match semantic {
                    Semantic::Shl => M::Shl,
                    Semantic::Shr => M::Shr,
                    Semantic::Sar => M::Sar,
                    Semantic::Rol => M::Rol,
                    Semantic::Ror => M::Ror,
                    Semantic::Rcl => M::Rcl,
                    _ => M::Rcr,
                };
                integer::shift(&mut flags, kind, value, count, width)
            }
        }
    };
    let has = |bit: u32| u64::from(flags & bit != 0);
    let packed = has(integer::OF)
        | (1 - has(integer::CF)) << 1
        | has(integer::ZF) << 2
        | has(integer::SF) << 3
        | has(integer::AF) << 4
        | (1 - has(integer::PF)) << 5;
    u64::from(result) | packed << 32
}

#[cfg(test)]
mod tests {
    use super::super::decode::{Fetcher, Op};
    use iced_x86::code_asm::*;

    /// The ops the forms below decode to, one per instruction.
    fn decode(build: impl Fn(&mut CodeAssembler)) -> Vec<Op> {
        let mut a = CodeAssembler::new(32).unwrap();
        build(&mut a);
        let code = a.assemble(0x10000).unwrap();
        let mut memory = adv_cpu_core::Memory::new(0x10000, 4096).unwrap();
        memory.protect(0x10000, 4096, 7).unwrap();
        memory.write(0x10000, &code).unwrap();
        let mut fetcher = Fetcher::new(&memory);
        fetcher.helpers = Some(Vec::new());
        let (mut pc, mut ops) = (0x10000, Vec::new());
        while pc < 0x10000 + code.len() as u32 {
            let (op, len) = fetcher.fetch(pc).unwrap();
            ops.push(op);
            pc += len;
        }
        ops
    }

    // spec: A64-TR-4
    #[test]
    fn shifts_without_native_forms_run_as_operation_helpers() {
        let ops = decode(|a| {
            a.shr(dword_ptr(ebx + 8), 1).unwrap();
            a.sar(dword_ptr(esi), 5).unwrap();
            a.rol(word_ptr(ebx), 3).unwrap();
            a.rcl(byte_ptr(edi), cl).unwrap();
            a.ror(dh, cl).unwrap();
            a.rcr(cx, 1).unwrap();
            a.shrd(ebx, edx, cl).unwrap();
            a.shld(eax, esi, 7).unwrap();
            a.shrd(word_ptr(edi), ax, 3).unwrap();
            a.shld(dword_ptr(ebx), ecx, cl).unwrap();
        });
        assert_eq!(ops.len(), 10);
        for op in ops {
            assert!(matches!(op, Op::Apply(_)), "{op:?}");
        }
        // A zero count still checks the write, which the interpreter does.
        let zero = decode(|a| a.shl(dword_ptr(ebx), 32).unwrap());
        assert!(matches!(zero[..], [Op::Helper(_)]), "{zero:?}");
    }
}

//! Bounded ARM64 templates for the versioned fixed-register entry ABI.
//! These encodings deliberately cover only the declared integer subset; all
//! other forms leave before the instruction and use the existing interpreter.
use super::{BUDGET, DISPATCH, FALLBACK, LOAD, STORE};
use iced_x86::{Code, ConditionCode as C, Instruction, Mnemonic as M, OpKind, Register};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy)]
enum Label {
    Guest(u32),
    Exit(u32),
}
#[derive(Clone, Copy)]
enum Branch {
    B,
    Equal,
    Zero64(u8),
    Nonzero32(u8),
}
struct Fixup {
    at: usize,
    label: Label,
    kind: Branch,
}

/// Host addresses of the checked scalar access helpers. Each takes the entry
/// frame in x0 and follows AAPCS64, so guest registers in x19-x28 survive.
#[derive(Clone, Copy)]
pub(super) struct Helpers {
    /// `(frame, address, kind) -> value | status << 32`, where kind is the
    /// width in bytes plus `SIGNED`; nonzero status means nothing was read.
    pub load: u64,
    /// `(frame, address, value, kind) -> 0` to continue, 1 after a store that
    /// must leave native code, 2 when refused. Kind is width plus CHECK_WRITE
    /// for permission-only probes, which return 0 without writing.
    pub store: u64,
    /// `(frame, pop) -> store status`; stages all GPR effects in the frame.
    pub stack_all: u64,
}

/// Load kind flag: sign-extend the loaded value to 32 bits.
pub(super) const SIGNED: u32 = 0x100;
/// Store kind flag: validate write permission without writing (zero shifts).
pub(super) const CHECK_WRITE: u32 = 0x100;

pub(super) struct Emitter {
    helpers: Helpers,
    pub words: Vec<u32>,
    pub entries: BTreeMap<u32, usize>,
    exits: BTreeMap<u32, usize>,
    fixups: Vec<Fixup>,
}

pub(super) fn register(r: Register) -> Option<u8> {
    use Register::*;
    Some(match r {
        EAX => 19,
        ECX => 20,
        EDX => 21,
        EBX => 22,
        ESP => 23,
        EBP => 24,
        ESI => 25,
        EDI => 26,
        _ => return Option::None,
    })
}
fn register_part(r: Register) -> Option<(u8, u32, u32)> {
    let full = register(r.full_register32())?;
    let shift = u32::from(matches!(
        r,
        Register::AH | Register::CH | Register::DH | Register::BH
    )) * 8;
    let bits = r.size() as u32 * 8;
    matches!(bits, 8 | 16 | 32).then_some((full, shift, bits))
}
fn setcc(m: M) -> bool {
    matches!(
        m,
        M::Seto
            | M::Setno
            | M::Setb
            | M::Setae
            | M::Sete
            | M::Setne
            | M::Setbe
            | M::Seta
            | M::Sets
            | M::Setns
            | M::Setp
            | M::Setnp
            | M::Setl
            | M::Setge
            | M::Setle
            | M::Setg
    )
}
fn cmovcc(m: M) -> bool {
    matches!(
        m,
        M::Cmovo
            | M::Cmovno
            | M::Cmovb
            | M::Cmovae
            | M::Cmove
            | M::Cmovne
            | M::Cmovbe
            | M::Cmova
            | M::Cmovs
            | M::Cmovns
            | M::Cmovp
            | M::Cmovnp
            | M::Cmovl
            | M::Cmovge
            | M::Cmovle
            | M::Cmovg
    )
}
fn shift(m: M) -> bool {
    matches!(m, M::Shl | M::Sal | M::Shr | M::Sar | M::Rol | M::Ror)
}
fn double_shift(i: &Instruction) -> bool {
    matches!(i.mnemonic(), M::Shld | M::Shrd)
        && i.op1_kind() == OpKind::Register
        && register_part(i.op1_register()).is_some_and(|(_, _, bits)| matches!(bits, 16 | 32))
        && (i.op2_kind() == OpKind::Immediate8
            || i.op2_kind() == OpKind::Register && i.op2_register() == Register::CL)
}
fn shift_count(i: &Instruction) -> bool {
    i.op1_kind() == OpKind::Immediate8
        || i.op1_kind() == OpKind::Register && i.op1_register() == Register::CL
}
fn imm(i: &Instruction, op: u32) -> Option<u32> {
    matches!(i.op_kind(op), OpKind::Immediate32 | OpKind::Immediate8to32)
        .then(|| i.immediate(op) as u32)
}
fn source(i: &Instruction, op: u32) -> bool {
    i.op_kind(op) == OpKind::Register && register(i.op_register(op)).is_some()
        || imm(i, op).is_some()
}
fn scalar32(i: &Instruction, op: u32) -> bool {
    i.op_kind(op) == OpKind::Register && register(i.op_register(op)).is_some()
        || i.op_kind(op) == OpKind::Memory && i.memory_size().size() == 4 && address(i)
}
fn rmw_source(i: &Instruction) -> bool {
    i.op1_kind() == OpKind::Register && register(i.op1_register().full_register32()).is_some()
        || matches!(
            i.op1_kind(),
            OpKind::Immediate8
                | OpKind::Immediate16
                | OpKind::Immediate32
                | OpKind::Immediate8to16
                | OpKind::Immediate8to32
        )
}
fn rmw(i: &Instruction) -> bool {
    i.op0_kind() == OpKind::Memory
        && matches!(i.memory_size().size(), 1 | 2 | 4)
        && address(i)
        && match i.mnemonic() {
            M::Add | M::Adc | M::Sub | M::Sbb | M::And | M::Or | M::Xor => rmw_source(i),
            M::Inc | M::Dec | M::Not | M::Neg => true,
            m if shift(m) => shift_count(i),
            M::Shld | M::Shrd => i.memory_size().size() >= 2 && double_shift(i),
            _ => false,
        }
}
fn address(i: &Instruction) -> bool {
    // Word addressing has different wrapping and remains in Tier 0.
    (i.memory_base() == Register::None || register(i.memory_base()).is_some())
        && (i.memory_index() == Register::None || register(i.memory_index()).is_some())
        && i.memory_displ_size() != 2
        && i.segment_prefix() != Register::GS
}

/// (width, reads ESI, writes EDI). Match codes rather than the SIMD MOVSD name.
fn string(i: &Instruction) -> Option<(u32, bool, bool)> {
    let form = match i.code() {
        Code::Movsb_m8_m8 => (1, true, true),
        Code::Movsw_m16_m16 => (2, true, true),
        Code::Movsd_m32_m32 => (4, true, true),
        Code::Stosb_m8_AL => (1, false, true),
        Code::Stosw_m16_AX => (2, false, true),
        Code::Stosd_m32_EAX => (4, false, true),
        Code::Lodsb_AL_m8 => (1, true, false),
        Code::Lodsw_AX_m16 => (2, true, false),
        Code::Lodsd_EAX_m32 => (4, true, false),
        _ => return None,
    };
    ((0..i.op_count()).all(|n| {
        matches!(
            i.op_kind(n),
            OpKind::Register | OpKind::MemorySegESI | OpKind::MemoryESEDI
        )
    }) && i.segment_prefix() != Register::GS)
        .then_some(form)
}

pub(super) fn counter_branch(i: &Instruction) -> Option<bool> {
    Some(match i.code() {
        Code::Loop_rel8_16_CX
        | Code::Loop_rel8_32_CX
        | Code::Loope_rel8_16_CX
        | Code::Loope_rel8_32_CX
        | Code::Loopne_rel8_16_CX
        | Code::Loopne_rel8_32_CX
        | Code::Jcxz_rel8_16
        | Code::Jcxz_rel8_32 => true,
        Code::Loop_rel8_16_ECX
        | Code::Loop_rel8_32_ECX
        | Code::Loope_rel8_16_ECX
        | Code::Loope_rel8_32_ECX
        | Code::Loopne_rel8_16_ECX
        | Code::Loopne_rel8_32_ECX
        | Code::Jecxz_rel8_16
        | Code::Jecxz_rel8_32 => false,
        _ => return None,
    })
}

pub(super) fn supported(i: &Instruction) -> bool {
    if i.has_lock_prefix() {
        return false;
    }
    if string(i).is_some() {
        return true;
    }
    if i.has_rep_prefix() || i.has_repne_prefix() {
        return false;
    }
    if rmw(i) {
        return true;
    }
    if counter_branch(i).is_some() {
        return true;
    }
    let dst = i.op0_kind() == OpKind::Register && register(i.op0_register()).is_some();
    match i.mnemonic() {
        M::Nop => true,
        M::Mov => {
            dst && source(i, 1)
                || dst
                    && i.op1_kind() == OpKind::Memory
                    && i.memory_size().size() == 4
                    && address(i)
                || i.op0_kind() == OpKind::Memory
                    && i.memory_size().size() == 4
                    && source(i, 1)
                    && address(i)
        }
        M::Lea => dst && i.op1_kind() == OpKind::Memory && address(i),
        M::Movzx | M::Movsx => {
            dst && i.op1_kind() == OpKind::Memory
                && matches!(i.memory_size().size(), 1 | 2)
                && address(i)
                || i.op0_kind() == OpKind::Register
                    && register_part(i.op0_register())
                        .is_some_and(|(_, _, bits)| matches!(bits, 16 | 32))
                    && i.op1_kind() == OpKind::Register
                    && register_part(i.op1_register())
                        .is_some_and(|(_, _, bits)| matches!(bits, 8 | 16))
        }
        M::Imul => {
            i.op_count() == 1 && scalar32(i, 0)
                || dst
                    && matches!(i.op_count(), 2 | 3)
                    && (i.op1_kind() == OpKind::Register && register(i.op1_register()).is_some()
                        || i.op1_kind() == OpKind::Memory
                            && i.memory_size().size() == 4
                            && address(i))
                    && (i.op_count() == 2 || imm(i, 2).is_some())
        }
        M::Mul | M::Div | M::Idiv => scalar32(i, 0),
        M::Bt | M::Bts | M::Btr | M::Btc => {
            i.op0_kind() == OpKind::Register
                && register_part(i.op0_register())
                    .is_some_and(|(_, _, bits)| matches!(bits, 16 | 32))
                && (i.op1_kind() == OpKind::Immediate8
                    || i.op1_kind() == OpKind::Register
                        && register_part(i.op1_register()).is_some())
        }
        M::Bsf | M::Bsr => {
            i.op0_kind() == OpKind::Register
                && register_part(i.op0_register())
                    .is_some_and(|(_, _, bits)| matches!(bits, 16 | 32))
                && (i.op1_kind() == OpKind::Register && register_part(i.op1_register()).is_some()
                    || i.op1_kind() == OpKind::Memory && address(i))
        }
        M::Bswap => dst,
        M::Cdq => true,
        M::Pushad | M::Popad => true,
        M::Shld | M::Shrd => {
            i.op0_kind() == OpKind::Register
                && register_part(i.op0_register())
                    .is_some_and(|(_, _, bits)| matches!(bits, 16 | 32))
                && double_shift(i)
        }
        M::Xchg => {
            i.op0_kind() == OpKind::Register
                && register_part(i.op0_register()).is_some()
                && i.op1_kind() == OpKind::Register
                && register_part(i.op1_register()).is_some()
        }
        M::Add | M::Adc | M::Sub | M::Sbb | M::Cmp | M::And | M::Or | M::Xor | M::Test => {
            let memory =
                |op| i.op_kind(op) == OpKind::Memory && i.memory_size().size() == 4 && address(i);
            dst && (source(i, 1) || memory(1))
                || !matches!(i.mnemonic(), M::Adc | M::Sbb)
                    && i.op0_kind() == OpKind::Register
                    && register_part(i.op0_register()).is_some_and(|(_, _, bits)| bits < 32)
                    && rmw_source(i)
                // Memory destinations that do not write use the same ALU.
                || matches!(i.mnemonic(), M::Cmp | M::Test) && memory(0) && source(i, 1)
        }
        M::Push => i.stack_pointer_increment() == -4 && source(i, 0),
        M::Pop => dst && i.stack_pointer_increment() == 4,
        M::Call | M::Jmp => {
            i.op0_kind() == OpKind::NearBranch32
                || dst
                || i.op0_kind() == OpKind::Memory && i.memory_size().size() == 4 && address(i)
        }
        M::Ret => i.op_count() == 0 || i.op0_kind() == OpKind::Immediate16,
        M::Inc | M::Dec | M::Neg | M::Not => {
            i.op0_kind() == OpKind::Register && register_part(i.op0_register()).is_some()
        }
        m if shift(m) => {
            i.op0_kind() == OpKind::Register
                && register_part(i.op0_register()).is_some()
                && shift_count(i)
        }
        _ if setcc(i.mnemonic()) => {
            i.op0_kind() == OpKind::Register
                && register_part(i.op0_register()).is_some_and(|(_, _, bits)| bits == 8)
                || i.op0_kind() == OpKind::Memory && address(i)
        }
        _ if cmovcc(i.mnemonic()) => {
            i.op0_kind() == OpKind::Register
                && register_part(i.op0_register())
                    .is_some_and(|(_, _, bits)| matches!(bits, 16 | 32))
                && (i.op1_kind() == OpKind::Register && register_part(i.op1_register()).is_some()
                    || i.op1_kind() == OpKind::Memory && address(i))
        }
        _ => {
            i.condition_code() != C::None
                && i.is_jcc_short_or_near()
                && i.op0_kind() == OpKind::NearBranch32
        }
    }
}

impl Emitter {
    pub fn new(helpers: Helpers) -> Self {
        Self {
            helpers,
            words: Vec::new(),
            entries: BTreeMap::new(),
            exits: BTreeMap::new(),
            fixups: Vec::new(),
        }
    }
    fn word(&mut self, x: u32) {
        self.words.push(x);
    }
    fn mov(&mut self, d: u8, s: u8) {
        self.word(0x2a00_03e0 | (s as u32) << 16 | d as u32);
    }
    fn constant(&mut self, d: u8, n: u32) {
        self.word(0x5280_0000 | (n & 0xffff) << 5 | d as u32);
        if n >> 16 != 0 {
            self.word(0x72a0_0000 | (n >> 16) << 5 | d as u32);
        }
    }
    fn binary(&mut self, code: u32, d: u8, a: u8, b: u8) {
        self.word(code | (b as u32) << 16 | (a as u32) << 5 | d as u32);
    }
    fn lsr(&mut self, d: u8, a: u8, n: u8) {
        self.word(0x5300_7c00 | (n as u32) << 16 | (a as u32) << 5 | d as u32);
    }
    fn lsl(&mut self, d: u8, a: u8, n: u8) {
        if n == 0 {
            self.mov(d, a);
        } else {
            self.word(
                0x5300_0000
                    | ((32 - n) as u32) << 16
                    | ((31 - n) as u32) << 10
                    | (a as u32) << 5
                    | d as u32,
            );
        }
    }
    fn mask(&mut self, d: u8, a: u8, bits: u32) {
        self.constant(17, bits);
        self.binary(0x0a00_0000, d, a, 17);
    }
    fn load32(&mut self, d: u8, offset: u32) {
        self.word(0xb940_0000 | (offset / 4) << 10 | 27 << 5 | d as u32);
    }
    fn store32(&mut self, d: u8, offset: u32) {
        self.word(0xb900_0000 | (offset / 4) << 10 | 27 << 5 | d as u32);
    }
    fn jump(&mut self, label: Label, kind: Branch) {
        self.fixups.push(Fixup {
            at: self.words.len(),
            label,
            kind,
        });
        self.word(0);
    }
    fn operand(&mut self, d: u8, i: &Instruction, op: u32) {
        if let Some(r) = register(i.op_register(op)).filter(|_| i.op_kind(op) == OpKind::Register) {
            self.mov(d, r);
        } else if i.op_kind(op) == OpKind::Register {
            self.read_register(d, i.op_register(op), false);
        } else {
            self.constant(d, i.immediate(op) as u32);
        }
    }
    fn read_register(&mut self, d: u8, r: Register, signed: bool) {
        let (s, shift, bits) = register_part(r).expect("admitted register");
        self.word(
            (if signed { 0x1300_0000 } else { 0x5300_0000 })
                | shift << 16
                | (shift + bits - 1) << 10
                | (s as u32) << 5
                | d as u32,
        );
    }
    fn write_register(&mut self, r: Register, s: u8) {
        let (d, shift, bits) = register_part(r).expect("admitted register");
        if bits == 32 {
            self.mov(d, s);
        } else {
            self.word(
                0x3300_0000
                    | ((32 - shift) % 32) << 16
                    | (bits - 1) << 10
                    | (s as u32) << 5
                    | d as u32,
            );
        }
    }
    fn ea(&mut self, i: &Instruction, segment: bool) {
        self.constant(9, i.memory_displacement32());
        if let Some(r) = register(i.memory_base()) {
            self.binary(0x0b00_0000, 9, 9, r);
        }
        if let Some(r) = register(i.memory_index()) {
            self.lsl(10, r, i.memory_index_scale().trailing_zeros() as u8);
            self.binary(0x0b00_0000, 9, 9, 10);
        }
        if segment && i.segment_prefix() == Register::FS {
            self.load32(10, 72);
            self.binary(0x0b00_0000, 9, 9, 10);
        }
    }
    fn address64(&mut self, d: u8, n: u64) {
        self.word(0xd280_0000 | ((n & 0xffff) as u32) << 5 | d as u32);
        for (shift, movk) in [(16, 0xf2a0_0000), (32, 0xf2c0_0000), (48, 0xf2e0_0000)] {
            self.word(movk | (((n >> shift) & 0xffff) as u32) << 5 | d as u32);
        }
    }
    /// Call a helper with x0 = frame and w1/w2/w3 already set. Caller-saved
    /// flags, remaining units and stop are spilled to the frame around it.
    fn call(&mut self, target: u64) {
        self.store32(8, 32);
        self.word(0xf900_1767); // str x7,[x27,#40]
        self.word(0xa9bf_7bfd); // stp x29,x30,[sp,#-16]!
        self.word(0x9100_03fd); // mov x29,sp
        self.word(0xaa1b_03e0); // mov x0,x27
        self.address64(16, target);
        self.word(0xd63f_0200); // blr x16
        self.word(0xa8c1_7bfd); // ldp x29,x30,[sp],#16
        self.load32(8, 32);
        self.word(0xf940_1767); // ldr x7,[x27,#40]
        self.load32(6, 48);
    }
    /// Load `kind` from the address in w1 into w0. A refused load committed
    /// nothing, so the whole instruction reruns exactly in Tier 0.
    fn load(&mut self, pc: u32, kind: u32) {
        self.constant(2, kind);
        self.call(self.helpers.load);
        self.word(0xd360_fc09); // lsr x9,x0,#32
        let ok = self.words.len();
        self.word(0);
        self.constant(5, pc);
        self.jump(Label::Exit(FALLBACK), Branch::B);
        self.words[ok] = 0x3400_0009 | ((self.words.len() - ok) as u32) << 5; // cbz w9
    }
    fn store(&mut self, width: u32) {
        self.constant(3, width);
        self.call(self.helpers.store);
    }
    /// Continue after the store helper. `commit` applies the instruction's
    /// register effects once the store is performed; a refused store reruns
    /// the instruction in Tier 0 and one that must leave dispatches `target`.
    /// A dynamic target is staged at frame offset 76 and always dispatches.
    fn stored(&mut self, pc: u32, target: Option<u32>, link: bool, commit: impl Fn(&mut Self)) {
        self.word(0x7100_081f); // cmp w0,#2
        let refused = self.words.len();
        self.word(0);
        commit(self);
        self.charge();
        if let Some(target) = target {
            let leave = self.words.len();
            self.word(0);
            if link {
                self.link();
            }
            self.jump(Label::Guest(target), Branch::B);
            self.words[leave] = 0x3500_0000 | ((self.words.len() - leave) as u32) << 5; // cbnz w0
            self.constant(5, target);
        } else {
            self.load32(5, 76);
        }
        self.jump(Label::Exit(DISPATCH), Branch::B);
        self.words[refused] = 0x5400_0000 | ((self.words.len() - refused) as u32) << 5; // b.eq
        self.constant(5, pc);
        self.jump(Label::Exit(FALLBACK), Branch::B);
    }
    fn esp_add(&mut self, n: u32) {
        self.word(0x1100_0000 | n << 10 | 23 << 5 | 23); // add w23,w23,#n
    }
    fn esp_sub(&mut self, d: u8) {
        self.word(0x5100_1000 | 23 << 5 | d as u32); // sub wd,w23,#4
    }
    /// Branch over the next `words` instructions when w`r` is nonzero.
    fn skip_nonzero(&mut self, r: u8, words: u32) {
        self.word(0x3500_0000 | (words + 1) << 5 | r as u32);
    }
    fn charge(&mut self) {
        self.word(0xd100_04e7);
    } // sub x7,x7,#1
    fn link(&mut self) {
        self.word(0xf940_1f6c); // ldr x12,[x27,#56]
        self.word(0x9100_058c); // add x12,x12,#1
        self.word(0xf900_1f6c); // str x12,[x27,#56]
    }
    /// Replace bit `lsb` of w`d` with bit 0 of w`n` (BFI).
    fn bfi(&mut self, d: u8, n: u8, lsb: u32) {
        self.word(0x3300_0000 | ((32 - lsb) % 32) << 16 | (n as u32) << 5 | d as u32);
    }
    /// w14 = 1 when the host condition holds (CSET).
    fn cset14(&mut self, condition: u32) {
        self.word(0x1a9f_07ee | (condition ^ 1) << 12);
    }
    fn flags(&mut self, dst: u8, subtract: bool, logic: bool, preserve_carry: bool) {
        // ADDS/SUBS/ANDS already produced NZCV. CSET reads it directly and
        // BFI replaces one guest bit, so no other EFLAGS bit is disturbed.
        const CS: u32 = 2;
        const CC: u32 = 3;
        const VS: u32 = 6;
        self.szp(dst);
        self.cset14(VS);
        self.bfi(dst, 14, 11);
        if !preserve_carry {
            // ARM carry after subtraction is the inverse of x86 borrow.
            self.cset14(if subtract { CC } else { CS });
            self.bfi(dst, 14, 0);
        }
        // Logical AF is architecturally undefined; match Tier 0 preservation.
        if !logic {
            self.binary(0x4a00_0000, 14, 9, 10);
            self.binary(0x4a00_0000, 14, 14, 11);
            self.lsr(14, 14, 4);
            self.bfi(dst, 14, 4);
        }
    }
    /// NZCV describes the width-aligned result; w11 holds its low byte.
    fn szp(&mut self, dst: u8) {
        for (condition, guest) in [(4, 7), (0, 6)] {
            self.cset14(condition);
            self.bfi(dst, 14, guest);
        }
        // PF: even parity of the low result byte, folded into bit 0.
        for (n, shift) in [(11, 4), (14, 2), (14, 1)] {
            // eor w14,wn,wn,lsr #shift
            self.word(0x4a40_0000 | n << 16 | shift << 10 | n << 5 | 14);
        }
        self.word(0x5200_01ce); // eor w14,w14,#1
        self.bfi(dst, 14, 2);
    }
    /// Compute w11 and flags from w9/w10, retaining the unshifted operands
    /// for AF. Narrow operands align to bit 31 so host NZCV has x86 width.
    fn alu(&mut self, m: M, width: u32, flags: u8) {
        if matches!(m, M::Adc | M::Sbb) {
            self.carry_alu(m == M::Sbb, width, flags);
            return;
        }
        let logic = matches!(m, M::And | M::Or | M::Xor | M::Test);
        let sub = matches!(m, M::Sub | M::Cmp | M::Dec | M::Neg);
        let op = match m {
            M::Add | M::Inc => 0x2b00_0000,
            M::Sub | M::Cmp | M::Dec | M::Neg => 0x6b00_0000,
            M::And | M::Test => 0x0a00_0000,
            M::Or => 0x2a00_0000,
            M::Xor => 0x4a00_0000,
            _ => unreachable!(),
        };
        if width < 4 {
            self.lsl(15, 9, (32 - width * 8) as u8);
            self.lsl(16, 10, (32 - width * 8) as u8);
            self.binary(op, 11, 15, 16);
        } else {
            self.binary(op, 11, 9, 10);
        }
        if logic {
            self.binary(0x6a00_0000, 31, 11, 11);
        }
        if width < 4 {
            self.lsr(11, 11, (32 - width * 8) as u8);
        }
        self.flags(flags, sub, logic, matches!(m, M::Inc | M::Dec));
    }
    /// Carry input is the canonical guest CF, never an earlier host NZCV.
    fn carry_alu(&mut self, subtract: bool, width: u32, flags: u8) {
        self.mask(13, 8, 1);
        if width == 4 {
            if subtract {
                self.binary(0x6b00_0000, 31, 31, 13); // cmp wzr,w13
            } else {
                self.word(0x7100_05bf); // cmp w13,#1
            }
            self.binary(if subtract { 0x7a00_0000 } else { 0x3a00_0000 }, 11, 9, 10);
            self.flags(flags, subtract, false, false);
            return;
        }
        let bits = width * 8;
        let mask = (1 << bits) - 1;
        self.mask(9, 9, mask);
        self.mask(10, 10, mask);
        let op = if subtract { 0x4b00_0000 } else { 0x0b00_0000 };
        self.binary(op, 11, 9, 10);
        self.binary(op, 11, 11, 13);
        // The bit above the narrow result is carry/borrow, even after wrap.
        self.lsr(13, 11, bits as u8);
        self.bfi(flags, 13, 0);
        self.mask(11, 11, mask);
        self.lsl(15, 11, (32 - bits) as u8);
        self.binary(0x6a00_0000, 31, 15, 15);
        self.flags(flags, subtract, false, true);
        // Signed overflow uses the original operands, including when b+CF
        // crosses the signed boundary. AF above likewise uses original b.
        self.binary(0x4a00_0000, 15, 9, 11);
        self.binary(0x4a00_0000, 16, if subtract { 9 } else { 11 }, 10);
        self.binary(0x0a00_0000, 15, 15, 16);
        self.lsr(15, 15, (bits - 1) as u8);
        self.bfi(flags, 15, 11);
    }
    /// Shift w9 by the masked count in w10 into w11. Match Tier 0's
    /// deterministic preservation of AF, multi-bit OF and oversized SHL/SHR CF.
    fn shift_value(&mut self, m: M, width: u32, flags: u8) {
        let bits = width * 8;
        let left = matches!(m, M::Shl | M::Sal | M::Rol);
        let rotate = matches!(m, M::Rol | M::Ror);
        self.mask(10, 10, 31);
        self.mov(11, 9);
        let zero = self.words.len();
        self.word(0);
        if rotate {
            // Repeat a narrow value across 32 bits before the host rotate.
            self.mov(15, 9);
            if bits == 8 {
                self.lsl(16, 15, 8);
                self.binary(0x2a00_0000, 15, 15, 16);
            }
            if bits < 32 {
                self.lsl(16, 15, 16);
                self.binary(0x2a00_0000, 15, 15, 16);
            }
            if left {
                self.binary(0x4b00_0000, 16, 31, 10);
            } else {
                self.mov(16, 10);
            }
            self.binary(0x1ac0_2c00, 11, 15, 16); // rorv
        } else {
            if m == M::Sar && bits < 32 {
                self.word(0x1300_0000 | (bits - 1) << 10 | 9 << 5 | 9); // sbfx
            }
            self.binary(
                if left {
                    0x1ac0_2000
                } else if m == M::Sar {
                    0x1ac0_2800
                } else {
                    0x1ac0_2400
                },
                11,
                9,
                10,
            );
        }
        if bits < 32 {
            self.mask(11, 11, (1 << bits) - 1);
        }
        if rotate {
            self.lsr(14, 11, if left { 0 } else { (bits - 1) as u8 });
            self.bfi(flags, 14, 0);
        } else {
            if left {
                self.constant(16, bits);
                self.binary(0x4b00_0000, 16, 16, 10);
            } else {
                self.word(0x5100_0400 | 10 << 5 | 16); // sub w16,w10,#1
            }
            self.binary(0x1ac0_2400, 14, 9, 16);
            if bits < 32 && m != M::Sar {
                self.word(0x7100_001f | bits << 10 | 10 << 5); // cmp w10,#bits
                self.word(0x5400_0048); // b.hi +8
            }
            self.bfi(flags, 14, 0);
            self.lsl(15, 11, (32 - bits) as u8);
            self.binary(0x6a00_0000, 31, 15, 15);
            self.szp(flags);
        }
        self.word(0x7100_055f); // cmp w10,#1
        let multiple = self.words.len();
        self.word(0);
        if left {
            self.lsr(14, 11, (bits - 1) as u8);
            self.binary(0x4a00_0000, 14, 14, flags);
        } else if m == M::Shr {
            self.lsr(14, 9, (bits - 1) as u8);
        } else if m == M::Sar {
            self.constant(14, 0);
        } else {
            self.lsr(14, 11, (bits - 1) as u8);
            self.lsr(15, 11, (bits - 2) as u8);
            self.binary(0x4a00_0000, 14, 14, 15);
        }
        self.bfi(flags, 14, 11);
        self.words[zero] = 0x3400_000a | ((self.words.len() - zero) as u32) << 5;
        self.words[multiple] = 0x5400_0001 | ((self.words.len() - multiple) as u32) << 5;
    }
    /// Concatenation matches Tier 0 even for undefined 16-bit counts 17..31.
    /// w9 is the destination, w13 the source and w10 the count; w11 is output.
    fn double_shift_value(&mut self, i: &Instruction, width: u32, flags: u8) {
        let bits = width * 8;
        let left = i.mnemonic() == M::Shld;
        self.read_register(13, i.op1_register(), false);
        if i.op2_kind() == OpKind::Register {
            self.mov(10, 20);
        } else {
            self.constant(10, u32::from(i.immediate8()));
        }
        self.mask(10, 10, 31);
        self.mov(11, 9);
        let zero = self.words.len();
        self.word(0);
        let (high, low) = if left { (9, 13) } else { (13, 9) };
        if bits == 32 {
            self.word(0xd360_7c00 | high << 5 | 15); // lsl x15,xhigh,#32
            self.binary(0xaa00_0000, 15, 15, low as u8);
            self.binary(if left { 0x9ac0_2000 } else { 0x9ac0_2400 }, 11, 15, 10);
            if left {
                self.word(0xd360_fd6b);
            } // lsr x11,x11,#32
        } else {
            self.lsl(15, high as u8, 16);
            self.binary(0x2a00_0000, 15, 15, low as u8);
            if left {
                self.binary(0x4b00_0000, 16, 31, 10);
            } else {
                self.mov(16, 10);
            }
            self.binary(0x1ac0_2c00, 11, 15, 16);
            if left {
                self.lsr(11, 11, 16);
            } else {
                self.mask(11, 11, 0xffff);
            }
        }
        if left {
            self.constant(16, bits * 2);
            self.binary(0x4b00_0000, 16, 16, 10);
        } else {
            self.word(0x5100_0550);
        } // sub w16,w10,#1
        self.binary(
            if bits == 32 { 0x9ac0_2400 } else { 0x1ac0_2400 },
            14,
            15,
            16,
        );
        self.bfi(flags, 14, 0);
        self.lsl(15, 11, (32 - bits) as u8);
        self.binary(0x6a00_0000, 31, 15, 15);
        self.szp(flags);
        self.word(0x7100_055f); // cmp w10,#1
        let multiple = self.words.len();
        self.word(0);
        self.binary(0x4a00_0000, 14, 9, 11);
        self.lsr(14, 14, (bits - 1) as u8);
        self.bfi(flags, 14, 11);
        self.words[zero] = 0x3400_000a | ((self.words.len() - zero) as u32) << 5;
        self.words[multiple] = 0x5400_0001 | ((self.words.len() - multiple) as u32) << 5;
    }
    fn memory_rmw(&mut self, i: &Instruction) {
        let width = i.memory_size().size() as u32;
        self.ea(i, true);
        self.mov(1, 9);
        self.load(i.ip32(), width);
        self.mov(9, 0);
        self.mov(12, 8);
        if double_shift(i) {
            self.double_shift_value(i, width, 12);
        } else if i.mnemonic() == M::Not {
            self.word(0x2a29_03eb); // mvn w11,w9
        } else {
            match i.mnemonic() {
                M::Inc | M::Dec => self.constant(10, 1),
                M::Neg => {
                    self.mov(10, 9);
                    self.constant(9, 0);
                }
                _ if i.op1_kind() == OpKind::Register => {
                    let r = i.op1_register();
                    self.mov(10, register(r.full_register32()).expect("admitted source"));
                    if matches!(r, Register::AH | Register::CH | Register::DH | Register::BH) {
                        self.lsr(10, 10, 8);
                    }
                }
                _ => self.constant(10, i.immediate(1) as u32),
            }
            if shift(i.mnemonic()) {
                self.shift_value(i.mnemonic(), width, 12);
            } else {
                self.alu(i.mnemonic(), width, 12);
            }
        }
        // Frame scratch survives the helper. Guest flags remain in w8 until
        // the store succeeds; refusal may rerun the entire instruction.
        self.store32(12, 76);
        self.mov(2, 11);
        self.constant(3, width);
        if shift(i.mnemonic()) || double_shift(i) {
            // Zero shifts check write permission but must not mutate memory.
            self.word(0x7100_015f); // cmp w10,#0
            self.cset14(0);
            self.lsl(14, 14, CHECK_WRITE.trailing_zeros() as u8);
            self.binary(0x2a00_0000, 3, 3, 14);
        }
        self.ea(i, true);
        self.mov(1, 9);
        self.call(self.helpers.store);
        self.stored(i.ip32(), Some(i.next_ip32()), false, |e| e.load32(8, 76));
    }
    /// Resolve before changing ESP: CALL ESP and CALL [ESP - 4] must use
    /// the old register/memory value even when the push aliases the source.
    fn indirect_target(&mut self, i: &Instruction) {
        if i.op0_kind() == OpKind::Memory {
            self.ea(i, true);
            self.mov(1, 9);
            self.load(i.ip32(), 4);
            self.mov(5, 0);
        } else {
            self.operand(5, i, 0);
        }
    }
    fn multiply_divide(&mut self, i: &Instruction) {
        if i.op0_kind() == OpKind::Memory {
            self.ea(i, true);
            self.mov(1, 9);
            self.load(i.ip32(), 4);
            self.mov(10, 0);
        } else {
            self.operand(10, i, 0);
        }
        let signed = matches!(i.mnemonic(), M::Imul | M::Idiv);
        if matches!(i.mnemonic(), M::Mul | M::Imul) {
            self.binary(if signed { 0x9b20_7c00 } else { 0x9ba0_7c00 }, 11, 19, 10);
            if signed {
                self.word(0x9340_7d6f); // sxtw x15,w11
                self.binary(0xeb00_0000, 31, 11, 15);
            } else {
                self.word(0xd360_fd6f); // lsr x15,x11,#32
                self.word(0x7100_01ff); // cmp w15,#0
            }
            self.cset14(1);
            self.bfi(8, 14, 0);
            self.bfi(8, 14, 11);
            self.mov(19, 11);
            self.word(0xd360_fd75); // lsr x21,x11,#32
            return;
        }
        // AArch64 integer divide cannot trap. Reject zero before computing;
        // check the full quotient before touching EAX/EDX or retirement state.
        self.constant(5, i.ip32());
        self.word(0x7100_015f); // cmp w10,#0
        self.jump(Label::Exit(FALLBACK), Branch::Equal);
        self.word(0xd360_7ea9); // lsl x9,x21,#32
        self.binary(0xaa00_0000, 9, 9, 19);
        if signed {
            self.word(0x9340_7d4a);
        } // sxtw x10,w10
        self.binary(if signed { 0x9ac0_0c00 } else { 0x9ac0_0800 }, 11, 9, 10);
        if signed {
            self.word(0x9340_7d6f);
        }
        // sxtw x15,w11
        else {
            self.mov(15, 11);
        }
        self.binary(0xeb00_0000, 31, 11, 15);
        self.word(0x5400_0040); // b.eq +8
        self.jump(Label::Exit(FALLBACK), Branch::B);
        // MSUB computes the exact remainder, with the dividend's sign. Even
        // INT64_MIN / -1 fails the fit check above (AArch64 returns INT64_MIN).
        self.word(0x9b0a_a575); // msub x21,x11,x10,x9
        self.mov(19, 11);
        self.mov(21, 21); // clear the high host half of the guest GPR
    }
    fn condition(&mut self, c: C) {
        let (bit, invert) = match c {
            C::o => (11, false),
            C::no => (11, true),
            C::b => (0, false),
            C::ae => (0, true),
            C::e => (6, false),
            C::ne => (6, true),
            C::s => (7, false),
            C::ns => (7, true),
            C::p => (2, false),
            C::np => (2, true),
            _ => (0, false),
        };
        match c {
            C::be | C::a => {
                self.lsr(10, 8, 6);
                self.binary(0x2a00_0000, 9, 8, 10);
            }
            C::l | C::ge | C::le | C::g => {
                self.lsr(9, 8, 7);
                self.lsr(10, 8, 11);
                self.binary(0x4a00_0000, 9, 9, 10);
                if matches!(c, C::le | C::g) {
                    self.lsr(10, 8, 6);
                    self.binary(0x2a00_0000, 9, 9, 10);
                }
            }
            _ => self.lsr(9, 8, bit),
        }
        self.mask(9, 9, 1);
        if invert || matches!(c, C::a | C::ge | C::g) {
            self.constant(17, 1);
            self.binary(0x4a00_0000, 9, 9, 17);
        }
    }
    /// One iteration is one retirement unit. Reenter the ordinary instruction
    /// guard between elements, keeping PC on REP until the final commit.
    fn string_instruction(&mut self, i: &Instruction, width: u32, read: bool, write: bool) {
        let repeat = i.has_rep_prefix() || i.has_repne_prefix();
        if repeat {
            self.skip_nonzero(20, 2);
            self.charge();
            self.jump(Label::Guest(i.next_ip32()), Branch::B);
        }
        if read {
            self.mov(1, 25);
            if i.segment_prefix() == Register::FS {
                self.load32(9, 72);
                self.binary(0x0b00_0000, 1, 1, 9);
            }
            self.load(i.ip32(), width);
        }
        let refused = if write {
            self.mov(2, if read { 0 } else { 19 });
            self.mov(1, 26);
            self.store(width);
            self.word(0x7100_081f); // cmp w0,#2
            let at = self.words.len();
            self.word(0);
            Some(at)
        } else {
            self.write_register(
                match width {
                    1 => Register::AL,
                    2 => Register::AX,
                    _ => Register::EAX,
                },
                0,
            );
            None
        };
        // delta = width - 2 * width * DF; guest flags are unchanged.
        self.lsr(9, 8, 10);
        self.mask(9, 9, 1);
        self.lsl(9, 9, (width.trailing_zeros() + 1) as u8);
        self.constant(10, width);
        self.binary(0x4b00_0000, 9, 10, 9);
        if read {
            self.binary(0x0b00_0000, 25, 25, 9);
        }
        if write {
            self.binary(0x0b00_0000, 26, 26, 9);
        }
        if repeat {
            self.word(0x5100_0694);
        } // sub w20,w20,#1
        self.charge();
        let more = self.words.len();
        if repeat {
            self.word(0);
        }
        if write {
            self.skip_nonzero(0, 1);
        }
        self.jump(Label::Guest(i.next_ip32()), Branch::B);
        if write {
            self.constant(5, i.next_ip32());
            self.jump(Label::Exit(DISPATCH), Branch::B);
        }
        if repeat {
            self.words[more] = 0x3500_0014 | ((self.words.len() - more) as u32) << 5;
            if write {
                self.skip_nonzero(0, 1);
            }
            self.jump(Label::Guest(i.ip32()), Branch::B);
            if write {
                self.constant(5, i.ip32());
                self.jump(Label::Exit(DISPATCH), Branch::B);
            }
        }
        if let Some(at) = refused {
            self.words[at] = 0x5400_0000 | ((self.words.len() - at) as u32) << 5;
            self.constant(5, i.ip32());
            self.jump(Label::Exit(FALLBACK), Branch::B);
        }
    }
    pub fn instruction(&mut self, i: &Instruction) {
        let pc = i.ip32();
        self.entries.insert(pc, self.words.len() * 4);
        self.constant(5, pc);
        self.binary(0x6b00_0000, 31, 5, 6); // stop guard, flags live in w8
        self.jump(Label::Exit(DISPATCH), Branch::Equal);
        self.jump(Label::Exit(BUDGET), Branch::Zero64(7));
        if !supported(i) {
            self.jump(Label::Exit(FALLBACK), Branch::B);
            return;
        }
        if let Some((width, read, write)) = string(i) {
            self.string_instruction(i, width, read, write);
            return;
        }
        if rmw(i) {
            self.memory_rmw(i);
            return;
        }
        let d = register(i.op0_register()).unwrap_or(31);
        match i.mnemonic() {
            M::Mov if i.op1_kind() == OpKind::Memory => {
                self.ea(i, true);
                self.mov(1, 9);
                self.constant(2, 4);
                self.call(self.helpers.load);
                self.word(0xd360_fc09); // lsr x9,x0,#32
                self.skip_nonzero(9, 3);
                self.mov(d, 0);
                self.charge();
                self.jump(Label::Guest(i.next_ip32()), Branch::B);
                // The helper refused: leave through the checked exit, which
                // reports the exact fault. The call clobbered w5.
                self.constant(5, pc);
                self.ea(i, true);
                self.store32(9, 64);
                self.constant(9, u32::from(d - 19));
                self.store32(9, 76);
                self.constant(9, i.next_ip32());
                self.store32(9, 68);
                self.jump(Label::Exit(LOAD), Branch::B);
                return;
            }
            M::Mov if i.op0_kind() == OpKind::Memory => {
                self.ea(i, true);
                self.mov(1, 9);
                self.operand(2, i, 1);
                self.store(4);
                self.skip_nonzero(0, 2);
                self.charge();
                self.jump(Label::Guest(i.next_ip32()), Branch::B);
                // A committed store that changed code or the memory epoch
                // leaves so the owner revalidates before the next instruction.
                self.word(0x7100_041f); // cmp w0,#1
                let refused = self.words.len();
                self.word(0);
                self.charge();
                self.constant(5, i.next_ip32());
                self.jump(Label::Exit(DISPATCH), Branch::B);
                self.words[refused] = 0x5400_0001 | ((self.words.len() - refused) as u32) << 5; // b.ne
                self.constant(5, pc);
                self.ea(i, true);
                self.store32(9, 64);
                self.operand(9, i, 1);
                self.store32(9, 68);
                self.constant(9, i.next_ip32());
                self.store32(9, 76);
                self.jump(Label::Exit(STORE), Branch::B);
                return;
            }
            M::Mov => self.operand(d, i, 1),
            M::Movzx | M::Movsx => {
                if i.op1_kind() == OpKind::Register {
                    self.read_register(9, i.op1_register(), i.mnemonic() == M::Movsx);
                    self.write_register(i.op0_register(), 9);
                } else {
                    self.ea(i, true);
                    self.mov(1, 9);
                    let signed = if i.mnemonic() == M::Movsx { SIGNED } else { 0 };
                    self.load(pc, i.memory_size().size() as u32 | signed);
                    self.mov(d, 0);
                }
            }
            M::Mul | M::Div | M::Idiv => self.multiply_divide(i),
            M::Imul if i.op_count() == 1 => self.multiply_divide(i),
            M::Imul => {
                if i.op1_kind() == OpKind::Memory {
                    self.ea(i, true);
                    self.mov(1, 9);
                    self.load(pc, 4);
                    self.mov(10, 0);
                } else {
                    self.operand(10, i, 1);
                }
                if i.op_count() == 3 {
                    self.mov(9, 10);
                    self.operand(10, i, 2);
                } else {
                    self.mov(9, d);
                }
                self.binary(0x9b20_7c00, 11, 9, 10); // smull x11,w9,w10
                self.word(0x9340_7d6f); // sxtw x15,w11
                self.binary(0xeb00_0000, 31, 11, 15); // cmp x11,x15
                self.cset14(1);
                self.bfi(8, 14, 0);
                self.bfi(8, 14, 11);
                self.mov(d, 11);
            }
            M::Cdq => self.word(0x131f_7e75), // asr w21,w19,#31
            M::Bt | M::Bts | M::Btr | M::Btc => {
                self.read_register(9, i.op0_register(), false);
                self.operand(10, i, 1);
                self.mask(10, 10, i.op0_register().size() as u32 * 8 - 1);
                self.binary(0x1ac0_2400, 14, 9, 10);
                self.bfi(8, 14, 0);
                if i.mnemonic() != M::Bt {
                    self.constant(11, 1);
                    self.binary(0x1ac0_2000, 11, 11, 10);
                    self.binary(
                        match i.mnemonic() {
                            M::Bts => 0x2a00_0000,
                            M::Btr => 0x0a20_0000,
                            _ => 0x4a00_0000,
                        },
                        11,
                        9,
                        11,
                    );
                    self.write_register(i.op0_register(), 11);
                }
            }
            M::Bsf | M::Bsr => {
                if i.op1_kind() == OpKind::Memory {
                    self.ea(i, true);
                    self.mov(1, 9);
                    self.load(pc, i.memory_size().size() as u32);
                    self.mov(9, 0);
                } else {
                    self.read_register(9, i.op1_register(), false);
                }
                self.word(0x7100_013f); // cmp w9,#0
                self.cset14(0);
                self.bfi(8, 14, 6);
                let zero = self.words.len();
                self.word(0);
                if i.mnemonic() == M::Bsf {
                    self.word(0x5ac0_0129);
                } // rbit w9,w9
                self.word(0x5ac0_112b); // clz w11,w9
                if i.mnemonic() == M::Bsr {
                    self.constant(9, 31);
                    self.binary(0x4b00_0000, 11, 9, 11);
                }
                self.write_register(i.op0_register(), 11);
                self.words[zero] = 0x3400_0009 | ((self.words.len() - zero) as u32) << 5;
            }
            M::Bswap => self.word(0x5ac0_0800 | (d as u32) << 5 | d as u32),
            M::Xchg => {
                self.read_register(9, i.op0_register(), false);
                self.read_register(10, i.op1_register(), false);
                self.write_register(i.op0_register(), 10);
                self.write_register(i.op1_register(), 9);
            }
            M::Push => {
                self.esp_sub(1);
                self.operand(2, i, 0);
                self.store(4);
                self.stored(pc, Some(i.next_ip32()), false, |e| e.esp_sub(23));
                return;
            }
            M::Pushad | M::Popad => {
                for n in 0..8 {
                    self.store32(19 + n, u32::from(n) * 4);
                }
                self.constant(1, u32::from(i.mnemonic() == M::Popad));
                self.call(self.helpers.stack_all);
                self.stored(pc, Some(i.next_ip32()), false, |e| {
                    for n in 0..8 {
                        e.load32(19 + n, u32::from(n) * 4);
                    }
                });
                return;
            }
            M::Pop => {
                self.mov(1, 23);
                self.load(pc, 4);
                // POP ESP stores the loaded value after the increment.
                self.esp_add(4);
                self.mov(d, 0);
            }
            M::Call => {
                let target = if i.op0_kind() == OpKind::NearBranch32 {
                    Some(i.near_branch32())
                } else {
                    self.indirect_target(i);
                    self.store32(5, 76);
                    None
                };
                self.esp_sub(1);
                self.constant(2, i.next_ip32());
                self.store(4);
                self.stored(pc, target, true, |e| e.esp_sub(23));
                return;
            }
            M::Ret => {
                self.mov(1, 23);
                self.load(pc, 4);
                let release = if i.op_count() == 0 {
                    0
                } else {
                    u32::from(i.immediate16())
                };
                self.constant(10, 4 + release);
                self.binary(0x0b00_0000, 23, 23, 10);
                self.mov(5, 0);
                self.charge();
                self.jump(Label::Exit(DISPATCH), Branch::B);
                return;
            }
            M::Lea => {
                self.ea(i, false);
                self.mov(d, 9);
            }
            M::Not => {
                self.read_register(9, i.op0_register(), false);
                self.word(0x2a29_03eb); // mvn w11,w9
                self.write_register(i.op0_register(), 11);
            }
            M::Shld | M::Shrd => {
                self.read_register(9, i.op0_register(), false);
                self.double_shift_value(i, i.op0_register().size() as u32, 8);
                self.write_register(i.op0_register(), 11);
            }
            m if shift(m) => {
                self.read_register(9, i.op0_register(), false);
                if i.op1_kind() == OpKind::Register {
                    self.mov(10, 20);
                } else {
                    self.constant(10, i.immediate8() as u32);
                }
                self.shift_value(m, i.op0_register().size() as u32, 8);
                self.write_register(i.op0_register(), 11);
            }
            M::Nop => {}
            _ if setcc(i.mnemonic()) => {
                self.condition(i.condition_code());
                if i.op0_kind() == OpKind::Memory {
                    self.mov(2, 9);
                    self.ea(i, true);
                    self.mov(1, 9);
                    self.store(1);
                    self.stored(pc, Some(i.next_ip32()), false, |_| {});
                    return;
                }
                self.write_register(i.op0_register(), 9);
            }
            _ if cmovcc(i.mnemonic()) => {
                // The source read is unconditional, including its faults.
                if i.op1_kind() == OpKind::Memory {
                    self.ea(i, true);
                    self.mov(1, 9);
                    self.load(pc, i.memory_size().size() as u32);
                    self.mov(11, 0);
                } else {
                    self.read_register(11, i.op1_register(), false);
                }
                self.condition(i.condition_code());
                self.word(0x3400_0049); // cbz w9,+8
                self.write_register(i.op0_register(), 11);
            }
            M::Jmp => {
                if i.op0_kind() != OpKind::NearBranch32 {
                    self.indirect_target(i);
                    self.charge();
                    self.jump(Label::Exit(DISPATCH), Branch::B);
                    return;
                }
                self.charge();
                self.link();
                self.jump(Label::Guest(i.near_branch32()), Branch::B);
                return;
            }
            M::Loop | M::Loope | M::Loopne | M::Jcxz | M::Jecxz => {
                let r = if counter_branch(i).unwrap() {
                    Register::CX
                } else {
                    Register::ECX
                };
                self.read_register(9, r, false);
                let zero = matches!(i.mnemonic(), M::Jcxz | M::Jecxz);
                if !zero {
                    self.word(0x5100_0529); // sub w9,w9,#1
                    self.write_register(r, 9);
                    self.read_register(9, r, false);
                }
                self.word(0x7100_013f); // cmp w9,#0
                self.cset14(if zero { 0 } else { 1 });
                self.mov(9, 14);
                if matches!(i.mnemonic(), M::Loope | M::Loopne) {
                    self.lsr(10, 8, 6);
                    if i.mnemonic() == M::Loopne {
                        self.word(0x5200_014a);
                    } // eor w10,w10,#1
                    self.binary(0x0a00_0000, 9, 9, 10);
                }
                self.charge();
                self.link();
                self.jump(
                    Label::Guest(i.near_branch_target() as u32),
                    Branch::Nonzero32(9),
                );
                self.jump(Label::Guest(i.next_ip32()), Branch::B);
                return;
            }
            _ if i.is_jcc_short_or_near() => {
                self.charge();
                self.link();
                self.condition(i.condition_code());
                self.jump(Label::Guest(i.near_branch32()), Branch::Nonzero32(9));
                self.jump(Label::Guest(i.next_ip32()), Branch::B);
                return;
            }
            m => {
                // Left operand in w9, right operand in w10.
                if i.op1_kind() == OpKind::Memory {
                    self.ea(i, true);
                    self.mov(1, 9);
                    self.load(pc, 4);
                    self.mov(10, 0);
                    self.mov(9, d);
                } else if i.op0_kind() == OpKind::Memory {
                    self.ea(i, true);
                    self.mov(1, 9);
                    self.load(pc, 4);
                    self.mov(9, 0);
                    self.operand(10, i, 1);
                } else {
                    self.read_register(9, i.op0_register(), false);
                    match m {
                        M::Inc | M::Dec => self.constant(10, 1),
                        M::Neg => {
                            self.mov(10, 9);
                            self.constant(9, 0);
                        }
                        _ => self.operand(10, i, 1),
                    }
                }
                let width = if i.op0_kind() == OpKind::Register {
                    i.op0_register().size() as u32
                } else {
                    4
                };
                self.alu(m, width, 8);
                if !matches!(m, M::Cmp | M::Test) {
                    self.write_register(i.op0_register(), 11);
                }
            }
        }
        self.charge();
        self.jump(Label::Guest(i.next_ip32()), Branch::B);
    }
    pub fn finish(mut self) -> Result<Self, String> {
        let missing: BTreeSet<_> = self
            .fixups
            .iter()
            .filter_map(|f| match f.label {
                Label::Guest(pc) if !self.entries.contains_key(&pc) => Some(pc),
                _ => None,
            })
            .collect();
        for pc in missing {
            self.entries.insert(pc, self.words.len() * 4);
            self.constant(5, pc);
            self.jump(Label::Exit(DISPATCH), Branch::B);
        }
        for reason in [DISPATCH, FALLBACK, BUDGET, LOAD, STORE] {
            self.exits.insert(reason, self.words.len() * 4);
            self.store32(5, 36);
            self.word(0xf900_1767); // str x7,[x27,#40]
            self.constant(9, reason);
            self.store32(9, 52);
            self.word(0xd65f_03c0);
        }
        for f in &self.fixups {
            let to = match f.label {
                Label::Guest(pc) => self.entries[&pc],
                Label::Exit(r) => self.exits[&r],
            };
            let delta = (to as i64 - f.at as i64 * 4) / 4;
            let (base, bits) = match f.kind {
                Branch::B => (0x1400_0000, 26),
                Branch::Equal => (0x5400_0000, 19),
                Branch::Zero64(r) => (0xb400_0000 | r as u32, 19),
                Branch::Nonzero32(r) => (0x3500_0000 | r as u32, 19),
            };
            if !(-(1i64 << (bits - 1))..(1i64 << (bits - 1))).contains(&delta) {
                return Err("Tier 1 branch displacement exceeds bounded unit".into());
            }
            let displacement = (delta as u32) & ((1 << bits) - 1);
            self.words[f.at] = base
                | if bits == 26 {
                    displacement
                } else {
                    displacement << 5
                };
        }
        Ok(self)
    }
}

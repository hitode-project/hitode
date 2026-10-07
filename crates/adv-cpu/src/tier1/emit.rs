//! Bounded ARM64 templates for the versioned fixed-register entry ABI.
//! These encodings deliberately cover only the declared integer subset; all
//! other forms leave before the instruction and use the existing interpreter.
use super::{BUDGET, DISPATCH, FALLBACK, LOAD, STORE};
use iced_x86::{ConditionCode as C, Instruction, Mnemonic as M, OpKind, Register};
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
    /// `(frame, address) -> value | status << 32`; nonzero status leaves.
    pub load: u64,
    /// `(frame, address, value) -> 0` to continue, 1 after a store that
    /// must leave native code, 2 when not performed.
    pub store: u64,
}

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
fn imm(i: &Instruction, op: u32) -> Option<u32> {
    matches!(i.op_kind(op), OpKind::Immediate32 | OpKind::Immediate8to32)
        .then(|| i.immediate(op) as u32)
}
fn source(i: &Instruction, op: u32) -> bool {
    i.op_kind(op) == OpKind::Register && register(i.op_register(op)).is_some()
        || imm(i, op).is_some()
}
fn address(i: &Instruction) -> bool {
    // Word addressing has different wrapping and remains in Tier 0.
    (i.memory_base() == Register::None || register(i.memory_base()).is_some())
        && (i.memory_index() == Register::None || register(i.memory_index()).is_some())
        && i.memory_displ_size() != 2
        && i.segment_prefix() != Register::GS
}

pub(super) fn supported(i: &Instruction) -> bool {
    if i.has_lock_prefix() || i.has_rep_prefix() || i.has_repne_prefix() {
        return false;
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
        M::Add | M::Sub | M::Cmp | M::And | M::Or | M::Xor | M::Test => dst && source(i, 1),
        M::Inc | M::Dec | M::Neg | M::Not => dst,
        M::Jmp => i.op0_kind() == OpKind::NearBranch32,
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
        } else {
            self.constant(d, imm(i, op).expect("admitted operand"));
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
    /// Call a helper with x0 = frame and w1/w2 already set. Caller-saved
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
    fn flags(&mut self, subtract: bool, logic: bool, preserve_carry: bool) {
        // ADDS/SUBS/ANDS already produced NZCV. MOV/UBFM never change it.
        self.word(0xd53b_420c); // mrs x12,nzcv
        if preserve_carry {
            self.mask(0, 8, 1);
        }
        // Logical AF is architecturally undefined; match Tier 0 preservation.
        self.mask(8, 8, !(if logic { 0x8c5 } else { 0x8d5 }));
        for (host, guest) in [(31, 7), (30, 6), (29, 0), (28, 11)] {
            self.lsr(14, 12, host);
            self.mask(14, 14, 1);
            if host == 29 && subtract {
                self.constant(17, 1);
                self.binary(0x4a00_0000, 14, 14, 17);
            }
            if host == 29 && preserve_carry {
                self.mov(14, 0);
            }
            self.lsl(14, 14, guest);
            self.binary(0x2a00_0000, 8, 8, 14);
        }
        self.lsr(14, 11, 4);
        self.binary(0x4a00_0000, 14, 14, 11);
        self.lsr(15, 14, 2);
        self.binary(0x4a00_0000, 14, 14, 15);
        self.lsr(15, 14, 1);
        self.binary(0x4a00_0000, 14, 14, 15);
        self.word(0x2a20_03ee | 14 << 16); // mvn w14,w14
        self.mask(14, 14, 1);
        self.lsl(14, 14, 2);
        self.binary(0x2a00_0000, 8, 8, 14);
        if !logic {
            self.binary(0x4a00_0000, 14, 9, 10);
            self.binary(0x4a00_0000, 14, 14, 11);
            self.mask(14, 14, 16);
            self.binary(0x2a00_0000, 8, 8, 14);
        }
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
        let d = register(i.op0_register()).unwrap_or(31);
        match i.mnemonic() {
            M::Mov if i.op1_kind() == OpKind::Memory => {
                self.ea(i, true);
                self.mov(1, 9);
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
                self.call(self.helpers.store);
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
            M::Lea => {
                self.ea(i, false);
                self.mov(d, 9);
            }
            M::Not => self.word(0x2a20_03e0 | (d as u32) << 16 | d as u32),
            M::Nop => {}
            M::Jmp => {
                self.charge();
                self.link();
                self.jump(Label::Guest(i.near_branch32()), Branch::B);
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
                self.mov(9, d);
                if matches!(m, M::Inc | M::Dec) {
                    self.constant(10, 1);
                } else if m == M::Neg {
                    self.mov(10, 9);
                    self.constant(9, 0);
                } else {
                    self.operand(10, i, 1);
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
                self.binary(op, 11, 9, 10);
                if logic {
                    self.binary(0x6a00_0000, 31, 11, 11);
                }
                self.flags(sub, logic, matches!(m, M::Inc | M::Dec));
                if !matches!(m, M::Cmp | M::Test) {
                    self.mov(d, 11);
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

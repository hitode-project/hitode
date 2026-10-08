//! Backend-independent GIR v1. Every identity is local to its owning region.
use iced_x86::ConditionCode;

macro_rules! index {
    ($($name:ident),*) => {$(
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub usize);
    )*};
}
index!(
    ValueId,
    BlockId,
    MemoryId,
    CommitId,
    SnapshotId,
    DependencyId
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Width {
    B1,
    B8,
    B16,
    B32,
    B64,
    Token,
}
impl Width {
    pub fn bits(self) -> u32 {
        match self {
            Self::B1 => 1,
            Self::B8 => 8,
            Self::B16 => 16,
            Self::B32 => 32,
            Self::B64 => 64,
            Self::Token => 0,
        }
    }
    pub fn mask(self) -> u64 {
        match self.bits() {
            64 => u64::MAX,
            0 => 0,
            n => (1u64 << n) - 1,
        }
    }
    pub fn integer(self) -> bool {
        self != Self::Token
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binary {
    Add,
    Sub,
    And,
    Or,
    Xor,
    Mul,
    Shl,
    Shr,
    Sar,
    Rol,
    Ror,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pure {
    Binary(Binary, ValueId, ValueId),
    Extract {
        value: ValueId,
        lsb: u8,
    },
    Insert {
        base: ValueId,
        value: ValueId,
        lsb: u8,
    },
    Extend {
        value: ValueId,
        signed: bool,
    },
    Select {
        condition: ValueId,
        yes: ValueId,
        no: ValueId,
    },
    Condition {
        flags: ValueId,
        code: ConditionCode,
    },
    Nonzero(ValueId),
    FlagBit {
        flags: ValueId,
        bit: u32,
    },
    FlagPack {
        bits: [ValueId; 6],
        rest: ValueId,
    },
}

pub const CF: u32 = 1;
pub const PF: u32 = 4;
pub const AF: u32 = 16;
pub const ZF: u32 = 64;
pub const SF: u32 = 128;
pub const OF: u32 = 2048;
pub const FLAG_BITS: [u32; 6] = [CF, PF, AF, ZF, SF, OF];
pub const ARITHMETIC: u32 = CF | PF | AF | ZF | SF | OF;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlagKind {
    Add,
    Sub,
    Logic,
    Shl,
    Shr,
    Sar,
    Rol,
    Ror,
    SignedMultiply,
    UnsignedMultiply,
}

/// The mask bounds defined bits. Shift masks are refined by the runtime count;
/// zero counts and ISA-undefined bits retain `preserved`, exactly as Tier 0.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Flags {
    pub defined: u32,
    pub preserved: ValueId,
    pub kind: FlagKind,
    pub width: Width,
    pub lhs: ValueId,
    pub rhs: ValueId,
    pub carry: ValueId,
    pub result: ValueId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueKind {
    Constant(u64),
    Parameter { block: BlockId, index: usize },
    Pure(Pure),
    Flags(Flags),
    Load(MemoryId),
    Effect(MemoryId),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Value {
    pub width: Width,
    pub kind: ValueKind,
}

/// Integer state only. Unrepresented architectural state is left untouched;
/// instructions that observe or modify it must exit before execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateMap {
    pub gpr: [ValueId; 8],
    pub flags: ValueId,
}
impl StateMap {
    pub fn values(&self) -> impl Iterator<Item = ValueId> + '_ {
        self.gpr.into_iter().chain([self.flags])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write {
        value: ValueId,
        enabled: Option<ValueId>,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AliasDomain {
    Guest,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryOp {
    pub address: ValueId,
    pub width: Width,
    pub access: Access,
    pub alias: AliasDomain,
    pub input: ValueId,
    pub output: ValueId,
    pub result: Option<ValueId>,
    pub commit: CommitId,
    pub snapshot: SnapshotId,
    /// Reserved handles are indices, never host pointers. G1.1 proves nothing.
    pub proof: Option<DependencyId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitPoint {
    pub instruction: u32,
    pub pc: u32,
    pub next_pc: u32,
    pub units: u32,
    pub subprogress: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependency {
    pub start: u32,
    pub length: u32,
    pub version: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivateSlot {
    pub slot: u32,
    pub value: ValueId,
    pub width: Width,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub state: StateMap,
    pub commit: CommitId,
    /// Roots in the same pure value DAG; no guest memory rereads on recovery.
    pub recipes: Vec<ValueId>,
    pub private_slots: Vec<PrivateSlot>,
    pub dependencies: Vec<DependencyId>,
    pub lease: Option<DependencyId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operation {
    Define(ValueId),
    Memory(MemoryId),
    Commit {
        point: CommitId,
        state: StateMap,
        next_pc: ValueId,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reject {
    Invalid,
    Prefix,
    Segment,
    AddressSize,
    Register,
    Width,
    Atomic,
    Instruction,
    Form,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitReason {
    Complete,
    Unsupported(Reject),
    Indirect,
    Guard,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exit {
    pub state: StateMap,
    pub pc: ValueId,
    pub token: ValueId,
    pub reason: ExitReason,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    pub block: BlockId,
    pub arguments: Vec<ValueId>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Block(Edge),
    Exit(Exit),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Terminator {
    Jump(Target),
    Branch {
        condition: ValueId,
        taken: Target,
        fallthrough: Target,
    },
    Call {
        callee: Target,
        return_pc: u32,
    },
    Return {
        actual: ValueId,
        expected: u32,
        matched: Edge,
        mismatch: Exit,
    },
    Guard {
        condition: ValueId,
        passed: Edge,
        failed: SnapshotId,
        token: ValueId,
    },
    Exit(Exit),
}
impl Terminator {
    pub fn edges(&self) -> Vec<&Edge> {
        match self {
            Self::Jump(Target::Block(e))
            | Self::Call {
                callee: Target::Block(e),
                ..
            } => vec![e],
            Self::Branch {
                taken, fallthrough, ..
            } => [taken, fallthrough]
                .into_iter()
                .filter_map(|t| match t {
                    Target::Block(e) => Some(e),
                    _ => None,
                })
                .collect(),
            Self::Return { matched, .. } => vec![matched],
            Self::Guard { passed, .. } => vec![passed],
            _ => vec![],
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub entry_state: StateMap,
    pub entry_token: ValueId,
    pub inline_depth: usize,
    pub parameters: Vec<ValueId>,
    pub operations: Vec<Operation>,
    pub terminator: Terminator,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    pub entry: BlockId,
    pub blocks: Vec<Block>,
    pub values: Vec<Value>,
    pub memory: Vec<MemoryOp>,
    pub commits: Vec<CommitPoint>,
    pub snapshots: Vec<Snapshot>,
    pub recovery: Vec<super::recovery::Dag>,
    pub dependencies: Vec<Dependency>,
    pub semantic_version: u32,
    pub inline_limit: usize,
    pub inline_budget: usize,
}

impl ValueKind {
    pub fn inputs(&self) -> Vec<ValueId> {
        match self {
            Self::Pure(p) => match p {
                Pure::Binary(_, a, b) => vec![*a, *b],
                Pure::Extract { value, .. } | Pure::Extend { value, .. } | Pure::Nonzero(value) => {
                    vec![*value]
                }
                Pure::Insert { base, value, .. } => vec![*base, *value],
                Pure::Select { condition, yes, no } => vec![*condition, *yes, *no],
                Pure::Condition { flags, .. } | Pure::FlagBit { flags, .. } => vec![*flags],
                Pure::FlagPack { bits, rest } => bits.iter().copied().chain([*rest]).collect(),
            },
            Self::Flags(f) => vec![f.preserved, f.lhs, f.rhs, f.carry, f.result],
            _ => vec![],
        }
    }
}

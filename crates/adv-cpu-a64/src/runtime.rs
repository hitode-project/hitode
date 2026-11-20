//! The execution thread's runtime context and its generated-code ABI.
//!
//! Fixed entry inputs occupy the first cache line. Each admitted execution
//! gets a private runtime with its own countdown, exit and recovery data.
//! Compile-time offsets let the emitters use the same layout as the owner.

use std::mem::offset_of;

use adv_cpu_core::{CpuState, floating::ValueFrame};
#[cfg(native)]
use adv_cpu_core::{IndirectBranch, IndirectBranchKind, Memory};

use crate::asm::Asm;
use crate::encode::Size::W;
use crate::encode::*;

/// What `x29` points at while translated code runs. The fixed entry inputs
/// and the saved host SP occupy the first cache line.
#[repr(C, align(64))]
#[derive(Debug)]
pub struct Runtime {
    pub cpu: *mut CpuState,
    /// Guest window base, 4 GiB aligned: `x27`.
    pub window: u64,
    /// Entry shadow stack (also used during dispatch) or worker data alias.
    pub shadow: u64,
    /// Initial safepoint countdown: `x9`.
    pub countdown: u64,
    /// Host stack pointer inside `enter`'s frame, for helpers and `exit`.
    pub host_sp: u64,
    /// `helper(runtime, eip, link) -> unit or 0`, called by `dispatch`.
    pub helper: u64,
    /// Owner data for `helper`.
    pub owner: u64,
    /// The indirect table: 65536 entries of {guest EIP, unit}.
    pub table: u64,
    /// The empty shadow stack.
    pub shadow_top: u64,
    /// Where `shadow_reset` resumes.
    pub resume: u64,
    /// Performs checked scalar accesses and transfers vectors through the
    /// operand frame. A refusal returns the recovery registers and continuation.
    pub access: u64,
    /// The page class table: a byte per guest page, its permissions as
    /// `Memory::native_permissions` has them.
    pub classes: u64,
    /// The ordinary-memory alias and raw permission table, biased to guest zero.
    pub alias: u64,
    pub alias_classes: u64,
    /// Read, write and observed-code masks in groups of 64 guest pages.
    pub plain_pages: u64,
    pub memory_base: u32,
    pub memory_pages: u32,
    /// Read the owning bridge's timestamp without publishing guest state.
    pub timestamp: u64,
    /// `lookup_miss(runtime, eip, branch) -> unit or 0`.
    pub lookup_miss: u64,
    /// `call(runtime, site, countdown) -> status`, called by `call` for
    /// a host call whose site packs EIP, thunk, INT length and recognition:
    /// zero resumes and nonzero carries an exit plus one.
    pub call: u64,
    /// `Memory::code_lines`, biased to guest zero: which 64-byte lines of
    /// each page hold watched code.
    pub code_lines: u64,
    #[cfg(test)]
    pub channel_batches: u64,
    #[cfg(test)]
    pub native_batches: u64,
    #[cfg(test)]
    pub vector_regions: u64,
    /// What direct bodies receive in x0: the window and `classes`.
    pub body: adv_cpu_core::body::BodyContext,
    pub body_value: u32,
    pub body_stack: u32,
    /// Whether proved spin backedges can hand off this execution turn.
    pub spin_yield: u32,
    /// Read at exhausted computation budgets only.
    pub recall: u64,
    pub recall_mask: u32,
    pub computation_pending: u64,
    pub computation_pending_mask: u32,
    pub completed: u64,
    /// The most `completed` an optimized form refills to; past it a
    /// checkpoint leaves, so the owner's run stays within its fuel.
    pub completed_limit: u64,
    /// Nonzero: the proved form leaves when this many returns to its head
    /// have counted it down to zero.
    pub head_exit: u32,
    /// Checkpoints that kept running past a recall to reach the head.
    pub deferred: u32,
    /// Optimized entry parameters, one 16-byte cell per `Func::entry` source.
    pub opt_params: u64,
    /// Fixed-offset scratch that belongs to this generated invocation.
    pub recovery: Recovery,
    /// Nonzero while the owner has a REP continuation.
    pub rep_pending: u32,
    /// The machine's `OfferGate` that head entry blocks read (A64-OPT-8).
    pub offer_gate: u64,
    /// Owner access stub entry that preserves optimized register values.
    pub resident_access: u64,
    /// An observation or a stable patch that returns to the live unit.
    pub plan_observation: u64,
    pub resident_entry_counter: u64,
    pub resident_crossing_counter: u64,
    pub(crate) value: u64,
    pub(crate) value_access: u64,
    pub(crate) locked_access: u64,
    pub(crate) value_frame: ValueFrame,
}

const _: () = assert!(offset_of!(Runtime, host_sp) + size_of::<u64>() <= 64);

impl Runtime {
    pub fn new(cpu: *mut CpuState, window: u64) -> Self {
        assert_eq!(window % (1 << 32), 0, "the window is 4 GiB aligned");
        Self {
            call: 0,
            timestamp: 0,
            #[cfg(test)]
            channel_batches: 0,
            #[cfg(test)]
            native_batches: 0,
            #[cfg(test)]
            vector_regions: 0,
            cpu,
            window,
            shadow: 0,
            countdown: u64::MAX >> 1,
            host_sp: 0,
            helper: 0,
            owner: 0,
            table: 0,
            shadow_top: 0,
            resume: 0,
            access: 0,
            classes: 0,
            alias: 0,
            alias_classes: 0,
            plain_pages: 0,
            memory_base: 0,
            memory_pages: 0,
            code_lines: 0,
            lookup_miss: 0,
            body: adv_cpu_core::body::BodyContext::default(),
            body_value: 0,
            body_stack: 0,
            spin_yield: 0,
            recall: 0,
            recall_mask: 0,
            computation_pending: 0,
            computation_pending_mask: 0,
            completed: 0,
            completed_limit: u64::MAX,
            head_exit: 0,
            deferred: 0,
            opt_params: 0,
            recovery: Recovery::default(),
            rep_pending: 0,
            offer_gate: 0,
            resident_access: 0,
            plan_observation: 0,
            resident_entry_counter: 0,
            resident_crossing_counter: 0,
            value: 0,
            value_access: 0,
            locked_access: 0,
            value_frame: ValueFrame::default(),
        }
    }
}

/// Host state shared by optimized exits and signal-context capture.
#[repr(C)]
#[derive(Clone, Debug, Default)]
pub struct RegisterDump {
    pub x: [u64; 31],
    pub v: [u128; 32],
    pub sp: u64,
}

/// Raw Tier-1 registers that recovery may need at an inline entry.
#[repr(C)]
#[derive(Debug, Default)]
pub struct OptEntryState {
    pub regs: [u32; 8],
    pub nzcv: u64,
    pub pf: u64,
    pub af: u64,
    pub df: u64,
}

/// Registers used by a precise fault's side-table recipe.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct FaultRegisters {
    pub simd: [[u8; 16]; 16],
    pub temps: [u64; 2],
}

#[cfg(native)]
#[repr(C)]
pub(crate) struct AccessResult {
    /// The loaded scalar, or the x87 publication continuation on refusal.
    pub value: u64,
    /// Null on success. On refusal the checked-access stub captures registers here.
    pub recovery: *mut FaultRegisters,
}

/// Optimized recovery payload. Generated code copies spills out before
/// unwinding its stack. Only a recovery exit exposes this data to Rust.
#[repr(C)]
#[derive(Debug, Default)]
pub struct Recovery {
    pub entry: OptEntryState,
    pub dump: RegisterDump,
    pub saved: u64,
    pub cells: u64,
    pub record: u32,
    pub recalled: u32,
    pub head: u32,
    pub version: u32,
    pub detail: u64,
}

/// An event at a precise guest instruction boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// Instruction fetch failed or the encoding was invalid.
    Fetch,
    /// TF or RF requires an exact instruction boundary.
    DebugFlags,
    /// The fault capture and side table identify the instruction to replay.
    Replay,
    CheckedAccess,
    /// A waiting x87 instruction has an unmasked exception pending.
    PendingX87,
    /// An event answered by the guest system profile.
    System,
    /// An architectural exception, with any state changes already committed.
    Exception(u8),
    /// A core semantic gap, with the decoded instruction form.
    Unsupported(u16),
    /// A software interrupt completed; the owner supplies the trap action.
    Software {
        vector: u8,
        len: u8,
    },
}

/// A plan decision that must be applied after leaving generated code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanEvent {
    BodyStack,
    ResidentRecovery,
    DiscoverBackEdge,
    EnvironmentMiss,
    OfferReady,
    /// The high word names the resident head, independently of the exit EIP.
    Probation(u8),
    /// A detached computation transferred indirectly. The high word holds the source EIP.
    WorkerBranch(u8),
}

#[cfg(native)]
impl PlanEvent {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::BodyStack => "body_stack",
            Self::ResidentRecovery => "resident_recovery",
            Self::DiscoverBackEdge => "external_discovery",
            Self::EnvironmentMiss => "environment_miss",
            Self::OfferReady => "offer_ready",
            Self::Probation(_) => "probation",
            Self::WorkerBranch(_) => "worker_branch",
        }
    }
}

pub(crate) const DISCOVER_INTERNAL: u16 = 0;
pub(crate) const RESIDENT_LINK_SAMPLE: u16 = 1;
#[cfg(native)]
pub(crate) const RESIDENT_VERSION_MISS: u16 = 3;
#[cfg(native)]
pub(crate) const CHECK_OFFER: u16 = 2;

/// The generated entry point's return registers, x0 through x3.
#[cfg(native)]
pub(crate) struct RawExit {
    pub reason: u64,
    /// The countdown minus completed refills, which can wrap below zero.
    pub remaining: u64,
    pub shadow: u64,
    pub deferred: u64,
}

#[cfg(native)]
#[derive(Clone, Copy)]
pub(crate) struct Enter(pub usize);

#[cfg(native)]
impl Enter {
    /// Enters a published stub and unit with the platform call-preserved map.
    pub(crate) unsafe fn run(self, runtime: *mut Runtime, unit: usize) -> RawExit {
        let (reason, remaining, shadow, deferred);
        // SAFETY: the caller keeps the generated code and runtime live. Both
        // entry stubs preserve the C ABI and return four caller-saved words.
        unsafe {
            std::arch::asm!(
                "blr x16",
                in("x16") self.0,
                inlateout("x0") runtime => reason,
                inlateout("x1") unit => remaining,
                lateout("x2") shadow,
                lateout("x3") deferred,
                clobber_abi("C"),
            );
        }
        RawExit {
            reason,
            remaining,
            shadow,
            deferred,
        }
    }
}

pub(crate) const REP_BUDGET: u16 = 0x3000;
pub(crate) const VALUE_MODEL_DISPATCH: u16 = 1;

#[cfg(native)]
impl RawExit {
    pub(crate) fn commit_repeat(&self, cpu: &mut CpuState) {
        if self.reason as u32 & !0xff == u32::from(REP_BUDGET) {
            adv_cpu_core::strings::commit_budget_compare(
                cpu,
                self.reason as u8,
                (self.reason >> 32) as u32,
            );
        }
    }
}

/// Why translated code returned to the owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// Look up or translate the unit at EIP.
    Dispatch,
    Plan(PlanEvent),
    Event(Event),
    /// A completed service call requests an execution-owner yield.
    Yield,
    /// A completed spin backedge requests a productive handoff.
    Reschedule,
    Recall,
    Computation,
    /// An ordinary unit exhausted its instruction budget at EIP. The owner
    /// samples EIP, then dispatches.
    Budget,
    /// An optimized record and register dump await materialization.
    Optimized,
    /// Owner dispatch selected a first visit in the core interpreter.
    FirstVisit,
    /// Owner dispatch selected a mapping whose writes cannot invalidate code.
    Untracked,
}

impl Exit {
    pub const fn word(self) -> u16 {
        match self {
            Self::Event(Event::Fetch) => 0,
            Self::Dispatch => 1,
            Self::Event(Event::Replay) => 2,
            Self::Event(Event::DebugFlags) => 3,
            Self::Yield => 4,
            Self::Reschedule => 5,
            Self::Recall => 6,
            Self::Computation => 7,
            Self::Budget => 8,
            Self::Optimized => 9,
            Self::FirstVisit => 10,
            Self::Untracked => 15,
            Self::Event(Event::PendingX87) => 11,
            Self::Plan(PlanEvent::BodyStack) => 12,
            Self::Plan(PlanEvent::ResidentRecovery) => 15,
            Self::Plan(PlanEvent::DiscoverBackEdge) => 16,
            Self::Plan(PlanEvent::EnvironmentMiss) => 18,
            Self::Plan(PlanEvent::OfferReady) => 19,
            Self::Plan(PlanEvent::Probation(version)) => 0x4100 | version as u16,
            Self::Plan(PlanEvent::WorkerBranch(kind)) => 0x4000 | kind as u16,
            Self::Event(Event::CheckedAccess) => 13,
            Self::Event(Event::System) => 14,
            Self::Event(Event::Exception(vector)) => 0x100 | vector as u16,
            Self::Event(Event::Unsupported(code)) => 0x8000 | code,
            Self::Event(Event::Software { vector, len }) => {
                0x2000 | (len as u16) << 8 | vector as u16
            }
        }
    }

    pub const fn raw(self) -> u32 {
        self.word() as u32
    }

    pub fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            0 => Some(Self::Event(Event::Fetch)),
            3 => Some(Self::Event(Event::DebugFlags)),
            1 => Some(Self::Dispatch),
            2 => Some(Self::Event(Event::Replay)),
            4 => Some(Self::Yield),
            5 => Some(Self::Reschedule),
            6 => Some(Self::Recall),
            7 => Some(Self::Computation),
            8 => Some(Self::Budget),
            9 => Some(Self::Optimized),
            11 => Some(Self::Event(Event::PendingX87)),
            12 => Some(Self::Plan(PlanEvent::BodyStack)),
            13 => Some(Self::Event(Event::CheckedAccess)),
            14 => Some(Self::Event(Event::System)),
            15 => Some(Self::Plan(PlanEvent::ResidentRecovery)),
            16 => Some(Self::Plan(PlanEvent::DiscoverBackEdge)),
            18 => Some(Self::Plan(PlanEvent::EnvironmentMiss)),
            19 => Some(Self::Plan(PlanEvent::OfferReady)),
            0x100..=0x1ff => Some(Self::Event(Event::Exception(raw as u8))),
            0x8000..=0xffff => Some(Self::Event(Event::Unsupported((raw & 0x7fff) as u16))),
            0x2000..=0x2fff => Some(Self::Event(Event::Software {
                vector: raw as u8,
                len: (raw >> 8 & 15) as u8,
            })),
            0x3000..=0x30ff => Some(Self::Budget),
            0x4001..=0x4003 => Some(Self::Plan(PlanEvent::WorkerBranch(raw as u8))),
            0x4100..=0x41ff => Some(Self::Plan(PlanEvent::Probation(raw as u8))),
            _ => None,
        }
    }
}

/// A saved guest CPU and its continuation between the owner and workers.
#[derive(Clone)]
pub struct ExecutionContext {
    pub(crate) cpu: CpuState,
    pub(crate) offered: Option<u32>,
    pub(crate) resume: Option<(u32, Exit)>,
    pub(crate) fuel_debt: u64,
}

impl ExecutionContext {
    pub fn cpu(&self) -> &CpuState {
        &self.cpu
    }

    pub fn cpu_mut(&mut self) -> &mut CpuState {
        &mut self.cpu
    }

    #[cfg(native)]
    pub(crate) fn has_resume(&self) -> bool {
        self.resume.is_some_and(|(pc, _)| pc == self.cpu.eip)
    }

    #[cfg(native)]
    pub(crate) fn finish_computation(&mut self, exit: Exit) {
        self.offered = (exit == Exit::Recall).then_some(self.cpu.eip);
        self.resume = matches!(exit, Exit::Event(_)).then_some((self.cpu.eip, exit));
    }
}

/// Indirect table entries; the slot folds high EIP bits into the low bits.
pub const TABLE_LEN: usize = 1 << 16;

/// The indirect table slot of `eip`.
pub const fn table_slot(eip: u32) -> usize {
    ((eip ^ (eip >> 16)) as usize) & (TABLE_LEN - 1)
}

/// Shadow entries above the top that never match, for `ret_miss` to read.
pub const SHADOW_SENTINELS: usize = 4;

/// An `access` kind is the width in bytes, with these bits.
pub const ACCESS_STORE: u32 = 32;
pub const ACCESS_SIGNED: u32 = 64;

pub(crate) const fn field(offset: usize) -> u32 {
    offset as u32
}

pub(crate) const TABLE: u32 = field(offset_of!(Runtime, table));
pub(crate) const TIMESTAMP: u32 = field(offset_of!(Runtime, timestamp));
#[cfg(test)]
pub(crate) const CHANNEL_BATCHES: u32 = field(offset_of!(Runtime, channel_batches));
#[cfg(test)]
pub(crate) const NATIVE_BATCHES: u32 = field(offset_of!(Runtime, native_batches));
pub(crate) const CLASSES: u32 = field(offset_of!(Runtime, classes));
pub(crate) const BODY: u32 = field(offset_of!(Runtime, body));
pub(crate) const BODY_VALUE: u32 = field(offset_of!(Runtime, body_value));
pub(crate) const BODY_CLASSES: u32 =
    BODY + offset_of!(adv_cpu_core::body::BodyContext, classes) as u32;
pub(crate) const BODY_STACK: u32 = field(offset_of!(Runtime, body_stack));
pub(crate) const SPIN_YIELD: u32 = field(offset_of!(Runtime, spin_yield));
pub(crate) const RECALL: u32 = field(offset_of!(Runtime, recall));
pub(crate) const RECALL_MASK: u32 = field(offset_of!(Runtime, recall_mask));
pub(crate) const COMPUTATION_PENDING: u32 = field(offset_of!(Runtime, computation_pending));
pub(crate) const COMPUTATION_PENDING_MASK: u32 =
    field(offset_of!(Runtime, computation_pending_mask));
pub(crate) const COMPLETED: u32 = field(offset_of!(Runtime, completed));
pub(crate) const COMPLETED_LIMIT: u32 = field(offset_of!(Runtime, completed_limit));
pub(crate) const HEAD_EXIT: u32 = field(offset_of!(Runtime, head_exit));
pub(crate) const RECOVERY_RECALLED: u32 = field(offset_of!(Runtime, recovery.recalled));
pub(crate) const RECOVERY_CELLS: u32 = field(offset_of!(Runtime, recovery.cells));
pub(crate) const RECOVERY_HEAD: u32 = field(offset_of!(Runtime, recovery.head));
pub(crate) const OFFER_GATE: u32 = field(offset_of!(Runtime, offer_gate));
pub(crate) const DEFERRED: u32 = field(offset_of!(Runtime, deferred));
/// Inner checkpoints a recall waits through before it leaves where it is:
/// a few outer iterations of a hot computation.
pub(crate) const RECALL_GRACE: u32 = 64;
pub(crate) const COMPUTATION_SLICE: u32 = 1 << 16;
pub(crate) const ALIAS: u32 = field(offset_of!(Runtime, alias));
pub(crate) const ALIAS_CLASSES: u32 = field(offset_of!(Runtime, alias_classes));
pub(crate) const CODE_LINES: u32 = field(offset_of!(Runtime, code_lines));
pub(crate) const CPU_STATE: u32 = field(offset_of!(Runtime, cpu));
pub(crate) const EIP: u32 = field(offset_of!(CpuState, eip));
pub(crate) const CLOCK_READ: u32 = field(offset_of!(CpuState, clock_read));
pub(crate) const FS_BASE: u32 = field(offset_of!(CpuState, fs_base));
pub(crate) const XMM: u32 = field(offset_of!(CpuState, xmm));
pub(crate) const MMX: u32 = field(offset_of!(CpuState, mmx));
pub(crate) const X87_REGS: u32 = field(offset_of!(CpuState, x87.regs));
pub(crate) const X87_STATUS: u32 = field(offset_of!(CpuState, x87.status));
pub(crate) const X87_TAG: u32 = field(offset_of!(CpuState, x87.tag));
pub(crate) const X87_CONTROL: u32 = field(offset_of!(CpuState, x87.control));
pub(crate) const X87_OPCODE: u32 = field(offset_of!(CpuState, x87.opcode));
pub(crate) const X87_IP: u32 = field(offset_of!(CpuState, x87.ip));
pub(crate) const MMX_MODE: u32 = field(offset_of!(CpuState, mmx_mode));
pub(crate) const MXCSR: u32 = field(offset_of!(CpuState, mxcsr));
pub(crate) const EFLAGS: u32 = field(offset_of!(CpuState, eflags));
pub(crate) const SEGMENTS: u32 = field(offset_of!(CpuState, segments.selectors));

/// CF PF AF ZF SF DF OF: the flags the stubs map.
pub const MAPPED_FLAGS: u32 = 0xcd5;

#[cfg(native)]
pub(crate) const RECOVERY_VERSION: u32 = field(offset_of!(Runtime, recovery.version));
pub(crate) const PLAN_OBSERVATION: u32 = field(offset_of!(Runtime, plan_observation));

/// Byte offsets from a backward link's patch word.
pub(crate) const BACK_COUNT: usize = 20;
pub(crate) const SENSOR_BYTES: usize = 5 * 4;
pub(crate) const BACK_UNIT: usize = BACK_COUNT + SENSOR_BYTES;
pub(crate) const BACK_COUNTER: usize = BACK_UNIT + 4;
pub(crate) const BACK_END: usize = BACK_COUNTER + 8 + 16;

/// A64A-PLACE-3. Only scratch registers are live here. Neither the active
/// countdown nor its one-word replacement changes the guest NZCV image.
pub(crate) fn emit_sensor(
    asm: &mut Asm,
    counter: crate::asm::Label,
    address: Reg,
    value: Reg,
    trigger: crate::asm::Label,
) {
    asm.ldr_literal(address, counter);
    asm.emit_all(&[
        ldr(W, value, address, 0),
        sub_imm(W, value, value, 1, false),
        str(W, value, address, 0),
    ]);
    asm.cbz(W, false, value, trigger);
}

pub(crate) const VALUE_ACCESS_REFUSED: u32 = 1 << 9;
pub(crate) const VALUE_STORE_READY: u32 = 1 << 8;
pub(crate) const VALUE_UNSUPPORTED: u32 = 1 << 15;
pub(crate) const VALUE_ADDRESS: u32 = VALUE_FRAME + field(offset_of!(ValueFrame, address));
pub(crate) const VALUE_OPERAND: u32 = VALUE_FRAME + field(offset_of!(ValueFrame, operand));
pub(crate) const VALUE_XMM: u32 = VALUE_FRAME + field(offset_of!(ValueFrame, xmm));
pub(crate) const VALUE_MMX: u32 = VALUE_FRAME + field(offset_of!(ValueFrame, mmx));
pub(crate) const VALUE_REGS: u32 = VALUE_FRAME + field(offset_of!(ValueFrame, regs));
pub(crate) const VALUE_EFLAGS: u32 = VALUE_FRAME + field(offset_of!(ValueFrame, eflags));
pub(crate) const VALUE_MXCSR: u32 = VALUE_FRAME + field(offset_of!(ValueFrame, mxcsr));
const VALUE_FRAME: u32 = field(offset_of!(Runtime, value_frame));

/// Host base of the alias data view relative to guest address zero.
#[cfg(native)]
pub(crate) fn alias_base(memory: &Memory) -> u64 {
    (memory.ptr.as_ptr() as usize).wrapping_sub(memory.base as usize) as u64
}

/// Publish the source and kind saved in the generated branch payload.
#[cfg(native)]
pub(crate) fn record_branch(cpu: &mut CpuState, branch: u64, target: u32) {
    if branch != 0 {
        cpu.last_indirect_branch = IndirectBranch {
            source: branch as u32,
            target,
            kind: match branch >> 32 {
                1 => IndirectBranchKind::Call,
                2 => IndirectBranchKind::Jump,
                _ => IndirectBranchKind::Return,
            },
        };
    }
}

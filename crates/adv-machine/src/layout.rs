//! Guest address-space layout for the host-owned regions.
//!
//! Images load at their preferred base (TL2026 has no relocations), so everything the host
//! owns lives where Win32 x86 processes never put images: 0x1000_0000..0x7000_0000 is the
//! dynamic region (heaps, stacks, VirtualAlloc), 0x7e00_0000.. holds host code/tables, and the
//! TEB/PEB sit where Windows puts them.

pub const PAGE: u32 = 0x1000;
/// Windows' allocation granularity; VirtualAlloc and stacks are aligned to this.
pub const GRANULE: u32 = 0x1_0000;

/// Dynamic allocations (heaps, stacks, VirtualAlloc, resource DLL mappings).
pub const DYN_START: u32 = 0x1000_0000;
pub const DYN_END: u32 = 0x7000_0000;

/// Hookable host thunks with a relocatable trap token, one per 32-byte slot.
pub const THUNK_BASE: u32 = 0x7e00_0000;
pub const THUNK_STRIDE: u32 = 32;
pub const THUNK_CAP: u32 = 0x4000;
pub const THUNK_SIZE: u32 = THUNK_STRIDE * THUNK_CAP;

/// Where a parked guest's EIP points. Mapped, never executed.
pub const PARK_ADDR: u32 = 0x7e10_0000;
/// One page for the GDT (never visible to guest code).
pub const GDT_ADDR: u32 = 0x7e11_0000;

/// Fake HMODULEs for host-implemented modules (kernel32, pal.dll, ...): one granule each,
/// reserved but unmapped.
pub const HOST_MODULE_BASE: u32 = 0x7e40_0000;
pub const HOST_MODULE_CAP: u32 = 0x40;

/// Regions the host owns; reserved in the VM so images never load over them.
pub const RESERVED: &[(u32, u32)] = &[
    (0, 0x1_0000),           // null guard
    (THUNK_BASE, 0x20_0000), // thunks, park page, GDT
    (HOST_MODULE_BASE, HOST_MODULE_CAP * GRANULE),
    (0x7ff0_0000, 0x10_0000), // TEBs, PEB, KUSER_SHARED_DATA
];

pub const PEB_ADDR: u32 = 0x7ffd_f000;
/// TEB of context `i` is at `TEB_TOP - i * PAGE`.
pub const TEB_TOP: u32 = 0x7ffd_e000;
pub const MAX_CONTEXTS: u32 = 64;

pub const DEFAULT_STACK: u32 = 0x10_0000;

pub const fn thunk_addr(index: u32) -> u32 {
    THUNK_BASE + index * THUNK_STRIDE
}

pub const fn teb_addr(ctx: u32) -> u32 {
    TEB_TOP - ctx * PAGE
}

/// TEB/NT_TIB field offsets (x86).
pub mod teb {
    pub const EXCEPTION_LIST: u32 = 0x00;
    pub const STACK_BASE: u32 = 0x04;
    pub const STACK_LIMIT: u32 = 0x08;
    pub const SELF: u32 = 0x18;
    pub const PROCESS_ID: u32 = 0x20;
    pub const THREAD_ID: u32 = 0x24;
    pub const TLS_POINTER: u32 = 0x2c;
    pub const PEB: u32 = 0x30;
    pub const LAST_ERROR: u32 = 0x34;
    pub const TLS_SLOTS: u32 = 0xe10;
    pub const TLS_SLOT_COUNT: u32 = 64;
}

/// PEB field offsets (x86).
pub mod peb {
    pub const IMAGE_BASE: u32 = 0x08;
    pub const PROCESS_HEAP: u32 = 0x18;
}

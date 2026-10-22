//! The NTSTATUS values the platform returns.

pub type Status = u32;

pub const SUCCESS: Status = 0;
pub const NOT_IMPLEMENTED: Status = 0xc000_0002;
pub const INVALID_INFO_CLASS: Status = 0xc000_0003;
pub const INFO_LENGTH_MISMATCH: Status = 0xc000_0004;
pub const ACCESS_VIOLATION: Status = 0xc000_0005;
pub const INVALID_HANDLE: Status = 0xc000_0008;
pub const INVALID_PARAMETER: Status = 0xc000_000d;
pub const NO_MEMORY: Status = 0xc000_0017;
pub const CONFLICTING_ADDRESSES: Status = 0xc000_0018;
pub const NOT_MAPPED_VIEW: Status = 0xc000_0019;
pub const UNABLE_TO_FREE_VM: Status = 0xc000_001a;
pub const NOT_COMMITTED: Status = 0xc000_002d;
pub const INVALID_PAGE_PROTECTION: Status = 0xc000_0045;
pub const FREE_VM_NOT_AT_BASE: Status = 0xc000_009f;
pub const MEMORY_NOT_ALLOCATED: Status = 0xc000_00a0;

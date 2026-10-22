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
pub const NO_SUCH_FILE: Status = 0xc000_000f;
pub const OBJECT_TYPE_MISMATCH: Status = 0xc000_0024;
pub const OBJECT_NAME_INVALID: Status = 0xc000_0033;
pub const OBJECT_NAME_NOT_FOUND: Status = 0xc000_0034;
pub const OBJECT_PATH_NOT_FOUND: Status = 0xc000_003a;
pub const INVALID_IMAGE_FORMAT: Status = 0xc000_007b;
pub const BAD_DEVICE_TYPE: Status = 0xc000_00cb;

/// Why a system call did not complete: a status for the guest, or a case
/// the platform does not handle yet, which stops the machine.
#[derive(Debug)]
pub enum Error {
    Status(Status),
    Unsupported(String),
}

impl From<Status> for Error {
    fn from(status: Status) -> Self {
        Error::Status(status)
    }
}

/// Fails with `status` unless it reports success.
pub fn check(status: Status) -> Result<(), Error> {
    if status == SUCCESS {
        Ok(())
    } else {
        Err(Error::Status(status))
    }
}

/// The outcome of a system call body.
pub fn finish(result: Result<(), Error>) -> adv_machine::Outcome {
    match result {
        Ok(()) => adv_machine::Outcome::Return(SUCCESS),
        Err(Error::Status(status)) => adv_machine::Outcome::Return(status),
        Err(Error::Unsupported(what)) => adv_machine::Outcome::Fail(what),
    }
}

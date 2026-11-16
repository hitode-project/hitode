//! Guest argument faults. DirectX runtimes validate the pointers a caller
//! passes and answer an inaccessible one with `E_INVALIDARG`
//! (`DDERR_INVALIDPARAMS`, `DSERR_INVALIDPARAM`) instead of faulting, so a
//! front reports such a fault as that error, not as a failed run.

use adv_cpu::backend::MemFault;
use adv_device::surface::SurfaceMemory;
use adv_machine::{Env, Outcome};
use std::borrow::Cow;

pub const E_INVALIDARG: u32 = 0x8007_0057;

/// Why a front call produced no value.
#[derive(Debug)]
pub enum CallError {
    /// A caller-supplied pointer does not reach accessible guest memory.
    Argument(String),
    /// The host cannot continue the call.
    Fail(String),
}

impl From<String> for CallError {
    fn from(error: String) -> Self {
        Self::Fail(error)
    }
}

impl From<&str> for CallError {
    fn from(error: &str) -> Self {
        Self::Fail(error.to_owned())
    }
}

impl From<MemFault> for CallError {
    fn from(fault: MemFault) -> Self {
        Self::Argument(fault.to_string())
    }
}

impl CallError {
    /// The call's outcome: `E_INVALIDARG` for an argument fault, kept visible
    /// to diagnostics because callers often ignore the result.
    pub fn outcome(self, env: &mut Env<'_>) -> Outcome {
        match self {
            Self::Argument(fault) => {
                let thunk = env.sys.thunks.get(env.thunk);
                let api = format!("{}!{}", thunk.module, thunk.name);
                adv_abi::warn_unavailable(
                    env,
                    &api,
                    &format!("inaccessible argument ({fault}); returning E_INVALIDARG"),
                );
                Outcome::Return(E_INVALIDARG)
            }
            Self::Fail(error) => Outcome::Fail(error),
        }
    }
}

/// Surface-memory access to caller-supplied pointers.
pub trait Arguments {
    fn arg_read(&self, addr: u32, len: usize) -> Result<Cow<'_, [u8]>, CallError>;
    fn arg_u32(&self, addr: u32) -> Result<u32, CallError>;
    fn arg_write(&mut self, addr: u32, data: &[u8]) -> Result<(), CallError>;
    fn arg_put_u32(&mut self, addr: u32, value: u32) -> Result<(), CallError>;
}

impl<M: SurfaceMemory + ?Sized> Arguments for M {
    fn arg_read(&self, addr: u32, len: usize) -> Result<Cow<'_, [u8]>, CallError> {
        self.read(addr, len).map_err(CallError::Argument)
    }
    fn arg_u32(&self, addr: u32) -> Result<u32, CallError> {
        self.u32(addr).map_err(CallError::Argument)
    }
    fn arg_write(&mut self, addr: u32, data: &[u8]) -> Result<(), CallError> {
        self.write(addr, data).map_err(CallError::Argument)
    }
    fn arg_put_u32(&mut self, addr: u32, value: u32) -> Result<(), CallError> {
        self.put_u32(addr, value).map_err(CallError::Argument)
    }
}

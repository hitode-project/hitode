//! Nonblocking host calls with memory and selected process data, never CPU control.
use adv_cpu::backend::{Guest, GuestExt, MemFault, Reg};

use crate::{Clock, Env, Outcome};

pub type LeafHandler = fn(&mut LeafEnv<'_>) -> u64;

pub struct LeafEnv<'a> {
    cpu: &'a mut dyn Guest,
    arguments: Option<&'a [u32]>,
    pub clock: &'a Clock,
    pub teb: u32,
    pub tid: u32,
    edx: u32,
    wide: bool,
}

impl<'a> LeafEnv<'a> {
    pub(crate) fn new(env: &'a mut Env<'_>) -> Self {
        let (teb, tid) = (env.ctx().teb, env.ctx().tid);
        let edx = env.cpu.reg(Reg::Edx);
        let arguments = env
            .sys
            .invocation_arguments
            .as_ref()
            .filter(|args| args.context == env.context && args.thunk == env.thunk)
            .map(|args| &*args.values);
        Self {
            cpu: env.cpu,
            arguments,
            clock: &env.sys.clock,
            teb,
            tid,
            edx,
            wide: true,
        }
    }

    pub fn arg(&self, i: u32) -> u32 {
        if let Some(arguments) = self.arguments {
            return arguments.get(i as usize).copied().unwrap_or(0);
        }
        self.cpu.arg(i).unwrap_or_else(|error| {
            log::warn!("arg {i} unreadable: {error}");
            0
        })
    }

    pub fn read(&self, addr: u32, bytes: &mut [u8]) -> Result<(), MemFault> {
        self.cpu.read(addr, bytes)
    }

    pub fn write(&mut self, addr: u32, bytes: &[u8]) -> Result<(), MemFault> {
        self.cpu.write(addr, bytes)
    }

    pub fn read_u32(&self, addr: u32) -> Result<u32, MemFault> {
        self.cpu.read_u32(addr)
    }

    pub fn write_u32(&mut self, addr: u32, value: u32) -> Result<(), MemFault> {
        self.cpu.write_u32(addr, value)
    }

    /// Preserve EDX for the same 32-bit return as the ordinary handler.
    pub fn return32(&mut self, value: u32) -> u64 {
        self.wide = false;
        u64::from(self.edx) << 32 | u64::from(value)
    }
}

pub fn trap(env: &mut Env<'_>) -> Outcome {
    let handler = env.sys.thunks.get(env.thunk).f.leaf.expect("leaf binding");
    let mut env = LeafEnv::new(env);
    let value = handler(&mut env);
    if env.wide {
        Outcome::Return64(value)
    } else {
        Outcome::Return(value as u32)
    }
}

#[cfg(all(test, target_arch = "aarch64"))]
mod tests;

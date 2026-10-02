//! Shared host boundary for native and interpreted execution. Handlers cannot re-enter
//! the CPU; callbacks and waits are returned as explicit scheduler actions.
use crate::{CpuState, Memory, READ, WRITE};
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Convention {
    Cdecl(u8),
    Stdcall(u8),
}
impl Convention {
    pub fn argc(self) -> usize {
        match self {
            Self::Cdecl(n) | Self::Stdcall(n) => n as usize,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ApiSpec {
    pub id: u32,
    pub module: String,
    pub name: String,
    pub entry: u32,
    pub convention: Convention,
}

impl ApiSpec {
    pub fn new(id: u32, module: &str, name: &str, entry: u32, convention: Convention) -> Self {
        Self {
            id,
            module: module.into(),
            name: name.into(),
            entry,
            convention,
        }
    }
    /// MOV EAX,id is position independent and relocatable by an inline-hook trampoline.
    /// INT carries identity in EAX; RET retains the actual guest ABI/return chain.
    pub fn stub(&self) -> [u8; 16] {
        let mut bytes = [0x90; 16];
        bytes[0] = 0xb8;
        bytes[1..5].copy_from_slice(&self.id.to_le_bytes());
        bytes[5..7].copy_from_slice(&[0xcd, 0x2e]);
        match self.convention {
            Convention::Cdecl(_) | Convention::Stdcall(0) => bytes[7] = 0xc3,
            Convention::Stdcall(n) => {
                bytes[7] = 0xc2;
                bytes[8..10].copy_from_slice(&(u16::from(n) * 4).to_le_bytes());
            }
        }
        bytes
    }
}

pub struct ApiCall<'a> {
    pub spec: &'a ApiSpec,
    pub args: &'a [u32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    Event(u32),
    Until(u64),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resume {
    Returned(u32),
    Signaled(u32),
    Deadline(u64),
}

#[derive(Debug)]
pub enum ApiOutcome {
    Return(u32),
    Callback {
        target: u32,
        args: Vec<u32>,
        convention: Convention,
        continuation: u64,
    },
    Wait {
        wait: Wait,
        continuation: u64,
    },
}

pub trait HostServices {
    fn call(&mut self, call: ApiCall<'_>, memory: &mut ApiMemory<'_>)
    -> Result<ApiOutcome, String>;
    fn resume(
        &mut self,
        token: u64,
        reason: Resume,
        memory: &mut ApiMemory<'_>,
    ) -> Result<ApiOutcome, String> {
        let _ = (token, reason, memory);
        Err("unexpected host continuation".into())
    }
}

/// Guest-facing access checks. No borrowed slice may survive a handler return.
pub struct ApiMemory<'a> {
    pub(crate) inner: &'a mut Memory,
}
impl<'a> ApiMemory<'a> {
    pub fn new(inner: &'a mut Memory) -> Self {
        Self { inner }
    }
    fn check(&self, addr: u32, len: usize, permission: u8) -> Result<(), String> {
        let r = self.inner.committed(addr, len)?;
        if !r.is_empty()
            && self.inner.perms[r.start / 4096..=(r.end - 1) / 4096]
                .iter()
                .any(|p| p & permission == 0)
        {
            return Err(format!("guest access denied at {addr:#x} for {len} bytes"));
        }
        Ok(())
    }
    pub fn read(&self, addr: u32, len: usize) -> Result<&[u8], String> {
        self.check(addr, len, READ)?;
        self.inner.read(addr, len)
    }
    pub fn write(&mut self, addr: u32, bytes: &[u8]) -> Result<(), String> {
        self.check(addr, bytes.len(), WRITE)?;
        self.inner.write(addr, bytes)
    }
    pub fn u32(&self, addr: u32) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.read(addr, 4)?.try_into().unwrap()))
    }
    pub fn put_u32(&mut self, addr: u32, value: u32) -> Result<(), String> {
        self.write(addr, &value.to_le_bytes())
    }
    pub fn writable(&self, addr: u32, len: usize) -> Result<(), String> {
        self.check(addr, len, WRITE)
    }
    pub fn copy(&mut self, src: u32, dst: u32, len: usize) -> Result<(), String> {
        self.check(src, len, READ)?;
        self.check(dst, len, WRITE)?;
        let source = self.inner.range(src, len)?;
        let dest = self.inner.range(dst, len)?;
        if !dest.is_empty()
            && self.inner.perms[dest.start / 4096..=(dest.end - 1) / 4096]
                .iter()
                .any(|p| p & crate::EXEC != 0)
        {
            self.inner.touch(dst, len);
        }
        // SAFETY: both ranges checked; ptr::copy supports overlapping surfaces.
        unsafe {
            std::ptr::copy(
                self.inner.ptr.as_ptr().add(source.start),
                self.inner.ptr.as_ptr().add(dest.start),
                len,
            )
        };
        Ok(())
    }
}

pub(crate) struct RejectServices;
impl HostServices for RejectServices {
    fn call(&mut self, call: ApiCall<'_>, _: &mut ApiMemory<'_>) -> Result<ApiOutcome, String> {
        Err(format!(
            "no host services for {}!{}",
            call.spec.module, call.spec.name
        ))
    }
}

pub(crate) struct Bridge<'a> {
    pub apis: &'a BTreeMap<u32, ApiSpec>,
    pub services: &'a mut dyn HostServices,
    pub memory: *mut Memory,
    pub pending: Option<Result<ApiOutcome, String>>,
    pub calls: u64,
    pub raw: Option<&'a mut dyn RawTrap>,
    pub parked: bool,
    pub deadline: Option<std::time::Instant>,
    pub bulk_calls: u64,
    pub bulk_bytes: u64,
}

/// Runtime integration hook. It runs with the CPU paused; true requests a scheduler park.
pub(crate) trait RawTrap {
    fn call(
        &mut self,
        state: &mut CpuState,
        memory: &mut Memory,
        vector: u32,
    ) -> Result<bool, String>;
}

pub(crate) fn call_raw(bridge: &mut Bridge<'_>, state: &mut CpuState, vector: u32) -> u32 {
    let before = unsafe { &*bridge.memory }.epoch;
    let raw = bridge.raw.as_mut().expect("raw trap handler");
    let pc = state.eip;
    bridge.calls += 1;
    let result = catch_unwind(AssertUnwindSafe(|| {
        raw.call(state, unsafe { &mut *bridge.memory }, vector)
    }))
    .unwrap_or_else(|_| Err("runtime trap handler panicked".into()));
    match result {
        Ok(parked) => {
            bridge.parked = parked;
            if parked
                || state.eip != pc
                || state.eflags & (1 << 8) != 0
                || state.x87.status & 0x80 != 0
                || unsafe { &*bridge.memory }.epoch != before
            {
                2
            } else {
                0
            }
        }
        Err(e) => {
            bridge.pending = Some(Err(e));
            1
        }
    }
}

/// C ABI called directly by generated native code. No panic crosses the boundary.
pub(crate) unsafe extern "C" fn host_call(
    bridge: *mut std::ffi::c_void,
    state: *mut CpuState,
) -> u32 {
    // SAFETY: Machine creates both objects for the run; native code is paused here.
    let (bridge, state) = unsafe { (&mut *bridge.cast::<Bridge<'_>>(), &mut *state) };
    let before = unsafe { &*bridge.memory }.epoch;
    if bridge.raw.is_some() {
        return call_raw(bridge, state, 0x2e);
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        let spec = bridge
            .apis
            .get(&state.regs[0])
            .ok_or_else(|| format!("unknown API id {}", state.regs[0]))?;
        let mut args = [0; 16];
        let count = spec.convention.argc();
        let mut memory = ApiMemory {
            inner: unsafe { &mut *bridge.memory },
        };
        for (index, arg) in args[..count].iter_mut().enumerate() {
            *arg = memory.u32(state.regs[4].wrapping_add(4 + index as u32 * 4))?;
        }
        bridge.calls += 1;
        bridge.services.call(
            ApiCall {
                spec,
                args: &args[..count],
            },
            &mut memory,
        )
    }))
    .unwrap_or_else(|_| Err("host handler panicked".into()));
    match result {
        Ok(ApiOutcome::Return(value)) => {
            state.regs[0] = value;
            // A handler may have written code. Do not continue a stale native region.
            if unsafe { &*bridge.memory }.epoch == before {
                0
            } else {
                2
            }
        }
        other => {
            bridge.pending = Some(other);
            1
        }
    }
}

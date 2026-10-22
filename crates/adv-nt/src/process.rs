//! Process startup: what Wine's ntdll.so does before the first instruction
//! of ntdll.dll runs (`start_main_thread` and `init_syscall_frame`).
//!
//! ntdll.dll is mapped at its preferred base and its dispatcher pointers are
//! bound to the platform's thunks. The main thread starts in
//! `LdrInitializeThunk` with the CONTEXT that `NtContinue` later resumes at
//! `RtlUserThreadStart`, as in Wine.

use std::collections::HashMap;

use adv_cpu::backend::{Backend, GuestExt, MemFault, Prot};
use adv_machine::layout::{DEFAULT_STACK, PAGE, teb};
use adv_machine::{CtxId, Machine};
use adv_pe::{ExportTarget, Pe, PeError};

use crate::{context, dispatch, image};

/// TEB.WOW32Reserved: stubs that cannot reach the dispatcher pointer call
/// through this field.
const TEB_SYSCALL_DISPATCHER: u32 = 0xc0;
/// The process block holds the PEB, the debug channel table ntdll.dll reads
/// at `PEB + PAGE`, and the main thread's start CONTEXT.
const BLOCK_SIZE: u32 = 3 * PAGE;
const START_CONTEXT: u32 = 2 * PAGE;

/// x87 control word and MXCSR of a new thread.
const FPU_CONTROL: u16 = 0x27f;
const MXCSR: u32 = 0x1f80;
const EFLAGS: u32 = 0x202;
/// The stack space `init_syscall_frame` leaves above the start ESP.
const STACK_TOP_RESERVE: u32 = 16;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("ntdll.dll: {0}")]
    Pe(#[from] PeError),
    #[error("ntdll.dll: {0}")]
    Image(#[from] image::Error),
    #[error("ntdll.dll exports no {0}")]
    Export(&'static str),
    #[error("no address space for the process block")]
    AddressSpace,
    #[error(transparent)]
    Memory(#[from] MemFault),
    #[error("{0}")]
    Context(String),
}

/// The mapped ntdll.dll.
pub struct Ntdll {
    pub base: u32,
    exports: HashMap<String, u32>,
}

impl Ntdll {
    /// The address of a named export that is not forwarded.
    pub fn export(&self, name: &'static str) -> Result<u32, Error> {
        self.exports.get(name).copied().ok_or(Error::Export(name))
    }
}

pub struct Process {
    pub ntdll: Ntdll,
    pub peb: u32,
    pub main: CtxId,
}

/// Maps ntdll.dll and prepares the main thread, which runs once the machine
/// does. `machine` must use [`dispatch::Platform`].
pub fn start<B: Backend>(machine: &mut Machine<B>, ntdll: &[u8]) -> Result<Process, Error> {
    let pe = Pe::parse(ntdll)?;
    let base = pe.image_base;
    image::map(&mut machine.cpu, &mut machine.sys.vm, &pe, base)?;
    let exports = pe
        .exports()?
        .into_iter()
        .filter_map(|export| match (export.name, export.target) {
            (Some(name), ExportTarget::Rva(rva)) => Some((name, base + rva)),
            _ => None,
        })
        .collect();
    let ntdll = Ntdll { base, exports };

    let syscall = machine.resolve_import(dispatch::MODULE, dispatch::SYSCALL)?;
    let unix_call = machine.resolve_import(dispatch::MODULE, dispatch::UNIX_CALL)?;
    machine
        .cpu
        .write_u32(ntdll.export(dispatch::SYSCALL)?, syscall)?;
    machine
        .cpu
        .write_u32(ntdll.export(dispatch::UNIX_CALL)?, unix_call)?;
    // Any nonzero handle: ntdll.dll only passes it back to the dispatcher.
    machine
        .cpu
        .write(ntdll.export("__wine_unixlib_handle")?, &1u64.to_le_bytes())?;

    let block = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, BLOCK_SIZE, Prot::RW)
        .map_err(|_| Error::AddressSpace)?;
    // Zeroed, the debug channel table is empty and every channel is off.
    let peb = block;
    machine.sys.peb = peb;

    let start_context = block + START_CONTEXT;
    let main = machine.spawn(
        "main",
        ntdll.export("LdrInitializeThunk")?,
        &[start_context, 0, 0, 0],
        DEFAULT_STACK,
    )?;
    let thread = &machine.sys.contexts[main as usize];
    let (teb, stack_base) = (thread.teb, thread.stack_hi);
    machine
        .cpu
        .write_u32(teb + TEB_SYSCALL_DISPATCHER, syscall)?;
    debug_assert_eq!(machine.cpu.read_u32(teb + teb::PEB)?, peb);

    let mut state = machine
        .visit_context(Some(main), |env| env.cpu.capture_architecture())
        .map_err(Error::Context)?
        .map_err(|error| Error::Context(error.to_string()))?;
    state.eip = ntdll.export("RtlUserThreadStart")?;
    // RtlUserThreadStart(entry in EAX, argument in EBX). The main thread's
    // argument is the PEB; its entry comes with the main module, which is
    // not mapped yet.
    state.regs[0] = 0;
    state.regs[3] = peb;
    state.regs[4] = stack_base - STACK_TOP_RESERVE;
    state.eflags = EFLAGS;
    state.x87.control = FPU_CONTROL;
    state.mxcsr = MXCSR;
    machine.cpu.write(start_context, &context::encode(&state))?;

    Ok(Process { ntdll, peb, main })
}

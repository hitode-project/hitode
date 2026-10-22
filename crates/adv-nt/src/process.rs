//! Process startup: what Wine's ntdll.so does before the first instruction
//! of ntdll.dll runs (`start_main_thread` and `init_syscall_frame`).
//!
//! ntdll.dll is mapped at its preferred base and its dispatcher pointers are
//! bound to the platform's thunks. The main thread starts in
//! `LdrInitializeThunk` with the CONTEXT that `NtContinue` later resumes at
//! `RtlUserThreadStart`, as in Wine.

use std::collections::HashMap;
use std::path::Path;

use adv_cpu::backend::{Backend, GuestExt, MemFault};
use adv_machine::layout::{DEFAULT_STACK, PAGE, teb};
use adv_machine::{CtxId, Machine};
use adv_pe::{ExportTarget, Pe, PeError};

use crate::file::Files;
use crate::memory::{self, Kind, Memory, PAGE_GUARD_READWRITE, PAGE_READWRITE};
use crate::params::{self, Startup};
use crate::protocol::InitProcessDoneRequest;
use crate::server::{self, Connection, FirstThread, Link, Progress};
use crate::{context, dispatch, image};

/// TEB.WOW32Reserved: stubs that cannot reach the dispatcher pointer call
/// through this field.
const TEB_SYSCALL_DISPATCHER: u32 = 0xc0;
/// The process block holds the PEB, the debug channel table ntdll.dll reads
/// at `PEB + PAGE`, and the main thread's start CONTEXT.
const BLOCK_SIZE: u32 = 3 * PAGE;
const START_CONTEXT: u32 = 2 * PAGE;

/// Guest threads share one host thread.
const PROCESSORS: u32 = 1;
const VER_PLATFORM_WIN32_NT: u32 = 2;

/// PEB fields (i386) the platform sets, as Wine's `init_peb` and
/// `load_global_options` do.
mod peb {
    pub const PROCESS_PARAMETERS: u32 = 0x10;
    pub const NUMBER_OF_PROCESSORS: u32 = 0x64;
    pub const HEAP_SEGMENT_RESERVE: u32 = 0x78;
    pub const HEAP_SEGMENT_COMMIT: u32 = 0x7c;
    pub const HEAP_DECOMMIT_TOTAL_FREE_THRESHOLD: u32 = 0x80;
    pub const HEAP_DECOMMIT_FREE_BLOCK_THRESHOLD: u32 = 0x84;
    pub const OS_MAJOR_VERSION: u32 = 0xa4;
    pub const OS_MINOR_VERSION: u32 = 0xa8;
    /// With OSCSDVersion, zero, in its high half.
    pub const OS_BUILD_NUMBER: u32 = 0xac;
    pub const OS_PLATFORM_ID: u32 = 0xb0;
}

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
    #[error("server: {0}")]
    Server(#[from] server::Error),
    #[error("{0} failed with status {1:#x}")]
    Status(&'static str, u32),
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
    /// The process and main thread as the server registered them.
    pub thread: FirstThread,
}

/// Connects to the server at `socket`, maps ntdll.dll and prepares the main
/// thread, which runs once the machine does. `machine` must use
/// [`dispatch::Platform`]; `prefix` is the Wine prefix whose drives the
/// process sees; `progress` runs the server whenever the process waits for
/// it.
pub fn start<B: Backend>(
    machine: &mut Machine<B>,
    ntdll: &[u8],
    startup: &Startup<'_>,
    prefix: &Path,
    socket: &Path,
    mut progress: Box<dyn Progress>,
) -> Result<Process, Error> {
    let (mut connection, registered) = Connection::connect(socket, &mut *progress)?;
    machine.sys.pid = registered.pid;
    let pe = Pe::parse(ntdll)?;
    let base = pe.image_base;
    let mut memory = Memory::default();
    image::map(
        &mut machine.cpu,
        &mut machine.sys.vm,
        &mut memory,
        &pe,
        base,
    )?;
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

    let block = memory::allocate_committed(
        &mut machine.cpu,
        &mut machine.sys.vm,
        &mut memory,
        BLOCK_SIZE,
    )
    .ok_or(Error::AddressSpace)?;
    // Zeroed, the debug channel table is empty and every channel is off.
    let peb = block;
    machine.sys.peb = peb;
    let size = params::size(startup);
    let parameters =
        memory::allocate_committed(&mut machine.cpu, &mut machine.sys.vm, &mut memory, size)
            .ok_or(Error::AddressSpace)?;
    let pid = machine.sys.pid;
    machine
        .cpu
        .write(parameters, &params::build(startup, parameters, pid))?;
    for (offset, value) in [
        (peb::PROCESS_PARAMETERS, parameters),
        (peb::NUMBER_OF_PROCESSORS, PROCESSORS),
        (peb::HEAP_SEGMENT_RESERVE, 0x10_0000),
        (peb::HEAP_SEGMENT_COMMIT, 0x1_0000),
        (peb::HEAP_DECOMMIT_TOTAL_FREE_THRESHOLD, 0x1_0000),
        (peb::HEAP_DECOMMIT_FREE_BLOCK_THRESHOLD, 0x1000),
        (peb::OS_MAJOR_VERSION, 10),
        (peb::OS_MINOR_VERSION, 0),
        (peb::OS_BUILD_NUMBER, 19045),
        (peb::OS_PLATFORM_ID, VER_PLATFORM_WIN32_NT),
    ] {
        machine.cpu.write_u32(peb + offset, value)?;
    }

    let start_context = block + START_CONTEXT;
    let main = machine.spawn(
        "main",
        ntdll.export("LdrInitializeThunk")?,
        &[start_context, 0, 0, 0],
        DEFAULT_STACK,
    )?;
    let thread = &machine.sys.contexts[main as usize];
    let (teb, stack_base) = (thread.teb, thread.stack_hi);
    // The machine's stack: a guard page under the committed pages.
    let stack = thread.stack_lo - PAGE;
    let mut pages = vec![Some(PAGE_READWRITE); ((stack_base - stack) / PAGE) as usize];
    pages[0] = Some(PAGE_GUARD_READWRITE);
    memory.insert(stack, Kind::Private, PAGE_READWRITE, pages);
    machine.ext.insert(memory);
    machine
        .cpu
        .write_u32(teb + teb::THREAD_ID, registered.tid)?;
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

    let mut done = InitProcessDoneRequest {
        teb: u64::from(teb),
        peb: u64::from(peb),
        ..Default::default()
    };
    let (reply, _) = connection.call(&mut *progress, &mut done, &[], &mut [])?;
    if reply.header.error != 0 {
        return Err(Error::Status("init_process_done", reply.header.error));
    }
    machine.ext.insert(Files {
        dosdevices: prefix.join("dosdevices"),
    });
    machine.ext.insert(Link {
        connection,
        progress,
    });
    Ok(Process {
        ntdll,
        peb,
        main,
        thread: registered,
    })
}

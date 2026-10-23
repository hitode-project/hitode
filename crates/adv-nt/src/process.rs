//! Process startup, what Wine's ntdll.so does before the first instruction
//! of ntdll.dll runs (`start_main_thread` and `init_syscall_frame`), and
//! process exit.
//!
//! ntdll.dll is mapped at its preferred base and its dispatcher pointers are
//! bound to the platform's thunks. The main thread starts in
//! `LdrInitializeThunk` with the CONTEXT that `NtContinue` later resumes at
//! `RtlUserThreadStart`, as in Wine.

use std::collections::HashMap;
use std::path::Path;

use adv_cpu::backend::{ArchitecturalState, Backend, Guest, GuestExt, MemFault};
use adv_machine::layout::{DEFAULT_STACK, PAGE, teb};
use adv_machine::{CtxId, Env, Machine, Sys};
use adv_pe::{ExportTarget, Pe, PeError};

use crate::file::{self, Files};
use crate::memory::{self, Kind, Memory, PAGE_GUARD_READWRITE, PAGE_READWRITE};
use crate::params::{self, Startup};
use crate::protocol::{InitProcessDoneRequest, TerminateProcessRequest};
use crate::section::{self, View};
use crate::server::{self, Connection, FirstThread, Link, Progress};
use crate::{context, debug, dispatch, exception, image, kuser, nls, status, system};

/// TEB.WOW32Reserved: stubs that cannot reach the dispatcher pointer call
/// through this field.
const TEB_SYSCALL_DISPATCHER: u32 = 0xc0;
/// TEB.ActivationContextStack, its FrameListCache, and the pointer to it.
const TEB_ACTIVATION_CONTEXT_STACK: u32 = 0x184;
const TEB_ACTIVATION_FRAME_LIST: u32 = 0x188;
const TEB_ACTIVATION_CONTEXT_STACK_POINTER: u32 = 0x1a8;
/// TEB.StaticUnicodeString and the 261 characters of its buffer.
const TEB_STATIC_UNICODE_STRING: u32 = 0xbf8;
const TEB_STATIC_UNICODE_BUFFER: u32 = 0xc00;
const STATIC_UNICODE_BUFFER_SIZE: u32 = 261 * 2;
/// A thread's TEB and the debug buffers ntdll.dll keeps after it.
const TEB_PAGES: u32 = 2;
/// The process block holds the PEB and the debug channel table ntdll.dll
/// reads at `PEB + PAGE`.
const BLOCK_SIZE: u32 = 2 * PAGE;

/// Guest threads share one host thread.
pub const PROCESSORS: u32 = 1;
const VER_PLATFORM_WIN32_NT: u32 = 2;

/// PEB fields (i386) the platform sets, as Wine's `init_peb` and
/// `load_global_options` do.
mod peb {
    pub const IMAGE_BASE_ADDRESS: u32 = 0x08;
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
    pub const IMAGE_SUBSYSTEM: u32 = 0xb4;
    pub const IMAGE_SUBSYSTEM_MAJOR: u32 = 0xb8;
    pub const IMAGE_SUBSYSTEM_MINOR: u32 = 0xbc;
}

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const SYNCHRONIZE: u32 = 0x10_0000;
const FILE_SHARE_READ: u32 = 1;
const FILE_SHARE_DELETE: u32 = 4;
const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
const FILE_NON_DIRECTORY_FILE: u32 = 0x40;
/// STANDARD_RIGHTS_REQUIRED, SECTION_QUERY, SECTION_MAP_READ and
/// SECTION_MAP_EXECUTE, as Wine opens images.
const SECTION_ACCESS: u32 = 0xf_0000 | 1 | 4 | 8;
const SEC_IMAGE: u32 = 0x100_0000;
const IMAGE_FILE_DLL: u16 = 0x2000;

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
/// Maps the main image as Wine's `load_main_exe` does: through a section of
/// the file, so the server knows the process's image.
fn load_main_image(env: &mut Env<'_>, image: &str) -> Result<View, Error> {
    let name: Vec<u16> = format!(r"\??\{image}").encode_utf16().collect();
    let failed = |what: &str, error: status::Error| match error {
        status::Error::Status(code) => Error::Status("main image", code),
        status::Error::Unsupported(reason) => Error::Context(format!("{what}: {reason}")),
    };
    let file = file::open_path(
        env,
        name,
        GENERIC_READ | SYNCHRONIZE,
        FILE_SHARE_READ | FILE_SHARE_DELETE,
        FILE_SYNCHRONOUS_IO_NONALERT | FILE_NON_DIRECTORY_FILE,
    )
    .map_err(|error| failed("main image", error))?;
    let mapping = section::create_mapping(
        env,
        SECTION_ACCESS,
        None,
        0,
        memory::PAGE_EXECUTE_READ,
        SEC_IMAGE,
        file,
    );
    server::close(env, file).map_err(|error| failed("main image", error))?;
    let (mapping, _) = mapping.map_err(|error| failed("main image section", error))?;
    let view = section::map_image(env, mapping);
    server::close(env, mapping).map_err(|error| failed("main image", error))?;
    let view = view.map_err(|error| failed("main image view", error))?;
    if view.info.image_charact & IMAGE_FILE_DLL != 0 {
        return Err(Error::Status("main image", status::INVALID_IMAGE_FORMAT));
    }
    Ok(view)
}

pub fn start<B: Backend>(
    machine: &mut Machine<B>,
    ntdll: &[u8],
    startup: &Startup<'_>,
    prefix: &Path,
    socket: &Path,
    mut progress: Box<dyn Progress>,
) -> Result<Process, Error> {
    // Connecting needs the server to run; `progress` drives it.
    let time = server::Time::now(&mut machine.sys.clock);
    let (connection, registered) = Connection::connect(socket, &mut *progress, time)?;
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
    let app_name = startup.image.rsplit(['\\', '/']).next().unwrap_or_default();
    machine.cpu.write(
        block + PAGE,
        &debug::table(startup.debug, app_name, PAGE as usize),
    )?;
    let size = params::size(startup);
    let parameters =
        memory::allocate_committed(&mut machine.cpu, &mut machine.sys.vm, &mut memory, size)
            .ok_or(Error::AddressSpace)?;
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

    // ntdll.dll keeps each thread's debug buffers after its TEB.
    machine.sys.teb_pages = TEB_PAGES;
    let ldr_initialize_thunk = ntdll.export("LdrInitializeThunk")?;
    let main = new_context(
        &mut machine.cpu,
        &mut machine.sys,
        &mut memory,
        ldr_initialize_thunk,
        syscall,
        registered.tid,
        DEFAULT_STACK,
    )?;
    let teb = machine.sys.contexts[main as usize].teb;
    machine.ext.insert(memory);
    let features = machine.cpu.cpuid_features();
    kuser::map(
        &mut machine.cpu,
        features,
        system::physical_pages(),
        PROCESSORS,
    )?;
    debug_assert_eq!(machine.cpu.read_u32(teb + teb::PEB)?, peb);

    machine.ext.insert(nls::Locale(startup.locale));
    machine.ext.insert(Files {
        dosdevices: prefix.join("dosdevices"),
    });
    machine.ext.insert(Link::new(
        connection,
        progress,
        time,
        main,
        registered.tid,
        &mut machine.sys,
    ));
    machine.ext.insert(exception::Dispatchers {
        exception: ntdll.export("KiUserExceptionDispatcher")?,
        apc: ntdll.export("KiUserApcDispatcher")?,
    });
    machine.sys.exception_handler = Some(exception::deliver);
    machine.sys.context_run = Some(kuser::refresh);

    let (view, initial) =
        machine
            .visit_context(Some(main), |env| {
                let mut handles = [0; 3];
                for (index, (fd, access)) in [
                    (startup.stdin, GENERIC_READ),
                    (startup.stdout, GENERIC_WRITE),
                    (startup.stderr, GENERIC_WRITE),
                ]
                .into_iter()
                .enumerate()
                {
                    if let Some(fd) = fd {
                        handles[index] = server::fd_to_handle(env, fd, access | SYNCHRONIZE)
                            .map_err(|error| match error {
                                status::Error::Status(code) => {
                                    Error::Status("standard handle", code)
                                }
                                status::Error::Unsupported(reason) => Error::Context(reason),
                            })?;
                    }
                }
                env.cpu.write(
                    parameters,
                    &params::build(startup, parameters, env.sys.pid, handles),
                )?;
                let view = load_main_image(env, startup.image)?;
                let state = env
                    .cpu
                    .capture_architecture()
                    .map_err(|error| Error::Context(error.to_string()))?;
                Ok::<_, Error>((view, state))
            })
            .map_err(Error::Context)??;
    for (offset, value) in [
        (peb::IMAGE_BASE_ADDRESS, view.base),
        (peb::IMAGE_SUBSYSTEM, view.info.subsystem),
        (
            peb::IMAGE_SUBSYSTEM_MAJOR,
            u32::from(view.info.subsystem_major),
        ),
        (
            peb::IMAGE_SUBSYSTEM_MINOR,
            u32::from(view.info.subsystem_minor),
        ),
    ] {
        machine.cpu.write_u32(peb + offset, value)?;
    }

    let start = ThreadStart {
        initial,
        ldr_initialize_thunk,
        rtl_user_thread_start: ntdll.export("RtlUserThreadStart")?,
        syscall,
    };
    // The main thread's argument is the PEB.
    write_start_context(
        &mut machine.cpu,
        &machine.sys,
        &start,
        main,
        view.base + view.info.entry_point,
        peb,
    )?;
    machine.ext.insert(start);

    let mut done = InitProcessDoneRequest {
        teb: u64::from(teb),
        peb: u64::from(peb),
        ..Default::default()
    };
    let (reply, _) = machine
        .ext
        .get_mut::<Link>()
        .call(main, &mut done, &[], &mut [])?;
    if reply.header.error != 0 {
        return Err(Error::Status("init_process_done", reply.header.error));
    }
    Ok(Process {
        ntdll,
        peb,
        main,
        thread: registered,
    })
}

/// What every thread starts from, as Wine's `call_init_thunk` prepares it.
pub struct ThreadStart {
    /// The registers of a thread that has not run, under the start CONTEXT
    /// of every thread.
    initial: ArchitecturalState,
    ldr_initialize_thunk: u32,
    rtl_user_thread_start: u32,
    /// The system call entry, which each TEB holds.
    syscall: u32,
}

/// Creates the machine context of thread `tid`, which starts in
/// `LdrInitializeThunk` once [`write_start_context`] has written its
/// CONTEXT.
///
/// As Wine's `call_init_thunk`, the start CONTEXT tops the stack, and
/// LdrInitializeThunk(context, 0, 0, 0) is called under it, so the stack
/// ntdll.dll clears before it continues lies below the CONTEXT.
fn new_context(
    cpu: &mut dyn Guest,
    sys: &mut Sys,
    memory: &mut Memory,
    ldr_initialize_thunk: u32,
    syscall: u32,
    tid: u32,
    stack_size: u32,
) -> Result<CtxId, MemFault> {
    let context = sys.spawn(cpu, "thread", ldr_initialize_thunk, &[], stack_size)?;
    let thread = &sys.contexts[context as usize];
    let (teb, stack_base, stack_lo) = (thread.teb, thread.stack_hi, thread.stack_lo);
    // The machine's stack: a guard page under the committed pages.
    let stack = stack_lo - PAGE;
    let mut pages = vec![Some(PAGE_READWRITE); ((stack_base - stack) / PAGE) as usize];
    pages[0] = Some(PAGE_GUARD_READWRITE);
    memory.insert(stack, Kind::Private, PAGE_READWRITE, pages);
    init_teb(cpu, teb, tid, syscall)?;

    let start_context = start_context(stack_base);
    let frame = start_context - 20;
    let return_address = cpu.read_u32(stack_base - STACK_TOP_RESERVE - 4)?;
    for (index, value) in [return_address, start_context, 0, 0, 0]
        .into_iter()
        .enumerate()
    {
        cpu.write_u32(frame + 4 * index as u32, value)?;
    }
    sys.set_start_stack(context, frame);
    Ok(context)
}

fn start_context(stack_base: u32) -> u32 {
    ((stack_base - STACK_TOP_RESERVE) & !3) - context::SIZE as u32
}

/// Writes the start CONTEXT of `context`, which resumes at
/// RtlUserThreadStart(entry in EAX, argument in EBX).
fn write_start_context(
    cpu: &mut dyn Guest,
    sys: &Sys,
    start: &ThreadStart,
    context: CtxId,
    entry: u32,
    argument: u32,
) -> Result<(), MemFault> {
    let stack_base = sys.contexts[context as usize].stack_hi;
    let mut state = start.initial.clone();
    state.eip = start.rtl_user_thread_start;
    state.regs = [0; 8];
    state.regs[0] = entry;
    state.regs[3] = argument;
    state.regs[4] = stack_base - STACK_TOP_RESERVE;
    state.eflags = EFLAGS;
    state.x87.control = FPU_CONTROL;
    state.mxcsr = MXCSR;
    cpu.write(start_context(stack_base), &context::encode(&state))
}

/// Creates thread `tid` of the process, which runs entry(argument) once
/// the loader has attached it.
pub fn create_thread(
    env: &mut Env<'_>,
    tid: u32,
    entry: u32,
    argument: u32,
    stack_size: u32,
) -> Result<CtxId, MemFault> {
    let Env { cpu, sys, ext, .. } = env;
    let start = ext.get::<ThreadStart>();
    let (ldr_initialize_thunk, syscall) = (start.ldr_initialize_thunk, start.syscall);
    let context = new_context(
        *cpu,
        sys,
        ext.get_mut::<Memory>(),
        ldr_initialize_thunk,
        syscall,
        tid,
        stack_size.max(DEFAULT_STACK),
    )?;
    write_start_context(
        *cpu,
        sys,
        ext.get::<ThreadStart>(),
        context,
        entry,
        argument,
    )?;
    Ok(context)
}

/// The TEB fields Wine's `init_teb` sets beyond the machine's, and the
/// thread's server id and system call entry.
fn init_teb(cpu: &mut dyn Guest, teb: u32, tid: u32, syscall: u32) -> Result<(), MemFault> {
    let frames = teb + TEB_ACTIVATION_FRAME_LIST;
    for (offset, value) in [
        (teb::THREAD_ID, tid),
        (TEB_SYSCALL_DISPATCHER, syscall),
        (
            TEB_ACTIVATION_CONTEXT_STACK_POINTER,
            teb + TEB_ACTIVATION_CONTEXT_STACK,
        ),
        (TEB_ACTIVATION_FRAME_LIST, frames),
        (TEB_ACTIVATION_FRAME_LIST + 4, frames),
        (TEB_STATIC_UNICODE_STRING, STATIC_UNICODE_BUFFER_SIZE << 16),
        (
            TEB_STATIC_UNICODE_STRING + 4,
            teb + TEB_STATIC_UNICODE_BUFFER,
        ),
    ] {
        cpu.write_u32(teb + offset, value)?;
    }
    Ok(())
}

/// NtTerminateProcess(process, exit_code): ntdll.dll first ends the other
/// threads (process 0), then the process itself.
pub fn terminate(env: &mut Env<'_>, args: &[u32]) -> adv_machine::Outcome {
    let &[process, exit_code] = args else {
        unreachable!()
    };
    let mut request = TerminateProcessRequest {
        handle: process,
        exit_code: exit_code as i32,
        ..Default::default()
    };
    let reply = match server::request(env, &mut request, &[], &mut []) {
        Ok((reply, _)) => reply,
        Err(error) => return status::finish(Err(error)),
    };
    if reply.self_ == 0 {
        return adv_machine::Outcome::Return(reply.header.error);
    }
    if process == 0 {
        let others = env
            .sys
            .contexts
            .iter()
            .filter(|context| !context.is_done())
            .count()
            > 1;
        if others {
            return adv_machine::Outcome::Fail("ending a process's other threads".into());
        }
        return adv_machine::Outcome::Return(reply.header.error);
    }
    adv_machine::Outcome::ExitProcess(exit_code)
}

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

use crate::environment::Environment;
use crate::file::{self, Files};
use crate::memory::{self, Kind, Memory, PAGE_GUARD_READWRITE, PAGE_READWRITE};
use crate::params::{self, Startup};
use crate::protocol::{InitProcessDoneRequest, TerminateProcessRequest};
use crate::section::{self, View};
use crate::server::{self, Connection, FirstThread, Link, Progress};
use crate::{
    context, debug, dispatch, exception, image, kuser, nls, registry, status, system, win32k,
};

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
    pub const LDR: u32 = 0x0c;
    pub const PROCESS_PARAMETERS: u32 = 0x10;
    pub const API_SET_MAP: u32 = 0x38;
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

/// PEB_LDR_DATA and LDR_DATA_TABLE_ENTRY fields (i386) that ntdll's loader keeps.
mod ldr {
    pub const IN_LOAD_ORDER: u32 = 0x0c;
    pub const DLL_BASE: u32 = 0x18;
    pub const BASE_DLL_NAME: u32 = 0x2c;
}

/// The base of the loaded module `name` (case-insensitive), from the PEB's
/// load-order list as Wine's loader keeps it.
pub fn module_base(cpu: &dyn Guest, peb: u32, name: &str) -> Option<u32> {
    let head = cpu.read_u32(peb + peb::LDR).ok()? + ldr::IN_LOAD_ORDER;
    let mut entry = cpu.read_u32(head).ok()?;
    // The list is bounded by the address space; a cycle that misses the
    // head ends at the bound.
    for _ in 0..65536 {
        if entry == head {
            break;
        }
        if read_unicode_string(cpu, entry + ldr::BASE_DLL_NAME)?.eq_ignore_ascii_case(name) {
            return cpu.read_u32(entry + ldr::DLL_BASE).ok();
        }
        entry = cpu.read_u32(entry).ok()?;
    }
    None
}

/// The address of the named export of the image mapped at `base`, if it
/// exports the name and does not forward it.
pub fn module_export(cpu: &dyn Guest, base: u32, name: &str) -> Option<u32> {
    let nt = base + cpu.read_u32(base + 0x3c).ok()?;
    // The export directory and its size, in the PE32 optional header.
    let (rva, size) = (cpu.read_u32(nt + 0x78).ok()?, cpu.read_u32(nt + 0x7c).ok()?);
    let directory = base + rva;
    let count = cpu.read_u32(directory + 0x18).ok()?;
    let [functions, names, ordinals] =
        [0x1c, 0x20, 0x24].map(|at| cpu.read_u32(directory + at).map_or(0, |rva| base + rva));
    let mut text = vec![0; name.len() + 1];
    for index in 0..count {
        cpu.read(base + cpu.read_u32(names + 4 * index).ok()?, &mut text)
            .ok()?;
        if text[..name.len()] == *name.as_bytes() && text[name.len()] == 0 {
            let ordinal = u32::from(cpu.read_u16(ordinals + 2 * index).ok()?);
            let target = cpu.read_u32(functions + 4 * ordinal).ok()?;
            return (!(rva..rva + size).contains(&target)).then_some(base + target);
        }
    }
    None
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

/// The mapped ntdll.dll. The process keeps a copy, where the Unix side
/// finds the exports it shares with ntdll.so.
#[derive(Clone)]
pub struct Ntdll {
    pub base: u32,
    exports: HashMap<String, u32>,
}

impl Ntdll {
    /// The address of a named export that is not forwarded.
    pub fn export(&self, name: &'static str) -> Result<u32, Error> {
        self.exports.get(name).copied().ok_or(Error::Export(name))
    }

    /// The address of the named export, if it is one and not forwarded.
    pub fn get(&self, name: &str) -> Option<u32> {
        self.exports.get(name).copied()
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
/// [`dispatch::Platform`]; `apisets` is Wine's apisetschema.dll; `prefix` is the Wine prefix whose drives the
/// process sees; `progress` runs the server whenever the process waits for
/// it.
/// apisetschema.dll's `.apiset` section, when it holds a version 6 map.
fn api_set_map(dll: &[u8]) -> Result<&[u8], Error> {
    let invalid = || Error::Context("apisetschema.dll holds no API set map".into());
    let pe =
        Pe::parse(dll).map_err(|error| Error::Context(format!("apisetschema.dll: {error}")))?;
    let section = pe
        .sections
        .iter()
        .find(|section| section.name == ".apiset")
        .ok_or_else(invalid)?;
    let start = section.raw_offset as usize;
    let map = dll
        .get(start..start + section.virtual_size as usize)
        .ok_or_else(invalid)?;
    let field = |offset: usize| u32::from_le_bytes(map[offset..offset + 4].try_into().unwrap());
    if map.len() < 8 || field(0) != 6 || field(4) as usize > map.len() {
        return Err(invalid());
    }
    Ok(map)
}

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

/// The main thread's stack, as Wine's `virtual_alloc_thread_stack` sizes it:
/// the image's reserve, and no less than the default. The thread exists
/// before the image is mapped, so its file is read through the prefix.
fn main_stack_size(prefix: &Path, image: &str) -> u32 {
    let reserve = image
        .split_once(':')
        .and_then(|(drive, path)| {
            let device = format!("dosdevices/{}:", drive.to_ascii_lowercase());
            std::fs::read(
                prefix
                    .join(device)
                    .join(path.trim_start_matches('\\').replace('\\', "/")),
            )
            .ok()
        })
        .and_then(|bytes| Pe::parse(&bytes).ok().map(|pe| pe.stack_reserve));
    reserve.unwrap_or(0).max(DEFAULT_STACK)
}

const DISPLAY_DEVICE: &str = r"\Registry\Machine\System\CurrentControlSet\Control\Video\{00000000-0000-0000-0000-000000000000}\0000";

pub fn start<B: Backend>(
    machine: &mut Machine<B>,
    ntdll: &[u8],
    apisets: &[u8],
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
    // ntdll.dll only passes it back to the dispatcher.
    machine.cpu.write(
        ntdll.export("__wine_unixlib_handle")?,
        &u64::from(dispatch::NTDLL_UNIXLIB).to_le_bytes(),
    )?;

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
    for (offset, value) in [
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
    // As Wine's `load_apiset_dll`: the map that resolves `api-ms-win-*`
    // names, which holds offsets from its own start.
    let map = api_set_map(apisets)?;
    let at = memory::allocate_committed(
        &mut machine.cpu,
        &mut machine.sys.vm,
        &mut memory,
        map.len() as u32,
    )
    .ok_or(Error::AddressSpace)?;
    machine.cpu.write(at, map)?;
    machine.cpu.write_u32(peb + peb::API_SET_MAP, at)?;

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
        main_stack_size(prefix, startup.image),
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
    machine.ext.insert(ntdll.clone());
    machine.ext.insert(win32k::Dispatcher(
        ntdll.export("KiUserCallbackDispatcher")?,
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
                // As Wine's `build_initial_params`, once the server is there.
                let mut environment = Environment::new(startup.environment);
                environment.add_dynamic();
                environment.add_registry(env).map_err(|error| match error {
                    status::Error::Status(code) => Error::Status("environment", code),
                    status::Error::Unsupported(reason) => Error::Context(reason),
                })?;
                let environment = environment.block();
                let size = params::size(startup, &environment);
                let memory = env.ext.get_mut::<Memory>();
                let parameters = memory::allocate_committed(env.cpu, &mut env.sys.vm, memory, size)
                    .ok_or(Error::AddressSpace)?;
                env.cpu
                    .write_u32(env.sys.peb + peb::PROCESS_PARAMETERS, parameters)?;
                env.cpu.write(
                    parameters,
                    &params::build(startup, &environment, parameters, env.sys.pid, handles),
                )?;
                // win32u looks the driver up under the desktop's display
                // device, the null GUID while no explorer names another.
                registry::set_volatile_string(
                    env,
                    DISPLAY_DEVICE,
                    "GraphicsDriver",
                    startup.graphics_driver,
                )
                .map_err(|error| match error {
                    status::Error::Status(code) => Error::Status("graphics driver", code),
                    status::Error::Unsupported(reason) => Error::Context(reason),
                })?;
                if !startup.fonts.is_empty() {
                    let fonts = crate::environment::user_key(env)
                        .map(|user| format!(r"{user}\Software\Wine\Fonts"))
                        .and_then(|key| {
                            registry::set_volatile_string(env, &key, "Path", startup.fonts)
                        });
                    fonts.map_err(|error| match error {
                        status::Error::Status(code) => Error::Status("font path", code),
                        status::Error::Unsupported(reason) => Error::Context(reason),
                    })?;
                }
                crate::file::mount_drives(env).map_err(|error| match error {
                    status::Error::Status(code) => Error::Status("drive letters", code),
                    status::Error::Unsupported(reason) => Error::Context(reason),
                })?;
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

/// What follows once the desktop exists.
pub type Started = Box<dyn FnOnce(&mut Env<'_>) -> adv_machine::Outcome>;

/// Starts explorer's desktop in this process: it creates and shows the
/// desktop window on the calling thread, as explorer's desktop thread does.
/// The crate with a user driver installs it.
#[derive(Clone, Copy)]
pub struct Desktop(pub fn(&mut Env<'_>, Started) -> adv_machine::Outcome);

/// `ImagePathName` and `CommandLine` in `RTL_USER_PROCESS_PARAMETERS` (i386).
const PARAMS_IMAGE: u32 = 0x38;
const PARAMS_COMMAND_LINE: u32 = 0x40;

/// NtCreateUserProcess(*process, *thread, ..., params, ...): a machine runs
/// one process, so creating another fails as on a system that refuses it.
/// When win32u starts explorer for its desktop, [`Desktop`] first makes the
/// desktop here, which win32u's next request then finds.
pub fn create_user_process(env: &mut Env<'_>, args: &[u32]) -> adv_machine::Outcome {
    let refuse = |_: &mut Env<'_>| adv_machine::Outcome::Return(status::NOT_SUPPORTED);
    let params = args[8];
    let text = |at| read_unicode_string(env.cpu, params + at).unwrap_or_default();
    let desktop = text(PARAMS_IMAGE)
        .to_ascii_lowercase()
        .ends_with("\\explorer.exe")
        && text(PARAMS_COMMAND_LINE).ends_with(" /desktop");
    if desktop && let Some(&Desktop(start)) = env.ext.try_get::<Desktop>() {
        return start(env, Box::new(refuse));
    }
    log::warn!("NtCreateUserProcess: another process is not supported");
    refuse(env)
}

fn read_unicode_string(cpu: &dyn Guest, at: u32) -> Option<String> {
    let length = usize::from(cpu.read_u16(at).ok()?);
    let buffer = cpu.read_u32(at + 4).ok()?;
    let mut bytes = vec![0; length];
    cpu.read(buffer, &mut bytes).ok()?;
    let (units, _) = bytes.as_chunks::<2>();
    let units: Vec<u16> = units.iter().map(|&unit| u16::from_le_bytes(unit)).collect();
    Some(String::from_utf16_lossy(&units))
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
        // The server has killed every thread but the caller's; their
        // contexts end where they stand, as `terminate_thread` ends one.
        let others: Vec<_> = (0..env.sys.contexts.len() as u32)
            .filter(|&context| {
                context != env.context && !env.sys.contexts[context as usize].is_done()
            })
            .collect();
        for context in others {
            server::end_thread(env, context);
            env.sys.terminate_context(context, exit_code);
        }
        return adv_machine::Outcome::Return(reply.header.error);
    }
    adv_machine::Outcome::ExitProcess(exit_code)
}

/// NtSuspendProcess(process)
pub fn suspend(env: &mut Env<'_>, args: &[u32]) -> adv_machine::Outcome {
    status::finish((|| {
        crate::information::current_process(env, args[0])?;
        let mut request = crate::protocol::SuspendProcessRequest {
            handle: args[0],
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)?;
        crate::thread::refresh_suspension(env)
    })())
}

/// NtResumeProcess(process)
pub fn resume(env: &mut Env<'_>, args: &[u32]) -> adv_machine::Outcome {
    status::finish((|| {
        crate::information::current_process(env, args[0])?;
        let mut request = crate::protocol::ResumeProcessRequest {
            handle: args[0],
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)?;
        crate::thread::refresh_suspension(env)
    })())
}

//! Wine's server inside the Hitode process.
//!
//! The vendored wineserver (`vendor/wine`, Wine 11.0) runs unmodified on its
//! own stack. Where it would block in `poll`, it returns to the caller instead,
//! and [`Server::step`] resumes it once a client has written a request or the
//! returned timeout has passed. It reads time from the caller's [`Clock`] and
//! never sleeps, signals, forks or exits the host process (`c/host.c`).
//!
//! The server keeps its state in C globals, so one server runs per host
//! process. It listens on Wine's usual master socket, which clients find
//! through [`Server::socket_dir`]. The shim keeps request, reply and wait
//! bytes in shared memory, retaining the pipes as descriptor identities.
//! Its close and SCM_RIGHTS wrappers keep each duplicate attached to its queue;
//! clients use the same wrappers, serialized with the server by the execution owner.
//!
//! Apple and Android targets build the server; elsewhere the crate is empty.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use std::cell::Cell;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use corosensei::stack::DefaultStack;
use corosensei::{Coroutine, CoroutineResult, Yielder};

/// The protocol version of the vendored server; clients must match it.
pub const PROTOCOL_VERSION: u32 = 930;

const STACK_SIZE: usize = 1 << 20;

/// The time sources the server reads, in 100 ns ticks.
pub trait Clock {
    /// Monotonic time from an arbitrary origin.
    fn monotonic(&self) -> u64;
    /// Wall-clock time since 1970-01-01 UTC.
    fn unix(&self) -> u64;
}

/// Services provided by the execution owner while the server runs.
/// Signals are queued; memory transfers complete before returning to Wine.
pub trait Tracing {
    /// Optional immutable file backing, used for PE metadata on virtual volumes.
    fn read_file(
        &mut self,
        _fd: i32,
        _offset: u64,
        _bytes: &mut [u8],
    ) -> Option<std::io::Result<usize>> {
        None
    }
    fn signal(&mut self, pid: i32, tid: i32, signal: i32) -> bool;
    fn read_memory(&mut self, pid: i32, address: u64, data: &mut [u8]) -> u32;
    fn write_memory(&mut self, pid: i32, address: u64, data: &[u8]) -> u32;
}

struct NoTracing;
impl Tracing for NoTracing {
    fn signal(&mut self, _: i32, _: i32, _: i32) -> bool {
        false
    }
    fn read_memory(&mut self, _: i32, _: u64, _: &mut [u8]) -> u32 {
        0xc000_0022
    }
    fn write_memory(&mut self, _: i32, _: u64, _: &[u8]) -> u32 {
        0xc000_0022
    }
}

pub struct Config {
    /// The Wine prefix; it must exist and belong to the current user.
    pub prefix: PathBuf,
    /// The absolute private directory for the server's runtime files.
    pub server_dir: PathBuf,
    /// The root of the Wine installation, which holds `share/wine/nls`.
    pub install: PathBuf,
    /// The server's debug level (`wineserver -d`); traces go to stderr.
    pub debug: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Waiting for a client request or, when given, until the timeout passes.
    Waiting(Option<Duration>),
    /// Stopped with this status: the last client left, or a fatal error.
    Exited(i32),
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("a wineserver already ran in this process")]
    AlreadyStarted,
    #[error("path contains a NUL byte: {0}")]
    Path(PathBuf),
    #[error("server event watcher: {0}")]
    Events(std::io::Error),
}

enum Suspend {
    Wait(c_int),
    Exit(c_int),
}

pub struct Server {
    coroutine: Coroutine<(), Suspend, c_int, DefaultStack>,
    state: State,
    watch: *mut c_void,
    wake: Box<Mutex<Box<dyn Fn() + Send>>>,
}

/// The strings the server reads through `getenv`, and its executable path.
struct Environment {
    prefix: CString,
    executable: CString,
    server_root: CString,
}

static STARTED: AtomicBool = AtomicBool::new(false);
static ENVIRONMENT: OnceLock<Environment> = OnceLock::new();

thread_local! {
    /// The clock of the running step; set only while the server runs.
    static CLOCK: Cell<Option<*const dyn Clock>> = const { Cell::new(None) };
    static TRACING: Cell<Option<*mut dyn Tracing>> = const { Cell::new(None) };
    /// The server coroutine's yielder; set only while the server runs.
    static YIELDER: Cell<*const Yielder<(), Suspend>> = const { Cell::new(std::ptr::null()) };
    static FLUSH: Cell<bool> = const { Cell::new(false) };
}

unsafe extern "C" {
    fn hitode_server_main(debug: c_int) -> c_int;
    static server_dir: *const c_char;
    fn hitode_server_running(running: c_int);
    fn hitode_server_input() -> u64;
    fn hitode_server_key_state(tid: u32, key: u32) -> c_int;
    fn hitode_server_keyboard_state(tid: u32, state: *mut u8) -> c_int;
    fn hitode_server_user(
        tid: u32,
        op: u32,
        a: u32,
        b: u32,
        c: u32,
        d: u32,
        out: *mut u32,
    ) -> c_int;
    fn hitode_server_close_handle(tid: u32, handle: u32) -> u32;
    fn hitode_server_object(tid: u32, handle: u32) -> usize;
    fn hitode_server_file_kind(tid: u32, handle: u32) -> i32;
    fn hitode_server_sync_fd(
        tid: u32,
        handle: u32,
        access: u32,
        kind: i32,
        status: *mut u32,
    ) -> i32;
    fn hitode_server_queue() -> c_int;
    fn hitode_server_ready() -> c_int;
    fn hitode_names_revision() -> u64;
    fn hitode_event_watch(
        queue: c_int,
        notify: extern "C" fn(*mut c_void),
        owner: *mut c_void,
    ) -> *mut c_void;
    fn hitode_event_unwatch(watch: *mut c_void);
    static mut master_socket_timeout: i64;
    fn flush_registry();
}

/// Advances after every server host call that may create, remove, rename or
/// change a file name's attributes.
pub fn names_revision() -> u64 {
    // SAFETY: an atomic load with no preconditions.
    unsafe { hitode_names_revision() }
}

/// Whether the thread already owns a queue, without creating one.
pub fn has_queue(tid: u32) -> bool {
    // SAFETY: NULL requests only the existence check.
    unsafe { hitode_server_keyboard_state(tid, std::ptr::null_mut()) != 0 }
}

/// Copy the queue state without synchronizing it, as NtUserGetKeyboardState does.
/// # Safety
/// `state` must hold 256 writable bytes.
pub unsafe fn keyboard_state(tid: u32, state: *mut u8) -> bool {
    // SAFETY: the caller supplies the output; server state is execution-owner serial.
    unsafe { hitode_server_keyboard_state(tid, state) != 0 }
}

/// Snapshot or update an existing USER service state without a request.
pub fn user_service(tid: u32, op: u32, args: [u32; 4]) -> Option<[u32; 4]> {
    let mut out = [0; 4];
    // SAFETY: the execution owner serializes the server, with a complete output.
    let complete = unsafe {
        hitode_server_user(
            tid,
            op,
            args[0],
            args[1],
            args[2],
            args[3],
            out.as_mut_ptr(),
        )
    };
    (complete != 0).then_some(out)
}

/// Existing synchronization state, without a request or descriptor transfer.
pub fn sync_fd(tid: u32, handle: u32, access: u32, kind: i32) -> Result<i32, u32> {
    let mut status = 0;
    // SAFETY: clients run only while the server is suspended; status is writable.
    let fd = unsafe { hitode_server_sync_fd(tid, handle, access, kind, &mut status) };
    if status == 0 { Ok(fd) } else { Err(status) }
}

/// File kind without transferring a Unix descriptor.
pub fn file_kind(tid: u32, handle: u32) -> i32 {
    // SAFETY: clients and the server run serially on the execution owner.
    unsafe { hitode_server_file_kind(tid, handle) }
}

/// The server object's identity, for sharing descriptor state at admission.
pub fn object_identity(tid: u32, handle: u32) -> usize {
    // SAFETY: clients and the server run serially on the execution owner.
    unsafe { hitode_server_object(tid, handle) }
}

/// Close through the same handle table while the server is suspended.
#[cfg(any(target_vendor = "apple", target_os = "android"))]
pub fn close_handle(tid: u32, handle: u32) -> u32 {
    // SAFETY: the execution owner serializes clients and the in-process server.
    unsafe { hitode_server_close_handle(tid, handle) }
}

/// Wine's non-async key state, without creating a thread's message queue.
pub fn key_state(tid: u32, key: u32) -> Option<u8> {
    // SAFETY: the execution owner serializes this call with the server; C
    // retains no references and preserves the server's error state.
    u8::try_from(unsafe { hitode_server_key_state(tid, key) }).ok()
}

impl Server {
    /// Keep this prefix available between guest sessions.
    pub fn keep_alive(&mut self) {
        // SAFETY: Wine's globals are accessed only by the execution owner.
        unsafe { master_socket_timeout = i64::MAX };
    }

    /// Drain disconnected clients and save registry changes on Wine's stack.
    pub fn flush(&mut self, clock: &dyn Clock) {
        FLUSH.set(true);
        loop {
            let state = self.step(clock);
            if matches!(state, State::Exited(_)) {
                FLUSH.set(false);
                break;
            }
            if !matches!(state, State::Waiting(Some(Duration::ZERO))) && !self.kernel_ready() {
                break;
            }
        }
    }

    /// Starts the server and runs it until it first waits.
    pub fn start(config: &Config, clock: &dyn Clock) -> Result<Server, Error> {
        let c_path = |path: &Path| {
            CString::new(path.as_os_str().as_bytes()).map_err(|_| Error::Path(path.to_owned()))
        };
        let environment = Environment {
            prefix: c_path(&config.prefix)?,
            server_root: c_path(&config.server_dir)?,
            executable: c_path(&config.install.join("wineserver"))?,
        };
        if STARTED.swap(true, Ordering::SeqCst) {
            return Err(Error::AlreadyStarted);
        }
        let _ = ENVIRONMENT.set(environment);
        let debug = config.debug;
        let coroutine = Coroutine::with_stack(
            DefaultStack::new(STACK_SIZE).expect("server stack"),
            move |yielder: &Yielder<(), Suspend>, ()| {
                YIELDER.set(yielder);
                // SAFETY: the server's startup function; it runs on this
                // coroutine and only leaves it through the host services.
                unsafe { hitode_server_main(debug) }
            },
        );
        let mut server = Server {
            coroutine,
            state: State::Waiting(Some(Duration::ZERO)),
            watch: std::ptr::null_mut(),
            wake: Box::new(Mutex::new(Box::new(|| {}))),
        };
        server.step(clock);
        // SAFETY: the first wait established Wine's event set. The boxed wake
        // stays pinned until Drop stops and joins the watcher.
        server.watch = unsafe {
            hitode_event_watch(
                hitode_server_queue(),
                event_wake,
                (&raw mut *server.wake).cast(),
            )
        };
        if server.watch.is_null() {
            return Err(Error::Events(std::io::Error::last_os_error()));
        }
        Ok(server)
    }

    /// Resumes the server: it handles what its clients have sent and the
    /// timeouts that have passed, then returns when it would wait again.
    pub fn step(&mut self, clock: &dyn Clock) -> State {
        self.step_with_tracing(clock, &mut NoTracing)
    }

    pub fn step_with_tracing(&mut self, clock: &dyn Clock, tracing: &mut dyn Tracing) -> State {
        if let State::Exited(_) = self.state {
            return self.state;
        }
        // SAFETY: the pointer outlives the resume below, the only time the
        // server reads it; it is cleared before `clock` goes out of scope.
        let clock: *const (dyn Clock + '_) = clock;
        CLOCK.set(Some(unsafe {
            std::mem::transmute::<*const (dyn Clock + '_), *const dyn Clock>(clock)
        }));
        let tracing: *mut (dyn Tracing + '_) = tracing;
        // SAFETY: as with CLOCK, only the synchronous resume can access this pointer.
        TRACING.set(Some(unsafe {
            std::mem::transmute::<*mut (dyn Tracing + '_), *mut dyn Tracing>(tracing)
        }));
        // SAFETY: the execution owner serializes channel access with this resume.
        unsafe { hitode_server_running(1) };
        self.state = match self.coroutine.resume(()) {
            CoroutineResult::Yield(Suspend::Wait(timeout)) => {
                State::Waiting(u64::try_from(timeout).ok().map(Duration::from_millis))
            }
            CoroutineResult::Yield(Suspend::Exit(status)) | CoroutineResult::Return(status) => {
                State::Exited(status)
            }
        };
        unsafe { hitode_server_running(0) };
        CLOCK.set(None);
        TRACING.set(None);
        self.state
    }

    /// Changes to channels whose reader is the server, excluding its own writes.
    pub fn input_generation(&self) -> u64 {
        // SAFETY: channel state is accessed only by the execution owner.
        unsafe { hitode_server_input() }
    }

    /// Ordinary descriptors have become ready since the last kernel query.
    pub fn kernel_ready(&self) -> bool {
        // SAFETY: the watcher and execution owner share only this atomic flag.
        unsafe { hitode_server_ready() != 0 }
    }

    /// Wake the execution owner when Wine's kernel event set becomes readable.
    pub fn set_waker(&mut self, wake: Box<dyn Fn() + Send>) {
        *self.wake.lock().unwrap() = wake;
        if self.kernel_ready() {
            self.wake.lock().unwrap()();
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// The directory holding the master socket named `socket`.
    pub fn socket_dir(&self) -> Option<PathBuf> {
        // SAFETY: the server sets `server_dir` once, before it first waits,
        // and never frees it.
        let dir = unsafe { server_dir };
        (!dir.is_null()).then(|| {
            // SAFETY: a NUL-terminated string from asprintf.
            let dir = unsafe { CStr::from_ptr(dir) };
            PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes()))
        })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if !self.watch.is_null() {
            // SAFETY: stop joins the watcher before its callback storage is freed.
            unsafe { hitode_event_unwatch(self.watch) };
        }
        if !self.coroutine.done() {
            // SAFETY: the server's frames are C code with nothing to unwind;
            // its globals stay behind, which is why it never runs again.
            unsafe { self.coroutine.force_reset() };
        }
    }
}

extern "C" fn event_wake(owner: *mut c_void) {
    // SAFETY: Server pins this mutex until the watcher has joined.
    let wake = unsafe { &*owner.cast::<Mutex<Box<dyn Fn() + Send>>>() };
    wake.lock().unwrap()();
}

fn suspend(reason: Suspend) {
    let yielder = YIELDER.get();
    assert!(
        !yielder.is_null(),
        "server host service called outside the server"
    );
    // SAFETY: set by the coroutine body, which outlives every host call.
    unsafe { &*yielder }.suspend(reason);
}

#[unsafe(no_mangle)]
extern "C" fn hitode_host_wait(timeout: c_int) {
    suspend(Suspend::Wait(timeout));
    if FLUSH.replace(false) {
        // SAFETY: this runs on Wine's coroutine with its host services installed.
        unsafe { flush_registry() };
    }
}

#[unsafe(no_mangle)]
extern "C" fn hitode_host_exit(status: c_int) -> ! {
    suspend(Suspend::Exit(status));
    unreachable!("an exited server was resumed");
}

#[unsafe(no_mangle)]
extern "C" fn hitode_host_ticks(clock: c_int) -> u64 {
    let current = CLOCK.get().expect("server clock read outside a step");
    // SAFETY: set for the duration of the step that runs this call.
    let current = unsafe { &*current };
    if clock == 0 {
        current.monotonic()
    } else {
        current.unix()
    }
}

#[unsafe(no_mangle)]
extern "C" fn hitode_host_getenv(name: *const c_char) -> *const c_char {
    let environment = ENVIRONMENT.get().expect("server environment");
    // SAFETY: the server passes NUL-terminated names.
    match unsafe { CStr::from_ptr(name) }.to_bytes() {
        b"WINEPREFIX" => environment.prefix.as_ptr(),
        // Hitode builds i386 prefixes only.
        b"WINEARCH" => c"win32".as_ptr(),
        _ => std::ptr::null(),
    }
}

#[unsafe(no_mangle)]
extern "C" fn hitode_host_executable() -> *const c_char {
    ENVIRONMENT
        .get()
        .expect("server environment")
        .executable
        .as_ptr()
}

#[unsafe(no_mangle)]
extern "C" fn hitode_host_server_root() -> *const c_char {
    ENVIRONMENT
        .get()
        .expect("server environment")
        .server_root
        .as_ptr()
}

#[unsafe(no_mangle)]
extern "C" fn hitode_host_file(fd: c_int, offset: u64, bytes: *mut u8, size: usize) -> isize {
    let Some(tracing) = TRACING.get() else {
        return -2;
    };
    // SAFETY: the callback executes within the step owning the tracing borrow.
    let tracing = unsafe { &mut *tracing };
    let bytes = if size == 0 {
        &mut []
    } else {
        // SAFETY: pread's caller supplies a writable buffer of size bytes.
        unsafe { std::slice::from_raw_parts_mut(bytes, size) }
    };
    match tracing.read_file(fd, offset, bytes) {
        None => -2,
        Some(Ok(count)) => count as isize,
        Some(Err(_)) => -1,
    }
}

#[unsafe(no_mangle)]
extern "C" fn hitode_host_signal(pid: c_int, tid: c_int, signal: c_int) -> c_int {
    let Some(tracing) = TRACING.get() else {
        return 0;
    };
    // SAFETY: the callback is inside the synchronous step owning this mutable borrow.
    i32::from(unsafe { &mut *tracing }.signal(pid, tid, signal))
}

#[unsafe(no_mangle)]
extern "C" fn hitode_host_memory(
    pid: c_int,
    address: u64,
    size: u32,
    data: *mut u8,
    write: c_int,
) -> u32 {
    let Some(tracing) = TRACING.get() else {
        return 0xc000_0022;
    };
    // SAFETY: the tracing borrow is installed for exactly this step.
    let tracing = unsafe { &mut *tracing };
    if write != 0 {
        // SAFETY: Wine supplies a readable source buffer of size bytes.
        tracing.write_memory(pid, address, unsafe {
            std::slice::from_raw_parts(data, size as usize)
        })
    } else {
        // SAFETY: Wine supplies a writable destination buffer of size bytes.
        tracing.read_memory(pid, address, unsafe {
            std::slice::from_raw_parts_mut(data, size as usize)
        })
    }
}

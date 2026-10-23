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
//! through [`Server::socket_dir`].
//!
//! Apple and Android targets build the server; elsewhere the crate is empty.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use std::cell::Cell;
use std::ffi::{CStr, CString, c_char, c_int};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
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
}

enum Suspend {
    Wait(c_int),
    Exit(c_int),
}

pub struct Server {
    coroutine: Coroutine<(), Suspend, c_int, DefaultStack>,
    state: State,
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
    /// The server coroutine's yielder; set only while the server runs.
    static YIELDER: Cell<*const Yielder<(), Suspend>> = const { Cell::new(std::ptr::null()) };
}

unsafe extern "C" {
    fn hitode_server_main(debug: c_int) -> c_int;
    static server_dir: *const c_char;
}

impl Server {
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
        };
        server.step(clock);
        Ok(server)
    }

    /// Resumes the server: it handles what its clients have sent and the
    /// timeouts that have passed, then returns when it would wait again.
    pub fn step(&mut self, clock: &dyn Clock) -> State {
        if let State::Exited(_) = self.state {
            return self.state;
        }
        // SAFETY: the pointer outlives the resume below, the only time the
        // server reads it; it is cleared before `clock` goes out of scope.
        let clock: *const (dyn Clock + '_) = clock;
        CLOCK.set(Some(unsafe {
            std::mem::transmute::<*const (dyn Clock + '_), *const dyn Clock>(clock)
        }));
        self.state = match self.coroutine.resume(()) {
            CoroutineResult::Yield(Suspend::Wait(timeout)) => {
                State::Waiting(u64::try_from(timeout).ok().map(Duration::from_millis))
            }
            CoroutineResult::Yield(Suspend::Exit(status)) | CoroutineResult::Return(status) => {
                State::Exited(status)
            }
        };
        CLOCK.set(None);
        self.state
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
        if !self.coroutine.done() {
            // SAFETY: the server's frames are C code with nothing to unwind;
            // its globals stay behind, which is why it never runs again.
            unsafe { self.coroutine.force_reset() };
        }
    }
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

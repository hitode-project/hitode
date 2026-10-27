//! The client side of the wineserver protocol.
//!
//! As in Wine, a process talks to the server over a Unix socket, which also
//! carries file descriptors, and each thread over a request pipe and a reply
//! pipe. The server runs in the same host process and never at the same time
//! as its clients, so the client never blocks on a descriptor: when it needs
//! the server to act, it asks the runtime for [`Progress`] and tries again.

use std::cell::UnsafeCell;
use std::collections::{HashMap, HashSet};
use std::io;
use std::mem::{size_of, zeroed};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::rc::Rc;
use std::time::Instant;

use adv_cpu::backend::Guest;
use adv_machine::sched::{Handle, Object};
use adv_machine::{CtxId, Env, Sys};

use crate::counters::Tally;

use crate::protocol::{
    AllocFileHandleRequest, CloseHandleRequest, FIXED_SIZE, GetHandleFdRequest,
    InitFirstThreadRequest, InitThreadRequest, PROTOCOL_VERSION, Request,
};

// The shim owns the table shared by all processes on this execution owner.
#[cfg(any(target_vendor = "apple", target_os = "android"))]
mod pipes {
    unsafe extern "C" {
        #[link_name = "hitode_pipe"]
        pub fn pipe(fds: *mut libc::c_int) -> libc::c_int;
        #[link_name = "hitode_read"]
        pub fn read(fd: libc::c_int, buf: *mut libc::c_void, size: usize) -> isize;
        #[link_name = "hitode_write"]
        pub fn write(fd: libc::c_int, buf: *const libc::c_void, size: usize) -> isize;
        #[link_name = "hitode_sendmsg"]
        pub fn sendmsg(fd: libc::c_int, msg: *const libc::msghdr, flags: libc::c_int) -> isize;
        #[link_name = "hitode_recvmsg"]
        pub fn recvmsg(fd: libc::c_int, msg: *mut libc::msghdr, flags: libc::c_int) -> isize;
        pub fn hitode_server_connected();
        pub fn hitode_channel_forget(fd: libc::c_int);
        pub fn hitode_channel_watch(
            fd: libc::c_int,
            notify: extern "C" fn(*mut libc::c_void, usize),
            owner: *mut libc::c_void,
            token: usize,
        );
    }
}
#[cfg(not(any(target_vendor = "apple", target_os = "android")))]
mod pipes {
    pub use libc::{pipe, read, recvmsg, sendmsg, write};
    pub unsafe fn hitode_channel_forget(_: libc::c_int) {}
}

struct PipeFd(OwnedFd);
impl AsRawFd for PipeFd {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}
impl Drop for PipeFd {
    fn drop(&mut self) {
        // SAFETY: OwnedFd closes this descriptor immediately after unregistering it.
        unsafe { pipes::hitode_channel_forget(self.as_raw_fd()) };
    }
}

#[cfg(any(target_vendor = "apple", target_os = "android"))]
extern "C" fn channel_ready(owner: *mut libc::c_void, context: usize) {
    // SAFETY: the box stays live until all watched read ends close. Server steps
    // and delivery run serially on the execution owner.
    unsafe { &mut *(*owner.cast::<UnsafeCell<HashSet<CtxId>>>()).get() }.insert(context as CtxId);
}

/// i386, the machine of Hitode's prefixes.
pub const MACHINE_I386: u16 = 0x014c;

/// How often the client lets the server run without seeing an answer before
/// it decides that none is coming.
const PATIENCE: u32 = 4;

/// Lets the server handle what its clients have sent so far, and the
/// timeouts that have passed by `time`.
pub trait Progress {
    fn run_server(&mut self, time: Time);
    fn interrupt_pending(&self) -> bool {
        false
    }
    fn run_server_for(&mut self, time: Time, _cpu: &mut dyn Guest) {
        self.run_server(time);
    }
}

struct Active<'a> {
    progress: &'a mut dyn Progress,
    cpu: &'a mut dyn Guest,
}
impl Progress for Active<'_> {
    fn run_server(&mut self, time: Time) {
        self.progress.run_server_for(time, self.cpu);
    }
}

/// The guest's clocks, which the server reads as its own, in 100 ns ticks.
#[derive(Clone, Copy, Debug, Default)]
pub struct Time {
    /// The performance counter.
    pub monotonic: u64,
    /// Wall-clock time since 1970-01-01 UTC.
    pub unix: u64,
}

impl Time {
    pub fn now(clock: &mut adv_machine::Clock) -> Time {
        Time {
            monotonic: clock.now_us() * 10,
            unix: clock.system_time() - adv_machine::clock::UNIX_EPOCH_SYSTEM_TIME,
        }
    }
}

/// A process's connection with what drives its server, kept in the
/// machine's extensions.
pub struct Link {
    socket: UnixStream,
    pub progress: Box<dyn Progress>,
    /// The time the server sees when the client lets it run.
    time: Time,
    /// Host descriptors of handles the server allows to cache, as Wine's
    /// fd cache keeps them: until the handle is closed.
    fds: HashMap<u32, Cached>,
    threads: HashMap<CtxId, Thread>,
    inproc_device: Option<crate::ntsync::Fd>,
    inproc: HashMap<u32, InprocFd>,
    ready: Box<UnsafeCell<HashSet<CtxId>>>,
    /// Requests sent, by number; see [`crate::counters`].
    pub(crate) requests: HashMap<i32, Tally>,
}

/// A guest thread as the server knows it.
struct Thread {
    tid: u32,
    channel: Channel,
    /// Set when the server ends the thread's wait while the thread is
    /// blocked in the machine.
    wake: Handle,
    /// Whether the thread waits for a wake-up.
    waiting: bool,
    /// The wake-up the server sent, read from the pipe on the thread's
    /// behalf.
    pending: Option<WakeUp>,
    alert: Option<Rc<crate::ntsync::Fd>>,
}

struct Cached {
    fd: OwnedFd,
    kind: i32,
    options: u32,
}

/// A handle's host descriptor, valid for the system call that asked.
pub struct HandleFd {
    pub fd: RawFd,
    /// The server's `enum server_fd_type`.
    pub kind: i32,
    pub options: u32,
    /// Closed when this is dropped, for a descriptor the server does not
    /// let the process cache.
    _owned: Option<OwnedFd>,
}

impl Link {
    /// The link of a connected process whose first thread is `main`.
    pub fn new(
        connection: Connection,
        progress: Box<dyn Progress>,
        time: Time,
        main: CtxId,
        tid: u32,
        sys: &mut Sys,
    ) -> Link {
        let mut link = Link {
            socket: connection.socket,
            progress,
            time,
            fds: HashMap::new(),
            threads: HashMap::new(),
            inproc_device: connection.inproc_device,
            inproc: HashMap::new(),
            ready: Box::new(UnsafeCell::new(HashSet::new())),
            requests: HashMap::new(),
        };
        link.add_thread(main, tid, connection.channel, sys);
        link
    }

    fn add_thread(&mut self, context: CtxId, tid: u32, channel: Channel, sys: &mut Sys) {
        let wake = sys.objects.insert(Object::Event {
            manual_reset: false,
            signaled: false,
        });
        #[cfg(any(target_vendor = "apple", target_os = "android"))]
        // SAFETY: the read end unregisters before ready is dropped.
        unsafe {
            pipes::hitode_channel_watch(
                channel.wait.as_raw_fd(),
                channel_ready,
                (&raw mut *self.ready).cast(),
                context as usize,
            );
        }
        self.threads.insert(
            context,
            Thread {
                tid,
                channel,
                wake,
                waiting: false,
                pending: None,
                alert: None,
            },
        );
    }

    fn thread(&mut self, context: CtxId) -> Result<&mut Thread, Error> {
        self.threads
            .get_mut(&context)
            .ok_or(Error::NoThread(context))
    }

    /// Sends a request from the thread of `context`; see [`Channel::call`].
    pub fn call<R: Request>(
        &mut self,
        context: CtxId,
        cpu: &mut dyn Guest,
        request: &mut R,
        data: &[&[u8]],
        reply_data: &mut [u8],
    ) -> Result<(R::Reply, usize), Error> {
        if R::NUMBER == crate::protocol::GetApcResultRequest::NUMBER {
            // The server closes this handle when returning the APC result.
            // Its number can then name a new, still-pending APC.
            // SAFETY: NUMBER selects the generated repr(C) request layout.
            let result =
                unsafe { &*(request as *const R).cast::<crate::protocol::GetApcResultRequest>() };
            self.inproc.remove(&result.handle);
        }
        if R::NUMBER == crate::protocol::DupHandleRequest::NUMBER {
            // SAFETY: NUMBER selects the generated repr(C) request layout.
            let duplicate =
                unsafe { &*(request as *const R).cast::<crate::protocol::DupHandleRequest>() };
            if duplicate.options & 1 != 0 {
                self.inproc.remove(&duplicate.src_handle);
            }
        }
        let (time, since) = (self.time, Instant::now());
        let thread = self
            .threads
            .get_mut(&context)
            .ok_or(Error::NoThread(context))?;
        let answer = thread.channel.call(
            &mut Active {
                progress: &mut *self.progress,
                cpu,
            },
            time,
            request,
            data,
            reply_data,
        );
        self.requests.entry(R::NUMBER).or_default().add(since);
        answer
    }

    /// Passes on a request the guest prepared.
    pub fn call_raw(
        &mut self,
        context: CtxId,
        cpu: &mut dyn Guest,
        fixed: &mut [u8; FIXED_SIZE],
        data: &[&[u8]],
        reply_data: &mut [u8],
    ) -> Result<usize, Error> {
        let word = |offset| u32::from_ne_bytes(fixed[offset..offset + 4].try_into().unwrap());
        if word(0) as i32 == crate::protocol::GetApcResultRequest::NUMBER {
            self.inproc.remove(&word(12));
        }
        if word(0) as i32 == crate::protocol::DupHandleRequest::NUMBER && word(32) & 1 != 0 {
            self.inproc.remove(&word(16));
        }
        let (time, since) = (self.time, Instant::now());
        let number = i32::from_ne_bytes(fixed[..4].try_into().unwrap());
        let thread = self
            .threads
            .get_mut(&context)
            .ok_or(Error::NoThread(context))?;
        let answer = thread.channel.call_raw(
            &mut Active {
                progress: &mut *self.progress,
                cpu,
            },
            time,
            fixed,
            data,
            reply_data,
        );
        self.requests.entry(number).or_default().add(since);
        answer
    }
}

fn link<'a>(env: &'a mut Env<'_>) -> Result<&'a mut Link, crate::status::Error> {
    env.ext
        .try_get_mut::<Link>()
        .ok_or_else(|| crate::status::Error::Unsupported("no server connection".into()))
}

/// Shows the server the guest's clocks as they read now.
pub fn set_time(env: &mut Env<'_>) {
    let time = Time::now(&mut env.sys.clock);
    crate::ntsync::set_time(time);
    if let Some(link) = env.ext.try_get_mut::<Link>() {
        link.time = time;
    }
}

/// Wakes the blocked threads whose waits the server ended: their
/// wake-ups are read now, so a thread that runs later finds its own.
pub fn deliver(env: &mut Env<'_>) -> bool {
    let mut delivered = crate::sync::deliver_inproc(env);
    let Some(link) = env.ext.try_get_mut::<Link>() else {
        return delivered;
    };
    #[cfg(not(any(target_vendor = "apple", target_os = "android")))]
    link.ready.get_mut().extend(link.threads.keys().copied());
    for context in std::mem::take(link.ready.get_mut()) {
        let Some(thread) = link.threads.get_mut(&context) else {
            continue;
        };
        if !thread.waiting || thread.pending.is_some() {
            continue;
        }
        // A broken pipe shows when the thread itself reads it.
        if let Ok(Some(wake_up)) = thread.channel.wake_up() {
            thread.pending = Some(wake_up);
            env.sys.set_event(thread.wake);
            delivered = true;
        }
    }
    delivered
}

/// Run the server for host activity, and deliver the resulting wake-ups.
pub fn run(env: &mut Env<'_>) {
    set_time(env);
    let link = env.ext.get_mut::<Link>();
    link.progress.run_server_for(link.time, env.cpu);
    deliver(env);
}

/// What ended a wait the thread started with `select`.
#[derive(Clone, Copy, Debug)]
pub struct WakeUp {
    pub cookie: u64,
    /// The wait's status; with no cookie, the exit code of the killed
    /// thread.
    pub signaled: u32,
}

/// Marks the current thread as waiting for a wake-up, and returns the
/// machine object that is set when one arrives.
pub fn begin_wait(env: &mut Env<'_>) -> Result<Handle, crate::status::Error> {
    let context = env.context;
    let thread = link(env)?.thread(context).map_err(broken)?;
    thread.waiting = true;
    // A wake can arrive in the same step as select's reply, before begin_wait.
    if thread.pending.is_none() {
        thread.pending = thread.channel.wake_up().map_err(broken)?;
    }
    let wake = thread.wake;
    if thread.pending.is_some() {
        env.sys.set_event(wake);
    }
    Ok(wake)
}

/// Takes the wake-up the server sent the current thread, letting the
/// server run first at the current time if none has arrived.
pub fn wake_up(env: &mut Env<'_>) -> Result<Option<WakeUp>, crate::status::Error> {
    set_time(env);
    let context = env.context;
    let link = link(env)?;
    let time = link.time;
    let thread = link
        .threads
        .get_mut(&context)
        .ok_or(Error::NoThread(context));
    let thread = thread.map_err(broken)?;
    let wake_up = match thread.pending.take() {
        Some(wake_up) => Some(wake_up),
        None => {
            link.progress.run_server(time);
            let thread = link.thread(context).map_err(broken)?;
            thread.channel.wake_up().map_err(broken)?
        }
    };
    if wake_up.is_some() {
        link.thread(context).map_err(broken)?.waiting = false;
    }
    deliver(env);
    Ok(wake_up)
}

/// Take an already delivered signal wait reply without advancing the server.
pub fn poll_wake(env: &mut Env<'_>) -> Result<Option<WakeUp>, crate::status::Error> {
    let context = env.context;
    let thread = env.ext.get_mut::<Link>().thread(context).map_err(broken)?;
    let wake = match thread.pending.take() {
        Some(w) => Some(w),
        None => thread.channel.wake_up().map_err(broken)?,
    };
    if wake.is_some() {
        thread.waiting = false;
    }
    Ok(wake)
}

/// The machine context of the thread the server knows as `tid`.
pub fn context_of(env: &mut Env<'_>, tid: u32) -> Option<CtxId> {
    let link = env.ext.try_get::<Link>()?;
    link.threads
        .iter()
        .find_map(|(&context, thread)| (thread.tid == tid).then_some(context))
}

/// The server id of the current thread.
pub fn current_tid(env: &mut Env<'_>) -> Result<u32, crate::status::Error> {
    let context = env.context;
    Ok(link(env)?.thread(context).map_err(broken)?.tid)
}

/// A thread's request pipe, made before the server knows the thread.
pub struct NewThread {
    request: PipeFd,
    /// The server's end, which `new_thread` names.
    server_request: PipeFd,
}

impl NewThread {
    /// The descriptor `new_thread` takes as `request_fd`.
    pub fn request_fd(&self) -> i32 {
        self.server_request.as_raw_fd()
    }
}

/// Makes a request pipe for a new thread and sends the server its end, as
/// Wine's `NtCreateThreadEx` does before `new_thread`.
pub fn prepare_thread(env: &mut Env<'_>) -> Result<NewThread, crate::status::Error> {
    let tid = current_tid(env)?;
    let link = link(env)?;
    let (server_request, request) = pipe().map_err(|error| broken(error.into()))?;
    set_nonblocking(&request).map_err(|error| broken(error.into()))?;
    send_fd(&link.socket, tid, server_request.as_raw_fd()).map_err(|error| broken(error.into()))?;
    Ok(NewThread {
        request,
        server_request,
    })
}

/// Registers the thread the server created as `tid`, which runs as machine
/// context `context`, as Wine's `server_init_thread` does. Returns whether
/// the server holds it suspended.
pub fn init_thread(
    env: &mut Env<'_>,
    new: NewThread,
    context: CtxId,
    tid: u32,
    teb: u32,
    entry: u32,
) -> Result<bool, crate::status::Error> {
    let fail = |error: io::Error| broken(error.into());
    let NewThread { request, .. } = new;
    let (reply, reply_write) = pipe().map_err(fail)?;
    let (wait, wait_write) = pipe().map_err(fail)?;
    set_nonblocking(&reply).map_err(fail)?;
    set_nonblocking(&wait).map_err(fail)?;
    let Env { ext, sys, cpu, .. } = env;
    let link = ext
        .try_get_mut::<Link>()
        .ok_or_else(|| crate::status::Error::Unsupported("no server connection".into()))?;
    send_fd(&link.socket, tid, reply_write.as_raw_fd()).map_err(fail)?;
    send_fd(&link.socket, tid, wait_write.as_raw_fd()).map_err(fail)?;
    link.add_thread(
        context,
        tid,
        Channel {
            request,
            reply,
            wait,
        },
        sys,
    );
    let mut init = InitThreadRequest {
        unix_tid: context as i32 + 1,
        reply_fd: reply_write.as_raw_fd(),
        wait_fd: wait_write.as_raw_fd(),
        teb: u64::from(teb),
        entry: u64::from(entry),
        ..Default::default()
    };
    let (answer, _) = link
        .call(context, *cpu, &mut init, &[], &mut [])
        .map_err(broken)?;
    crate::status::check(answer.header.error)?;
    Ok(answer.suspend != 0)
}

/// Forgets the thread of `context`, which is ending: the server sees its
/// pipes close and ends the thread, waking whoever waits for it.
pub fn end_thread(env: &mut Env<'_>, context: CtxId) {
    set_time(env);
    let Some(link) = env.ext.try_get_mut::<Link>() else {
        return;
    };
    if let Some(thread) = link.threads.remove(&context) {
        env.sys.objects.close(thread.wake);
        drop(thread.channel);
        link.progress.run_server_for(link.time, env.cpu);
    }
    deliver(env);
}

fn broken(error: Error) -> crate::status::Error {
    crate::status::Error::Unsupported(format!("server: {error}"))
}

/// Sends a descriptor for a subsequent server request.
pub fn pass_fd(env: &mut Env<'_>, fd: RawFd) -> Result<(), crate::status::Error> {
    let tid = current_tid(env)?;
    send_fd(&link(env)?.socket, tid, fd).map_err(|error| broken(error.into()))
}

/// Makes an inheritable server handle for a host descriptor, as Wine's
/// `wine_server_fd_to_handle` does.
pub fn fd_to_handle(
    env: &mut Env<'_>,
    fd: RawFd,
    access: u32,
) -> Result<u32, crate::status::Error> {
    let tid = current_tid(env)?;
    send_fd(&link(env)?.socket, tid, fd).map_err(|error| broken(error.into()))?;
    let mut request = AllocFileHandleRequest {
        access,
        attributes: 2, // OBJ_INHERIT
        fd,
        ..Default::default()
    };
    let (reply, _) = self::request(env, &mut request, &[], &mut [])?;
    crate::status::check(reply.header.error)?;
    Ok(reply.handle)
}

/// The host descriptor of `handle`, from the cache or the server.
pub fn handle_fd(env: &mut Env<'_>, handle: u32) -> Result<HandleFd, crate::status::Error> {
    let context = env.context;
    let link = env.ext.get_mut::<Link>();
    if let Some(cached) = link.fds.get(&handle) {
        return Ok(HandleFd {
            fd: cached.fd.as_raw_fd(),
            kind: cached.kind,
            options: cached.options,
            _owned: None,
        });
    }
    let mut request = GetHandleFdRequest {
        handle,
        ..Default::default()
    };
    let (reply, _) = link
        .call(context, env.cpu, &mut request, &[], &mut [])
        .map_err(broken)?;
    crate::status::check(reply.header.error)?;
    let (tag, fd) = receive_fd(&link.socket, &mut *link.progress, link.time).map_err(broken)?;
    if tag != handle {
        return Err(crate::status::Error::Unsupported(format!(
            "the server sent the descriptor of handle {tag:#x} for {handle:#x}"
        )));
    }
    let raw = fd.as_raw_fd();
    let owned = if reply.cacheable != 0 {
        link.fds.insert(
            handle,
            Cached {
                fd,
                kind: reply.r#type,
                options: reply.options,
            },
        );
        None
    } else {
        Some(fd)
    };
    deliver(env);
    Ok(HandleFd {
        fd: raw,
        kind: reply.r#type,
        options: reply.options,
        _owned: owned,
    })
}

/// Closes `handle` in the server and forgets its descriptor.
pub fn close(env: &mut Env<'_>, handle: u32) -> Result<(), crate::status::Error> {
    link(env)?.fds.remove(&handle);
    link(env)?.inproc.remove(&handle);
    let mut request = CloseHandleRequest {
        handle,
        ..Default::default()
    };
    let (reply, _) = self::request(env, &mut request, &[], &mut [])?;
    crate::status::check(reply.header.error)
}

/// Sends a request on behalf of a system call. A status the server
/// returns is in the reply header; a broken connection stops the machine.
pub fn request<R: Request>(
    env: &mut Env<'_>,
    request: &mut R,
    data: &[&[u8]],
    reply_data: &mut [u8],
) -> Result<(R::Reply, usize), crate::status::Error> {
    let context = env.context;
    let answer = env
        .ext
        .get_mut::<Link>()
        .call(context, env.cpu, request, data, reply_data)
        .map_err(broken)?;
    deliver(env);
    Ok(answer)
}

/// Terminating the caller's job kills its server thread before a reply is
/// written. Wine receives SIGQUIT; this machine exits after that closure.
pub(crate) fn terminate_job(
    env: &mut Env<'_>,
    request: &mut crate::protocol::TerminateJobRequest,
) -> Result<bool, crate::status::Error> {
    let context = env.context;
    match env
        .ext
        .get_mut::<Link>()
        .call(context, env.cpu, request, &[], &mut [])
    {
        Ok((reply, _)) => {
            deliver(env);
            crate::status::check(reply.header.error)?;
            Ok(false)
        }
        Err(Error::Closed) => Ok(true),
        Err(error) => Err(broken(error)),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("the server speaks protocol version {0}")]
    Version(u32),
    /// The server did not answer a request that it answers at once.
    #[error("the server did not answer")]
    NoReply,
    #[error("the server closed the connection")]
    Closed,
    #[error("context {0} is not a thread of the process")]
    NoThread(CtxId),
}

/// What the server told the first thread of a new process.
#[derive(Debug)]
pub struct FirstThread {
    pub pid: u32,
    pub tid: u32,
    pub unix_pid: i32,
    pub session_id: u32,
    /// The server's start time, in 100 ns ticks since 1601.
    pub server_start: i64,
    /// The size of the startup information the process can fetch.
    pub info_size: u32,
    /// The machines the server supports, native first.
    pub machines: Vec<u16>,
}

/// One process's connection: its socket, and the pipes of its first
/// thread.
static NEXT_PROCESS: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(1);

pub struct Connection {
    socket: UnixStream,
    channel: Channel,
    inproc_device: Option<crate::ntsync::Fd>,
}

/// A thread's pipes to the server.
struct Channel {
    request: PipeFd,
    reply: PipeFd,
    /// The server writes here to end a wait the thread started with `select`.
    wait: PipeFd,
}

impl Connection {
    /// Connects a new process to the server and registers its first thread.
    pub fn connect(
        socket_dir: &Path,
        progress: &mut dyn Progress,
        time: Time,
    ) -> Result<(Connection, FirstThread), Error> {
        let socket = {
            let directory = std::fs::File::open(socket_dir)?;
            // There is no connectat; a helper thread with its own working
            // directory resolves the relative name, which fits sun_path
            // however long the directory's path is, without changing the
            // process's or the caller's.
            std::thread::spawn(move || {
                #[cfg(target_vendor = "apple")]
                unsafe extern "C" {
                    fn pthread_fchdir_np(fd: libc::c_int) -> libc::c_int;
                }
                // SAFETY: directory is live; the thread exits after connect.
                #[cfg(target_vendor = "apple")]
                let changed = unsafe { pthread_fchdir_np(directory.as_raw_fd()) };
                // SAFETY: as above; unsharing gives this thread alone the
                // new working directory.
                #[cfg(not(target_vendor = "apple"))]
                let changed = unsafe {
                    match libc::unshare(libc::CLONE_FS) {
                        0 => libc::fchdir(directory.as_raw_fd()),
                        error => error,
                    }
                };
                if changed == -1 {
                    return Err(io::Error::last_os_error());
                }
                UnixStream::connect("socket")
            })
            .join()
            .expect("socket connector")?
        };
        Self::from_socket(socket, progress, time)
    }

    pub fn from_socket(
        socket: UnixStream,
        progress: &mut dyn Progress,
        time: Time,
    ) -> Result<(Self, FirstThread), Error> {
        socket.set_nonblocking(true)?;
        // SAFETY: a connected client socket is new input for the server's event set.
        #[cfg(any(target_vendor = "apple", target_os = "android"))]
        unsafe {
            pipes::hitode_server_connected()
        };
        let (version, request) = receive_fd(&socket, progress, time)?;
        let request = PipeFd(request);
        if version != PROTOCOL_VERSION {
            return Err(Error::Version(version));
        }
        let (reply, reply_write) = pipe()?;
        let (wait, wait_write) = pipe()?;
        set_nonblocking(&request)?;
        set_nonblocking(&reply)?;
        set_nonblocking(&wait)?;
        // The first thread has no id yet; 0 names it.
        send_fd(&socket, 0, reply_write.as_raw_fd())?;
        send_fd(&socket, 0, wait_write.as_raw_fd())?;
        let mut channel = Channel {
            request,
            reply,
            wait,
        };

        let unix_pid = NEXT_PROCESS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut init = InitFirstThreadRequest {
            unix_pid,
            unix_tid: 1,
            reply_fd: reply_write.as_raw_fd(),
            wait_fd: wait_write.as_raw_fd(),
            ..Default::default()
        };
        let mut machines = [0u8; 16];
        let (answer, size) = channel.call(progress, time, &mut init, &[], &mut machines)?;
        // The server holds its own copies now, as in Wine's client.
        drop((reply_write, wait_write));
        if answer.header.error != 0 {
            return Err(Error::Io(io::Error::other(format!(
                "init_first_thread failed with status {:#x}",
                answer.header.error
            ))));
        }
        let machines = machines[..size]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| u16::from_ne_bytes(pair))
            .collect();
        let first = FirstThread {
            pid: answer.pid,
            tid: answer.tid,
            unix_pid,
            session_id: answer.session_id,
            server_start: answer.server_start,
            info_size: answer.info_size,
            machines,
        };
        let inproc_device = if answer.inproc_device != 0 {
            Some(receive_fd(&socket, progress, time)?.1.into())
        } else {
            None
        };
        Ok((
            Connection {
                socket,
                channel,
                inproc_device,
            },
            first,
        ))
    }
}

impl Channel {
    /// Sends a request with its variable data and returns the fixed reply and
    /// how many bytes of reply data were written to `reply_data`. The status
    /// is in the reply header.
    fn call<R: Request>(
        &mut self,
        progress: &mut dyn Progress,
        time: Time,
        request: &mut R,
        data: &[&[u8]],
        reply_data: &mut [u8],
    ) -> Result<(R::Reply, usize), Error> {
        const { assert!(size_of::<R>() <= FIXED_SIZE && size_of::<R::Reply>() <= FIXED_SIZE) };
        request.header().req = R::NUMBER;
        let mut fixed = [0u8; FIXED_SIZE];
        // SAFETY: `R` is a plain repr(C) layout no larger than the buffer.
        unsafe {
            std::ptr::copy_nonoverlapping(
                (request as *const R).cast(),
                fixed.as_mut_ptr(),
                size_of::<R>(),
            )
        };
        let size = self.call_raw(progress, time, &mut fixed, data, reply_data)?;
        // SAFETY: the reply is a plain repr(C) layout no larger than the buffer.
        let reply: R::Reply = unsafe { std::ptr::read_unaligned(fixed.as_ptr().cast()) };
        Ok((reply, size))
    }

    /// Sends a request whose fixed part, with its number, is in `fixed`, and
    /// replaces it with the fixed reply. The sizes in the header are set
    /// here. Returns how many bytes of reply data were written.
    fn call_raw(
        &mut self,
        progress: &mut dyn Progress,
        time: Time,
        fixed: &mut [u8; FIXED_SIZE],
        data: &[&[u8]],
        reply_data: &mut [u8],
    ) -> Result<usize, Error> {
        let number = i32::from_ne_bytes(fixed[..4].try_into().unwrap());
        crate::registry::invalidate_request(number);
        crate::file::invalidate_request(number);
        let request_size: u32 = data.iter().map(|part| part.len() as u32).sum();
        fixed[4..8].copy_from_slice(&request_size.to_ne_bytes());
        fixed[8..12].copy_from_slice(&(reply_data.len() as u32).to_ne_bytes());
        self.write(progress, time, fixed)?;
        for part in data {
            self.write(progress, time, part)?;
        }
        progress.run_server(time);

        self.read(progress, time, fixed)?;
        // SAFETY: every reply starts with the reply header.
        let header: crate::protocol::ReplyHeader =
            unsafe { std::ptr::read_unaligned(fixed.as_ptr().cast()) };
        let size = (header.reply_size as usize).min(reply_data.len());
        self.read(progress, time, &mut reply_data[..size])?;
        Ok(size)
    }

    /// The wake-up the server sent, if one is waiting in the pipe.
    fn wake_up(&mut self) -> Result<Option<WakeUp>, Error> {
        // struct wake_up_reply
        let mut reply = [0u8; 16];
        loop {
            // SAFETY: a valid descriptor and buffer.
            let read = unsafe {
                pipes::read(
                    self.wait.as_raw_fd(),
                    reply.as_mut_ptr().cast(),
                    reply.len(),
                )
            };
            match read {
                16 => break,
                0 => return Err(Error::Closed),
                read if read > 0 => {
                    return Err(Error::Io(io::Error::other(format!(
                        "partial wake-up of {read} bytes"
                    ))));
                }
                _ => {}
            }
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::EAGAIN) => return Ok(None),
                Some(libc::EINTR) => {}
                _ => return Err(error.into()),
            }
        }
        Ok(Some(WakeUp {
            cookie: u64::from_ne_bytes(reply[..8].try_into().unwrap()),
            signaled: u32::from_ne_bytes(reply[8..12].try_into().unwrap()),
        }))
    }

    fn write(
        &mut self,
        progress: &mut dyn Progress,
        time: Time,
        mut bytes: &[u8],
    ) -> Result<(), Error> {
        let mut idle = 0;
        while !bytes.is_empty() {
            // SAFETY: a valid descriptor and buffer.
            let written = unsafe {
                pipes::write(self.request.as_raw_fd(), bytes.as_ptr().cast(), bytes.len())
            };
            if written > 0 {
                bytes = &bytes[written as usize..];
                idle = 0;
                continue;
            }
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::EAGAIN) if idle < PATIENCE => {
                    progress.run_server(time);
                    idle += 1;
                }
                Some(libc::EAGAIN) => return Err(Error::NoReply),
                Some(libc::EPIPE) => return Err(Error::Closed),
                Some(libc::EINTR) => {}
                _ => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn read(
        &mut self,
        progress: &mut dyn Progress,
        time: Time,
        mut buffer: &mut [u8],
    ) -> Result<(), Error> {
        let mut idle = 0;
        while !buffer.is_empty() {
            // SAFETY: a valid descriptor and buffer.
            let read = unsafe {
                pipes::read(
                    self.reply.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            if read > 0 {
                buffer = &mut buffer[read as usize..];
                idle = 0;
                continue;
            }
            if read == 0 {
                return Err(Error::Closed);
            }
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::EAGAIN) if idle < PATIENCE => {
                    progress.run_server(time);
                    idle += 1;
                }
                Some(libc::EAGAIN) => return Err(Error::NoReply),
                Some(libc::EINTR) => {}
                _ => return Err(error.into()),
            }
        }
        Ok(())
    }
}

fn pipe() -> io::Result<(PipeFd, PipeFd)> {
    let mut fds = [0; 2];
    // SAFETY: `fds` has room for both ends.
    if unsafe { pipes::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    for fd in fds {
        // SAFETY: a descriptor we just created.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    // SAFETY: both descriptors are new and owned by nobody else.
    Ok(unsafe {
        (
            PipeFd(OwnedFd::from_raw_fd(fds[0])),
            PipeFd(OwnedFd::from_raw_fd(fds[1])),
        )
    })
}

fn set_nonblocking(fd: &PipeFd) -> io::Result<()> {
    // SAFETY: a valid descriptor.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    // SAFETY: as above.
    if flags < 0
        || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Sends `fd` to the server for thread `tid`; a later request names it by
/// its number in this process.
fn send_fd(socket: &UnixStream, tid: u32, fd: RawFd) -> io::Result<()> {
    let data = [tid, fd as u32];
    let mut iov = libc::iovec {
        iov_base: data.as_ptr() as *mut _,
        iov_len: size_of_val(&data),
    };
    let mut control = [0u8; 64];
    // SAFETY: a zeroed msghdr is valid; the buffers outlive the call, and the
    // control buffer has room for one descriptor.
    unsafe {
        let mut message: libc::msghdr = zeroed();
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = libc::CMSG_SPACE(size_of::<RawFd>() as u32) as _;
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(size_of::<RawFd>() as u32) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<RawFd>(), fd);
        if pipes::sendmsg(socket.as_raw_fd(), &message, 0) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Receives a descriptor the server sent with a handle value.
fn receive_fd(
    socket: &UnixStream,
    progress: &mut dyn Progress,
    time: Time,
) -> Result<(u32, OwnedFd), Error> {
    let mut idle = 0;
    loop {
        let mut handle = 0u32;
        let mut iov = libc::iovec {
            iov_base: (&raw mut handle).cast(),
            iov_len: size_of::<u32>(),
        };
        let mut control = [0u8; 64];
        // SAFETY: as in `send_fd`.
        let mut message: libc::msghdr = unsafe { zeroed() };
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = control.len() as _;
        // SAFETY: as above.
        let received = unsafe { pipes::recvmsg(socket.as_raw_fd(), &mut message, 0) };
        if received == size_of::<u32>() as isize {
            // SAFETY: the kernel filled in the control buffer of `message`.
            let header = unsafe { libc::CMSG_FIRSTHDR(&message) };
            // SAFETY: as above; an SCM_RIGHTS message carries a descriptor we now own.
            unsafe {
                if header.is_null() || (*header).cmsg_type != libc::SCM_RIGHTS {
                    return Err(Error::Io(io::Error::other(
                        "server message without a descriptor",
                    )));
                }
                let fd = std::ptr::read_unaligned(libc::CMSG_DATA(header).cast::<RawFd>());
                return Ok((handle, OwnedFd::from_raw_fd(fd)));
            }
        }
        if received == 0 {
            return Err(Error::Closed);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EAGAIN) if idle < PATIENCE => {
                progress.run_server(time);
                idle += 1;
            }
            Some(libc::EAGAIN) => return Err(Error::NoReply),
            Some(libc::EINTR) => {}
            _ => return Err(error.into()),
        }
    }
}

/// Return the execution lease before a queued server signal can target another context.
pub(crate) fn boundary(env: &mut Env<'_>, outcome: adv_machine::Outcome) -> adv_machine::Outcome {
    if env
        .ext
        .try_get::<Link>()
        .is_some_and(|link| link.progress.interrupt_pending())
        && !matches!(
            outcome,
            adv_machine::Outcome::HostRequest { .. }
                | adv_machine::Outcome::ExitProcess(_)
                | adv_machine::Outcome::ExitThread(_)
                | adv_machine::Outcome::Fail(_)
        )
    {
        adv_machine::Outcome::host_request(move |_| outcome)
    } else {
        outcome
    }
}

/// A cached synchronization descriptor; waits retain it across NtClose.
#[derive(Clone)]
pub(crate) struct InprocFd {
    pub fd: Rc<crate::ntsync::Fd>,
    pub kind: u32,
    pub access: u32,
}

pub(crate) fn inproc_device(env: &mut Env<'_>) -> Option<RawFd> {
    env.ext
        .try_get::<Link>()?
        .inproc_device
        .as_ref()
        .map(AsRawFd::as_raw_fd)
}

/// Wine's get_inproc_sync_fd and per-thread get_inproc_alert_fd requests.
pub(crate) fn inproc_fd(
    env: &mut Env<'_>,
    handle: Option<u32>,
) -> Result<InprocFd, crate::status::Error> {
    let context = env.context;
    let link = link(env)?;
    if let Some(handle) = handle {
        if let Some(cached) = link.inproc.get(&handle) {
            return Ok(cached.clone());
        }
    } else if let Some(alert) = &link.thread(context).map_err(broken)?.alert {
        // Only synchronization waits use alerts, so they need no cache entry.
        return Ok(InprocFd {
            fd: alert.clone(),
            kind: 1,
            access: 0,
        });
    }
    let name = if handle.is_some() {
        "get_inproc_sync_fd"
    } else {
        "get_inproc_alert_fd"
    };
    let number = crate::protocol::REQUEST_NAMES
        .iter()
        .position(|&s| s == name)
        .unwrap() as i32;
    let mut fixed = [0u8; FIXED_SIZE];
    fixed[..4].copy_from_slice(&number.to_ne_bytes());
    fixed[12..16].copy_from_slice(&handle.unwrap_or(0).to_ne_bytes());
    let link = env.ext.get_mut::<Link>();
    link.call_raw(context, env.cpu, &mut fixed, &[], &mut [])
        .map_err(broken)?;
    let word = |offset| u32::from_ne_bytes(fixed[offset..offset + 4].try_into().unwrap());
    crate::status::check(word(0))?;
    let (_, fd) = receive_fd(&link.socket, &mut *link.progress, link.time).map_err(broken)?;
    let fd = crate::ntsync::Fd::from(fd);
    let cached = if let Some(handle) = handle {
        let cached = InprocFd {
            fd: Rc::new(fd),
            kind: word(8),
            access: word(12),
        };
        if handle < 0xffff_fffa {
            link.inproc.insert(handle, cached.clone());
        }
        cached
    } else {
        let fd = Rc::new(fd);
        let cached = InprocFd {
            fd: fd.clone(),
            kind: 1,
            access: 0,
        };
        link.thread(context).map_err(broken)?.alert = Some(fd);
        cached
    };
    deliver(env);
    Ok(cached)
}

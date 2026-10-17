//! Interactive debugger service (`--debug` or `ADV_DEBUG=1`), read by
//! `tools/debug/hitode_debug.py`.
//!
//! A local socket answers one request per line, `<id> <command> [args]`,
//! with one JSON line `{"id", "ok", "text"}`. Commands that read only what
//! this module records (timeline, marks, samples, snapshots) are answered on
//! the socket thread, so they work while the guest is wedged. Commands that
//! read guest state wait for the execution loop's next boundary and report a
//! timeout instead of blocking. Nothing here changes guest execution.
//!
//! A frame gap of at least [`STALL_US`] that the execution thread spent
//! mostly in guest steps takes a snapshot by itself once [`STALL_SETTLE_US`]
//! has passed, so a felt stall is kept without the hotkey. Gaps spent
//! waiting are not: engines that present only on change leave those on
//! every still screen.
use crate::window::WakeEvent;
use adv_runtime::Engine;
use adv_runtime::boot::Runtime;
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

mod profile;

/// How long a socket request waits for a guest boundary.
const BOUNDARY_TIMEOUT: Duration = Duration::from_secs(5);
/// Frames the timeline keeps: ten minutes at 60 fps.
const TIMELINE_CAPACITY: usize = 36_000;
/// What a snapshot looks back over.
const SNAPSHOT_WINDOW_US: u64 = 30_000_000;
const SNAPSHOTS_KEPT: usize = 8;
/// A busy frame gap at least this long snapshots itself.
const STALL_US: u64 = 250_000;
/// How long after a stall its snapshot waits, so the window shows the
/// frames after it and nearby stalls share one snapshot.
const STALL_SETTLE_US: u64 = 2_000_000;
/// Stall snapshots one session writes at most.
const STALL_SNAPSHOTS: usize = 32;
/// Host functions where a sampled execution thread is waiting, not working.
const IDLE: &[&str] = &[
    "poll",
    "mach_msg2_trap",
    "mach_msg_trap",
    "__psynch_cvwait",
    "__psynch_mutexwait",
    "__workq_kernreturn",
    "kevent",
    "kevent_id",
    "semaphore_wait_trap",
    "semaphore_timedwait_trap",
    "__semwait_signal",
    "__ulock_wait",
    "__ulock_wait2",
    "__select",
    "__pselect",
    "nanosleep",
    "usleep",
];

pub(crate) fn unix_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

/// Whether the session should serve a debugger.
pub(crate) fn requested(option: bool) -> bool {
    option || std::env::var_os("ADV_DEBUG").is_some_and(|value| value != "0")
}

/// The folder holding one socket per debuggable session.
pub(crate) fn socket_dir() -> PathBuf {
    std::env::temp_dir().join("hitode-debug")
}

#[derive(Clone, Copy)]
struct FrameRow {
    unix_us: u64,
    frame: u64,
    /// Execution-thread time in guest steps since the previous frame.
    busy_us: u64,
    host_calls: u64,
    native_entries: u64,
    interpreter_entries: u64,
}

struct Request {
    line: String,
    reply: mpsc::Sender<Result<String, String>>,
}

struct Shared {
    info: String,
    timeline: Mutex<VecDeque<FrameRow>>,
    marks: Mutex<Vec<(u64, String)>>,
    snapshots: Mutex<VecDeque<(String, String)>>,
    profiler: Arc<profile::Profiler>,
    requests: Mutex<mpsc::Sender<Request>>,
    wake: WakeEvent,
    stopped: AtomicBool,
}

pub(crate) struct Debugger {
    shared: Arc<Shared>,
    receiver: mpsc::Receiver<Request>,
    socket: PathBuf,
    snapshot_dir: PathBuf,
    busy_us: u64,
    last: FrameRow,
    slow_baseline: HashMap<u32, [u64; 3]>,
    /// Slow-path counts at the previous snapshot.
    snapshot_baseline: HashMap<u32, [u64; 3]>,
    /// The longest busy gap waiting for its snapshot, and when it ended.
    stall: Option<(u64, u64)>,
    stall_snapshots: usize,
}

impl Debugger {
    /// Starts serving on the calling thread, which must be the execution thread.
    pub(crate) fn start(info: String, data_dir: &Path, wake: WakeEvent) -> Result<Self, String> {
        #[cfg(target_arch = "aarch64")]
        adv_runtime::jit_map_enable();
        let (sender, receiver) = mpsc::channel();
        let shared = Arc::new(Shared {
            info: format!("{info}\n{}", host_image()),
            timeline: Mutex::new(VecDeque::with_capacity(1024)),
            marks: Mutex::default(),
            snapshots: Mutex::default(),
            profiler: profile::Profiler::start_on_current_thread(),
            requests: Mutex::new(sender),
            wake,
            stopped: AtomicBool::new(false),
        });
        let directory = socket_dir();
        let socket = directory.join(format!("{}-{}.sock", std::process::id(), unix_us()));
        listen(&directory, &socket, shared.clone())?;
        eprintln!("debug: serving {}", socket.display());
        Ok(Self {
            shared,
            receiver,
            socket,
            snapshot_dir: data_dir.join("debug"),
            busy_us: 0,
            last: FrameRow {
                unix_us: unix_us(),
                frame: 0,
                busy_us: 0,
                host_calls: 0,
                native_entries: 0,
                interpreter_entries: 0,
            },
            slow_baseline: HashMap::new(),
            snapshot_baseline: HashMap::new(),
            stall: None,
            stall_snapshots: 0,
        })
    }

    /// Answers queued guest-state requests at this boundary.
    pub(crate) fn serve(&mut self, runtime: &mut Runtime) {
        while let Ok(request) = self.receiver.try_recv() {
            let result = self.execute(&request.line, runtime);
            let _ = request.reply.send(result);
        }
        if let Some((gap_us, ended)) = self.stall
            && unix_us() >= ended + STALL_SETTLE_US
        {
            self.stall = None;
            self.stall_snapshots += 1;
            let label = format!("stall-{}ms", gap_us / 1000);
            mark(&self.shared, label.clone());
            match self.snapshot(&label, runtime) {
                Ok(name) => eprintln!("debug: snapshot {name}"),
                Err(error) => eprintln!("debug: snapshot failed: {error}"),
            }
        }
    }

    /// Accounts one guest step and records a timeline row per new frame.
    pub(crate) fn after_step(&mut self, step: Duration, presented: u64, runtime: &Runtime) {
        self.busy_us += step.as_micros() as u64;
        if presented == self.last.frame {
            return;
        }
        let counters = runtime.machine.cpu.counters();
        let row = FrameRow {
            unix_us: unix_us(),
            frame: presented,
            busy_us: std::mem::take(&mut self.busy_us),
            host_calls: runtime.machine.sys.host_calls,
            native_entries: counters.native_entries,
            interpreter_entries: counters.interpreter_entries,
        };
        let delta = FrameRow {
            host_calls: row.host_calls - self.last.host_calls.min(row.host_calls),
            native_entries: row.native_entries - self.last.native_entries.min(row.native_entries),
            interpreter_entries: row.interpreter_entries
                - self.last.interpreter_entries.min(row.interpreter_entries),
            ..row
        };
        let gap_us = row.unix_us.saturating_sub(self.last.unix_us);
        // The wait for the first frame is loading, not a stall.
        if self.last.frame != 0
            && gap_us >= STALL_US
            && delta.busy_us * 2 >= gap_us
            && self.stall_snapshots < STALL_SNAPSHOTS
        {
            let longest = self.stall.map_or(0, |(gap, _)| gap).max(gap_us);
            self.stall = Some((longest, row.unix_us));
        }
        self.last = row;
        let mut timeline = self
            .shared
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if timeline.len() == TIMELINE_CAPACITY {
            timeline.pop_front();
        }
        timeline.push_back(delta);
    }

    /// Ctrl+Shift+F12: mark the moment and keep a snapshot of it.
    pub(crate) fn hotkey(&mut self, runtime: &mut Runtime) {
        mark(&self.shared, "hotkey".into());
        match self.snapshot("hotkey", runtime) {
            Ok(name) => eprintln!("debug: snapshot {name}"),
            Err(error) => eprintln!("debug: snapshot failed: {error}"),
        }
    }

    fn execute(&mut self, line: &str, runtime: &mut Runtime) -> Result<String, String> {
        let mut words = line.split_whitespace();
        let command = words.next().unwrap_or_default();
        let args: Vec<&str> = words.collect();
        match command {
            "threads" => Ok(threads(runtime)),
            "stack" => {
                let context = number(args.first().ok_or("stack requires CONTEXT")?)?;
                let frames = args.get(1).map_or(Ok(32), |n| number(n))?;
                runtime.stack_scan(context, frames as usize)
            }
            "calls" => {
                let count = args.first().map_or(Ok(64), |n| number(n))?;
                Ok(runtime
                    .machine
                    .sys
                    .recent_calls_tail(count as usize)
                    .join("\n"))
            }
            "gaps" => Ok(runtime.api_gaps_report()),
            "modules" => Ok(runtime.modules_report()),
            "mem" => {
                let address = number(args.first().ok_or("mem requires ADDRESS [LENGTH]")?)?;
                runtime.memory_dump(address, args.get(1).map_or(Ok(64), |n| number(n))?)
            }
            "dis" => {
                let address = number(args.first().ok_or("dis requires ADDRESS [COUNT]")?)?;
                runtime.disassemble(address, args.get(1).map_or(Ok(16), |n| number(n))? as usize)
            }
            "slow" => Ok(self.slow(runtime, args.first() == Some(&"reset"))),
            "report" => Ok(runtime.report()),
            "snap" => {
                let label = args.first().copied().unwrap_or("manual");
                let name = self.snapshot(label, runtime)?;
                let snapshots = self
                    .shared
                    .snapshots
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                Ok(snapshots
                    .back()
                    .map(|(_, text)| text.clone())
                    .unwrap_or_default())
                .map(|text| format!("{name}\n{text}"))
            }
            _ => Err(format!("unknown command {command:?}; try help")),
        }
    }

    /// Slow-path runs by EIP since the last `slow reset`, busiest first.
    fn slow(&mut self, runtime: &Runtime, reset: bool) -> String {
        #[cfg(target_arch = "aarch64")]
        {
            let executor = runtime.machine.cpu.executor();
            let sites = executor.slow_path_sites();
            if reset {
                self.slow_baseline = sites.into_iter().collect();
                return "slow-path counts reset".into();
            }
            let mut rows: Vec<_> = sites
                .into_iter()
                .map(|(eip, counts)| {
                    let base = self.slow_baseline.get(&eip).copied().unwrap_or_default();
                    (eip, [0, 1, 2].map(|i| counts[i].saturating_sub(base[i])))
                })
                .filter(|(_, counts)| counts.iter().sum::<u64>() > 0)
                .collect();
            rows.sort_by_key(|(_, counts)| std::cmp::Reverse(counts.iter().sum::<u64>()));
            let mut text =
                vec!["   total interpreted  faulted   helper  eip        place / form".to_owned()];
            for (eip, [interpreted, faulted, helper]) in rows.into_iter().take(40) {
                text.push(format!(
                    "{:>8} {interpreted:>11} {faulted:>8} {helper:>8}  {eip:#010x} {} {:?}",
                    interpreted + faulted + helper,
                    runtime.guest_place(eip),
                    executor.instruction_form(eip)
                ));
            }
            text.join("\n")
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            let _ = (runtime, reset, &self.slow_baseline);
            "slow-path sites are recorded by the a64 backend only".into()
        }
    }

    /// Writes and keeps a snapshot; returns its name.
    fn snapshot(&mut self, label: &str, runtime: &mut Runtime) -> Result<String, String> {
        let now = unix_us();
        let since = now.saturating_sub(SNAPSHOT_WINDOW_US);
        let label: String = label
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .take(32)
            .collect();
        let name = format!("snap-{}-{label}", now / 1000);
        let mut text = String::new();
        let mut section = |title: &str, body: String| {
            text.push_str(&format!("== {title}\n{body}\n\n"));
        };
        section("info", self.shared.info.clone());
        section("marks", marks_text(&self.shared, 0));
        section("timeline (last 30 s)", timeline_text(&self.shared, since));
        section(
            "profile (last 30 s)",
            profile_summary(&self.shared, since, runtime),
        );
        section("samples (last 30 s)", samples_text(&self.shared, since));
        section("threads", threads(runtime));
        section(
            "calls",
            runtime.machine.sys.recent_calls_tail(256).join("\n"),
        );
        section("gaps", runtime.api_gaps_report());
        let reset = std::mem::take(&mut self.slow_baseline);
        self.slow_baseline = std::mem::take(&mut self.snapshot_baseline);
        section(
            "slow paths (since previous snapshot)",
            self.slow(runtime, false),
        );
        self.slow_baseline = HashMap::new();
        section("slow paths (whole session)", self.slow(runtime, false));
        self.slow(runtime, true);
        self.snapshot_baseline = std::mem::replace(&mut self.slow_baseline, reset);
        section("modules", runtime.modules_report());
        section("report", runtime.report());
        std::fs::create_dir_all(&self.snapshot_dir).map_err(|e| e.to_string())?;
        let path = self.snapshot_dir.join(format!("{name}.txt"));
        std::fs::write(&path, &text).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut snapshots = self
            .shared
            .snapshots
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if snapshots.len() == SNAPSHOTS_KEPT {
            snapshots.pop_front();
        }
        snapshots.push_back((name.clone(), text));
        Ok(name)
    }
}

impl Drop for Debugger {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::Release);
        let _ = std::fs::remove_file(&self.socket);
    }
}

fn number(text: &str) -> Result<u32, String> {
    match text.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => text.parse(),
    }
    .map_err(|_| format!("invalid number {text:?}"))
}

fn threads(runtime: &Runtime) -> String {
    let mut text = runtime.contexts_report();
    for context in runtime.machine.sys.contexts.iter().filter(|c| !c.is_done()) {
        if let Ok(stack) = runtime.stack_scan(context.id, 12) {
            text.push_str(&format!("\n{stack}"));
        }
    }
    text
}

fn mark(shared: &Shared, label: String) {
    shared
        .marks
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push((unix_us(), label));
}

fn marks_text(shared: &Shared, since: u64) -> String {
    let marks = shared.marks.lock().unwrap_or_else(|p| p.into_inner());
    marks
        .iter()
        .filter(|(at, _)| *at >= since)
        .map(|(at, label)| format!("{at}\t{label}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn timeline_text(shared: &Shared, since: u64) -> String {
    let timeline = shared.timeline.lock().unwrap_or_else(|p| p.into_inner());
    let start = timeline.partition_point(|row| row.unix_us < since);
    let mut text =
        vec!["unix_us\tframe\tbusy_us\thost_calls\tnative_entries\tinterpreter_entries".to_owned()];
    text.extend(timeline.range(start..).map(|row| {
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            row.unix_us,
            row.frame,
            row.busy_us,
            row.host_calls,
            row.native_entries,
            row.interpreter_entries
        )
    }));
    text.join("\n")
}

/// What one sample was doing: in generated code, in a host function called
/// from generated code, in other host code, or waiting.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Kind {
    Guest,
    HostFromGuest,
    Host,
    Idle,
}

struct Classified {
    unix_us: u64,
    kind: Kind,
    /// Guest unit start of the innermost generated-code frame.
    eip: Option<u32>,
    /// Innermost host frame, if the sample was in host code.
    host: Option<usize>,
}

fn classify(sample: &profile::Sample) -> Classified {
    let frames = &sample.frames[..usize::from(sample.depth)];
    let at_ns = u128::from(sample.unix_us) * 1000;
    let guest = |pc: usize| {
        #[cfg(target_arch = "aarch64")]
        {
            adv_runtime::jit_map_lookup(pc, at_ns)
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            let _ = (pc, at_ns);
            None
        }
    };
    let pc = frames.first().copied().unwrap_or_default();
    if let Some(eip) = guest(pc) {
        return Classified {
            unix_us: sample.unix_us,
            kind: Kind::Guest,
            eip: Some(eip),
            host: None,
        };
    }
    let eip = frames.iter().skip(1).find_map(|&frame| guest(frame));
    let kind = match eip {
        Some(_) => Kind::HostFromGuest,
        None if symbol(pc).is_some_and(|(_, name)| IDLE.contains(&name.as_str())) => Kind::Idle,
        None => Kind::Host,
    };
    Classified {
        unix_us: sample.unix_us,
        kind,
        eip,
        host: Some(pc),
    }
}

/// Image file name and dynamic symbol for a host address. Symbols of the
/// runner itself are usually not dynamic, so the client symbolizes those.
fn symbol(pc: usize) -> Option<(String, String)> {
    #[cfg(unix)]
    {
        let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
        // SAFETY: dladdr only reads loader tables; `pc` need not be valid.
        if unsafe { libc::dladdr(pc as *const libc::c_void, &mut info) } == 0 {
            return None;
        }
        let text = |p: *const libc::c_char| {
            (!p.is_null())
                .then(|| {
                    unsafe { std::ffi::CStr::from_ptr(p) }
                        .to_string_lossy()
                        .into_owned()
                })
                .unwrap_or_default()
        };
        let image = text(info.dli_fname);
        let image = image.rsplit('/').next().unwrap_or_default().to_owned();
        Some((image, text(info.dli_sname)))
    }
    #[cfg(not(unix))]
    {
        let _ = pc;
        None
    }
}

/// `host_image=PATH` and `host_base=ADDRESS` of the runner, so a client can
/// symbolize samples after the process has ended.
fn host_image() -> String {
    #[cfg(unix)]
    {
        let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
        // SAFETY: dladdr only reads loader tables.
        if unsafe { libc::dladdr(host_image as *const libc::c_void, &mut info) } != 0
            && !info.dli_fname.is_null()
        {
            let path = unsafe { std::ffi::CStr::from_ptr(info.dli_fname) }.to_string_lossy();
            return format!(
                "host_image={path}\nhost_base={:#x}",
                info.dli_fbase as usize
            );
        }
    }
    String::new()
}

fn samples_text(shared: &Shared, since: u64) -> String {
    let mut text = vec!["unix_us\tkind\teip\tpc\timage\tsymbol".to_owned()];
    for sample in shared.profiler.since(since) {
        let row = classify(&sample);
        let (image, name) = row.host.and_then(symbol).unwrap_or_default();
        text.push(format!(
            "{}\t{:?}\t{}\t{}\t{image}\t{name}",
            row.unix_us,
            row.kind,
            row.eip
                .map_or_else(|| "-".into(), |eip| format!("{eip:#010x}")),
            row.host.map_or_else(|| "-".into(), |pc| format!("{pc:#x}")),
        ));
    }
    text.join("\n")
}

fn profile_summary(shared: &Shared, since: u64, runtime: &Runtime) -> String {
    let samples: Vec<_> = shared.profiler.since(since).iter().map(classify).collect();
    if samples.is_empty() {
        return format!("no samples (sampler at {} Hz)", shared.profiler.hz());
    }
    let mut kinds: HashMap<Kind, usize> = HashMap::new();
    let mut units: HashMap<u32, usize> = HashMap::new();
    let mut hosts: HashMap<String, usize> = HashMap::new();
    for sample in &samples {
        *kinds.entry(sample.kind).or_default() += 1;
        if let Some(eip) = sample.eip {
            *units.entry(eip).or_default() += 1;
        }
        if let (Some(pc), Kind::Host | Kind::HostFromGuest) = (sample.host, sample.kind) {
            let (image, name) = symbol(pc).unwrap_or_default();
            *hosts.entry(format!("{image}!{name}")).or_default() += 1;
        }
    }
    let total = samples.len();
    let share = |n: usize| 100.0 * n as f64 / total as f64;
    let mut text = vec![format!(
        "{total} samples: guest {:.1}%, host from guest {:.1}%, host {:.1}%, idle {:.1}%",
        share(kinds.get(&Kind::Guest).copied().unwrap_or_default()),
        share(kinds.get(&Kind::HostFromGuest).copied().unwrap_or_default()),
        share(kinds.get(&Kind::Host).copied().unwrap_or_default()),
        share(kinds.get(&Kind::Idle).copied().unwrap_or_default()),
    )];
    let mut units: Vec<_> = units.into_iter().collect();
    units.sort_by_key(|&(eip, n)| (std::cmp::Reverse(n), eip));
    text.push("guest units (start EIP):".into());
    for (eip, n) in units.into_iter().take(25) {
        text.push(format!(
            "  {n:>6} {:5.1}% {eip:#010x} {}",
            share(n),
            runtime.guest_place(eip)
        ));
    }
    let mut hosts: Vec<_> = hosts.into_iter().collect();
    hosts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    text.push("host functions (dynamic symbols; the runner's own need the client):".into());
    for (name, n) in hosts.into_iter().take(15) {
        text.push(format!("  {n:>6} {:5.1}% {name}", share(n)));
    }
    text.join("\n")
}

/// Answers what this module records without the execution thread.
fn immediate(shared: &Shared, command: &str, args: &[&str]) -> Option<Result<String, String>> {
    let seconds_back = |default: u64| {
        let seconds = args
            .first()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(default);
        unix_us().saturating_sub(seconds * 1_000_000)
    };
    Some(match command {
        "ping" => Ok("pong".into()),
        "help" => Ok(HELP.into()),
        "info" => Ok(format!(
            "{}\nsampler_hz={}\nframes={}",
            shared.info,
            shared.profiler.hz(),
            shared
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .back()
                .map_or(0, |row| row.frame)
        )),
        "timeline" => Ok(timeline_text(shared, seconds_back(30))),
        "marks" => Ok(marks_text(shared, 0)),
        "mark" => {
            let label = if args.is_empty() {
                "mark".into()
            } else {
                args.join(" ")
            };
            mark(shared, label);
            Ok("marked".into())
        }
        "samples" => Ok(samples_text(shared, seconds_back(30))),
        "profile" => match args.first().copied() {
            None => Ok(format!("sampler at {} Hz", shared.profiler.hz())),
            Some(_) if !shared.profiler.supported => {
                Err("the in-process sampler supports Apple arm64 hosts only".into())
            }
            Some("off") => {
                shared.profiler.set_hz(0);
                Ok("sampler off".into())
            }
            Some(rate) => rate
                .parse::<u32>()
                .map(|hz| {
                    shared.profiler.set_hz(hz);
                    format!("sampler at {} Hz", shared.profiler.hz())
                })
                .map_err(|_| "profile [off|HZ]".into()),
        },
        "snaps" => Ok(shared
            .snapshots
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>()
            .join("\n")),
        "pull" => {
            let snapshots = shared.snapshots.lock().unwrap_or_else(|p| p.into_inner());
            let wanted = args.first().copied().unwrap_or("");
            snapshots
                .iter()
                .rev()
                .find(|(name, _)| wanted.is_empty() || name == wanted)
                .map(|(name, text)| format!("{name}\n{text}"))
                .ok_or_else(|| format!("no snapshot {wanted:?}"))
        }
        _ => return None,
    })
}

const HELP: &str = "\
answered at once: ping, help, info, timeline [SECONDS], marks, mark [LABEL],
  samples [SECONDS], profile [off|HZ], snaps, pull [NAME]
at a guest boundary: threads, stack CONTEXT [FRAMES], calls [COUNT], gaps,
  modules, mem ADDRESS [LENGTH], dis ADDRESS [COUNT], slow [reset], report,
  snap [LABEL]";

#[cfg(unix)]
fn listen(directory: &Path, socket: &Path, shared: Arc<Shared>) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    std::fs::create_dir_all(directory).map_err(|e| format!("{}: {e}", directory.display()))?;
    let _ = std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700));
    let listener = UnixListener::bind(socket).map_err(|e| format!("{}: {e}", socket.display()))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    std::thread::Builder::new()
        .name("adv-debug-listen".into())
        .spawn(move || {
            while !shared.stopped.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let shared = shared.clone();
                        let _ = stream.set_nonblocking(false);
                        let _ = std::thread::Builder::new()
                            .name("adv-debug-client".into())
                            .spawn(move || connection(stream, &shared));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(100)),
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(not(unix))]
fn listen(_: &Path, _: &Path, _: Arc<Shared>) -> Result<(), String> {
    Err("the debugger socket needs a Unix host".into())
}

#[cfg(unix)]
fn connection(stream: std::os::unix::net::UnixStream, shared: &Shared) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else {
            return;
        };
        let line = line.trim();
        let (id, rest) = line.split_once(' ').unwrap_or((line, ""));
        let result = answer(shared, rest.trim());
        let (ok, text) = match result {
            Ok(text) => (true, text),
            Err(text) => (false, text),
        };
        let reply = format!(
            "{{\"id\":{},\"ok\":{ok},\"text\":{}}}\n",
            crate::json(id),
            crate::json(&text)
        );
        if writer.write_all(reply.as_bytes()).is_err() || shared.stopped.load(Ordering::Acquire) {
            return;
        }
    }
}

fn answer(shared: &Shared, line: &str) -> Result<String, String> {
    let mut words = line.split_whitespace();
    let command = words.next().unwrap_or_default();
    let args: Vec<&str> = words.collect();
    if let Some(result) = immediate(shared, command, &args) {
        return result;
    }
    let (reply, response) = mpsc::channel();
    let request = Request {
        line: line.to_owned(),
        reply,
    };
    let sent = shared
        .requests
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .send(request);
    if sent.is_err() {
        return Err("the session has ended".into());
    }
    shared.wake.notify();
    let started = Instant::now();
    response.recv_timeout(BOUNDARY_TIMEOUT).unwrap_or_else(|_| {
        Err(format!(
            "no guest boundary within {} ms: the execution thread is busy in one step or stuck \
             in host code; `samples 5` shows where",
            started.elapsed().as_millis()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_take_hex_and_decimal() {
        assert_eq!(number("0x401000"), Ok(0x401000));
        assert_eq!(number("12"), Ok(12));
        assert!(number("zz").is_err());
    }
}

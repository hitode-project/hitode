//! Bounded backend comparison; outcomes alone do not establish CPU/HLE causality.
//! Input: ADV_BISECT_EXE (use $ADV_SAMPLES), ADV_BISECT_BACKENDS (default
//! interpreter,unicorn), ADV_BISECT_SECONDS (1..=120, default 10), optional
//! ADV_BISECT_CODEPAGE. Run: cargo test -p adv-runtime --test backend_bisect
//! backend_bisect -- --ignored --exact --nocapture. Output is statistics only;
//! every backend runs in a child with an external deadline covering boot/run/drop.
//! Each run retains a unique target directory, image/runner snapshots and cmd/hashes.
//! Artifacts use CARGO_TARGET_DIR when set, otherwise this worktree's target.
#![cfg(windows)]
use adv_cpu::backend::{Backend, Exit, Guest, Prot, Reg, TrapAction, TrapHandler};
use adv_machine::{RunResult, SliceResult};
use adv_runtime::boot::Runtime;
use std::fs;
use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
#[path = "support/cpu.rs"]
mod backend;
const WORKER: &str = "ADV_BISECT_WORKER";
const DATA: &str = "ADV_BISECT_DATA";
const BACKEND: &str = "ADV_BISECT_BACKEND";
static NEXT: AtomicU32 = AtomicU32::new(0);
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
struct Summary {
    outcome: &'static str,
    elapsed_ms: u128,
    details: String,
}
fn target_directory() -> PathBuf {
    let path = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    fs::create_dir_all(&path).unwrap();
    fs::canonicalize(path).unwrap()
}
fn runner(path: &Path) -> PathBuf {
    path.join("backend-bisect-runner.exe")
}
fn directory(label: &str) -> PathBuf {
    let root = target_directory().join("investigations/backend-bisect");
    fs::create_dir_all(&root).unwrap();
    let path = root.join(format!(
        "{label}-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path).unwrap();
    fs::copy(std::env::current_exe().unwrap(), runner(&path)).expect("freeze test runner");
    path
}
fn wait(child: &mut OwnedChild, started: Instant, limit: Duration) -> &'static str {
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            return if status.success() {
                "completed"
            } else {
                "worker_failed"
            };
        }
        let remaining = limit.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            if child.0.kill().is_err() {
                // Completion may race the deadline between try_wait and kill.
                if let Some(status) = child.0.try_wait().unwrap() {
                    return if status.success() {
                        "completed"
                    } else {
                        "worker_failed"
                    };
                }
                panic!("cannot stop the owned worker");
            }
            child.0.wait().expect("reap owned worker");
            return "hard_timeout";
        }
        std::thread::sleep(remaining.min(Duration::from_millis(5)));
    }
}
fn supervise(mode: &str, name: &str, path: &Path, limit: Duration) -> Summary {
    let started = Instant::now();
    let temp = path.join("tmp");
    fs::create_dir_all(&temp).unwrap();
    let mut cmd = Command::new(runner(path));
    cmd.args([
        "--ignored",
        "--exact",
        "backend_bisect_child",
        "--test-threads=1",
    ])
    .env(WORKER, mode)
    .env(DATA, path)
    .env(BACKEND, name)
    .env("TEMP", &temp)
    .env("TMP", &temp)
    .current_dir(path)
    .creation_flags(0x08000000)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    if mode == "run" {
        cmd.env(
            "ADV_BISECT_EXE",
            fs::canonicalize(std::env::var_os("ADV_BISECT_EXE").unwrap()).unwrap(),
        );
    }
    let mut child = OwnedChild(cmd.spawn().expect("start isolated worker"));
    let outcome = wait(&mut child, started, limit);
    Summary {
        outcome,
        elapsed_ms: started.elapsed().as_millis(),
        details: fs::read_to_string(path.join("summary.txt"))
            .or_else(|_| fs::read_to_string(path.join("progress.txt")))
            .unwrap_or_default(),
    }
}
fn hash(path: &Path, temp: &Path) -> String {
    // The path is an environment argument, never interpolated PowerShell code.
    // Normalize extended local paths to their Win32 spelling for PowerShell.
    let path = path.to_string_lossy();
    let path = path.strip_prefix(r"\\?\").unwrap_or(&path);
    let started = Instant::now();
    let mut child = OwnedChild(
        Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                "$f=[IO.File]::OpenRead($env:ADV_BISECT_HASH_ITEM); $h=[Security.Cryptography.SHA256]::Create(); try { [BitConverter]::ToString($h.ComputeHash($f)).Replace('-','') } finally { $h.Dispose(); $f.Dispose() }",
            ])
            .env("ADV_BISECT_HASH_ITEM", path)
            .env("TEMP", temp)
            .env("TMP", temp)
            .creation_flags(0x08000000)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let result = wait(&mut child, started, Duration::from_secs(5));
    if result != "completed" {
        let mut error = Vec::new();
        child
            .0
            .stderr
            .take()
            .unwrap()
            .read_to_end(&mut error)
            .unwrap();
        fs::write(
            Path::new(path).parent().unwrap().join("hash-error.txt"),
            error,
        )
        .unwrap();
        panic!("bounded hash process failed; local error retained");
    }
    let mut output = String::new();
    child
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();
    let output = output.trim();
    assert!(output.len() == 64 && output.bytes().all(|b| b.is_ascii_hexdigit()));
    output.into()
}
fn evidence(path: &Path, name: &str, seconds: u64, image: &Path) {
    let temp = path.join("tmp");
    fs::create_dir_all(&temp).unwrap();
    let source = fs::canonicalize(std::env::var_os("ADV_BISECT_EXE").unwrap()).unwrap();
    let reference = std::env::var_os("ADV_SAMPLES")
        .and_then(|root| fs::canonicalize(root).ok())
        .and_then(|root| {
            source
                .strip_prefix(root)
                .ok()
                .map(|relative| format!("$ADV_SAMPLES/{}", relative.display()))
        })
        .unwrap_or_else(|| source.display().to_string());
    let command = format!(
        concat!(
            "Command: cargo test -p adv-runtime --test backend_bisect backend_bisect -- --ignored --exact --nocapture\n",
            "Worker: {} --ignored --exact backend_bisect_child --test-threads=1\n",
            "ADV_BISECT_EXE={reference}\nADV_BISECT_BACKENDS={name}\nADV_BISECT_SECONDS={seconds}\n",
            "ADV_BISECT_CODEPAGE={}\nCARGO_TARGET_DIR={}\nCARGO_PROFILE_DEV_DEBUG={}\nCARGO_INCREMENTAL={}\n",
            "TEMP={}\nTMP={}\ninput snapshot SHA-256={}\ntest runner SHA-256={}\n"
        ),
        runner(path).display(),
        std::env::var("ADV_BISECT_CODEPAGE").unwrap_or_else(|_| "default".into()),
        target_directory().display(),
        std::env::var("CARGO_PROFILE_DEV_DEBUG").unwrap_or_default(),
        std::env::var("CARGO_INCREMENTAL").unwrap_or_default(),
        temp.display(),
        temp.display(),
        hash(image, &temp),
        hash(&runner(path), &temp),
        reference = reference,
        name = name,
        seconds = seconds,
    );
    fs::write(path.join("cmd.txt"), command).unwrap();
}
fn seconds() -> u64 {
    let value = std::env::var("ADV_BISECT_SECONDS")
        .map_or(Ok(10), |s| s.parse::<u64>())
        .expect("ADV_BISECT_SECONDS must be an integer");
    assert!(
        (1..=120).contains(&value),
        "ADV_BISECT_SECONDS must be between 1 and 120"
    );
    value
}
fn public_call(module: &str, name: &str) -> Option<String> {
    static EXPORTS: OnceLock<std::collections::BTreeSet<(&str, &str)>> = OnceLock::new();
    let exports = EXPORTS.get_or_init(|| {
        include_str!("../../../data/abi/system-exports.tsv")
            .lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| {
                let mut fields = line.split('\t');
                let module = fields.next()?;
                let name = fields.nth(1)?;
                (!name.is_empty()).then_some((module, name))
            })
            .collect()
    });
    let mut module = module.to_ascii_lowercase();
    if !module.ends_with(".dll") {
        module.push_str(".dll");
    }
    exports
        .contains(&(module.as_str(), name))
        .then(|| format!("{module}!{name}"))
}
fn statistics(runtime: &Runtime<backend::Cpu>, outcome: &str, exit: Option<u32>) -> String {
    let sys = &runtime.machine.sys;
    let (first, latest, source) = if sys.first_cpu_fault.is_some() {
        (
            sys.first_cpu_fault.as_ref(),
            sys.latest_cpu_fault.as_ref(),
            "live",
        )
    } else if let Some(process) = runtime
        .process_statuses()
        .iter()
        .find(|p| p.failure.is_some() && p.first_cpu_fault.is_some())
    {
        (
            process.first_cpu_fault.as_ref(),
            process.latest_cpu_fault.as_ref(),
            "retained",
        )
    } else {
        (None, None, "none")
    };
    let null_access = first
        .and_then(|fault| fault.access())
        .map_or_else(|| "none".into(), |(_, address)| (address == 0).to_string());
    let same_pc = first.zip(latest).map_or_else(
        || "none".into(),
        |(first, latest)| (first.fault.pc == latest.fault.pc).to_string(),
    );
    // Resolve only fixed public exports; do not format caller addresses,
    // arguments, private handlers, MachineError or fault-reason strings.
    let calls = sys
        .recent
        .iter()
        .filter_map(|&(_, thunk, _)| {
            let thunk = sys.thunks.get(thunk);
            public_call(&thunk.module, &thunk.name)
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        concat!(
            "worker_stage={} guest_outcome={outcome} frames={} host_calls={} failure_code={} exit_code={} ",
            "first_fault_source={source} first_fault_kind={} first_fault_null_access={null_access} ",
            "first_fault_pc_equal_latest={same_pc} recent_public_calls={}\n"
        ),
        if outcome == "running" {
            "step"
        } else {
            "finished"
        },
        sys.completed_frames,
        sys.host_calls,
        sys.guest_failure_code
            .map_or_else(|| "none".into(), |code| code.to_string()),
        exit.map_or_else(|| "none".into(), |code| code.to_string()),
        first.map_or("none", |fault| fault.kind_name()),
        if calls.is_empty() { "none" } else { &calls },
        source = source,
        outcome = outcome,
        null_access = null_access,
        same_pc = same_pc,
    )
}

#[test]
fn public_call_filter_excludes_private_names_and_keeps_native_exports() {
    assert_eq!(
        public_call("kernel32", "Sleep"),
        Some("kernel32.dll!Sleep".into())
    );
    assert_eq!(
        public_call("ntdll.dll", "NtRaiseException"),
        Some("ntdll.dll!NtRaiseException".into())
    );
    assert_eq!(public_call("<win32>", "thread_exception_handler"), None);
    assert_eq!(public_call("private.dll", "Sleep"), None);
    assert_eq!(public_call("kernel32.dll", "private_handler"), None);
}
#[test]
#[ignore = "needs ADV_BISECT_EXE"]
fn backend_bisect() {
    let exe = fs::canonicalize(std::env::var_os("ADV_BISECT_EXE").expect("ADV_BISECT_EXE"))
        .expect("input executable");
    let seconds = seconds();
    let names =
        std::env::var("ADV_BISECT_BACKENDS").unwrap_or_else(|_| "interpreter,unicorn".into());
    let mut seen = std::collections::BTreeSet::new();
    for name in names.split(',').map(str::trim) {
        assert!(
            matches!(name, "interpreter" | "jit" | "unicorn"),
            "unknown backend"
        );
        assert!(seen.insert(name), "duplicate backend");
        let path = directory(name);
        let overlay = path.join("data/overlay");
        fs::create_dir_all(&overlay).unwrap();
        // The VFS opens this snapshot before the original main image, while
        // the original game directory still supplies read-only assets/imports.
        let snapshot = overlay.join(exe.file_name().unwrap());
        fs::copy(&exe, &snapshot).unwrap();
        evidence(&path, name, seconds, &snapshot);
        let result = supervise("run", name, &path, Duration::from_secs(seconds));
        let output = format!(
            "backend={name} outcome={} elapsed_ms={} {}\n",
            result.outcome,
            result.elapsed_ms,
            result.details.trim()
        );
        print!("{output}");
        fs::write(path.join("result.txt"), output).unwrap();
    }
}
#[test]
#[ignore = "internal isolated worker"]
fn backend_bisect_child() {
    let mode = std::env::var(WORKER).expect("worker mode");
    let path = PathBuf::from(std::env::var_os(DATA).expect("worker directory"));
    assert_eq!(
        fs::canonicalize(std::env::current_exe().unwrap()).unwrap(),
        fs::canonicalize(runner(&path)).unwrap(),
        "worker must execute the frozen runner"
    );
    match mode.as_str() {
        "hang" => std::thread::sleep(Duration::from_secs(30)),
        "cpu" => {
            struct NoTrap;
            impl TrapHandler for NoTrap {
                fn on_trap(&mut self, _: &mut dyn Guest, _: u8) -> TrapAction {
                    panic!("authored loop trapped")
                }
            }
            let mut cpu = backend::Cpu::new(&std::env::var(BACKEND).unwrap()).unwrap();
            cpu.map(0x400000, 4096, Prot::RX).unwrap();
            cpu.write(0x400000, &[0xeb, 0xfe]).unwrap();
            cpu.set_reg(Reg::Eip, 0x400000);
            cpu.set_execution_budget(Some(Duration::from_millis(20)));
            assert!(
                matches!(cpu.run(&mut NoTrap), Exit::BudgetExceeded { .. }),
                "loop did not stop on budget"
            );
            fs::write(path.join("summary.txt"), "budget_stopped=true\n").unwrap();
        }
        "run" => {
            let started = Instant::now();
            let limit = Duration::from_secs(seconds()).saturating_sub(Duration::from_millis(200));
            fs::write(path.join("progress.txt"), "worker_stage=boot\n").unwrap();
            let exe = std::env::var_os("ADV_BISECT_EXE").expect("input executable");
            let mut runtime = match Runtime::with_cpu(
                &exe,
                path.join("data"),
                backend::Cpu::new(&std::env::var(BACKEND).unwrap()).unwrap(),
                64,
                32,
            ) {
                Ok(runtime) => runtime,
                Err(_) => {
                    fs::write(
                        path.join("summary.txt"),
                        "worker_stage=boot guest_outcome=load_failed frames=0 host_calls=0\n",
                    )
                    .unwrap();
                    return;
                }
            };
            if let Ok(codepage) = std::env::var("ADV_BISECT_CODEPAGE") {
                runtime
                    .set_codepage(codepage.parse().expect("integer codepage"))
                    .unwrap();
            }
            let (outcome, exit) = loop {
                let remaining = limit.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    break ("soft_timeout", None);
                }
                fs::write(
                    path.join("progress.txt"),
                    statistics(&runtime, "running", None),
                )
                .unwrap();
                match runtime.step_for(remaining.min(Duration::from_millis(100))) {
                    SliceResult::Yielded | SliceResult::Complete(RunResult::HostRequest) => {}
                    SliceResult::Complete(RunResult::Idle {
                        next_deadline: Some(_),
                    }) => std::thread::sleep(Duration::from_millis(5).min(remaining)),
                    SliceResult::Complete(RunResult::Idle {
                        next_deadline: None,
                    }) => break ("idle", None),
                    SliceResult::Complete(RunResult::Exited(code)) => break ("exited", Some(code)),
                    SliceResult::Complete(RunResult::Failed(_)) => break ("failed", None),
                }
            };
            fs::write(
                path.join("summary.txt"),
                statistics(&runtime, outcome, exit),
            )
            .unwrap();
        }
        _ => panic!("unknown worker mode"),
    }
}
#[test]
fn parent_kills_a_worker_that_never_returns_without_reusing_data() {
    let a = directory("timeout-control");
    let b = directory("timeout-control");
    assert_ne!(a, b);
    fs::write(a.join("sentinel"), b"preserved").unwrap();
    let result = supervise("hang", "unicorn", &b, Duration::from_millis(150));
    assert_eq!(result.outcome, "hard_timeout");
    assert!(result.elapsed_ms < 2000);
    assert_eq!(fs::read(a.join("sentinel")).unwrap(), b"preserved");
}
#[test]
fn authored_cpu_loops_stop_on_all_backends_with_external_deadlines() {
    for name in ["interpreter", "jit", "unicorn"] {
        let path = directory(name);
        let result = supervise("cpu", name, &path, Duration::from_secs(3));
        assert_eq!(result.outcome, "completed");
        assert_eq!(result.details, "budget_stopped=true\n");
    }
}

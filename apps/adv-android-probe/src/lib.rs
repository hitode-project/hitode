//! A0 application-process evidence: actual x86 guest instructions through the
//! shared interpreter and Cranelift JIT. No game assets or signal handlers.
use adv_cpu::backend::{Guest, Prot};
use adv_cpu::interpreter::{Interpreter, Step};
use adv_cpu::{CpuState, EXEC, Executor, Memory, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use iced_x86::{Decoder, DecoderOptions, Mnemonic};
use jni::JNIEnv;
use jni::objects::{JClass, JString};
use jni::sys::jstring;
use serde_json::{Value, json};
use std::any::Any;
use std::fs::{self, File};
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Once;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const CODE: u32 = 0x10000;
const CALLEE: u32 = 0x12000;
const DATA: u32 = 0x28000;
const STOP: u32 = 0x38000;
const STACK: u32 = 0x3f000;
const WINDOW: usize = 0x30000;
const ITERATIONS: u32 = 1024;
const BUDGET: u64 = 100_000;
const ARITH_FLAGS: u32 = 0x8d5;
const REPORT_NAME: &str = "cpu-probe.json";

struct Program {
    name: &'static str,
    regions: Vec<(u32, Vec<u8>)>,
    expected: [u32; 4], // EAX, ECX, EDX, EBX
    expected_data: u32,
    require_link: bool,
}

struct Trace {
    state: CpuState,
    data: u32,
    stack: Vec<u8>,
    details: Value,
}

fn assemble(
    at: u32,
    build: impl FnOnce(&mut CodeAssembler) -> Result<(), iced_x86::IcedError>,
) -> Result<Vec<u8>, String> {
    let mut assembler = CodeAssembler::new(32).map_err(|e| e.to_string())?;
    build(&mut assembler).map_err(|e| e.to_string())?;
    assembler.assemble(u64::from(at)).map_err(|e| e.to_string())
}

fn programs() -> Result<Vec<Program>, String> {
    let integer = assemble(CODE, |a| {
        a.mov(eax, -1)?;
        a.add(eax, 1)?;
        a.adc(ebx, 41)?;
        a.mov(edx, i32::MAX)?;
        a.add(edx, 1)?;
        a.seto(cl)?;
        a.mov(dword_ptr(DATA), ebx)?;
        a.ret()
    })?;
    let sum = assemble(CODE, |a| {
        let mut again = a.create_label();
        a.xor(eax, eax)?;
        a.mov(ecx, ITERATIONS)?;
        a.set_label(&mut again)?;
        a.add(eax, ecx)?;
        a.dec(ecx)?;
        a.jnz(again)?;
        a.mov(dword_ptr(DATA), eax)?;
        a.ret()
    })?;
    let linked = assemble(CODE, |a| {
        let mut again = a.create_label();
        a.xor(eax, eax)?;
        a.mov(ecx, ITERATIONS)?;
        a.set_label(&mut again)?;
        a.call(u64::from(CALLEE))?;
        a.dec(ecx)?;
        a.jnz(again)?;
        a.mov(dword_ptr(DATA), eax)?;
        a.ret()
    })?;
    let callee = assemble(CALLEE, |a| {
        a.add(eax, 3)?;
        a.ret()
    })?;
    Ok(vec![
        Program {
            name: "integer_carry_overflow",
            regions: vec![(CODE, integer)],
            expected: [0, 1, 0x8000_0000, 42],
            expected_data: 42,
            require_link: false,
        },
        Program {
            name: "integer_loop_1024",
            regions: vec![(CODE, sum)],
            expected: [ITERATIONS * (ITERATIONS + 1) / 2, 0, 0, 0],
            expected_data: ITERATIONS * (ITERATIONS + 1) / 2,
            require_link: false,
        },
        Program {
            name: "linked_call_return_loop_1024",
            regions: vec![(CODE, linked), (CALLEE, callee)],
            expected: [ITERATIONS * 3, 0, 0, 0],
            expected_data: ITERATIONS * 3,
            require_link: true,
        },
    ])
}

fn initial_state() -> CpuState {
    let mut state = CpuState {
        eip: CODE,
        eflags: 0x202,
        ..CpuState::default()
    };
    state.regs[4] = STACK;
    state
}

fn setup_memory(program: &Program) -> Result<Memory, String> {
    let mut memory = Memory::new(CODE, WINDOW)?;
    memory.protect(CODE, WINDOW, READ | WRITE)?;
    for (at, bytes) in &program.regions {
        memory.write(*at, bytes)?;
        memory.protect(*at, 4096, READ | EXEC)?;
    }
    memory.protect(STOP, 4096, READ | EXEC)?;
    memory.write(STACK, &STOP.to_le_bytes())?;
    Ok(memory)
}

fn trace(state: CpuState, memory: &Memory, details: Value) -> Result<Trace, String> {
    let data = memory.read(DATA, 4)?;
    let data = u32::from_le_bytes(data.try_into().map_err(|_| "invalid data length")?);
    Ok(Trace {
        state,
        data,
        stack: memory.read(STACK - 16, 32)?.to_vec(),
        details,
    })
}

fn interpreter(program: &Program) -> Result<Trace, String> {
    let mut memory = setup_memory(program)?;
    let mut state = initial_state();
    let mut interpreter = Interpreter::new();
    let mut instructions = 0;
    let started = Instant::now();
    while state.eip != STOP && instructions < BUDGET {
        match interpreter.step(&mut state, &mut memory) {
            Step::Executed => instructions += 1,
            step => return Err(format!("interpreter stopped: {step:?}")),
        }
    }
    if state.eip != STOP {
        return Err(format!("interpreter budget exhausted at {:#x}", state.eip));
    }
    trace(
        state,
        &memory,
        json!({
            "interpreter_instructions": instructions,
            "decodes": interpreter.decode_count(),
            "execution_us": started.elapsed().as_micros() as u64,
            "native_entries": 0,
        }),
    )
}

fn jit(program: &Program) -> Result<Trace, String> {
    let mut executor = Executor::new(CODE, WINDOW)?;
    executor.enable_auto_jit(false);
    executor.set_async_compilation(false);
    executor.set_region_linking(true);
    executor.protect(CODE, WINDOW, READ | WRITE)?;
    for (at, bytes) in &program.regions {
        executor.write(*at, bytes)?;
        executor.protect(*at, 4096, READ | EXEC)?;
    }
    executor.protect(STOP, 4096, READ | EXEC)?;
    executor.write(STACK, &STOP.to_le_bytes())?;
    for (at, bytes) in &program.regions {
        let mut region = Region::new(*at, bytes.len());
        // The callee's RET can tail-link into the continuation in the caller.
        region.entries.extend(
            Decoder::with_ip(32, bytes, u64::from(*at), DecoderOptions::NONE)
                .into_iter()
                .filter(|instruction| instruction.mnemonic() == Mnemonic::Call)
                .map(|instruction| instruction.next_ip32()),
        );
        executor.add_region(region)?;
    }
    executor.state = initial_state();
    let started = Instant::now();
    let outcome = executor.run(STOP, BUDGET)?;
    let execution_us = started.elapsed().as_micros() as u64;
    if outcome != RunOutcome::ReachedStop {
        return Err(format!("JIT stopped: {outcome:?}"));
    }
    if executor.stats.native_entries == 0
        || executor.stats.native_blocks == 0
        || executor.stats.native_code_bytes == 0
    {
        return Err(format!(
            "no generated machine code executed: {:?}",
            executor.stats
        ));
    }
    if executor.stats.interpreter_instructions != 0 {
        return Err(format!(
            "unexpected interpreter fallback: {:?}",
            executor.stats
        ));
    }
    // Unlinked, every call and return of the loop re-enters from Rust.
    if program.require_link && executor.stats.native_entries >= u64::from(ITERATIONS) {
        return Err(format!(
            "region linking did not execute: {:?}",
            executor.stats
        ));
    }
    let stats = &executor.stats;
    trace(
        executor.state.clone(),
        executor.memory(),
        json!({
            "compiler": "Cranelift 0.125; adv-cpu Executor::add_region",
            "execution_us": execution_us,
            "compile_us": stats.compile_time.as_micros() as u64,
            "compilations": stats.compilations,
            "native_regions": stats.native_regions,
            "native_code_bytes": stats.native_code_bytes,
            "native_entries": stats.native_entries,
            "native_blocks": stats.native_blocks,
            "interpreter_instructions": stats.interpreter_instructions,
            "interpreter_entries": stats.interpreter_entries,
        }),
    )
}

fn check_expected(program: &Program, actual: &Trace) -> Result<(), String> {
    if actual.state.regs[..4] != program.expected
        || actual.state.regs[4] != STACK + 4
        || actual.state.eip != STOP
        || actual.data != program.expected_data
    {
        return Err(format!(
            "wrong result: regs={:?}, eip={:#x}, data={}, expected={:?}/{}",
            actual.state.regs,
            actual.state.eip,
            actual.data,
            program.expected,
            program.expected_data
        ));
    }
    Ok(())
}

fn compare(reference: &Trace, actual: &Trace) -> Result<(), String> {
    let r = &reference.state;
    let a = &actual.state;
    if r.regs != a.regs
        || r.eip != a.eip
        || r.eflags & ARITH_FLAGS != a.eflags & ARITH_FLAGS
        || r.xmm != a.xmm
        || r.mmx != a.mmx
        || r.x87 != a.x87
        || r.mxcsr != a.mxcsr
        || r.fault_addr != a.fault_addr
        || reference.data != actual.data
        || reference.stack != actual.stack
    {
        return Err(format!(
            "JIT/interpreter state mismatch: reference={r:?}, actual={a:?}, data={}/{}, stack={:?}/{:?}",
            reference.data, actual.data, reference.stack, actual.stack
        ));
    }
    Ok(())
}

fn panic_text(panic: Box<dyn Any + Send>) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string Rust panic".to_owned())
}

fn caught<T>(body: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(body))
        .unwrap_or_else(|panic| Err(format!("Rust panic contained: {}", panic_text(panic))))
}

#[cfg(target_os = "android")]
fn log_line(message: &str) {
    use std::ffi::{CString, c_char, c_int};
    #[link(name = "log")]
    unsafe extern "C" {
        fn __android_log_write(priority: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    }
    if let Ok(message) = CString::new(message.replace('\0', "?")) {
        // SAFETY: both pointers name NUL-terminated strings for this call only.
        unsafe {
            __android_log_write(4, c"AdvCpuProbe".as_ptr(), message.as_ptr());
        }
    }
}

#[cfg(not(target_os = "android"))]
fn log_line(message: &str) {
    eprintln!("AdvCpuProbe: {message}");
}

fn install_panic_hook() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        std::panic::set_hook(Box::new(|panic| {
            log_line(&format!("contained panic: {panic}"))
        }))
    });
}

fn persist(directory: &Path, report: &Value) -> Result<String, String> {
    let text = serde_json::to_string_pretty(report).map_err(|e| e.to_string())?;
    let temporary = directory.join("cpu-probe.json.tmp");
    let mut file = File::create(&temporary).map_err(|e| format!("create report: {e}"))?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("write report: {e}"))?;
    fs::rename(&temporary, directory.join(REPORT_NAME))
        .map_err(|e| format!("publish report: {e}"))?;
    Ok(text)
}

/// Exercise shared storage in the application's SELinux domain, independently
/// of shell acceptance. The callback retains its alias after guest unmap/reuse.
fn shared_pcm() -> Result<Value, String> {
    let mut cpu = adv_cpu::JitCpu::new(STOP, false)?;
    let shared = cpu
        .map_shared_audio(DATA, 8192)
        .map_err(|e| format!("{e:?}"))?;
    cpu.write(DATA, &[0x12, 0x34])
        .map_err(|e| format!("{e:?}"))?;
    cpu.write(DATA + 4096, &[0x56])
        .map_err(|e| format!("{e:?}"))?;
    let callback = shared.read_alias.clone();
    let reader = std::thread::spawn(move || {
        for _ in 0..4096 {
            if callback.load_u8(0) != 0x12 || callback.load_u8(4096) != 0x56 {
                return Err("shared callback observed unrelated bytes".to_string());
            }
            let _ = callback.load_u8(1); // concurrently changing PCM byte
        }
        Ok(callback)
    });
    for value in 0..4096u32 {
        cpu.write(DATA + 1, &[value as u8])
            .map_err(|e| format!("{e:?}"))?;
    }
    let callback = reader.join().map_err(|_| "PCM reader panicked")??;
    cpu.protect(DATA, 8192, Prot(0))
        .map_err(|e| format!("{e:?}"))?;
    if callback.load_u8(0) != 0x12 {
        return Err("guest protection changed callback mapping".into());
    }
    cpu.unmap(DATA, 8192).map_err(|e| format!("{e:?}"))?;
    let replacement = cpu
        .map_shared_audio(DATA, 8192)
        .map_err(|e| format!("{e:?}"))?;
    cpu.write(DATA, &[0xa5]).map_err(|e| format!("{e:?}"))?;
    drop(cpu);
    if callback.load_u8(0) != 0x12
        || callback.load_u8(4096) != 0x56
        || replacement.read_alias.load_u8(0) != 0xa5
    {
        return Err("PCM alias lost ownership across unmap/reuse/destruction".into());
    }
    Ok(json!({
        "bytes": shared.len,
        "concurrent_byte_writes": 4096,
        "callback_iterations": 4096,
        "guest_protection_unmap_reuse_owner_drop": "passed",
        "old_alias_first_byte": callback.load_u8(0),
        "replacement_alias_first_byte": replacement.read_alias.load_u8(0),
    }))
}

/// Save the running stage before entering generated code, so an OS-level fatal
/// fault leaves a useful report. Existing ART/Android signal handlers are kept.
pub fn run_probe(directory: &Path, device: Value) -> Result<String, String> {
    install_panic_hook();
    fs::create_dir_all(directory).map_err(|e| format!("report directory: {e}"))?;
    #[cfg(unix)]
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    #[cfg(not(unix))]
    let page_size = 0i64;
    let mut report = json!({
        "schema": 1,
        "probe": "adv-cpu-android-application-process",
        "status": "running",
        "package": "org.advhost.cpuprobe",
        "build": {
            "version": env!("CARGO_PKG_VERSION"),
            "identifier": option_env!("ADV_ANDROID_PROBE_BUILD").unwrap_or("unlabelled"),
            "arch": std::env::consts::ARCH,
            "os": std::env::consts::OS,
        },
        "host": {
            "page_size": page_size,
            "guest_page_size": adv_cpu::PAGE_SIZE,
            "selinux_context": fs::read_to_string("/proc/self/attr/current").map(|s| s.trim_matches('\0').trim().to_owned()).ok(),
            "started_unix_ms": SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).ok(),
        },
        "device": device,
        "limits": { "guest_workers": 1, "iterations": ITERATIONS, "work_budget_per_path": BUDGET },
        "coverage": {
            "workloads": "self-authored x86-32 carry/overflow, integer loop, two-region call/return loop",
            "jit_evidence": "nonzero native entries, native blocks and generated code bytes; no interpreter fallback; linked entries required for call/return case",
            "compilation": "synchronous explicit regions, existing Cranelift JIT memory and publication path",
            "state_comparison": "all integer registers, EIP, defined arithmetic flags, XMM/MMX/x87/MXCSR, fault address, data and touched stack bytes; excludes execution fuel and executor bookkeeping",
            "shared_pcm": "POSIX dual mapping creation, concurrent byte-atomic access and alias ownership across guest protection/unmap/address reuse/machine destruction; no DAC",
            "pending": ["background compilation publication", "link target replacement/invalidation", "loop kernel/vector batches", "16 KiB runtime on device", "host fault recovery"],
        },
        "running_path": null,
        "results": [],
    });
    persist(directory, &report)?;
    let programs = programs()?;
    let mut all_passed = true;
    for program in &programs {
        let mut reference: Option<Trace> = None;
        for path in ["interpreter", "jit"] {
            report["running_path"] = json!(format!("{}/{path}", program.name));
            persist(directory, &report)?;
            log_line(&format!("starting {}/{path}", program.name));
            let result = caught(|| {
                let actual = if path == "interpreter" {
                    interpreter(program)?
                } else {
                    jit(program)?
                };
                check_expected(program, &actual)?;
                if path == "jit" {
                    compare(
                        reference.as_ref().ok_or("interpreter reference failed")?,
                        &actual,
                    )?;
                }
                Ok(actual)
            });
            let result_json = match result {
                Ok(actual) => {
                    let record = json!({
                        "workload": program.name, "path": path, "status": "passed",
                        "regs": actual.state.regs, "eip": actual.state.eip,
                        "eflags": actual.state.eflags,
                        "checked_arithmetic_flags_mask": ARITH_FLAGS,
                        "data_word": actual.data, "expected_data_word": program.expected_data,
                        "details": actual.details,
                    });
                    if path == "interpreter" {
                        reference = Some(actual);
                    }
                    record
                }
                Err(error) => {
                    all_passed = false;
                    json!({ "workload": program.name, "path": path, "status": "failed", "error": error })
                }
            };
            log_line(&result_json.to_string());
            report["results"]
                .as_array_mut()
                .ok_or("invalid report results")?
                .push(result_json);
            persist(directory, &report)?;
        }
    }
    report["running_path"] = json!("shared_pcm/application");
    persist(directory, &report)?;
    let pcm_result = match caught(shared_pcm) {
        Ok(details) => {
            json!({"workload": "shared_pcm", "path": "application", "status": "passed", "details": details})
        }
        Err(error) => {
            all_passed = false;
            json!({"workload": "shared_pcm", "path": "application", "status": "failed", "error": error})
        }
    };
    log_line(&pcm_result.to_string());
    report["results"]
        .as_array_mut()
        .ok_or("invalid report results")?
        .push(pcm_result);
    report["running_path"] = Value::Null;
    report["status"] = json!(if all_passed { "passed" } else { "failed" });
    report["host"]["finished_unix_ms"] = json!(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .ok()
    );
    let result = persist(directory, &report)?;
    log_line(&format!(
        "finished status={} report={}",
        report["status"],
        directory.join(REPORT_NAME).display()
    ));
    Ok(result)
}

/// JNI owns all Java references on the calling background thread. Every Rust
/// panic is caught before returning through this ABI; Java catches JNI errors.
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_advhost_cpuprobe_ProbeActivity_nativeRun(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    directory: JString<'_>,
    device_json: JString<'_>,
) -> jstring {
    let result = caught(|| {
        let directory: String = env
            .get_string(&directory)
            .map_err(|e| format!("JNI directory: {e}"))?
            .into();
        let device_json: String = env
            .get_string(&device_json)
            .map_err(|e| format!("JNI device info: {e}"))?
            .into();
        let device = serde_json::from_str(&device_json).map_err(|e| format!("device JSON: {e}"))?;
        run_probe(Path::new(&directory), device)
    });
    let text = result.unwrap_or_else(|error| {
        log_line(&format!("JNI probe failed: {error}"));
        json!({ "schema": 1, "status": "failed", "error": error }).to_string()
    });
    catch_unwind(AssertUnwindSafe(|| env.new_string(&text)))
        .ok()
        .and_then(Result::ok)
        .map_or(std::ptr::null_mut(), |string| string.into_raw())
}

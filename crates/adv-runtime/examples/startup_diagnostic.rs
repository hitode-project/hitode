//! Local, headless failure evidence with guest code/state snapshots. No game mutation.
//! startup_diagnostic <exe> <output-directory> [seconds=10] [codepage=932] [hex-pages...]
//! ADV_STARTUP_API_MODE=strict|discovery selects the diagnostic API policy.
//! ADV_STARTUP_GPU=1 installs the normal device for GPU initialization evidence.
use adv_cpu::backend::{ArchitecturalState, Guest, GuestExt, Reg};
use adv_machine::{
    Env, Outcome, RunResult, SliceResult,
    exception::{Checkpoint, Exception, ExceptionHandler},
};
use adv_runtime::boot::Runtime;
use iced_x86::{Code, Decoder, DecoderOptions, Instruction, OpKind, Register};
use std::collections::BTreeSet;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct FaultDiagnostics {
    original: ExceptionHandler,
    directory: PathBuf,
    faults: usize,
}

#[cfg(feature = "gpu")]
fn initialize<T>(future: impl std::future::Future<Output = T>) -> T {
    use std::task::{Context, Poll, Wake, Waker};
    struct Owner(std::thread::Thread);
    impl Wake for Owner {
        fn wake(self: std::sync::Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(std::sync::Arc::new(Owner(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(result) => return result,
            Poll::Pending => std::thread::park(),
        }
    }
}

fn register_value(register: Register, state: &ArchitecturalState, regs: &[u32; 8]) -> Option<u64> {
    match register {
        Register::None | Register::CS | Register::DS | Register::ES | Register::SS => Some(0),
        Register::FS => Some(u64::from(state.fs_base)),
        _ => {
            let full = register.full_register32();
            if !(Register::EAX..=Register::EDI).contains(&full) {
                return None;
            }
            let value = regs[full.number()];
            Some(u64::from(if register.size() == 2 {
                value & 0xffff
            } else {
                value
            }))
        }
    }
}

fn indirect_slot(instruction: &Instruction, state: &ArchitecturalState) -> Option<u32> {
    let mut regs = state.regs;
    match instruction.code() {
        Code::Retnd | Code::Retnd_imm16 => {
            let cleanup = if instruction.code() == Code::Retnd_imm16 {
                u32::from(instruction.immediate16())
            } else {
                0
            };
            return Some(regs[4].wrapping_sub(4 + cleanup));
        }
        // CALL has already pushed its return PC at the fetch-fault boundary.
        Code::Call_rm32 => regs[4] = regs[4].wrapping_add(4),
        Code::Jmp_rm32 => {}
        _ => return None,
    }
    if instruction.op0_kind() != OpKind::Memory {
        return None;
    }
    instruction
        .virtual_address(0, 0, |r, _, _| register_value(r, state, &regs))
        .map(|address| address as u32)
}

fn save_fault(
    env: &Env<'_>,
    event: &Exception,
    state: &ArchitecturalState,
    directory: &Path,
    sequence: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let folder = directory.join(format!("fault-{sequence:03}"));
    std::fs::create_dir(&folder)?;
    let branch = env.cpu.last_indirect_branch();
    let mut report = format!(
        "context={} event={event:#x?}\narchitecture={state:#x?}\nlast_indirect={branch:#x?}\n",
        env.context
    );
    let mut pages = BTreeSet::from([
        state.eip & !4095,
        state.regs[4] & !4095,
        env.ctx().teb & !4095,
    ]);
    if let Some(branch) = branch {
        let mut bytes = Vec::new();
        for offset in 0..15 {
            let Some(address) = branch.source.checked_add(offset) else {
                break;
            };
            let mut byte = [0];
            if env.cpu.read(address, &mut byte).is_err() {
                break;
            }
            bytes.push(byte[0]);
            pages.insert(address & !4095);
        }
        let instruction =
            Decoder::with_ip(32, &bytes, u64::from(branch.source), DecoderOptions::NONE).decode();
        let matches_pc = branch.target == state.eip;
        writeln!(
            report,
            "branch_target_matches_pc={matches_pc} source_bytes={bytes:02x?} decoded={:?}",
            instruction.code()
        )?;
        // These are current source/slot bytes, not an instruction-history log.
        // Only a committed transfer to this PC supports pre-transfer ESP recovery.
        if matches_pc && let Some(slot) = indirect_slot(&instruction, state) {
            let target = env.cpu.read_u32(slot);
            writeln!(
                report,
                "indirect_slot={slot:#010x} current_value={target:#x?}"
            )?;
            pages.insert(slot & !4095);
            pages.insert(slot.wrapping_add(3) & !4095);
        }
    }
    writeln!(report, "stack_words:")?;
    for offset in (0..256).step_by(4) {
        let Some(address) = state.regs[4].checked_add(offset) else {
            break;
        };
        match env.cpu.read_u32(address) {
            Ok(value) => {
                writeln!(report, "{address:08x}: {value:08x}")?;
                // A bounded set also retains caller/record pages for offline analysis.
                if pages.len() < 12 {
                    pages.insert(value & !4095);
                }
            }
            Err(error) => {
                writeln!(report, "{address:08x}: {error}")?;
                break;
            }
        }
    }
    for page in pages.into_iter().take(12) {
        let mut bytes = [0; 4096];
        match env.cpu.read(page, &mut bytes) {
            Ok(()) => std::fs::write(folder.join(format!("page-{page:08x}.bin")), bytes)?,
            Err(error) => writeln!(report, "page-{page:08x}: {error}")?,
        }
    }
    std::fs::write(folder.join("state.txt"), report)?;
    Ok(())
}

fn observe_exception(env: &mut Env<'_>, event: Exception, checkpoint: Checkpoint) -> Outcome {
    let (original, capture) = {
        let observer = env.ext.get_mut::<FaultDiagnostics>();
        let capture = if matches!(
            event,
            Exception::Fault(_) | Exception::Trap { vector: 3, .. }
        ) && observer.faults < 8
        {
            observer.faults += 1;
            Some((observer.directory.clone(), observer.faults))
        } else {
            None
        };
        (observer.original, capture)
    };
    if let Some((directory, sequence)) = capture
        && let Err(error) = save_fault(env, &event, &checkpoint.state, &directory, sequence)
    {
        // Diagnostic I/O cannot replace the guest's original exception outcome.
        eprintln!("fault-{sequence:03} diagnostic failed: {error}");
    }
    original(env, event, checkpoint)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        return Err(
            "startup_diagnostic <exe> <new-output-directory> [seconds=10] [codepage=932]".into(),
        );
    }
    let game = std::fs::canonicalize(&args[0])?;
    let requested = std::path::PathBuf::from(&args[1]);
    let parent = std::fs::canonicalize(
        requested
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new(".")),
    )?;
    let folder = parent.join(
        requested
            .file_name()
            .ok_or("output needs a new directory name")?,
    );
    if folder.starts_with(game.parent().ok_or("missing game directory")?) {
        return Err("output must be outside the game directory".into());
    }
    let seconds: f64 = args.get(2).map_or(Ok(10.0), |s| s.parse())?;
    if !seconds.is_finite() || seconds <= 0.0 || seconds > 300.0 {
        return Err("invalid seconds".into());
    }
    let codepage = args.get(3).map_or(Ok(932), |s| s.parse())?;
    if !matches!(codepage, 932 | 936) {
        return Err("codepage must be 932 or 936".into());
    }
    let api_mode = match std::env::var("ADV_STARTUP_API_MODE").as_deref() {
        Ok("discovery") => Some(adv_runtime::RunMode::Discovery),
        Ok("strict") => Some(adv_runtime::RunMode::Strict),
        Err(std::env::VarError::NotPresent) => None,
        _ => return Err("ADV_STARTUP_API_MODE must be strict or discovery".into()),
    };
    std::fs::create_dir(&folder)?;
    let mut runtime = Runtime::with_codec(
        &game,
        folder.join("overlay"),
        None,
        true,
        640,
        480,
        adv_runtime::vorbis::Policy::Native,
    )?;
    runtime.set_codepage(codepage)?;
    if std::env::var("ADV_STARTUP_GPU").as_deref() == Ok("1") {
        #[cfg(feature = "gpu")]
        runtime.install_gpu(
            initialize(adv_device::gpu::Gpu::request(false)).map_err(|error| error.to_string())?,
        )?;
        #[cfg(not(feature = "gpu"))]
        return Err("ADV_STARTUP_GPU requires the gpu feature".into());
    }
    runtime.set_auto_dialog_ms(Some(50));
    if let Some(mode) = api_mode {
        runtime.set_api_mode(mode);
    }
    if let Some(original) = runtime.machine.sys.exception_handler {
        runtime.machine.ext.insert(FaultDiagnostics {
            original,
            directory: folder.clone(),
            faults: 0,
        });
        runtime.machine.sys.exception_handler = Some(observe_exception);
    }
    runtime.machine.sys.trace = adv_machine::Trace::to_writer(Box::new(std::io::BufWriter::new(
        std::fs::File::create(folder.join("api.log"))?,
    )));
    runtime.machine.sys.trace.all = true;
    let start = Instant::now();
    loop {
        let result = runtime.step_for(Duration::from_millis(10));
        if matches!(
            result,
            SliceResult::Complete(RunResult::Failed(_) | RunResult::Exited(_))
        ) || start.elapsed().as_secs_f64() >= seconds
        {
            println!("{result:?}");
            std::fs::write(
                folder.join("state.txt"),
                format!("{result:?}\n{}", runtime.report()),
            )?;
            break;
        }
    }
    runtime.machine.sys.trace.flush();
    let cpu = &runtime.machine.cpu;
    let pc = cpu.reg(Reg::Eip);
    let sp = cpu.reg(Reg::Esp);
    let mut addresses = vec![pc, sp];
    for arg in args.iter().skip(4) {
        addresses.push(u32::from_str_radix(arg.trim_start_matches("0x"), 16)?);
    }
    for offset in (0..128).step_by(4) {
        if let Ok(address) = cpu.read_u32(sp.wrapping_add(offset)) {
            addresses.push(address);
        }
    }
    addresses.sort_unstable();
    addresses.dedup();
    for address in addresses {
        let page = address & !0xfff;
        let path = folder.join(format!("page-{page:08x}.bin"));
        if path.exists() {
            continue;
        }
        let mut data = [0u8; 4096];
        if cpu.read(page, &mut data).is_ok() {
            std::fs::write(path, data)?;
        }
    }
    Ok(())
}

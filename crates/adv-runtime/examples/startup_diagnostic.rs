//! Local, headless failure evidence with guest code/state snapshots. No game mutation.
//! startup_diagnostic <exe> <output-directory> [seconds=10] [codepage=932] [hex-pages...]
use adv_machine::{RunResult, SliceResult};
use adv_runtime::boot::Runtime;
use pal_cpu::{Guest, GuestExt, Reg};
use std::time::{Duration, Instant};

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
    std::fs::create_dir(&folder)?;
    let mut runtime = Runtime::with_codec(
        &game,
        folder.join("overlay"),
        true,
        640,
        480,
        adv_runtime::vorbis::Policy::Native,
    )?;
    runtime.set_codepage(codepage);
    runtime.set_auto_dialog_ms(Some(50));
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

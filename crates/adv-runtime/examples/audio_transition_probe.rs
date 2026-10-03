//! Read-only interactive audio diagnostic. All guest writes stay in the given overlay.
//! audio_transition_probe <EXE> <jit|jit-sync|interpreter|unicorn> <SECONDS> <OVERLAY>
//! stdin: click X Y, down VK, up VK, capture PATH, report PATH, trace PATH|off, close.
#[path = "../tests/support/cpu.rs"]
mod backend;
use adv_runtime::boot::Runtime;
use pal_cpu::{Guest, GuestExt, Reg};
use pal_machine::{RunResult, SliceResult};
use std::fmt::Write as _;
use std::{
    collections::BTreeMap,
    io::{BufRead, Seek, Write},
    time::{Duration, Instant},
};

fn diagnostic_report(r: &Runtime<backend::Cpu>) -> String {
    let mut report = r.report();
    let m = &r.machine;
    let _ = writeln!(report, "\nContext memory ranges:");
    for ctx in &m.sys.contexts {
        let sp = if ctx.id == m.sys.current {
            Some(m.cpu.reg(Reg::Esp))
        } else {
            ctx.blocked_stack()
        };
        let _ = writeln!(
            report,
            "ctx={} name={:?} stack={:08x}..{:08x} stack_vm={:?} teb={:08x} teb_vm={:?} sp={sp:08x?} sp_vm={:?}",
            ctx.id,
            ctx.name,
            ctx.stack_lo,
            ctx.stack_hi,
            m.sys.vm.containing(ctx.stack_lo),
            ctx.teb,
            m.sys.vm.containing(ctx.teb),
            sp.and_then(|sp| m.sys.vm.containing(sp))
        );
        if let Some(sp) = sp {
            let words: Vec<_> = (0..16)
                .map(|n| m.cpu.read_u32(sp.saturating_add(n * 4)).ok())
                .collect();
            let _ = writeln!(report, "ctx={} stack_words={words:08x?}", ctx.id);
        }
    }
    let _ = writeln!(report, "VM reservations sampled at allocation granules:");
    let mut address = 0u64;
    while address < 1 << 32 {
        if let Some((base, len)) = m.sys.vm.containing(address as u32) {
            let _ = writeln!(
                report,
                "vm={base:08x}..{:08x}",
                u64::from(base) + u64::from(len)
            );
            address = (u64::from(base) + u64::from(len)).div_ceil(65536) * 65536;
        } else {
            address += 65536;
        }
    }
    report
}

fn dump_failed_stacks(r: &Runtime<backend::Cpu>, overlay: &str) {
    let out = std::path::Path::new(overlay)
        .parent()
        .unwrap_or(std::path::Path::new("target"));
    let now = r.machine.sys.clock.now_us();
    for ctx in &r.machine.sys.contexts {
        let mut bytes = vec![0; ctx.stack_hi.saturating_sub(ctx.stack_lo) as usize];
        if r.machine.cpu.read(ctx.stack_lo, &mut bytes).is_ok() {
            let path = out.join(format!(
                "failure-{now}-ctx{}-stack-{:08x}.bin",
                ctx.id, ctx.stack_lo
            ));
            match std::fs::write(&path, &bytes) {
                Ok(()) => eprintln!("stack dump {path:?} bytes={}", bytes.len()),
                Err(error) => eprintln!("stack dump {path:?} failed: {error}"),
            }
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 4 {
        return Err(
            "audio_transition_probe <EXE> <jit|jit-sync|interpreter|unicorn> <SECONDS> <OVERLAY>"
                .into(),
        );
    }
    let cpu_name = if args[1] == "jit-sync" {
        "jit"
    } else {
        &args[1]
    };
    let mut r = Runtime::with_cpu(&args[0], &args[3], backend::Cpu::new(cpu_name)?, 800, 600)?;
    r.machine.cpu.set_async_compilation(args[1] == "jit");
    r.set_codepage(936);
    r.set_audio_enabled(true);
    r.set_auto_dialog_ms(Some(50));
    r.enable_audio_pcm_digests();
    let seconds = args[2].parse::<u64>().map_err(|_| "invalid seconds")?;
    let started = Instant::now();
    let (tx, rx) = std::sync::mpsc::channel();
    let mut previous_files = BTreeMap::new();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    while started.elapsed() < Duration::from_secs(seconds) {
        let files: BTreeMap<_, _> = r
            .machine
            .ext
            .get::<pal_win32::Win32>()
            .files
            .iter()
            .map(|(&id, file)| {
                let mut native = &file.file;
                (id, (file.guest_path.clone(), native.stream_position().ok()))
            })
            .collect();
        if files != previous_files {
            println!("files us={} {files:?}", r.machine.sys.clock.now_us());
            previous_files = files;
        }
        for line in rx.try_iter() {
            if line.trim().is_empty() {
                continue;
            }
            let (cmd, rest) = line.split_once(' ').unwrap_or((&line, ""));
            match cmd {
                "click" => {
                    let mut words = rest.split_whitespace();
                    let x = words
                        .next()
                        .ok_or("missing x")?
                        .parse::<u16>()
                        .map_err(|_| "bad x")?;
                    let y = words
                        .next()
                        .ok_or("missing y")?
                        .parse::<u16>()
                        .map_err(|_| "bad y")?;
                    let lp = u32::from(x) | (u32::from(y) << 16);
                    r.post_input(0x200, 0, lp);
                    r.post_input(0x201, 1, lp);
                    r.post_input(0x202, 0, lp);
                }
                "down" | "up" => {
                    let vk = rest.parse::<u32>().map_err(|_| "bad vk")?;
                    if vk > 255 {
                        return Err("vk exceeds255".into());
                    }
                    r.post_input(
                        if cmd == "down" { 0x100 } else { 0x101 },
                        vk,
                        if cmd == "down" { 1 } else { 0xc0000001 },
                    );
                }
                "report" => {
                    r.machine.sys.trace.flush();
                    std::fs::write(rest, diagnostic_report(&r)).map_err(|e| e.to_string())?;
                }
                "trace" => {
                    r.machine.sys.trace.flush();
                    r.machine.sys.trace = if rest == "off" {
                        pal_machine::Trace::default()
                    } else {
                        let file = std::fs::File::create(rest).map_err(|e| e.to_string())?;
                        let mut trace =
                            pal_machine::Trace::to_writer(Box::new(std::io::BufWriter::new(file)));
                        trace.all = true;
                        trace
                    };
                }
                "capture" => {
                    let frame = r.frame();
                    let mut rgba = frame.bgra.clone();
                    for pixel in rgba.as_chunks_mut::<4>().0 {
                        pixel.swap(0, 2);
                        pixel[3] = 255;
                    }
                    let file = std::fs::File::create(rest).map_err(|e| e.to_string())?;
                    let mut enc = png::Encoder::new(file, frame.width, frame.height);
                    enc.set_color(png::ColorType::Rgba);
                    enc.write_header()
                        .map_err(|e| e.to_string())?
                        .write_image_data(&rgba)
                        .map_err(|e| e.to_string())?;
                }
                "close" => {
                    r.machine.sys.trace.flush();
                    println!("{}", diagnostic_report(&r));
                    return Ok(());
                }
                _ => return Err(format!("unknown command {cmd}")),
            }
            println!("ok us={} {line}", r.machine.sys.clock.now_us());
            std::io::stdout().flush().map_err(|e| e.to_string())?;
        }
        match r.step_for(Duration::from_millis(8)) {
            SliceResult::Complete(RunResult::Failed(e)) => {
                r.machine.sys.trace.flush();
                dump_failed_stacks(&r, &args[3]);
                return Err(format!("{e}\n{}", diagnostic_report(&r)));
            }
            SliceResult::Complete(RunResult::Exited(code)) => {
                println!("exit {code}");
                return Ok(());
            }
            SliceResult::Complete(RunResult::Idle { next_deadline }) => {
                let now = r.machine.sys.clock.now_us();
                let delay = next_deadline.map_or(10_000, |at| at.saturating_sub(now).min(10_000));
                std::thread::sleep(Duration::from_micros(delay));
            }
            SliceResult::Yielded => {}
        }
    }
    r.machine.sys.trace.flush();
    println!("{}", diagnostic_report(&r));
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

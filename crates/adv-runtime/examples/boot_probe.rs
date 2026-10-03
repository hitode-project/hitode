//! Minimal loader probe; graphics and interactive frontend live in adv-run.
use adv_abi::Registry;
use adv_cpu::JitCpu;
use adv_machine::layout::PARK_ADDR;
use adv_machine::{Clock, Machine};
use adv_os::Process;
use adv_os::loader::HostDir;
use adv_win32::{Win32, Win32Config};
use std::path::PathBuf;
fn main() -> Result<(), String> {
    let path = PathBuf::from(std::env::args().nth(1).ok_or("expected exe path")?);
    let jit = match std::env::args().nth(2).as_deref() {
        None | Some("jit") => true,
        Some("interpreter") => false,
        _ => return Err("backend must be jit or interpreter".into()),
    };
    let parent = path.parent().ok_or("no parent")?;
    let name = path.file_name().ok_or("no exe name")?.to_string_lossy();
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, jit)?,
        registry,
        Clock::virtual_time(),
    )
    .map_err(|e| e.to_string())?;
    let mut config = Win32Config::new(parent, &name, PathBuf::from("target/probe-data"));
    config.auto_dialog_ms = Some(50);
    Win32::install(&mut m, config).map_err(|e| e.to_string())?;
    let exe = m
        .load(
            &HostDir::new(parent, "C:\\Game"),
            &format!("C:\\Game\\{name}"),
        )
        .map_err(|e| e.to_string())?;
    m.start_process(exe).map_err(|e| e.to_string())?;
    m.sys.call_limit = Some(50000);
    let result = m.run_virtual(5_000_000);
    println!("result={result:?}\nstats={:?}", m.cpu.stats());
    Ok(())
}

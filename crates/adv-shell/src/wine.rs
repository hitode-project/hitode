//! A program on the Wine userland (issue #58), selected per run with
//! `--wine DIST`. It runs headless for now; the host window comes next.
use std::path::Path;
use std::time::{Duration, Instant};

use adv_runtime::wine::session::{Config, Session, Stop};

use super::Options;

pub(super) fn run(options: &Options, dist: &Path) -> Result<u8, String> {
    if !options.headless {
        return Err("--wine runs headless for now".into());
    }
    let wake = adv_sync::WakeEvent::new().map_err(|e| e.to_string())?;
    let mut background =
        adv_sync::background::Builder::new(wake.clone()).map_err(|e| e.to_string())?;
    let sender = background
        .sender_with_capacity_wake(8, 8, wake.clone())
        .map_err(|e| e.to_string())?;
    let mut service = background.start().map_err(|e| e.to_string())?;
    let root = std::path::absolute(options.data_dir.join("wine")).map_err(|e| e.to_string())?;
    let debug = std::env::var("WINEDEBUG").unwrap_or_default();
    let config = Config {
        dist,
        root: &root,
        exe: &options.exe,
        width: options.width,
        height: options.height,
        jit: options.jit,
        debug: &debug,
        stdout: Some(libc::STDOUT_FILENO),
    };
    let (gpu, software) = (options.headless_gpu, options.gpu_software);
    let gpu_wake = wake.clone();
    let session = Session::start(&config, wake, move |machine| {
        machine.cpu.set_background_sender(sender.into_local())?;
        if gpu {
            let gpu = pollster::block_on(adv_device::gpu::Gpu::request(software))
                .map_err(|e| e.to_string())?;
            Session::attach_gpu(machine, gpu, gpu_wake)?;
        }
        Ok(())
    });
    let result = session.and_then(|mut session| {
        let started = Instant::now();
        let until = options
            .seconds
            .map_or(started + Duration::from_secs(86_400 * 365), |seconds| {
                started + Duration::from_secs_f64(seconds)
            });
        let stop = session.run(until);
        session.capture()?;
        let frame = session.frame();
        eprintln!(
            "wine: {stop:?} after {:.2}s, {} frames presented",
            started.elapsed().as_secs_f64(),
            frame.frames
        );
        if let Some(path) = &options.screenshot {
            super::save_frame(path, frame)?;
        }
        match stop {
            Stop::Exited(code) => Ok(code as u8),
            Stop::Paused => Ok(0),
            Stop::Failed(error) => Err(error),
        }
    });
    service.shutdown();
    result
}

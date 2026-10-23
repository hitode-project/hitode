//! A program on the Wine userland (issue #58), selected per run with
//! `--wine DIST`. Headless, it runs on this thread; with a window, it is the
//! execution worker behind the same UI as the built-in stack.
use std::path::Path;
use std::time::{Duration, Instant};

use adv_runtime::wine::session::{Config, Session, Stop};
use adv_sync::WakeEvent;
use adv_sync::background::LocalSender;

use super::{Finished, Options, frontend, window};

/// How long the guest runs between looks at the UI's events.
const SLICE: Duration = Duration::from_millis(20);

/// What the session gets from the shell before the process starts.
struct Host {
    background: LocalSender,
    gpu: Option<adv_device::gpu::Gpu>,
    frames: Option<(adv_runtime::FrameProducer, WakeEvent)>,
}

fn start(options: &Options, dist: &Path, wake: WakeEvent, host: Host) -> Result<Session, String> {
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
    let gpu_wake = wake.clone();
    Session::start(&config, wake, move |machine| {
        machine.cpu.set_background_sender(host.background)?;
        if let Some(gpu) = host.gpu {
            Session::attach_gpu(machine, gpu, gpu_wake)?;
        }
        if let Some((frames, ui)) = host.frames {
            let store = machine.ext.get_mut::<adv_device::surface::SurfaceStore>();
            store.set_frame_mailbox(frames, ui);
            // The UI takes published frames itself.
            machine.set_frame_handoff(false);
        }
        Ok(())
    })
}

pub(super) fn run(options: &Options, dist: &Path) -> Result<u8, String> {
    let wake = WakeEvent::new().map_err(|e| e.to_string())?;
    let mut background =
        adv_sync::background::Builder::new(wake.clone()).map_err(|e| e.to_string())?;
    let sender = background
        .sender_with_capacity_wake(8, 8, wake.clone())
        .map_err(|e| e.to_string())?;
    let mut service = background.start().map_err(|e| e.to_string())?;
    let result = (|| {
        let gpu = if options.headless_gpu {
            // As the built-in stack attaches a headless GPU.
            let gpu = pollster::block_on(adv_device::gpu::Gpu::request(options.gpu_software))
                .map_err(|e| e.to_string())?;
            gpu.context()
                .set_execution_wake(wake.clone())
                .map_err(|_| "GPU wake already installed")?;
            Some(gpu)
        } else {
            None
        };
        let host = Host {
            background: sender.into_local(),
            gpu,
            frames: None,
        };
        let mut session = start(options, dist, wake, host)?;
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
    })();
    service.shutdown();
    result
}

/// The execution worker of a windowed run: the guest presents through the
/// UI's frame mailbox on the UI's GPU, and closing the window ends the run.
pub(super) fn run_execution(
    options: &Options,
    dist: &Path,
    mut worker: frontend::Worker,
    wake: WakeEvent,
    background: adv_sync::background::Sender,
) -> Result<Finished, String> {
    let started = Instant::now();
    let host = Host {
        background: background.into_local(),
        gpu: worker.gpu.take(),
        frames: Some((worker.take_frames(), worker.ui_wake.clone())),
    };
    let mut session = start(options, dist, wake, host)?;
    let mut performance = super::performance::Performance::default();
    let exit_code = loop {
        if worker.stopped() {
            return Err("UI stopped while the guest was running".into());
        }
        if worker
            .pump(&mut performance)
            .into_iter()
            .any(|event| matches!(event, window::Event::Close | window::Event::ForceClose))
        {
            break 0;
        }
        match session.run(Instant::now() + SLICE) {
            Stop::Paused => {}
            Stop::Exited(code) => break code as u8,
            Stop::Failed(error) => return Err(error),
        }
        if options
            .seconds
            .is_some_and(|seconds| started.elapsed().as_secs_f64() >= seconds)
        {
            break 0;
        }
    };
    Ok(Finished {
        exit_code,
        summary: adv_report::RunSummary::default(),
    })
}

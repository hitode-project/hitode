//! A program on the Wine userland (issue #58), selected per run with
//! `--wine DIST`. Headless, it runs on this thread; with a window, it is the
//! execution worker behind the same UI as the built-in stack. Either way the
//! guest's windows reach the host natively: dialogs, the menu bar, IME text,
//! focus, activation, minimizing and closing, and the control channel.
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adv_runtime::wine::session::{Config, Session, Stop};
use adv_runtime::{DialogEvent, DialogSet};
use adv_sync::WakeEvent;
use adv_sync::background::LocalSender;
use adv_sync::input::KeyState;

use super::{Finished, Options, control, frontend, json, save_frame, window, window_json};

/// How long the guest runs between looks at the host's events.
const SLICE: Duration = Duration::from_millis(20);
/// How long a closed window waits for the guest to exit by itself.
const CLOSE_GRACE: Duration = Duration::from_secs(2);
/// How often a headless run presses one dialog's default button.
const AUTO_CLICKS: u32 = 3;

/// What the session gets from the shell before the process starts.
struct Host {
    background: LocalSender,
    gpu: Option<adv_device::gpu::Gpu>,
    frames: Option<(adv_runtime::FrameProducer, WakeEvent)>,
}

fn start(options: &Options, dist: &Path, wake: WakeEvent, host: Host) -> Result<Session, String> {
    let prefix = match &options.wine_prefix {
        Some(prefix) => prefix.clone(),
        None => options
            .data_dir
            .parent()
            .unwrap_or(Path::new("."))
            .join("wine"),
    };
    let prefix = std::path::absolute(prefix).map_err(|e| e.to_string())?;
    let debug = std::env::var("WINEDEBUG").unwrap_or_default();
    let codepage = match options.codepage {
        Some(codepage) => codepage,
        None => crate::detect_codepage(&options.exe).map_err(|e| e.to_string())?,
    };
    let config = Config {
        dist,
        prefix: &prefix,
        drive: options.wine_drive.as_deref(),
        locale: adv_runtime::wine::session::locale(codepage),
        fonts: adv_runtime::wine::session::HOST_FONTS,
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
        let session = start(options, dist, wake.clone(), host)?;
        let keys = Arc::new(KeyState::default());
        let started = Instant::now();
        let (code, mut session) = drive(options, session, None, &keys, &wake)?;
        session.capture()?;
        let frame = session.frame();
        eprintln!(
            "wine: exit {code} after {:.2}s, {} frames presented",
            started.elapsed().as_secs_f64(),
            frame.frames
        );
        if let Some(path) = &options.screenshot {
            save_frame(path, frame)?;
        }
        Ok(code)
    })();
    service.shutdown();
    result
}

/// The execution worker of a windowed run: the guest presents through the
/// UI's frame mailbox on the UI's GPU.
pub(super) fn run_execution(
    options: &Options,
    dist: &Path,
    mut worker: frontend::Worker,
    wake: WakeEvent,
    keys: Arc<KeyState>,
    background: adv_sync::background::Sender,
) -> Result<Finished, String> {
    let host = Host {
        background: background.into_local(),
        gpu: worker.gpu.take(),
        frames: Some((worker.take_frames(), worker.ui_wake.clone())),
    };
    let session = start(options, dist, wake.clone(), host)?;
    let (exit_code, _) = drive(options, session, Some(worker), &keys, &wake)?;
    Ok(Finished {
        exit_code,
        summary: adv_report::RunSummary::default(),
    })
}

/// Headless answers to dialogs: each one's default button, pressed once its
/// dialog has waited the configured time, at most [`AUTO_CLICKS`] times.
#[derive(Default)]
struct AutoDialog {
    seen: HashMap<u32, (Instant, u32)>,
}

impl AutoDialog {
    fn answer(&mut self, delay: Duration, session: &mut Session, now: Instant) {
        let set = session.dialogs();
        self.seen
            .retain(|hwnd, _| set.dialogs.iter().any(|dialog| dialog.target.hwnd == *hwnd));
        for dialog in set.dialogs.iter().filter(|dialog| dialog.enabled) {
            let (since, clicks) = self.seen.entry(dialog.target.hwnd).or_insert((now, 0));
            if *clicks >= AUTO_CLICKS || now < *since + delay * (*clicks + 1) {
                continue;
            }
            let buttons: Vec<_> = dialog
                .controls
                .iter()
                .filter(|control| {
                    control.target.parent == dialog.target.hwnd
                        && control.class.eq_ignore_ascii_case("Button")
                        && control.style & 0xf <= 1
                        && control.visible
                        && control.enabled
                })
                .collect();
            let pick = buttons
                .iter()
                .find(|button| button.style & 0xf == 1)
                .or_else(|| buttons.iter().find(|button| button.target.id == 1))
                .or_else(|| buttons.first());
            if let Some(button) = pick {
                *clicks += 1;
                log::info!(
                    "headless: pressing [{}] in window \"{}\"",
                    button.text,
                    dialog.text
                );
                session.dialog_event(DialogEvent::Click(button.target));
            }
        }
    }
}

/// Run the guest and answer the host until it exits, the time limit passes
/// or a closed window's grace ends. Returns the exit code.
fn drive(
    options: &Options,
    mut session: Session,
    mut worker: Option<frontend::Worker>,
    keys: &Arc<KeyState>,
    wake: &WakeEvent,
) -> Result<(u8, Session), String> {
    let started = Instant::now();
    let mut audio = worker
        .is_some()
        .then(|| adv_device::Output::new(session.audio_stream()));
    let mut performance = super::performance::Performance::default();
    let mut control = options
        .control_stdin
        .then(|| control::Control::start(wake.clone(), keys.clone()))
        .transpose()?;
    let auto_delay = options
        .auto_dialog_ms
        .map(|ms| Duration::from_millis(u64::from(ms)));
    let mut auto = AutoDialog::default();
    let native = worker.is_some() || control.is_some() || auto_delay.is_some();
    let mut close_at: Option<Instant> = None;
    let mut minimized = false;
    let mut paused = false;
    let code = 'run: loop {
        if worker.as_ref().is_some_and(frontend::Worker::stopped) {
            return Err("UI stopped while the guest was running".into());
        }
        let events = worker
            .as_mut()
            .map(|worker| worker.pump(&mut performance))
            .unwrap_or_default();
        for event in events {
            match event {
                window::Event::Input {
                    message,
                    wparam,
                    lparam,
                } => session.input(message, wparam, lparam)?,
                #[cfg(host_view)]
                window::Event::Text(text, reply) => {
                    let _ = reply.send(session.text(&text));
                }
                window::Event::Close => {
                    paused = false;
                    session.close();
                    close_at.get_or_insert_with(Instant::now);
                }
                #[cfg(host_view)]
                window::Event::ForceClose => break 'run 0,
                #[cfg(host_view)]
                window::Event::AudioSuspended(sleeping) => {
                    if let Some(audio) = audio.as_mut().filter(|_| sleeping) {
                        audio.suspend();
                    }
                }
                #[cfg(host_view)]
                window::Event::Paused(value) => paused = value,
                #[cfg(host_view)]
                window::Event::Volume(percent) => {
                    if let Some(audio) = &audio {
                        audio.set_volume(percent);
                    }
                }
                window::Event::Repaint => {}
                window::Event::FocusLost => {
                    session.release_input();
                }
                window::Event::Menu(event) => {
                    session.menu_event(event);
                }
                window::Event::Dialog(event) => {
                    session.dialog_event(event);
                }
            }
        }
        if native {
            session.refresh_ui();
        }
        if let Some(controller) = control.as_mut() {
            for command in controller.drain() {
                let close = matches!(command, control::Command::Close);
                control::acknowledge(answer(&mut session, command));
                if close {
                    close_at.get_or_insert_with(Instant::now);
                    if let Some(worker) = worker.as_mut() {
                        worker.hide();
                    }
                }
            }
            controller.after_guest_step(true);
        }
        if let Some(delay) = auto_delay {
            auto.answer(delay, &mut session, Instant::now());
        }
        if options
            .seconds
            .is_some_and(|seconds| started.elapsed().as_secs_f64() >= seconds)
            || close_at.is_some_and(|at| at.elapsed() >= CLOSE_GRACE)
        {
            break 0;
        }
        if paused {
            if let Some(audio) = audio.as_mut() {
                audio.suspend();
            }
            if !control.as_ref().is_some_and(control::Control::has_pending) {
                wake.wait(Some(SLICE)).map_err(|e| e.to_string())?;
            }
            continue;
        }
        if let Some(audio) = audio.as_mut() {
            audio.update();
        }
        match session.run(Instant::now() + SLICE) {
            Stop::Paused => {}
            Stop::Exited(code) => break code as u8,
            Stop::Failed(error) => return Err(error),
        }
        if let Some(worker) = worker.as_mut() {
            let bar = session.menu_bar();
            if worker.needs_menu(bar.key) {
                worker.publish_menu(bar);
            }
            worker.publish_dialogs(session.dialogs());
            // A guest minimizing itself (a boss key) minimizes the host
            // window; restoring that window restores the guest.
            let now_minimized = session.minimized();
            if now_minimized && !minimized {
                worker.minimize();
            }
            minimized = now_minimized;
        }
    };
    Ok((code, session))
}

fn position(x: i16, y: i16) -> u32 {
    u32::from(x as u16) | u32::from(y as u16) << 16
}

/// One control command against the session.
fn answer(session: &mut Session, command: control::Command) -> Result<String, String> {
    use control::Command;
    let targeted = |session: &Session| session.input_view().top != 0;
    let post = |session: &mut Session, message, wparam, lparam| {
        targeted(session) && session.input(message, wparam, lparam).is_ok()
    };
    match command {
        Command::Click(x, y) => {
            let at = position(x, y);
            let posted = [(0x200, 0), (0x201, 1), (0x202, 0)]
                .into_iter()
                .filter(|&(message, wparam)| post(session, message, wparam, at))
                .count();
            if posted == 3 {
                Ok(format!("click {x} {y}"))
            } else {
                Err("click has no guest target window".into())
            }
        }
        Command::MouseDown(x, y)
        | Command::MouseUp(x, y)
        | Command::RightMouseDown(x, y)
        | Command::RightMouseUp(x, y) => {
            let right = matches!(
                command,
                Command::RightMouseDown(..) | Command::RightMouseUp(..)
            );
            let pressed = matches!(
                command,
                Command::MouseDown(..) | Command::RightMouseDown(..)
            );
            let buttons = match (pressed, right) {
                (false, _) => 0,
                (true, false) => 1,
                (true, true) => 2,
            };
            let message = match (right, pressed) {
                (false, true) => 0x201,
                (false, false) => 0x202,
                (true, true) => 0x204,
                (true, false) => 0x205,
            };
            let at = position(x, y);
            if post(session, 0x200, buttons, at) && post(session, message, buttons, at) {
                Ok(format!(
                    "{}mouse-{} {x} {y}",
                    if right { "right-" } else { "" },
                    if pressed { "down" } else { "up" }
                ))
            } else {
                Err("mouse button has no guest target window".into())
            }
        }
        Command::Move(x, y) => {
            if post(session, 0x200, 0, position(x, y)) {
                Ok(format!("move {x} {y}"))
            } else {
                Err("mouse move has no guest target window".into())
            }
        }
        Command::Key(key) => {
            if post(session, 0x100, key, 1) && post(session, 0x101, key, 0xc000_0001) {
                Ok(format!("key {key}"))
            } else {
                Err("key has no guest target window".into())
            }
        }
        Command::KeyDown(key) | Command::KeyUp(key) => {
            let pressed = matches!(command, Command::KeyDown(_));
            let (message, lparam) = if pressed {
                (0x100, 1)
            } else {
                (0x101, 0xc000_0001)
            };
            if post(session, message, key, lparam) {
                Ok(format!("key-{} {key}", if pressed { "down" } else { "up" }))
            } else {
                Err("key has no guest target window".into())
            }
        }
        Command::Text(text) => {
            let typed = session.text(&text);
            let total = text.chars().count();
            if typed == total {
                Ok(format!("text {typed}"))
            } else {
                Err(format!("text posted {typed} of {total} characters"))
            }
        }
        Command::Activate(active) => {
            if session.activate(active) {
                Ok(format!("activate {}", u8::from(active)))
            } else {
                Err("activation has no guest target window".into())
            }
        }
        Command::Windows => {
            let view = session.input_view();
            Ok(format!(
                "windows {{\"frames\": {}, \"top\": {}, \"focus\": {}, \"active\": {}, \"capture\": {}, \"windows\": [{}]}}",
                session.presented_frames(),
                view.top,
                view.focus,
                view.active,
                view.capture,
                view.windows
                    .iter()
                    .map(window_json)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
        Command::Hit(x, y) => Ok(format!(
            "hit {}",
            session
                .window_at(x.into(), y.into())
                .map_or_else(|| "null".into(), |window| window_json(&window))
        )),
        Command::Dialogs => Ok(format!(
            "dialogs {}",
            json(&format!("{:?}", session.dialogs()))
        )),
        Command::Dialog(id) | Command::DialogTrack(id, _) => {
            let set = session.dialogs();
            let found: Vec<_> = usable(&set)
                .flat_map(|dialog| dialog.controls.iter())
                .filter(|control| control.target.id == id && control.visible && control.enabled)
                .map(|control| control.target)
                .collect();
            let event = match command {
                Command::DialogTrack(_, value) => found.first().map(|&target| DialogEvent::Track {
                    target,
                    value,
                    code: 4,
                }),
                _ => found.first().map(|&target| DialogEvent::Click(target)),
            };
            match event {
                Some(event) if found.len() == 1 && session.dialog_event(event.clone()) => {
                    Ok(format!("dialog {id}"))
                }
                _ => Err(format!(
                    "dialog control {id} is missing, ambiguous or disabled"
                )),
            }
        }
        Command::DialogClose => {
            let set = session.dialogs();
            let found: Vec<_> = usable(&set).map(|dialog| dialog.target).collect();
            if found.len() == 1 && session.dialog_event(DialogEvent::Close(found[0])) {
                Ok("dialog-close".into())
            } else {
                Err("dialog-close requires one visible enabled dialog".into())
            }
        }
        Command::Menus => {
            fn visit(items: &[adv_runtime::MenuItem], path: &str, rows: &mut Vec<String>) {
                for item in items {
                    let label = format!("{path}/{}", String::from_utf16_lossy(&item.text));
                    if item.children.is_empty() {
                        rows.push(format!("{}:{}:{}", item.command.id, item.flags, label));
                    } else {
                        visit(&item.children, &label, rows);
                    }
                }
            }
            let bar = session.menu_bar();
            let mut rows = Vec::new();
            visit(&bar.items, "", &mut rows);
            Ok(format!(
                "menus enabled={} {}",
                bar.key.enabled,
                json(&rows.join(" | "))
            ))
        }
        Command::Menu(id) => {
            fn find(
                items: &[adv_runtime::MenuItem],
                id: u32,
                found: &mut Vec<adv_runtime::MenuCommand>,
            ) {
                for item in items {
                    if item.children.is_empty() && item.command.id == id {
                        found.push(item.command);
                    }
                    find(&item.children, id, found);
                }
            }
            let mut found = Vec::new();
            find(&session.menu_bar().items, id, &mut found);
            if found.len() == 1 && session.menu_event(adv_runtime::MenuEvent::Command(found[0])) {
                Ok(format!("menu {id}"))
            } else {
                Err(format!(
                    "menu {id} is missing, ambiguous, detached or disabled"
                ))
            }
        }
        Command::Capture(path) => {
            session.capture()?;
            save_frame(&path, session.frame()).map(|_| format!("capture {}", path.display()))
        }
        Command::Close => {
            session.close();
            Ok("close".into())
        }
        Command::Error(error) => Err(error),
        Command::Report(_)
        | Command::Memory(..)
        | Command::ResetMetrics
        | Command::AwaitFrame(_)
        | Command::Resume => Err(format!("{command:?} is not available on the Wine stack")),
    }
}

fn usable(set: &DialogSet) -> impl Iterator<Item = &adv_runtime::DialogWindow> {
    set.dialogs
        .iter()
        .filter(|dialog| dialog.visible && dialog.enabled)
}

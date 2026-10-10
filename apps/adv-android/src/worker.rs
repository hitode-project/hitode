use crate::{Frame, Packet, Session, held_key, packet_current, protocol, publish};
use adv_machine::{RunResult, SliceResult};
use adv_runtime::{
    DialogEvent, Engine, EngineOptions, MenuEvent, MenuItem, RunMode, boot::Runtime,
};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

pub fn frame_info(session: &Session) -> Value {
    let frame = session
        .frame
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    frame.as_ref().map_or_else(
        || json!({"serial":0,"width":0,"height":0}),
        |frame| json!({"serial":frame.serial,"width":frame.width,"height":frame.height}),
    )
}
fn store_frame(session: &Session, frame_serial: &mut u64, frame: Frame) {
    if frame.serial > *frame_serial {
        *frame_serial = frame.serial;
        *session
            .frame
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(Arc::new(frame));
        crate::notify_changed(session);
    }
}
fn find_menu(items: &[MenuItem], id: u32, found: &mut Vec<adv_runtime::MenuCommand>) {
    for item in items {
        if item.children.is_empty()
            && item.command.id == id
            && item.supported
            && item.flags & 3 == 0
        {
            found.push(item.command);
        }
        find_menu(&item.children, id, found);
    }
}
fn dispatch(
    runtime: &mut Runtime,
    session: &Session,
    command: protocol::Command,
    key_observation: Option<u64>,
    buttons: &mut u32,
) -> Result<(), String> {
    let post = |runtime: &mut Runtime, message, wparam, lparam| {
        if runtime.post_input(message, wparam, lparam) {
            Ok(())
        } else {
            Err("input has no guest target window".into())
        }
    };
    match command {
        protocol::Command::Key { key, pressed } => {
            if held_key(key) {
                if key_observation != Some(session.keys.observation(key)) {
                    return Ok(());
                }
            } else {
                session.keys.set(key, pressed);
            }
            let result = post(
                runtime,
                if pressed { 0x100 } else { 0x101 },
                key,
                if pressed { 1 } else { 0xc0000001 },
            );
            if result.is_err() {
                session.keys.set(key, false);
            }
            result
        }
        protocol::Command::Click { x, y, right } => {
            let position = u32::from(x as u16) | (u32::from(y as u16) << 16);
            let mask = if right { 2 } else { 1 };
            post(runtime, 0x200, *buttons, position)?;
            post(
                runtime,
                if right { 0x204 } else { 0x201 },
                *buttons | mask,
                position,
            )?;
            post(
                runtime,
                if right { 0x205 } else { 0x202 },
                *buttons,
                position,
            )
        }
        protocol::Command::Pointer {
            x,
            y,
            action,
            button,
        } => {
            let mask = if button == 1 { 2 } else { 1 };
            let message = match action.as_str() {
                "down" => {
                    *buttons |= mask;
                    session.keys.set(if button == 1 { 2 } else { 1 }, true);
                    if button == 1 { 0x204 } else { 0x201 }
                }
                "up" => {
                    *buttons &= !mask;
                    session.keys.set(if button == 1 { 2 } else { 1 }, false);
                    if button == 1 { 0x205 } else { 0x202 }
                }
                _ => 0x200,
            };
            post(
                runtime,
                message,
                *buttons,
                u32::from(x as u16) | (u32::from(y as u16) << 16),
            )
        }
        protocol::Command::Menu(id) => {
            let menu = runtime.menu_bar()?;
            let mut found = Vec::new();
            find_menu(&menu.items, id, &mut found);
            if menu.key.enabled
                && found.len() == 1
                && runtime.post_menu_event(MenuEvent::Command(found[0]))
            {
                Ok(())
            } else {
                Err("menu is missing, disabled, or ambiguous".into())
            }
        }
        protocol::Command::DialogClose => {
            let set = runtime.dialogs()?;
            let targets: Vec<_> = set
                .dialogs
                .iter()
                .filter(|dialog| dialog.visible && dialog.enabled)
                .map(|dialog| dialog.target)
                .collect();
            if targets.len() == 1 && runtime.post_dialog_event(DialogEvent::Close(targets[0])) {
                Ok(())
            } else {
                Err("dialog-close needs one visible enabled dialog".into())
            }
        }
        command @ (protocol::Command::Dialog { .. }
        | protocol::Command::Track { .. }
        | protocol::Command::Select { .. }
        | protocol::Command::Text { .. }) => {
            let (id, owner) = match &command {
                protocol::Command::Dialog { id, dialog }
                | protocol::Command::Track { id, dialog, .. }
                | protocol::Command::Select { id, dialog, .. }
                | protocol::Command::Text { id, dialog, .. } => (*id, *dialog),
                _ => unreachable!(),
            };
            let set = runtime.dialogs()?;
            let targets: Vec<_> = set
                .dialogs
                .iter()
                .filter(|dialog| {
                    dialog.visible
                        && dialog.enabled
                        && owner.is_none_or(|owner| owner == dialog.target.dialog)
                })
                .flat_map(|dialog| dialog.controls.iter())
                .filter(|control| {
                    control.target.id == id
                        && control.visible
                        && control.enabled
                        && control.supported
                })
                .map(|control| control.target)
                .collect();
            if targets.len() != 1 {
                return Err("dialog control is missing, disabled, or ambiguous".into());
            }
            let target = targets[0];
            let event = match command {
                protocol::Command::Track { value, .. } => DialogEvent::Track {
                    target,
                    value,
                    code: 4,
                },
                protocol::Command::Select { index, .. } => DialogEvent::Select { target, index },
                protocol::Command::Text { text, .. } => DialogEvent::Text { target, text },
                _ => DialogEvent::Click(target),
            };
            if runtime.post_dialog_event(event) {
                Ok(())
            } else {
                Err("guest rejected dialog event".into())
            }
        }
        protocol::Command::Pause | protocol::Command::Resume | protocol::Command::AudioFocus(_) => {
            Err("lifecycle command must use the immediate control channel".into())
        }
    }
}
fn audio_info(output: &adv_device::Output) -> Value {
    let metrics = output.metrics();
    json!({"requested":true,"running":output.is_running(),"device_running":output.is_running(),
        "callbacks":metrics.callbacks.load(Ordering::Relaxed),"starts":metrics.starts.load(Ordering::Relaxed),
        "stops":metrics.stops.load(Ordering::Relaxed),"non_silent_samples":metrics.non_silent_samples.load(Ordering::Relaxed),
        "device_errors":metrics.device_errors.load(Ordering::Relaxed),
        "device_sample_rate":metrics.device_sample_rate.load(Ordering::Relaxed),
        "device_channels":metrics.device_channels.load(Ordering::Relaxed),
        "output_frames":metrics.output_frames.load(Ordering::Relaxed),
        "native_clock_callbacks":metrics.native_clock_callbacks.load(Ordering::Relaxed),
        "device_consumed_frames":metrics.device_consumed_frames.load(Ordering::Relaxed),
        "device_queued_frames":metrics.device_queued_frames.load(Ordering::Relaxed)})
}
fn snapshot(
    runtime: &Runtime,
    session: &Session,
    output: &adv_device::Output,
    scheduling: &Value,
    frame_output: (u64, u64),
    status: &str,
    paused: bool,
    command_error: &Option<String>,
    commands: u64,
    pauses: u64,
    resumes: u64,
    failure: &Option<String>,
    exit_code: Option<u32>,
) -> Result<Value, String> {
    let bar = runtime.menu_bar()?;
    let pcm = runtime.audio_pcm_metrics();
    let ctrl_pressed = [0x11, 0xa2, 0xa3]
        .iter()
        .any(|key| session.keys.get(*key) & 0x80 != 0);
    Ok(
        json!({"session":session.id,"status":status,"error":failure,"failure":failure,
            "paused":paused,"guest_clock_us":runtime.machine.sys.clock.now_us(),
            "frame":frame_info(session),"frames":runtime.presented_frames(),
            "menu_enabled":bar.key.enabled,"menus":protocol::menus(&bar.items),"dialogs":protocol::dialogs(&runtime.dialogs()?),
            "audio":audio_info(output),"command_error":command_error,"commands":commands,"pauses":pauses,"resumes":resumes,
            "input_receipt":runtime.latest_input_receipt(),"guest_exit_code":exit_code,
            "input_epoch":session.input_epoch.load(Ordering::Acquire),
            "ctrl_pressed":ctrl_pressed,
            "api":{"mode":"strict","stub_calls":runtime.api_diagnostics().map_or(0, |diagnostics| diagnostics.stub_calls),
                "acceptance_eligible":runtime.api_acceptance_eligible(),
                "signature_conflicts":runtime.api_signature_conflicts().len()},
            "pcm":{"playback_starts":pcm.playback_starts,"missing_stream_frames":pcm.missing_stream_frames,
                "missing_output_frames":pcm.missing_output_frames},
            "build":option_env!("ADV_ANDROID_SHELL_BUILD").unwrap_or("unrecorded"),
            "guest_workers":1,"backend":"jit","gpu_enabled":cfg!(feature="gpu"),"scheduling":scheduling,
            "frame_output":{"published":frame_output.0,"duplicates":frame_output.1},
            "graphics":if cfg!(feature="gpu") {"existing GPU with Bitmap readback"} else {"software GDI/DirectDraw"}
        }),
    )
}
fn save_report(data: &Path, report: &Value) -> Result<(), String> {
    use std::io::Write;
    let temporary = data.join("android-session.json.tmp");
    let mut file = std::fs::File::create(&temporary).map_err(|error| error.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(report).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    drop(file);
    std::fs::rename(temporary, data.join("android-session.json")).map_err(|error| error.to_string())
}

fn startup(session: &Session, data: &Path, stage: &'static str) -> Result<(), String> {
    let report = json!({"session":session.id,"status":"starting","startup_stage":stage,"error":null,
        "frame":frame_info(session),"menus":[],"dialogs":[],
        "build":option_env!("ADV_ANDROID_SHELL_BUILD").unwrap_or("unrecorded")});
    publish(session, report.clone());
    save_report(data, &report)?;
    if session.stop.load(Ordering::Acquire) {
        return Err("runtime startup cancelled".into());
    }
    Ok(())
}
pub fn startup_error(session: &Session, data: &Path, error: &str) {
    let previous = session
        .poll
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let mut report: Value = serde_json::from_str(previous.as_str()).unwrap_or_else(|_| json!({}));
    let cancelled = error == "runtime startup cancelled";
    report["session"] = json!(session.id);
    report["status"] = json!(if cancelled { "stopped" } else { "error" });
    report["error"] = if cancelled { Value::Null } else { json!(error) };
    report["failure"] = report["error"].clone();
    report["frame"] = frame_info(session);
    if let Err(write_error) = std::fs::create_dir_all(data)
        .map_err(|error| error.to_string())
        .and_then(|_| save_report(data, &report))
    {
        report["report_error"] = json!(write_error);
    }
    publish(session, report);
}

pub fn run(
    session: &Session,
    receiver: mpsc::Receiver<Packet>,
    exe: &Path,
    data: &Path,
    codepage: u32,
) -> Result<(), String> {
    std::fs::create_dir_all(data).map_err(|error| error.to_string())?;
    startup(session, data, "background-create")?;
    let mut builder = adv_sync::background::Builder::new(session.wake.clone())
        .map_err(|error| error.to_string())?
        .with_work_policy(adv_sync::background::WorkPolicy::responsive(256));
    let background = builder.sender(8, 16).map_err(|error| error.to_string())?;
    startup(session, data, "background-start")?;
    let mut service = builder.start().map_err(|error| error.to_string())?;
    startup(session, data, "runtime-load")?;
    let mut runtime = Runtime::with_codec(
        exe,
        data,
        true,
        640,
        480,
        adv_runtime::vorbis::Policy::Native,
    )?;
    startup(session, data, "runtime-config")?;
    runtime.set_codepage(codepage)?;
    runtime.set_api_mode(RunMode::Strict);
    runtime.machine.cpu.set_waker(session.wake.clone())?;
    runtime
        .machine
        .cpu
        .set_background_sender(background.into_local())?;
    // The application keeps no persistent native code; the warm profile
    // lives in the data directory.
    runtime.machine.cpu.configure(&EngineOptions {
        compile_threads: Some(2),
        artifacts: Some(data.to_path_buf()),
        code_cache: false,
        ..EngineOptions::default()
    })?;
    startup(session, data, "codec-kernels")?;
    runtime.install_codec_kernels(
        adv_runtime::codec_kernels::Mode::Native,
        adv_runtime::codec_kernels::BUNDLED_SIGNATURES,
    )?;
    #[cfg(feature = "gpu")]
    {
        startup(session, data, "gpu-initialize")?;
        let gpu = pollster::block_on(adv_device::gpu::Gpu::request(false))
            .map_err(|error| error.to_string())?;
        gpu.context()
            .set_execution_wake(session.wake.clone())
            .map_err(|_| "GPU waker was already registered")?;
        runtime.set_deferred_presentation(true)?;
        runtime.install_gpu(gpu)?;
        let gpu_background = runtime.machine.cpu.background_sender()?;
        runtime.set_gpu_background(gpu_background, session.wake.clone());
    }
    runtime.set_host_keys(session.keys.clone());
    runtime.enable_input_receipts();
    runtime.set_audio_enabled(true);
    runtime
        .audio_stream()
        .set_waker(session.wake.clone())
        .map_err(|_| "audio waker was already registered")?;
    let skip_keys = session.keys.clone();
    let interrupt = session.wake.interrupt_flag();
    runtime.machine.set_time_skip_source(Some(Arc::new(move || {
        !interrupt.load(Ordering::Acquire)
            && [0x11, 0xa2, 0xa3]
                .iter()
                .any(|key| skip_keys.get(*key) & 0x80 != 0)
    })));
    startup(session, data, "audio-output")?;
    let mut output = adv_device::Output::new(runtime.audio_stream());
    startup(session, data, "first-step")?;
    let mut paused = false;
    let mut pauses = 0;
    let mut resumes = 0;
    let mut commands = 0;
    let mut buttons = 0;
    let mut command_error = None;
    let mut failure = None;
    let mut exit_code = None;
    let mut closing = None;
    let mut last_publish = Instant::now();
    let mut frame_serial = 0;
    let mut input_epoch = session.input_epoch.load(Ordering::Acquire);
    let mut input_fence: Option<(u64, Instant)> = None;
    #[cfg(feature = "gpu")]
    let mut capture = crate::gpu::Capture::new();
    #[cfg(feature = "gpu")]
    let mut frame_output = (0, 0);
    #[cfg(not(feature = "gpu"))]
    let frame_output = (0, 0);
    let scheduling = crate::scheduling::configure_execution_thread();
    let status;
    loop {
        session.wake.take_interrupt();
        if session.stop.load(Ordering::Acquire) && closing.is_none() {
            #[cfg(feature = "gpu")]
            capture.cancel();
            session.keys.release_all();
            runtime.release_input();
            output.suspend();
            if !runtime.set_suspended(false) {
                return Err("cannot resume guest for close".into());
            }
            paused = false;
            runtime.post_input(0x10, 0, 0);
            closing = Some(Instant::now());
        }
        let requested_pause = closing.is_none()
            && (session.lifecycle_paused.load(Ordering::Acquire)
                || !session.audio_focus.load(Ordering::Acquire));
        if requested_pause != paused {
            if requested_pause {
                #[cfg(feature = "gpu")]
                capture.cancel();
                runtime.release_input();
                buttons = 0;
                output.suspend();
                pauses += 1;
            } else {
                resumes += 1;
            }
            if !runtime.set_suspended(requested_pause) {
                return Err("cannot publish audio lifecycle pause".into());
            }
            paused = requested_pause;
            last_publish = Instant::now()
                .checked_sub(Duration::from_secs(1))
                .unwrap_or_else(Instant::now);
            let report = snapshot(
                &runtime,
                session,
                &output,
                &scheduling,
                frame_output,
                if paused { "paused" } else { "running" },
                paused,
                &command_error,
                commands,
                pauses,
                resumes,
                &failure,
                exit_code,
            )?;
            save_report(data, &report)?;
            publish(session, report);
        }
        let current_epoch = session.input_epoch.load(Ordering::Acquire);
        if session.release_input.swap(false, Ordering::AcqRel) || current_epoch != input_epoch {
            runtime.release_input();
            buttons = 0;
            input_epoch = current_epoch;
            input_fence = None;
        }
        if input_fence.is_some_and(|(_, at)| at.elapsed() >= Duration::from_secs(1)) {
            command_error = Some("input observation fence timed out".into());
            input_fence = None;
        }
        if input_fence.is_none() {
            for _ in 0..128 {
                let Ok(packet) = receiver.try_recv() else {
                    break;
                };
                let _input = session
                    .input_lock
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if !packet_current(session, &packet) {
                    continue;
                }
                commands += 1;
                if paused || closing.is_some() {
                    command_error = Some("input rejected while paused or stopping".into());
                    continue;
                }
                let before = runtime.latest_input_receipt();
                command_error = dispatch(
                    &mut runtime,
                    session,
                    packet.command,
                    packet.key_observation,
                    &mut buttons,
                )
                .err();
                if command_error.is_none() && runtime.latest_input_receipt() != before {
                    input_fence = Some((runtime.latest_input_receipt(), Instant::now()));
                }
                break;
            }
        }
        if closing.is_some_and(|at: Instant| at.elapsed() >= Duration::from_secs(2)) {
            status = "stopped";
            break;
        }
        let interrupts_before = runtime.machine.sys.scheduling.external_interrupts;
        let frame_yields_before = runtime.machine.sys.scheduling.frame_host_yields;
        let outcome = if paused {
            None
        } else {
            if let Some(at) = closing {
                runtime.machine.set_execution_budget(Some(
                    Duration::from_secs(2).saturating_sub(at.elapsed()),
                ));
            }
            Some(runtime.step_for(Duration::from_millis(8)))
        };
        if let Some((receipt, _)) = input_fence {
            let ordinary = runtime.machine.sys.scheduling.external_interrupts == interrupts_before;
            let full_or_idle = runtime.machine.sys.scheduling.frame_host_yields
                == frame_yields_before
                || matches!(
                    &outcome,
                    Some(SliceResult::Complete(RunResult::Idle { .. }))
                );
            if !paused && ordinary && runtime.input_receipt_completed(receipt, full_or_idle) {
                input_fence = None;
            }
        }
        if !paused && closing.is_none() {
            output.update();
        }
        #[cfg(feature = "gpu")]
        let latest = capture.update(&mut runtime, session)?;
        #[cfg(feature = "gpu")]
        {
            frame_output = capture.metrics();
        }
        #[cfg(not(feature = "gpu"))]
        let latest = {
            if !paused {
                runtime.capture_latest_frame()?;
            }
            let frame = runtime.frame();
            (frame.frames > frame_serial).then(|| Frame {
                serial: frame.frames,
                width: frame.width,
                height: frame.height,
                bgra: frame.bgra.clone(),
            })
        };
        if let Some(latest) = latest {
            store_frame(session, &mut frame_serial, latest);
        }
        match &outcome {
            Some(SliceResult::Complete(RunResult::Exited(code))) => {
                exit_code = Some(*code);
                if *code != 0 {
                    failure = Some(format!("guest exited with code {code}"));
                }
                status = if failure.is_some() {
                    "error"
                } else if closing.is_some() {
                    "stopped"
                } else {
                    "exited"
                };
                break;
            }
            Some(SliceResult::Complete(RunResult::Failed(error))) => {
                if closing.is_some() && error.message.contains("execution budget exhausted") {
                    status = "stopped";
                } else {
                    failure = Some(format!("{error}\n{}", error.recent.join("\n")));
                    status = "error";
                }
                break;
            }
            _ => {}
        }
        if last_publish.elapsed() >= Duration::from_millis(100) {
            publish(
                session,
                snapshot(
                    &runtime,
                    session,
                    &output,
                    &scheduling,
                    frame_output,
                    if paused {
                        "paused"
                    } else if closing.is_some() {
                        "stopping"
                    } else {
                        "running"
                    },
                    paused,
                    &command_error,
                    commands,
                    pauses,
                    resumes,
                    &failure,
                    exit_code,
                )?,
            );
            last_publish = Instant::now();
        }
        if paused || matches!(outcome, Some(SliceResult::Complete(RunResult::Idle { .. }))) {
            let now = runtime.machine.sys.clock.now_us();
            let delay = match outcome {
                Some(SliceResult::Complete(RunResult::Idle {
                    next_deadline: Some(deadline),
                })) => Duration::from_micros(deadline.saturating_sub(now))
                    .min(Duration::from_millis(50)),
                _ => Duration::from_millis(50),
            };
            let delay = output.next_wake().map_or(delay, |wake| {
                delay.min(wake.saturating_duration_since(Instant::now()))
            });
            #[cfg(feature = "gpu")]
            let delay = if !paused && closing.is_none() {
                capture
                    .next_wake(runtime.has_pending_frame())
                    .map_or(delay, |wake| {
                        delay.min(wake.saturating_duration_since(Instant::now()))
                    })
            } else {
                delay
            };
            session
                .wake
                .wait(Some(delay))
                .map_err(|error| error.to_string())?;
        }
    }
    #[cfg(feature = "gpu")]
    {
        // The guest is finished. Preserve the final report's complete snapshot
        // through the same asynchronous transfer; lifecycle/stop cancels it.
        let deadline = Instant::now() + Duration::from_secs(2);
        while runtime.has_pending_frame()
            && !session.stop.load(Ordering::Acquire)
            && !session.lifecycle_paused.load(Ordering::Acquire)
            && session.audio_focus.load(Ordering::Acquire)
        {
            if let Some(latest) = capture.update(&mut runtime, session)? {
                store_frame(session, &mut frame_serial, latest);
            }
            frame_output = capture.metrics();
            if !runtime.has_pending_frame() {
                break;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                capture.cancel();
                return Err("final GPU capture timed out".into());
            }
            let delay = capture
                .next_wake(runtime.has_pending_frame())
                .map_or(remaining, |wake| {
                    wake.saturating_duration_since(Instant::now())
                        .min(remaining)
                });
            session
                .wake
                .wait(Some(delay))
                .map_err(|error| error.to_string())?;
        }
        capture.cancel();
    }
    session.keys.release_all();
    runtime.release_input();
    output.suspend();
    let mut report = snapshot(
        &runtime,
        session,
        &output,
        &scheduling,
        frame_output,
        status,
        paused,
        &command_error,
        commands,
        pauses,
        resumes,
        &failure,
        exit_code,
    )?;
    report["codepage"] = json!(codepage);
    report["exe"] = json!(exe);
    report["data_dir"] = json!(data);
    report["runtime"] = json!(runtime.report());
    let hash = runtime
        .frame()
        .bgra
        .iter()
        .fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    report["frame_fnv1a64"] = json!(format!("{hash:016x}"));
    save_report(data, &report)?;
    runtime.machine.cpu.persist()?;
    drop(output);
    drop(runtime);
    service.shutdown();
    publish(session, report);
    Ok(())
}

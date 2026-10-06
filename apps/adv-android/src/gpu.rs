//! Reuse the device renderer and its reviewed readback ownership for Bitmap output.
use crate::Session;
use adv_runtime::boot::Runtime;
use adv_sync::background::{Priority, Task};
use std::sync::atomic::Ordering;
use std::sync::mpsc::TrySendError;
use std::time::{Duration, Instant};

fn interrupted(session: &Session) -> bool {
    session.stop.load(Ordering::Acquire)
        || session.lifecycle_paused.load(Ordering::Acquire)
        || !session.audio_focus.load(Ordering::Acquire)
}

pub fn capture(runtime: &mut Runtime, session: &Session) -> Result<(), String> {
    if !runtime.has_native_frame() {
        runtime.capture_latest_frame()?;
        return Ok(());
    }
    if interrupted(session) {
        return Ok(());
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    let (serial, mut ticket) = match runtime.begin_native_capture() {
        Ok(Some(capture)) => capture,
        Ok(None) | Err(adv_device::gpu::Error::Busy) => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    let sender = runtime.machine.cpu.background_sender()?;
    let job = ticket
        .take_completion_job()
        .ok_or("native readback job was already submitted")?;
    let mut task = Task::new(job)
        .finish_on_shutdown()
        .with_wake(session.wake.clone());
    loop {
        match sender.borrow_mut().try_submit(Priority::Urgent, task) {
            Ok(_) => break,
            Err(TrySendError::Full(returned)) => task = returned,
            Err(TrySendError::Disconnected(_)) => {
                ticket.cancel();
                return Err("GPU background service stopped".into());
            }
        }
        if interrupted(session) {
            ticket.cancel();
            return Ok(());
        }
        if Instant::now() >= deadline {
            ticket.cancel();
            return Err("GPU readback queue timed out".into());
        }
        session
            .wake
            .wait(Some(Duration::from_millis(1)))
            .map_err(|error| error.to_string())?;
    }
    loop {
        if let Some(result) = ticket.poll() {
            let pixels = result.map_err(|error| error.to_string())?;
            if !runtime.finish_native_capture(serial, ticket.width, ticket.height, &pixels)? {
                return Err("native frame changed during Bitmap readback".into());
            }
            return Ok(());
        }
        if interrupted(session) {
            ticket.cancel();
            return Ok(());
        }
        if Instant::now() >= deadline {
            ticket.cancel();
            return Err("GPU readback fence timed out".into());
        }
        session
            .wake
            .wait(Some(Duration::from_millis(1)))
            .map_err(|error| error.to_string())?;
    }
}

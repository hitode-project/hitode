//! Bounded Bitmap readback without waiting on the guest execution worker.
use crate::{Frame, Session};
use adv_runtime::boot::{NativeCapture, Runtime};
use adv_sync::background::{Priority, Task};
use std::sync::atomic::Ordering;
use std::sync::mpsc::TrySendError;
use std::time::{Duration, Instant};

const FRAME_INTERVAL: Duration = Duration::from_nanos(16_666_667);
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(2);

fn interrupted(session: &Session) -> bool {
    session.stop.load(Ordering::Acquire)
        || session.lifecycle_paused.load(Ordering::Acquire)
        || !session.audio_focus.load(Ordering::Acquire)
}

struct Flight {
    capture: NativeCapture,
    task: Option<Task>,
    deadline: Instant,
    pixels: Option<Vec<u8>>,
}

pub struct Capture {
    flight: Option<Flight>,
    next_request: Instant,
    next_output: Instant,
}
impl Capture {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            flight: None,
            next_request: now,
            next_output: now,
        }
    }

    pub fn cancel(&mut self) {
        if let Some(mut flight) = self.flight.take() {
            flight.capture.cancel();
        }
    }

    /// Fold this into the ordinary idle wait. Map completion also signals the
    /// worker's wake event, so a submitted transfer needs no periodic fence poll.
    pub fn next_wake(&self) -> Instant {
        match &self.flight {
            Some(flight) if flight.pixels.is_some() => self.next_output,
            Some(flight) if flight.task.is_some() => {
                (Instant::now() + Duration::from_millis(1)).min(flight.deadline)
            }
            Some(flight) => flight.deadline,
            None => self.next_request,
        }
    }

    fn submit(&mut self, runtime: &mut Runtime) -> Result<(), String> {
        let Some(flight) = self.flight.as_mut() else {
            return Ok(());
        };
        let Some(task) = flight.task.take() else {
            return Ok(());
        };
        let sender = runtime.machine.cpu.background_sender()?;
        match sender.borrow_mut().try_submit(Priority::Urgent, task) {
            Ok(_) => Ok(()),
            Err(TrySendError::Full(task)) => {
                flight.task = Some(task);
                Ok(())
            }
            Err(TrySendError::Disconnected(_)) => {
                self.cancel();
                Err("GPU background service stopped".into())
            }
        }
    }

    /// Try once to consume or submit. A full queue retains its one task; newer
    /// guest frames are coalesced until this transfer finishes.
    pub fn update(
        &mut self,
        runtime: &mut Runtime,
        session: &Session,
    ) -> Result<Option<Frame>, String> {
        if interrupted(session) {
            self.cancel();
            return Ok(None);
        }
        let now = Instant::now();
        if let Some(flight) = self.flight.as_mut() {
            if flight.pixels.is_none() {
                if let Some(result) = flight.capture.poll() {
                    flight.pixels = Some(result.map_err(|error| error.to_string())?);
                } else if now >= flight.deadline {
                    self.cancel();
                    return Err("GPU readback completion timed out".into());
                }
            }
        }
        let mut ready = None;
        if now >= self.next_output
            && self
                .flight
                .as_ref()
                .is_some_and(|flight| flight.pixels.is_some())
        {
            let mut flight = self.flight.take().unwrap();
            let pixels = flight.pixels.take().unwrap();
            if runtime.finish_native_frame_capture(&flight.capture, &pixels)? {
                ready = Some(Frame {
                    serial: flight.capture.serial(),
                    width: flight.capture.width(),
                    height: flight.capture.height(),
                    bgra: pixels,
                });
                self.next_output = now + FRAME_INTERVAL;
            }
        }
        if self.flight.is_some() {
            self.submit(runtime)?;
            return Ok(ready);
        }
        if now < self.next_request {
            return Ok(ready);
        }
        self.next_request = now + FRAME_INTERVAL;
        if let Some(serial) = runtime.native_frame_serial() {
            if serial <= runtime.frame().frames {
                return Ok(ready);
            }
            let mut capture = match runtime.begin_native_frame_capture() {
                Ok(Some(capture)) => capture,
                Ok(None) | Err(adv_device::gpu::Error::Busy) => return Ok(ready),
                Err(error) => return Err(error.to_string()),
            };
            let job = capture
                .take_completion_job()
                .ok_or("native readback job was already submitted")?;
            self.flight = Some(Flight {
                capture,
                task: Some(
                    Task::new(job)
                        .finish_on_shutdown()
                        .with_wake(session.wake.clone()),
                ),
                deadline: now + CAPTURE_TIMEOUT,
                pixels: None,
            });
            self.submit(runtime)?;
        } else if ready.is_none() && now >= self.next_output && runtime.capture_latest_frame()? {
            let frame = runtime.frame();
            ready = Some(Frame {
                serial: frame.frames,
                width: frame.width,
                height: frame.height,
                bgra: frame.bgra.clone(),
            });
            self.next_output = now + FRAME_INTERVAL;
        }
        Ok(ready)
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.cancel();
    }
}

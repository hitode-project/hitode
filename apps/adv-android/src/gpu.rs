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

fn software_frame(
    snapshot: &adv_device::surface::Snapshot,
    previous: Option<&Frame>,
) -> Option<Frame> {
    if previous.is_some_and(|previous| {
        previous.width == snapshot.width
            && previous.height == snapshot.height
            && previous.bgra == snapshot.bgra
    }) {
        return None;
    }
    Some(Frame {
        serial: snapshot.frames,
        width: snapshot.width,
        height: snapshot.height,
        bgra: snapshot.bgra.clone(),
    })
}

pub struct Capture {
    flight: Option<Flight>,
    next_request: Instant,
    next_output: Instant,
    published_frames: u64,
    duplicate_frames: u64,
}
impl Capture {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            flight: None,
            next_request: now,
            next_output: now,
            published_frames: 0,
            duplicate_frames: 0,
        }
    }

    pub fn cancel(&mut self) {
        if let Some(mut flight) = self.flight.take() {
            flight.capture.cancel();
        }
    }

    /// Fold this into the ordinary idle wait. Map completion also signals the
    /// worker's wake event, so a submitted transfer needs no periodic fence poll.
    pub fn next_wake(&self, pending_frame: bool) -> Option<Instant> {
        match &self.flight {
            Some(flight) if flight.pixels.is_some() => Some(self.next_output),
            Some(flight) if flight.task.is_some() => {
                Some((Instant::now() + Duration::from_millis(1)).min(flight.deadline))
            }
            Some(flight) => Some(flight.deadline),
            None if pending_frame => Some(self.next_request),
            None => None,
        }
    }

    pub fn metrics(&self) -> (u64, u64) {
        (self.published_frames, self.duplicate_frames)
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
                self.published_frames = self.published_frames.saturating_add(1);
            }
        }
        if self.flight.is_some() {
            self.submit(runtime)?;
            return Ok(ready);
        }
        if !runtime.has_pending_frame() {
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
            // Source capture accounting still advances for every presentation.
            // Keep the last distinct snapshot's serial as the Bitmap identity.
            let previous = session
                .frame
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            ready = software_frame(runtime.frame(), previous.as_deref());
            if ready.is_some() {
                self.published_frames = self.published_frames.saturating_add(1);
            } else {
                self.duplicate_frames = self.duplicate_frames.saturating_add(1);
            }
            self.next_output = now + FRAME_INTERVAL;
        }
        Ok(ready)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_device::surface::Snapshot;

    // spec: HOST-AND-5
    #[test]
    fn identical_software_pixels_do_not_publish_a_new_bitmap() {
        let previous = Frame {
            serial: 1,
            width: 2,
            height: 1,
            bgra: vec![0, 0, 255, 255, 0, 255, 0, 255],
        };
        let mut snapshot = Snapshot {
            frames: 99,
            width: 2,
            height: 1,
            bgra: previous.bgra.clone(),
        };
        assert!(software_frame(&snapshot, Some(&previous)).is_none());
        assert_eq!(snapshot.frames, 99, "source accounting remains current");

        snapshot.bgra[4] = 1;
        let changed = software_frame(&snapshot, Some(&previous)).unwrap();
        assert_eq!(changed.serial, 99);
        assert_eq!(changed.bgra, snapshot.bgra);
    }

    // spec: HOST-AND-5
    #[test]
    fn software_dimensions_and_first_frame_are_always_published() {
        let previous = Frame {
            serial: 1,
            width: 2,
            height: 1,
            bgra: vec![0; 8],
        };
        let snapshot = Snapshot {
            frames: 2,
            width: 1,
            height: 2,
            bgra: previous.bgra.clone(),
        };
        let changed = software_frame(&snapshot, Some(&previous)).unwrap();
        assert_eq!((changed.width, changed.height, changed.serial), (1, 2, 2));
        assert!(software_frame(&snapshot, None).is_some());
    }

    // spec: HOST-AND-5
    #[test]
    fn no_frame_has_no_capture_timer_even_after_previous_deadline_expires() {
        let mut capture = Capture::new();
        capture.next_request = Instant::now() - FRAME_INTERVAL;
        assert!(capture.next_wake(false).is_none());
        assert_eq!(capture.next_wake(true), Some(capture.next_request));
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.cancel();
    }
}

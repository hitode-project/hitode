//! Throttled macOS readout of the diagnostic layer's area counts.
use adv_gpu::{
    Buffer, Device, Queue, Token,
    resource::{BufferDesc, BufferUsage, Memory},
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Default)]
pub struct DamageHud(Arc<Mutex<State>>);

type Observer = Box<dyn Fn(Option<[Option<f64>; 2]>) + Send + Sync>;

#[derive(Default)]
struct State {
    enabled: bool,
    generation: u64,
    sampled: Option<Instant>,
    observer: Option<Observer>,
}

impl DamageHud {
    pub fn observe(&self, observer: impl Fn(Option<[Option<f64>; 2]>) + Send + Sync + 'static) {
        let mut state = self.0.lock().unwrap();
        if state.enabled {
            observer(Some([None; 2]));
        }
        state.observer = Some(Box::new(observer));
    }

    pub fn set_enabled(&self, enabled: bool) {
        let mut state = self.0.lock().unwrap();
        if state.enabled == enabled {
            return;
        }
        state.enabled = enabled;
        state.generation += 1;
        state.sampled = None;
        if let Some(observer) = &state.observer {
            observer(enabled.then_some([None; 2]));
        }
    }

    pub(super) fn sample(&self, device: &Device, upscale: bool) -> adv_gpu::Result<Option<Sample>> {
        let mut state = self.0.lock().unwrap();
        let now = Instant::now();
        if !state.enabled
            || state.observer.is_none()
            || state
                .sampled
                .is_some_and(|at| now.duration_since(at) < Duration::from_millis(250))
        {
            return Ok(None);
        }
        state.sampled = Some(now);
        let generation = state.generation;
        drop(state);
        let output = upscale
            .then(|| {
                device.create_buffer(&BufferDesc {
                    size: 4,
                    usage: BufferUsage::COPY_DST,
                    memory: Memory::Readback,
                    label: "damage HUD output area",
                })
            })
            .transpose()?;
        Ok(Some(Sample {
            hud: self.clone(),
            generation,
            producer: 0,
            output,
        }))
    }
}

pub(super) struct Sample {
    hud: DamageHud,
    generation: u64,
    pub(super) producer: u64,
    pub(super) output: Option<Buffer>,
}

impl Sample {
    pub(super) fn finish(self, queue: &mut Queue, token: Token, canvas: (u32, u32)) {
        let area = f64::from(canvas.0) * f64::from(canvas.1);
        queue.on_complete(
            token,
            Box::new(move || {
                let output = self.output.map(|buffer| {
                    // SAFETY: The copy into this private readback has completed.
                    let bytes = unsafe { buffer.mapped().unwrap().as_ref() };
                    f64::from(u32::from_ne_bytes(bytes[..4].try_into().unwrap())) / area
                });
                let state = self.hud.0.lock().unwrap();
                if state.generation == self.generation
                    && let Some(observer) = &state.observer
                {
                    observer(Some([Some(self.producer as f64 / area), output]));
                }
            }),
        );
    }
}

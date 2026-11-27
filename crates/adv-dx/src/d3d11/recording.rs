//! Direct submission of recorded modern commands.
use crate::record;
pub use crate::record::ResourceKey;
use adv_device::{gpu::Image, surface::Region};
use adv_device::{
    gpu::{
        Dispatch, DispatchArguments, Error, Expand, Gpu, ModernDraw, ModernTransfer, QuerySlot,
        SlotBindings,
    },
    surface::Rect,
};
use adv_gpu::ImageView;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

pub enum Command {
    Draw {
        draw: Box<ModernDraw>,
        vertex: SlotBindings,
        fragment: SlotBindings,
        statistics: Option<QuerySlot>,
    },
    Expand(Box<Expand>),
    Dispatch {
        pipeline: Arc<Dispatch>,
        bindings: SlotBindings,
        arguments: DispatchArguments,
    },
    Resolve {
        source: ImageView,
        destination: ImageView,
    },
    GenerateMips(ImageView),
    Transfer(ModernTransfer),
}

#[derive(Clone, Copy)]
pub enum Boundary {
    Map,
    Query,
    Flush,
}

struct Front;

impl record::Front for Front {
    type Payload = Command;
    type Sink<'a> = Gpu;
    type Completion = ();
    type Error = Error;
    type Boundary = Boundary;

    fn encode(gpu: &mut Gpu, command: &Command) -> Result<(), Error> {
        match command {
            Command::Draw {
                draw,
                vertex,
                fragment,
                statistics,
            } => match statistics {
                Some(statistics) => gpu.draw_modern_statistics(
                    (**draw).clone(),
                    vertex,
                    fragment,
                    statistics.clone(),
                ),
                None => gpu.draw_modern((**draw).clone(), vertex, fragment),
            },
            Command::Expand(expansion) => gpu.expand((**expansion).clone()),
            Command::Dispatch {
                pipeline,
                bindings,
                arguments,
            } => gpu.dispatch(pipeline, bindings, arguments.clone()),
            Command::Resolve {
                source,
                destination,
            } => gpu.resolve(source.clone(), destination.clone()),
            Command::GenerateMips(view) => gpu.generate_mips(view.clone()),
            Command::Transfer(transfer) => gpu.transfer(transfer.clone()),
        }
    }

    fn reason(boundary: Boundary) -> record::Reason {
        match boundary {
            Boundary::Map => record::Reason::Map,
            Boundary::Query => record::Reason::Query,
            Boundary::Flush => record::Reason::Barrier,
        }
    }
}

/// Capture has already resolved the resource identities and byte ranges
/// needed by the shaders when it submits these effects.
pub struct Effects<'a> {
    pub extent: Rect,
    pub bounds: Rect,
    pub key: u128,
    pub reads: &'a [ResourceKey],
    pub writes: &'a [(ResourceKey, bool)],
    pub retained_bytes: usize,
}

#[derive(Default)]
pub struct Recording {
    inner: record::Recording<Command>,
    damage: Option<(ResourceKey, Region)>,
    verification: Option<Arc<Verification>>,
    verification_serial: u64,
    #[cfg(feature = "d3d9-counters")]
    pub(super) counters: record::Counters,
}

#[derive(Default)]
struct Verification {
    progress: Mutex<VerificationProgress>,
    #[cfg(feature = "d3d9-counters")]
    mismatches: std::sync::atomic::AtomicU64,
}

type Pixels = (u32, u32, Vec<u8>);

#[derive(Default)]
struct VerificationProgress {
    next: u64,
    previous: Option<Pixels>,
    completed: BTreeMap<u64, (Region, Result<Pixels, Error>)>,
}

impl Recording {
    #[cfg(feature = "d3d9-counters")]
    pub fn count_indirect_readback(&mut self, bytes: u64) {
        self.counters.readback_bytes += bytes;
        self.counters.flushes[1] += 1;
        log::debug!(
            "D3D11 indirect argument readbacks: {}; readback bytes: {}",
            self.counters.flushes[1],
            self.counters.readback_bytes
        );
    }

    pub fn append(
        &mut self,
        gpu: &mut Gpu,
        command: Command,
        effects: Effects<'_>,
    ) -> Result<(), Error> {
        let op = |payload| {
            let kind = match &payload {
                Command::Draw { .. } | Command::Expand(_) => record::Kind::Draw,
                Command::Resolve { .. } => record::Kind::Copy,
                Command::Dispatch { .. } | Command::GenerateMips(_) | Command::Transfer(_) => {
                    record::Kind::Upload
                }
            };
            record::Op {
                payload,
                effects: record::damage::Input {
                    kind,
                    view: effects.extent,
                    attachment: None,
                    color_shape: 0,
                    slot: 0,
                    bounds: effects.bounds,
                    key: effects.key,
                    reads: effects.reads,
                    samples: &[],
                    writes: effects.writes,
                    ignores_sampled_color: false,
                    clear_keys: [None; 3],
                },
                bytes: effects.retained_bytes + size_of::<Command>(),
                fresh: false,
                reuse: None,
                trace: None,
            }
        };
        let admitted = self.inner.admit(op(command));
        if admitted.at_limit() {
            let command = self.inner.reject(admitted);
            self.submit(gpu)?;
            self.inner.admit(op(command));
        }
        for &(key, _) in effects.writes {
            self.written(key, effects.bounds);
        }
        Ok(())
    }

    pub fn flush(&mut self, gpu: &mut Gpu, boundary: Boundary) -> Result<(), Error> {
        use record::Front as _;
        self.submit(gpu)?;
        if matches!(Front::reason(boundary), record::Reason::Barrier) {
            gpu.flush()?;
        }
        Ok(())
    }

    pub(super) fn damage(&self, key: ResourceKey, size: (u32, u32)) -> Option<Region> {
        match &self.damage {
            Some((old, damage)) if *old == key && damage.size() == size => {
                (!damage.is_empty()).then(|| damage.clone())
            }
            _ => Some(Region::whole(size)),
        }
    }

    pub(super) fn written(&mut self, key: ResourceKey, bounds: Rect) {
        if let Some((old, damage)) = &mut self.damage
            && *old == key
        {
            damage.add(bounds.into());
        }
    }

    pub(super) fn verify(
        &mut self,
        gpu: &mut Gpu,
        image: &Image,
        damage: Region,
    ) -> Result<(), Error> {
        let history = self
            .verification
            .get_or_insert_with(Default::default)
            .clone();
        let (width, height) = (image.width(), image.height());
        let serial = self.verification_serial;
        gpu.observe_readback(image, move |pixels| {
            let mut progress = history.progress.lock().unwrap();
            progress
                .completed
                .insert(serial, (damage, pixels.map(|bytes| (width, height, bytes))));
            // The GPU listener can deliver completions concurrently with a
            // poll on the owner thread. Compare them in stream order.
            loop {
                let next = progress.next;
                let Some((damage, pixels)) = progress.completed.remove(&next) else {
                    break;
                };
                progress.next += 1;
                match pixels {
                    Ok(pixels) => {
                        if record::Recording::<Command>::verify_pixels(
                            &mut progress.previous,
                            pixels,
                            &damage,
                            false,
                        ) {
                            #[cfg(feature = "d3d9-counters")]
                            history
                                .mismatches
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            log::warn!("D3D11 frame pixel verification mismatch outside damage");
                        }
                    }
                    Err(error) => {
                        progress.previous = None;
                        log::warn!("D3D11 frame verification readback failed: {error}");
                    }
                }
            }
        })?;
        self.verification_serial += 1;
        Ok(())
    }

    pub(super) fn presented(
        &mut self,
        key: ResourceKey,
        size: (u32, u32),
        serial: u64,
        _pixels: u64,
        _submitted: bool,
    ) {
        self.inner.finish(0, Some(serial));
        match &mut self.damage {
            Some((old, damage)) if damage.size() == size => {
                *old = key;
                damage.clear();
            }
            _ => self.damage = Some((key, Region::empty(size))),
        }
        #[cfg(feature = "d3d9-counters")]
        {
            let c = &mut self.counters;
            c.presents += 1;
            c.flushes[0] += u64::from(_submitted);
            c.eliminated_frames += u64::from(_pixels == 0 && !_submitted);
            c.changed_pixels += _pixels;
            c.damage_pixels += _pixels;
            if let Some(verification) = &self.verification {
                c.verification_mismatches = verification
                    .mismatches
                    .load(std::sync::atomic::Ordering::Relaxed);
            }
            log::debug!(
                "D3D11 recording: presents {}; present flushes {}; wait flushes {}; static frames {}; published pixels {}; verification mismatches {}",
                c.presents,
                c.flushes[0],
                c.flushes[7],
                c.eliminated_frames,
                c.changed_pixels,
                c.verification_mismatches
            );
        }
    }

    pub(super) fn submit(&mut self, gpu: &mut Gpu) -> Result<bool, Error> {
        use record::Front as _;
        // Modern commands keep their original attachment and scissor state.
        // Only the presentation copy uses the conservative write regions.
        let plan = self.inner.unclipped_plan();
        let mut cursor = self.inner.cursor;
        let pending = cursor.ops < self.inner.commands.len();
        while cursor.ops < self.inner.commands.len() {
            if let Some(command) = plan.at(&self.inner, cursor.ops)
                && let Err(error) = Front::encode(gpu, command.payload)
            {
                self.inner.retire(&plan, cursor);
                return Err(error);
            }
            cursor.ops += 1;
        }
        self.inner.retire(&plan, cursor);
        self.inner.end(Some(plan));
        Ok(pending)
    }
}

#[cfg(test)]
mod tests;

//! Direct submission of recorded modern commands.
use crate::record;
pub use crate::record::ResourceKey;
use adv_device::{
    gpu::{
        Dispatch, DispatchArguments, Error, Gpu, ModernDraw, ModernTransfer, QuerySlot,
        SlotBindings,
    },
    surface::Rect,
};
use adv_gpu::ImageView;
use std::sync::Arc;

pub enum Command {
    Draw {
        draw: Box<ModernDraw>,
        vertex: SlotBindings,
        fragment: SlotBindings,
        statistics: Option<QuerySlot>,
    },
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
}

impl Recording {
    pub fn append(
        &mut self,
        gpu: &mut Gpu,
        command: Command,
        effects: Effects<'_>,
    ) -> Result<(), Error> {
        let op = |payload| {
            let kind = match &payload {
                Command::Draw { .. } => record::Kind::Draw,
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
            self.submit(gpu, record::Reason::CommandLimit)?;
            self.inner.admit(op(command));
        }
        Ok(())
    }

    pub fn flush(&mut self, gpu: &mut Gpu, boundary: Boundary) -> Result<(), Error> {
        use record::Front as _;
        self.submit(gpu, Front::reason(boundary))?;
        if matches!(boundary, Boundary::Flush) {
            gpu.flush()?;
        }
        Ok(())
    }

    pub(super) fn present(
        &mut self,
        gpu: &mut Gpu,
        key: ResourceKey,
        size: (u32, u32),
        serial: u64,
        first: bool,
    ) -> Result<adv_device::surface::Region, Error> {
        use adv_device::surface::Region;
        let damage = self.inner.damage.written_region(key, size);
        let damage = if first || damage.is_empty() {
            Region::whole(size)
        } else {
            damage
        };
        self.submit(gpu, record::Reason::Wait)?;
        self.inner.finish(u128::from(serial), Some(serial));
        Ok(damage)
    }

    fn submit(&mut self, gpu: &mut Gpu, reason: record::Reason) -> Result<(), Error> {
        use record::Front as _;
        let plan = self.inner.plan(reason);
        let mut cursor = self.inner.cursor;
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
        Ok(())
    }
}

#[cfg(test)]
mod tests;

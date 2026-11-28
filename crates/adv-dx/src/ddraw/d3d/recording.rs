//! Device7 submissions over the common version and pass planner.
use crate::argument::CallError;
use crate::record::{
    Counters, Cursor, Front, Kind, Op, Plan, Reason, Recording, ResourceKey, damage::Input,
    key::hash,
};
use adv_device::{
    gpu::{self, DrawState, Gpu, ProgrammableDraw, Target},
    surface::{Access, Presenter, Rect, Region, SurfaceMemory, Surfaces},
};
use std::sync::Arc;

pub(super) type Queue = Recording<Payload>;

pub(super) enum Payload {
    Clear {
        surface: u32,
        target: Target,
        rect: Rect,
        color: Option<[f64; 4]>,
        depth: Option<f32>,
        stencil: Option<u32>,
    },
    Draw {
        surface: u32,
        area: Rect,
        target: Target,
        draw: Arc<ProgrammableDraw>,
        state: DrawState,
        line_ranges: Option<Arc<[std::ops::Range<u32>]>>,
    },
}

pub(super) struct Adapter;
pub(super) struct Submission<'a> {
    gpu: &'a mut Gpu,
    clips: Option<&'a [Rect]>,
    clear_mask: u8,
}

pub(super) enum Boundary {
    SurfaceAccess,
    QueueLimit,
}

impl Front for Adapter {
    type Payload = Payload;
    type Sink<'a> = Submission<'a>;
    type Completion = ();
    type Error = gpu::Error;
    type Boundary = Boundary;

    fn encode(sink: &mut Submission<'_>, payload: &Payload) -> Result<(), gpu::Error> {
        match payload {
            Payload::Clear {
                target,
                rect,
                color,
                depth,
                stencil,
                ..
            } => {
                let color = color.filter(|_| sink.clear_mask & 1 != 0);
                let depth = depth.filter(|_| sink.clear_mask & 2 != 0);
                let stencil = stencil.filter(|_| sink.clear_mask & 4 != 0);
                if color.is_some() || depth.is_some() || stencil.is_some() {
                    sink.gpu
                        .clear_region(target, *rect, color, depth, stencil)?;
                }
                Ok(())
            }
            Payload::Draw {
                target,
                draw,
                state,
                line_ranges,
                ..
            } => sink.gpu.draw_programmable_line_ranges(
                target,
                draw.clone(),
                state,
                sink.clips,
                line_ranges.clone(),
            ),
        }
    }

    fn reason(boundary: Boundary) -> Reason {
        match boundary {
            Boundary::SurfaceAccess => Reason::Map,
            Boundary::QueueLimit => Reason::CommandLimit,
        }
    }
}

pub(super) fn flush<P: Presenter>(
    queue: &mut Queue,
    gpu: &mut Gpu,
    store: &mut Surfaces<P>,
    memory: &dyn SurfaceMemory,
    boundary: Boundary,
) -> Result<(), CallError> {
    if queue.commands.is_empty() {
        return Ok(());
    }
    let plan = queue.plan(Adapter::reason(boundary));
    execute(queue, gpu, store, memory, plan)
}

/// Run the planned commands. A color write makes the CPU copy stale only
/// where it executes, so a later surface access reads back no more.
fn execute<P: Presenter>(
    queue: &mut Queue,
    gpu: &mut Gpu,
    store: &mut Surfaces<P>,
    memory: &dyn SurfaceMemory,
    plan: Plan<Payload>,
) -> Result<(), CallError> {
    for index in queue.cursor.ops..queue.commands.len() {
        if let Some(op) = plan.at(queue, index) {
            let (surface, area, color) = match op.payload {
                Payload::Clear {
                    surface,
                    rect,
                    color,
                    ..
                } => (*surface, *rect, color.is_some() && op.clear_mask & 1 != 0),
                Payload::Draw {
                    surface,
                    area,
                    state,
                    ..
                } => (*surface, *area, state.color_write != 0),
            };
            if color {
                let whole = [area];
                for &rect in op.scissors.as_deref().unwrap_or(&whole) {
                    let rect = rect.clipped(area);
                    if !rect.empty() {
                        store.gpu_access(surface, rect, Access::Write, memory)?;
                    }
                }
            }
            Adapter::encode(
                &mut Submission {
                    gpu,
                    clips: op.scissors.as_deref(),
                    clear_mask: op.clear_mask,
                },
                op.payload,
            )
            .map_err(|error| error.to_string())?;
        }
        queue.retire(
            &plan,
            Cursor {
                ops: index + 1,
                scissors: 0,
            },
        );
    }
    queue.end(Some(plan));
    Ok(())
}

/// Plan the recording for presenting `slot`, run what the plan keeps and
/// return the cells that changed since the previous presentation. Without
/// a known change they are the whole surface.
pub(super) fn present<P: Presenter>(
    queue: &mut Queue,
    gpu: &mut Gpu,
    store: &mut Surfaces<P>,
    memory: &dyn SurfaceMemory,
    counters: &mut Counters,
    (slot, size): (u128, (u32, u32)),
) -> Result<Region, CallError> {
    let plan = queue.plan(Reason::Present {
        slot,
        verify: false,
    });
    queue.present_damage(
        &plan,
        size,
        false,
        |delta, current| match delta {
            Some(delta) if current => delta.clone(),
            _ => Region::whole(size),
        },
        counters,
    );
    execute(queue, gpu, store, memory, plan)?;
    Ok(queue.verify_region.clone().unwrap())
}

pub(super) fn key(surface: u32, aspect: u8) -> ResourceKey {
    ResourceKey((u64::from(surface), 0), 0, aspect)
}

pub(super) struct Effects<'a> {
    pub area: Rect,
    pub target: u32,
    pub depth: Option<u32>,
    pub view: Rect,
    pub reads: &'a [ResourceKey],
    pub bytes: usize,
    pub state_key: u128,
}

pub(super) fn admit<P: Presenter>(
    queue: &mut Queue,
    gpu: &mut Gpu,
    store: &mut Surfaces<P>,
    memory: &dyn SurfaceMemory,
    payload: Payload,
    effects: Effects<'_>,
) -> Result<(), CallError> {
    let (kind, clear_keys, depth_write, stencil_write, color_write) = match &payload {
        Payload::Clear {
            color,
            depth,
            stencil,
            ..
        } => (
            Kind::Clear,
            [
                color.map(|c| hash(c.map(f64::to_bits))),
                depth.map(|d| hash(d.to_bits())),
                stencil.map(hash),
            ],
            depth.is_some(),
            stencil.is_some(),
            color.is_some(),
        ),
        Payload::Draw { state, .. } => (
            Kind::Draw,
            [None; 3],
            state.depth_write,
            state.stencil.is_some(),
            state.color_write != 0,
        ),
    };
    let color = key(effects.target, 0);
    let mut reads = effects.reads.to_vec();
    let mut writes = Vec::new();
    if color_write {
        writes.push((color, kind == Kind::Clear && effects.area == effects.view));
    }
    if let Some(depth) = effects.depth {
        for (aspect, writes_aspect) in [(1, depth_write), (2, stencil_write)] {
            if writes_aspect {
                writes.push((
                    key(depth, aspect),
                    kind == Kind::Clear && effects.area == effects.view,
                ));
            }
            if kind == Kind::Draw {
                reads.push(key(depth, aspect));
            }
        }
    }
    let target = match &payload {
        Payload::Clear { target, .. } | Payload::Draw { target, .. } => target,
    };
    let slot = hash(target.image.id());
    let attachment = target.depth().map(|depth| {
        (
            Rect {
                x: 0,
                y: 0,
                w: depth.width(),
                h: depth.height(),
            },
            depth.format() as u8,
        )
    });
    let admitted = queue.admit(Op {
        payload,
        effects: Input {
            kind,
            view: effects.view,
            attachment,
            color_shape: 0,
            slot,
            bounds: effects.area,
            key: effects.state_key,
            reads: &reads,
            samples: &[],
            writes: &writes,
            ignores_sampled_color: false,
            clear_keys,
        },
        bytes: effects.bytes,
        fresh: false,
        reuse: None,
        trace: None,
    });
    if admitted.at_limit() {
        flush(queue, gpu, store, memory, Boundary::QueueLimit)?;
    }
    Ok(())
}

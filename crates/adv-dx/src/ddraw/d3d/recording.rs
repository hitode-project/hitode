//! Device7 submissions over the common version and pass planner.
use crate::record::{
    Cursor, Front, Kind, Op, Reason, Recording, ResourceKey, damage::Input, key::hash,
};
use adv_device::{
    gpu::{self, DrawState, Gpu, ProgrammableDraw, Target, Vertex},
    surface::Rect,
};
use std::sync::Arc;

pub(super) type Queue = Recording<Payload>;

pub(super) enum Payload {
    Clear {
        target: Target,
        rect: Rect,
        color: Option<[f64; 4]>,
        depth: Option<f32>,
        stencil: Option<u32>,
    },
    Lines {
        target: Target,
        vertices: Arc<Vec<Vertex>>,
        state: DrawState,
    },
    Draw {
        target: Target,
        draw: Arc<ProgrammableDraw>,
        state: DrawState,
    },
}

pub(super) struct Adapter;
pub(super) struct Submission<'a> {
    gpu: &'a mut Gpu,
    clips: Option<&'a [Rect]>,
    clear_mask: u8,
}

pub(super) enum Boundary {
    EndScene,
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
            Payload::Lines {
                target,
                vertices,
                state,
            } => sink
                .gpu
                .draw_arc_clips(target, vertices.clone(), None, *state, sink.clips),
            Payload::Draw {
                target,
                draw,
                state,
            } => sink
                .gpu
                .draw_programmable_arc_clips(target, draw.clone(), state, sink.clips),
        }
    }

    fn reason(boundary: Boundary) -> Reason {
        match boundary {
            Boundary::EndScene => Reason::Barrier,
            Boundary::SurfaceAccess => Reason::Map,
            Boundary::QueueLimit => Reason::CommandLimit,
        }
    }
}

pub(super) fn flush(
    queue: &mut Queue,
    gpu: &mut Gpu,
    boundary: Boundary,
) -> Result<(), gpu::Error> {
    if queue.commands.is_empty() {
        return Ok(());
    }
    let plan = queue.plan(Adapter::reason(boundary));
    for index in queue.cursor.ops..queue.commands.len() {
        if let Some(op) = plan.at(queue, index) {
            Adapter::encode(
                &mut Submission {
                    gpu,
                    clips: op.scissors.as_deref(),
                    clear_mask: op.clear_mask,
                },
                op.payload,
            )?;
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

pub(super) fn admit(
    queue: &mut Queue,
    gpu: &mut Gpu,
    payload: Payload,
    effects: Effects<'_>,
) -> Result<(), gpu::Error> {
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
        Payload::Draw { state, .. } | Payload::Lines { state, .. } => (
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
        Payload::Clear { target, .. }
        | Payload::Lines { target, .. }
        | Payload::Draw { target, .. } => target,
    };
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
            slot: u128::from(effects.target),
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
        flush(queue, gpu, Boundary::QueueLimit)?;
    }
    Ok(())
}

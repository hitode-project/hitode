//! Draw runs are built from the existing execution plan. Each command keeps its
//! own damage and version effects. A run shares one attachment set and one
//! scissor sequence; its vertices retain primitive order within each scissor.
use super::*;
use adv_device::gpu::{ProgrammableDraw, Topology};
use adv_device::surface::Rect;

fn draw_at(
    f: &Recording,
    n: usize,
) -> Option<(&fixed::Draw, Option<smallvec::SmallVec<[Rect; 8]>>)> {
    let entry = f.plan.as_ref().unwrap().at(&f.core, n)?;
    match &entry.payload.operation {
        Operation::Draw(draw) => Some((draw, entry.scissors)),
        _ => None,
    }
}

fn same_image(a: Option<&adv_device::gpu::Image>, b: Option<&adv_device::gpu::Image>) -> bool {
    a.map(|i| (i.id(), i.alpha_mode())) == b.map(|i| (i.id(), i.alpha_mode()))
}

fn compatible(a: &fixed::Draw, b: &fixed::Draw) -> bool {
    if a.state != b.state
        || !a.target.same_attachments(&b.target)
        || !same_image(a.image.as_ref(), b.image.as_ref())
    {
        return false;
    }
    match (&a.programmable, &b.programmable) {
        (None, None) => {
            !a.state.last_pixel
                || !matches!(a.state.topology, Topology::Lines | Topology::LineStrip)
        }
        (Some(a), Some(b)) => {
            a.instance_count == 1
                && b.instance_count == 1
                && a.program == b.program
                && a.streams.len() == b.streams.len()
                && a.streams.iter().zip(&b.streams).all(|(a, b)| {
                    !a.instance
                        && !b.instance
                        && a.stride == b.stride
                        && a.attributes == b.attributes
                        && (a.stride != 0 || a.bytes.as_slice() == b.bytes.as_slice())
                })
                && a.textures.iter().zip(&b.textures).all(|(a, b)| {
                    a.sampler == b.sampler && same_image(a.image.as_ref(), b.image.as_ref())
                })
                && a.constant_ranges().eq(b.constant_ranges())
        }
        _ => false,
    }
}

fn list(topology: Topology) -> Topology {
    match topology {
        Topology::TriangleStrip => Topology::Triangles,
        Topology::LineStrip => Topology::Lines,
        other => other,
    }
}

fn list_len(topology: Topology, n: usize) -> usize {
    match topology {
        Topology::TriangleStrip => (n - 2) * 3,
        Topology::LineStrip => (n - 1) * 2,
        _ => n,
    }
}

fn indices(topology: Topology, n: usize, at: impl Fn(usize) -> u32, mut push: impl FnMut(u32)) {
    match topology {
        Topology::TriangleStrip => {
            for i in 0..n - 2 {
                // Keep the first provoking vertex when reversing odd triangles.
                push(at(i));
                push(at(i + 1 + i % 2));
                push(at(i + 2 - i % 2));
            }
        }
        Topology::LineStrip => {
            for i in 0..n - 1 {
                push(at(i));
                push(at(i + 1));
            }
        }
        _ => (0..n).map(at).for_each(push),
    }
}

fn index_len(p: &ProgrammableDraw) -> usize {
    p.indices
        .as_ref()
        .map_or(p.vertex_count as usize, |i| i.len())
}

fn draw_size(d: &fixed::Draw) -> (usize, usize) {
    let topology = d.state.topology;
    match &d.programmable {
        Some(p) => (
            list_len(topology, index_len(p)),
            p.streams
                .iter()
                .filter(|s| s.stride != 0)
                .map(|s| s.stride as usize * p.vertex_count as usize)
                .sum(),
        ),
        None => (
            0,
            list_len(topology, d.vertices.len()) * std::mem::size_of::<adv_device::gpu::Vertex>(),
        ),
    }
}

fn combine<'a>(
    first: &fixed::Draw,
    draws: impl Iterator<Item = (&'a fixed::Draw, Option<&'a [u8]>)> + Clone,
) -> fixed::Draw {
    let mut merged = first.clone();
    let topology = first.state.topology;
    merged.state.topology = list(topology);
    if let Some(program) = &first.programmable {
        let programs = draws
            .clone()
            .map(|(d, _)| d.programmable.as_deref().unwrap());
        let mut program = program.as_ref().clone();
        program.vertex_count = programs.clone().map(|p| p.vertex_count).sum();
        let indexed = list(topology) != topology || programs.clone().any(|p| p.indices.is_some());
        program.indices = indexed.then(|| {
            let mut output = Vec::new();
            let mut base = 0;
            for p in programs.clone() {
                indices(
                    topology,
                    index_len(p),
                    |n| base + p.indices.as_ref().map_or(n as u32, |i| i[n]),
                    |n| output.push(n),
                );
                base += p.vertex_count;
            }
            output.into()
        });
        for (n, stream) in program.streams.iter_mut().enumerate() {
            if stream.stride == 0 {
                continue;
            }
            let stride = stream.stride as usize;
            let mut bytes = Vec::with_capacity(stride * program.vertex_count as usize);
            for (p, (_, capture)) in programs.clone().zip(draws.clone()) {
                let source = capture.unwrap_or_else(|| p.streams[n].bytes.as_slice());
                let len = stride * p.vertex_count as usize;
                let end = bytes.len() + len;
                bytes.extend_from_slice(&source[..source.len().min(len)]);
                bytes.resize(end, 0);
            }
            stream.bytes = bytes.into();
        }
        merged.programmable = Some(Arc::new(program));
    } else {
        let mut vertices = Vec::new();
        for (d, _) in draws {
            indices(
                topology,
                d.vertices.len(),
                |n| n as u32,
                |n| vertices.push(d.vertices[n as usize]),
            );
        }
        merged.vertices = vertices.into();
    }
    merged
}

pub(super) fn run(env: &mut Env<'_>, owner: Id) -> Result<bool, Error> {
    let f = frame(env, owner);
    let start = f.core.cursor.ops;
    let Some((first, scissors)) = draw_at(f, start) else {
        return Ok(false);
    };
    if scissors.as_ref().is_some_and(|clips| clips.is_empty()) {
        return Ok(false);
    }
    let (mut indices, mut bytes) = draw_size(first);
    // Zero-stride streams are shared by the run and uploaded only once.
    if let Some(p) = &first.programmable {
        bytes += p
            .streams
            .iter()
            .filter(|s| s.stride == 0)
            .map(|s| s.bytes.len().next_multiple_of(4))
            .sum::<usize>();
    }
    let mut end = start + 1;
    while let Some((next, next_scissors)) = draw_at(f, end) {
        if !f.core.can_merge(start, end) || next_scissors != scissors || !compatible(first, next) {
            break;
        }
        let (more_indices, more_bytes) = draw_size(next);
        if indices + more_indices > 0x300000 || bytes + more_bytes > 16 * 1024 * 1024 {
            break;
        }
        indices += more_indices;
        bytes += more_bytes;
        end += 1;
    }
    if end == start + 1 {
        return Ok(false);
    }
    let operation = Operation::Draw(combine(
        first,
        (start..end).map(|n| {
            let command = f.core.commands[n].as_ref().unwrap();
            let Operation::Draw(draw) = &command.operation else {
                unreachable!()
            };
            (
                draw,
                command
                    .vertices
                    .as_ref()
                    .map(|range| &f.vertices[range.clone()]),
            )
        }),
    ));
    let mut cursor = f.core.cursor;
    let count = scissors.as_ref().map_or(1, |clips| clips.len());
    while cursor.scissors < count {
        let clip = scissors.as_ref().map(|clips| clips[cursor.scissors]);
        let before = env.ext.get::<State>().frames.gpu_draws;
        env.ext
            .get_mut::<State>()
            .gpu
            .as_mut()
            .unwrap()
            .set_clip(clip);
        let result = Payload::encode(env, &operation);
        env.ext
            .get_mut::<State>()
            .gpu
            .as_mut()
            .unwrap()
            .set_clip(None);
        result?;
        let counters = &mut env.ext.get_mut::<State>().frames;
        counters.uncoalesced_gpu_draws += (counters.gpu_draws - before) * (end - start - 1) as u64;
        for n in start..end {
            let f = frame(env, owner);
            let node = f.core.node(n);
            let command = f.core.commands[n].take().unwrap();
            submitted(env, owner, node, &command.operation, clip);
            frame(env, owner).core.commands[n] = Some(command);
        }
        cursor.scissors += 1;
        let f = frame(env, owner);
        f.core.retire(f.plan.as_ref().unwrap(), cursor);
    }
    let f = frame(env, owner);
    f.core.retire(
        f.plan.as_ref().unwrap(),
        Cursor {
            ops: end,
            scissors: 0,
        },
    );
    Ok(true)
}

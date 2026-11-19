//! Draw runs are built from the existing execution plan. Each command keeps its
//! own damage and version effects. A run shares one attachment set and one
//! scissor sequence; its vertices retain primitive order within each scissor.
use super::*;
use adv_device::gpu::{ProgrammableDraw, Topology};
use adv_device::surface::Rect;

fn draw_at(f: &Recording, n: usize) -> Option<&fixed::Draw> {
    if f.passes.skip(n)
        || f.select
            .as_ref()
            .is_some_and(|select| f.nodes[n].is_some_and(|node| !select.contains_key(&node)))
    {
        return None;
    }
    match &f.commands[n] {
        Operation::Draw(draw) => Some(draw),
        _ => None,
    }
}

fn clip(f: &Recording, n: usize) -> Option<&[Rect]> {
    f.nodes[n].and_then(|node| f.damage.clip(node))
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
    draws: impl Iterator<Item = &'a fixed::Draw> + Clone,
) -> fixed::Draw {
    let mut merged = first.clone();
    let topology = first.state.topology;
    merged.state.topology = list(topology);
    if let Some(program) = &first.programmable {
        let programs = draws.map(|d| d.programmable.as_deref().unwrap());
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
            for p in programs.clone() {
                let source = &p.streams[n].bytes;
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
        for d in draws {
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
    let start = f.head;
    let Some(first) = draw_at(f, start) else {
        return Ok(false);
    };
    let clips = clip(f, start);
    if clips.is_some_and(|clips| {
        clips.is_empty() || clips.iter().any(|&r| f.passes.outside_damage(start, r))
    }) {
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
    while let Some(next) = f.commands.get(end).and_then(|_| draw_at(f, end)) {
        if !f.passes.can_merge(start, end)
            || clip(f, end) != clips
            || clips.is_some_and(|clips| clips.iter().any(|&r| f.passes.outside_damage(end, r)))
            || !compatible(first, next)
        {
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
    let operation = Operation::Draw(combine(first, (start..end).map(|n| draw_at(f, n).unwrap())));
    let clips = clips.map(schedule_clips);
    let mut part = f.part;
    let count = clips.as_ref().map_or(1, |clips| clips.len());
    while part < count {
        let clip = clips.as_ref().map(|clips| clips[part]);
        env.ext
            .get_mut::<State>()
            .gpu
            .as_mut()
            .unwrap()
            .set_clip(clip);
        let result = execute_gpu(env, &operation);
        env.ext
            .get_mut::<State>()
            .gpu
            .as_mut()
            .unwrap()
            .set_clip(None);
        result?;
        for n in start..end {
            let f = frame(env, owner);
            let operation = std::mem::replace(&mut f.commands[n], Operation::Consumed);
            let node = f.nodes[n];
            submitted(env, owner, node, &operation, clip);
            frame(env, owner).commands[n] = operation;
        }
        part += 1;
        frame(env, owner).part = part;
    }
    let f = frame(env, owner);
    for n in start..end {
        if let Some(node) = f.nodes[n] {
            f.damage.executed(
                node,
                f.select
                    .as_ref()
                    .and_then(|s| s.get(&node))
                    .map(Vec::as_slice),
            );
        }
        f.commands[n] = Operation::Consumed;
    }
    f.head = end;
    f.part = 0;
    f.partial = true;
    Ok(true)
}

fn schedule_clips(clips: &[Rect]) -> smallvec::SmallVec<[Rect; 8]> {
    smallvec::SmallVec::from_slice(clips)
}

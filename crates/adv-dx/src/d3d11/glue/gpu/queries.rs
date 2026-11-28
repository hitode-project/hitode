use super::*;
use crate::d3d11::object::query::{Active, Kind, Pending, Query, Sample};
use crate::shader::expand::statistics;
use adv_device::gpu::{DispatchArguments, ModernShader, QuerySlot, SlotBindings, StorageBinding};
use adv_gpu::queue::QueryKind;
use std::sync::Arc;

fn query_mut<'a>(env: &'a mut Env<'_>, pointer: u32) -> Result<&'a mut Query, Error> {
    ops::object_mut(env, table(env), pointer)?
        .query_mut()
        .ok_or(Error::InvalidArgument)
}

fn query_slot(gpu: &Gpu, kind: QueryKind, count: u32) -> Result<QuerySlot, Error> {
    Ok(QuerySlot {
        set: Arc::new(gpu.device().create_query_set(kind, count)?),
        index: 0,
    })
}

pub(in crate::d3d11::glue) fn begin_query(env: &mut Env<'_>) -> Result<(), Error> {
    let pointer = env.arg(1);
    let query = context::owned(env, pointer)
        .and_then(Object::query)
        .ok_or(Error::InvalidArgument)?;
    if matches!(query.kind, Kind::Event | Kind::Timestamp) {
        return Ok(());
    }
    let query = query_mut(env, pointer)?;
    query.result = None;
    query.active = Some(Active::default());
    if !null_device(env)? {
        let id = table(env).key(&env.sys.objects, pointer).unwrap();
        execution(env.ext)?.active_queries.insert(id, pointer);
    }
    Ok(())
}

pub(in crate::d3d11::glue) fn end_query(env: &mut Env<'_>) -> Result<(), Error> {
    let pointer = env.arg(1);
    let query = context::owned(env, pointer)
        .and_then(Object::query)
        .ok_or(Error::InvalidArgument)?;
    let kind = query.kind;
    let id = table(env).key(&env.sys.objects, pointer).unwrap();
    let mut active = query_mut(env, pointer)?.active.take().unwrap_or_default();
    let pending = if null_device(env)? {
        active.counters[0] = u64::from(kind == Kind::Event);
        Pending {
            buffer: None,
            completion: None,
            words: active.counters,
            samples: Vec::new(),
            native_count: 0,
            predicate: None,
        }
    } else {
        let execution = execution(env.ext)?;
        execution.active_queries.remove(&id);
        execution.barrier(Boundary::Query)?;
        let mut width = 8;
        match kind {
            Kind::Timestamp
                if execution
                    .gpu
                    .device()
                    .caps()
                    .features
                    .contains(adv_gpu::Features::TIMESTAMPS) =>
            {
                let slot = query_slot(&execution.gpu, QueryKind::Timestamp, 2)?;
                execution.gpu.timestamp(slot.clone())?;
                active.slots.push(QuerySlot { index: 1, ..slot });
            }
            Kind::Event => active.counters[0] = 1,
            Kind::Disjoint => {
                let set = execution
                    .gpu
                    .device()
                    .caps()
                    .features
                    .contains(adv_gpu::Features::TIMESTAMPS)
                    .then(|| query_slot(&execution.gpu, QueryKind::Timestamp, 2))
                    .transpose()?;
                let result = execution
                    .gpu
                    .timestamp_disjoint(set.as_ref().map(|slot| slot.set.as_ref()));
                active.counters[0] = result.frequency;
                active.counters[1] = u64::from(result.disjoint);
            }
            Kind::PipelineStatistics | Kind::StreamStatistics(_) => width = 16,
            Kind::Occlusion
            | Kind::OcclusionPredicate
            | Kind::Timestamp
            | Kind::StreamOverflow(_) => (),
        }
        let predicate = if let Kind::StreamOverflow(stream) = kind {
            let result = counter_buffer(&execution.gpu, 8)?;
            let start = stream.map_or(0, u32::from);
            let end = stream.map_or(4, |stream| u32::from(stream) + 1);
            let code = format!(
                "@group(3) @binding(0) var<storage, read> counters: array<vec4<u32>>;
                 @group(3) @binding(1) var<storage, read_write> result: u32;
                 @compute @workgroup_size(1) fn overflow() {{
                    for (var stream = {start}u; stream < {end}u; stream++) {{
                        let c = counters[stream];
                        if any(c.xy != c.zw) {{ result = 1u; }}
                    }}
                 }}"
            );
            for (source, _) in active.samples.drain(..) {
                let (pipeline, bindings) = helper(
                    &execution.gpu,
                    &code,
                    "overflow",
                    [(0, source), (1, result.clone())],
                )?;
                execution.gpu.dispatch(
                    &pipeline,
                    &bindings,
                    DispatchArguments::Direct([1, 1, 1]),
                )?;
            }
            active.samples.push((result.clone(), Sample::Predicate));
            Some(result)
        } else if kind == Kind::OcclusionPredicate {
            let result = counter_buffer(&execution.gpu, 8)?;
            if !active.slots.is_empty() {
                let size = active.slots.len() as u64 * 8;
                let buffer = execution.gpu.device().create_buffer(&BufferDesc {
                    size,
                    usage: BufferUsage::COPY_DST | BufferUsage::STORAGE,
                    memory: Memory::Device,
                    label: "D3D11 occlusion predicate samples",
                })?;
                for (index, slot) in active.slots.drain(..).enumerate() {
                    execution
                        .gpu
                        .read_query(slot, buffer.clone(), index as u64 * 8)?;
                }
                let code = "@group(3) @binding(0) var<storage, read> samples: array<vec2<u32>>;
                    @group(3) @binding(1) var<storage, read_write> result: u32;
                    @compute @workgroup_size(1) fn occluded() {
                        for (var i = 0u; i < arrayLength(&samples); i++) {
                            if any(samples[i] != vec2<u32>()) { result = 1u; break; }
                        }
                    }";
                let (pipeline, bindings) = helper(
                    &execution.gpu,
                    code,
                    "occluded",
                    [
                        (
                            0,
                            BufferRange {
                                buffer,
                                range: 0..size,
                            },
                        ),
                        (1, result.clone()),
                    ],
                )?;
                execution.gpu.dispatch(
                    &pipeline,
                    &bindings,
                    DispatchArguments::Direct([1, 1, 1]),
                )?;
            }
            active.samples.push((result.clone(), Sample::Predicate));
            Some(result)
        } else {
            None
        };
        let native_count = active.slots.len();
        let native_bytes = native_count as u64 * width;
        let size = native_bytes
            + active
                .samples
                .iter()
                .map(|(sample, _)| sample.range.end - sample.range.start)
                .sum::<u64>();
        let buffer = if size == 0 {
            None
        } else {
            let buffer = execution.gpu.device().create_buffer(&BufferDesc {
                size,
                memory: Memory::Readback,
                usage: BufferUsage::COPY_DST,
                label: "D3D11 query readback",
            })?;
            for (index, slot) in active.slots.into_iter().enumerate() {
                execution
                    .gpu
                    .read_query(slot, buffer.clone(), index as u64 * width)?;
            }
            let mut offset = native_bytes;
            for (source, _) in &active.samples {
                execution.gpu.transfer(ModernTransfer::CopyBuffer {
                    source: source.clone(),
                    destination: buffer.clone(),
                    offset,
                })?;
                offset += source.range.end - source.range.start;
            }
            Some(buffer)
        };
        #[cfg(feature = "d3d9-counters")]
        if let Some(buffer) = &buffer {
            execution.gpu.count_readback(buffer.desc().size);
        }
        Pending {
            buffer,
            completion: Some(execution.gpu.completion()?),
            words: active.counters,
            samples: active
                .samples
                .into_iter()
                .map(|(_, sample)| sample)
                .collect(),
            native_count,
            predicate,
        }
    };
    query_mut(env, pointer)?.result = Some(pending);
    Ok(())
}

pub(in crate::d3d11::glue) fn get_data(env: &mut Env<'_>) -> Result<u32, Error> {
    let (pointer, output, size, flags) = (env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    let query = context::owned(env, pointer)
        .and_then(Object::query)
        .ok_or(Error::InvalidArgument)?;
    if flags & !1 != 0 || (output != 0 && size != query.kind.size()) || (output == 0 && size != 0) {
        return Err(Error::InvalidArgument);
    }
    let kind = query.kind;
    let table = table(env);
    let Some(result) = table
        .get_mut(&mut env.sys.objects, pointer)
        .unwrap()
        .query_mut()
        .unwrap()
        .result
        .as_mut()
    else {
        return Ok(1);
    };
    if let Some(completion) = &result.completion {
        match completion.try_recv() {
            Ok(()) => result.completion = None,
            Err(TryRecvError::Empty) => return Ok(1),
            Err(TryRecvError::Disconnected) => return Err(Error::Device),
        }
    }
    if let Some(buffer) = result.buffer.take() {
        let mapping = buffer.mapped().unwrap();
        // SAFETY: The queue signaled completion for every query copy above.
        let bytes = unsafe { mapping.as_ref() };
        let words: &[u64] = bytemuck::cast_slice(bytes);
        let width = if kind == Kind::PipelineStatistics {
            2
        } else {
            1
        };
        let (native, mut rest) = words.split_at(result.native_count * width);
        if kind == Kind::PipelineStatistics {
            for &[clip, pixel] in native.as_chunks::<2>().0 {
                result.words[6] += clip;
                result.words[7] += pixel;
            }
        } else {
            result.words[0] += native.iter().sum::<u64>();
        }
        for sample in result.samples.drain(..) {
            let fields: &[usize] = match sample {
                Sample::Stream => &[0, 1],
                Sample::Predicate => &[0],
                Sample::Expansion => &statistics::GPU_COUNTERS,
                Sample::Indirect => &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
            };
            let (values, remaining) = rest.split_at(fields.len());
            for (&field, &value) in fields.iter().zip(values) {
                result.words[field] += value;
            }
            rest = remaining;
        }
    }

    if output != 0 {
        if matches!(kind, Kind::OcclusionPredicate | Kind::StreamOverflow(_)) {
            let value = u32::from(result.words[0] != 0);
            write(env, output, value)?;
        } else {
            env.cpu
                .write(
                    output,
                    &bytemuck::cast_slice(&result.words)[..size as usize],
                )
                .map_err(|_| Error::InvalidArgument)?;
        }
    }
    Ok(0)
}

pub(super) fn draw_slots(
    env: &mut Env<'_>,
    rasterize: bool,
    native_statistics: bool,
) -> Result<(Option<QuerySlot>, Option<QuerySlot>), Error> {
    let device = context_device(env)?;
    let table = table(env);
    let exec = execution(env.ext)?;
    let features = exec.gpu.device().caps().features;
    let mut occlusion = None::<QuerySlot>;
    let mut statistics = None::<QuerySlot>;
    let objects = &mut env.sys.objects;
    exec.active_queries
        .retain(|&id, pointer| table.key(objects, *pointer) == Some(id));
    for &pointer in exec.active_queries.values() {
        let object = table.get_mut(objects, pointer).unwrap();
        if object.device() != Some(device) {
            continue;
        }
        let query = object.query_mut().unwrap();
        let active = query.active.as_mut().unwrap();
        match query.kind {
            Kind::Occlusion | Kind::OcclusionPredicate
                if rasterize && features.contains(adv_gpu::Features::OCCLUSION_QUERIES) =>
            {
                if occlusion.is_none() {
                    occlusion = Some(query_slot(&exec.gpu, QueryKind::Occlusion, 1)?);
                }
                active.slots.extend(occlusion.clone());
            }
            Kind::PipelineStatistics => {
                if native_statistics
                    && features.contains(adv_gpu::Features::PIPELINE_STATISTICS_QUERY)
                {
                    if statistics.is_none() {
                        statistics = Some(query_slot(&exec.gpu, QueryKind::PipelineStatistics, 1)?);
                    }
                    active.slots.extend(statistics.clone());
                }
            }
            Kind::Event
            | Kind::Timestamp
            | Kind::Disjoint
            | Kind::StreamStatistics(_)
            | Kind::StreamOverflow(_)
            | Kind::Occlusion
            | Kind::OcclusionPredicate => (),
        }
    }
    Ok((occlusion, statistics))
}

pub(super) fn stream_queries(env: &mut Env<'_>) -> Result<Option<BufferRange>, Error> {
    let device = context_device(env)?;
    let table = table(env);
    let exec = execution(env.ext)?;
    let objects = &mut env.sys.objects;
    exec.active_queries
        .retain(|&id, pointer| table.key(objects, *pointer) == Some(id));
    let mut counters: Option<Buffer> = None;
    for &pointer in exec.active_queries.values() {
        let object = table.get_mut(objects, pointer).unwrap();
        if object.device() != Some(device) {
            continue;
        }
        let query = object.query_mut().unwrap();
        let (range, sample) = match query.kind {
            Kind::StreamStatistics(stream) => {
                let start = u64::from(stream) * 16;
                (start..start + 16, Sample::Stream)
            }
            Kind::StreamOverflow(_) => (0..64, Sample::Predicate),
            _ => continue,
        };
        if counters.is_none() {
            counters = Some(counter_buffer(&exec.gpu, 64)?.buffer);
        }
        query.active.as_mut().unwrap().samples.push((
            BufferRange {
                buffer: counters.as_ref().unwrap().clone(),
                range,
            },
            sample,
        ));
    }
    Ok(counters.map(|buffer| BufferRange {
        buffer,
        range: 0..64,
    }))
}

pub(super) fn predicate(env: &Env<'_>, pointer: u32) -> Option<BufferRange> {
    context::owned(env, pointer)?
        .query()?
        .result
        .as_ref()?
        .predicate
        .clone()
}

pub(super) fn active_statistics(env: &Env<'_>) -> Result<bool, Error> {
    let device = context_device(env)?;
    let Some(exec) = env.ext.get::<State>().execution.as_ref() else {
        return Ok(false);
    };
    Ok(exec.active_queries.iter().any(|(&key, &pointer)| {
        table(env).key(&env.sys.objects, pointer) == Some(key)
            && table(env)
                .get(&env.sys.objects, pointer)
                .is_some_and(|object| {
                    object.device() == Some(device)
                        && object
                            .query()
                            .is_some_and(|query| query.kind == Kind::PipelineStatistics)
                })
    }))
}

pub(super) fn record_statistics(
    env: &mut Env<'_>,
    counters: [u64; 11],
    sample: Option<(BufferRange, Sample)>,
) -> Result<(), Error> {
    let device = context_device(env)?;
    let table = table(env);
    let exec = execution(env.ext)?;
    let objects = &mut env.sys.objects;
    exec.active_queries
        .retain(|&id, pointer| table.key(objects, *pointer) == Some(id));
    for &pointer in exec.active_queries.values() {
        let object = table.get_mut(objects, pointer).unwrap();
        if object.device() == Some(device)
            && let Some(query) = object
                .query_mut()
                .filter(|query| query.kind == Kind::PipelineStatistics)
        {
            let active = query.active.as_mut().unwrap();
            for (value, count) in active.counters.iter_mut().zip(counters) {
                *value += count;
            }
            active.samples.extend(sample.clone());
        }
    }
    Ok(())
}

pub(super) fn counter_buffer(gpu: &Gpu, size: u64) -> Result<BufferRange, Error> {
    Ok(BufferRange {
        buffer: resource::upload(gpu, &[], BufferUsage::STORAGE | BufferUsage::COPY_SRC, size)?,
        range: 0..size,
    })
}

pub(super) fn helper<const N: usize>(
    gpu: &Gpu,
    code: &str,
    entry: &str,
    buffers: [(u32, BufferRange); N],
) -> Result<(Arc<adv_device::gpu::Dispatch>, SlotBindings), Error> {
    let shader = Arc::new(ModernShader::wgsl(
        xxhash_rust::xxh3::xxh3_64(code.as_bytes()),
        code,
        entry,
        adv_device::gpu::ShaderStage::Compute,
    )?);
    let mut bindings = SlotBindings::default();
    for (slot, buffer) in buffers {
        bindings
            .storage
            .set(slot as usize, StorageBinding::Buffer(buffer));
    }
    Ok((Arc::new(gpu.prepare_dispatch(shader)?), bindings))
}

pub(super) fn count_indirect(
    env: &mut Env<'_>,
    arguments: &BufferRange,
    count: statistics::Indirect,
    reads: &[ResourceKey],
) -> Result<(), Error> {
    let exec = execution(env.ext)?;
    let counters = counter_buffer(&exec.gpu, (statistics::INDIRECT_WORDS * 4) as u64)?;
    let (pipeline, bindings) = helper(
        &exec.gpu,
        &count.compute_at(arguments.range.start / 4),
        "indirect_statistics",
        [
            (
                statistics::INDIRECT_ARGUMENTS_SLOT,
                BufferRange {
                    buffer: arguments.buffer.clone(),
                    range: 0..arguments.buffer.desc().size,
                },
            ),
            (crate::shader::wgsl::STATISTICS_SLOT, counters.clone()),
        ],
    )?;
    let extent = extent_rect([1, 1, 1]);
    exec.recording.append(
        &mut exec.gpu,
        Command::Dispatch(Box::new(crate::d3d11::recording::Compute {
            pipeline,
            bindings,
            arguments: DispatchArguments::Direct([1, 1, 1]),
        })),
        Effects {
            extent,
            bounds: extent,
            key: 0,
            reads,
            writes: &[],
            retained_bytes: 0,
        },
    )?;
    record_statistics(env, [0; 11], Some((counters, Sample::Indirect)))
}

pub(super) enum ArgumentWords {
    Direct(Vec<u32>),
    Indirect(BufferRange),
}

pub(super) fn gate_arguments(
    ext: &mut Ext,
    predicate: &BufferRange,
    value: bool,
    args: ArgumentWords,
    reads: &[ResourceKey],
) -> Result<BufferRange, Error> {
    let exec = execution(ext)?;
    let source = match args {
        ArgumentWords::Direct(words) => {
            let bytes = bytemuck::cast_slice(&words);
            BufferRange {
                buffer: resource::upload(
                    &exec.gpu,
                    bytes,
                    BufferUsage::STORAGE,
                    bytes.len() as u64,
                )?,
                range: 0..bytes.len() as u64,
            }
        }
        ArgumentWords::Indirect(buffer) => buffer,
    };
    let size = source.range.end - source.range.start;
    let offset = source.range.start / 4;
    let buffer = exec.gpu.device().create_buffer(&BufferDesc {
        size,
        usage: BufferUsage::STORAGE | BufferUsage::INDIRECT,
        memory: Memory::Device,
        label: "D3D11 predicated arguments",
    })?;
    let destination = BufferRange {
        buffer,
        range: 0..size,
    };
    let code = format!(
        "@group(3) @binding(0) var<storage, read> source: array<u32>;
         @group(3) @binding(1) var<storage, read> predicate: u32;
         @group(3) @binding(2) var<storage, read_write> result: array<u32>;
         @compute @workgroup_size(1) fn gate() {{
             for (var i = 0u; i < {}u; i++) {{ result[i] = source[{offset}u + i]; }}
             if (predicate != 0u) == {value} {{ result[0] = 0u; }}
         }}",
        size / 4,
    );
    let (pipeline, bindings) = helper(
        &exec.gpu,
        &code,
        "gate",
        [
            (
                0,
                BufferRange {
                    range: 0..source.buffer.desc().size,
                    buffer: source.buffer,
                },
            ),
            (1, predicate.clone()),
            (2, destination.clone()),
        ],
    )?;
    let extent = extent_rect([1, 1, 1]);
    exec.recording.append(
        &mut exec.gpu,
        Command::Dispatch(Box::new(crate::d3d11::recording::Compute {
            pipeline,
            bindings,
            arguments: DispatchArguments::Direct([1, 1, 1]),
        })),
        Effects {
            extent,
            bounds: extent,
            key: 0,
            reads,
            writes: &[],
            retained_bytes: 0,
        },
    )?;
    Ok(destination)
}

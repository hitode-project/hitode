use super::*;
use crate::d3d11::object::query::{Active, Kind, Pending, Query};
use adv_device::gpu::{Predicate, QuerySlot};
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
            last: None,
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
            Kind::PipelineStatistics => width = 16,
            Kind::Occlusion | Kind::OcclusionPredicate | Kind::Timestamp => (),
        }
        let last = (active.slots.len() == 1).then(|| active.slots[0].clone());
        let buffer = if active.slots.is_empty() {
            None
        } else {
            let buffer = execution.gpu.device().create_buffer(&BufferDesc {
                size: active.slots.len() as u64 * width,
                memory: Memory::Readback,
                usage: BufferUsage::COPY_DST,
                label: "D3D11 query readback",
            })?;
            for (index, slot) in active.slots.into_iter().enumerate() {
                execution
                    .gpu
                    .read_query(slot, buffer.clone(), index as u64 * width)?;
            }
            Some(buffer)
        };
        Pending {
            buffer,
            completion: Some(execution.gpu.completion()?),
            words: active.counters,
            last,
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
        if kind == Kind::PipelineStatistics {
            for sample in bytes.as_chunks::<16>().0 {
                result.words[6] += u64::from_ne_bytes(sample[..8].try_into().unwrap());
                result.words[7] += u64::from_ne_bytes(sample[8..].try_into().unwrap());
            }
        } else {
            result.words[0] = bytes
                .as_chunks::<8>()
                .0
                .iter()
                .copied()
                .map(u64::from_ne_bytes)
                .sum();
        }
    }
    if output != 0 {
        if kind == Kind::OcclusionPredicate {
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
    vertices: u64,
    primitives: u64,
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
                if features.contains(adv_gpu::Features::OCCLUSION_QUERIES) =>
            {
                if occlusion.is_none() {
                    occlusion = Some(query_slot(&exec.gpu, QueryKind::Occlusion, 1)?);
                }
                active.slots.extend(occlusion.clone());
            }
            Kind::PipelineStatistics => {
                active.counters[0] += vertices;
                active.counters[1] += primitives;
                active.counters[2] += vertices;
                active.counters[5] += primitives;
                if features.contains(adv_gpu::Features::PIPELINE_STATISTICS_QUERY) {
                    if statistics.is_none() {
                        statistics = Some(query_slot(&exec.gpu, QueryKind::PipelineStatistics, 1)?);
                    }
                    active.slots.extend(statistics.clone());
                }
            }
            Kind::Event
            | Kind::Timestamp
            | Kind::Disjoint
            | Kind::Occlusion
            | Kind::OcclusionPredicate => (),
        }
    }
    Ok((occlusion, statistics))
}

pub(super) fn predicate(env: &Env<'_>, pointer: u32, value: bool) -> Option<Predicate> {
    let query = context::owned(env, pointer)?.query()?;
    Some(Predicate {
        query: query.result.as_ref()?.last.clone()?,
        inverted: !value,
    })
}

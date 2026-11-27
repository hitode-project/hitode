use super::*;
use crate::record::List;
use adv_abi::types::D3D11_BOX;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(in crate::d3d11) struct Recording {
    list: List<Operation>,
    mappings: BTreeMap<u32, Mapping>,
    queries: BTreeSet<u32>,
    open_queries: BTreeSet<u32>,
    pub references: BTreeSet<u32>,
}

enum Operation {
    Call(Command),
    ResetCounters(Vec<(u32, u32)>),
    Write {
        resource: u32,
        subresource: u32,
        bytes: Vec<u8>,
    },
}

struct Mapping {
    storage: map::Storage,
    mapped: Option<u32>,
}

struct Command {
    binding: u32,
    arguments: Box<[u32]>,
    data: Vec<(usize, Vec<u8>)>,
    state: Option<Snapshot>,
}

pub(in crate::d3d11::glue::context) fn record(
    env: &mut Env<'_>,
    method: &str,
) -> Result<(), Error> {
    if method == "flush" {
        return Ok(());
    }
    let function = env.sys.thunks.get(env.thunk).f;
    let arguments = (0..u32::from(function.conv.argc()))
        .map(|i| env.arg(i))
        .collect();
    let mut command = Command {
        binding: env.thunk,
        arguments,
        data: Vec::new(),
        state: None,
    };
    let mut references = BTreeSet::new();
    let objects: &[usize] = match method {
        "copy_region" => {
            command.capture(env, 8, size_of::<D3D11_BOX>() as u32)?;
            &[1, 6]
        }
        "update_resource" => {
            command.capture_update(env)?;
            &[1]
        }
        "copy_resource" => &[1, 2],
        "resolve" | "copy_structure_count" => &[1, 3],
        "clear_color" | "clear_uav_uint" | "clear_uav_float" => {
            command.capture(env, 2, 16)?;
            &[1]
        }
        "clear_view" => {
            command.capture(env, 2, 16)?;
            let bytes = command.arguments[4]
                .checked_mul(size_of::<RECT>() as u32)
                .ok_or(Error::InvalidArgument)?;
            command.capture(env, 3, bytes)?;
            &[1]
        }
        "clear_depth" | "generate_mips" | "begin_query" | "end_query" | "set_min_lod" => &[1],
        "draw_indirect" | "draw_indexed_indirect" | "dispatch_indirect" => &[1],
        _ => &[],
    };
    for &slot in objects {
        let pointer = command.arguments[slot];
        owned(env, pointer).ok_or(Error::InvalidArgument)?;
        references.insert(pointer);
    }
    if matches!(method, "begin_query" | "end_query") {
        let pointer = command.arguments[1];
        let query = owned(env, pointer)
            .and_then(Object::query)
            .ok_or(Error::InvalidArgument)?;
        let begin = method == "begin_query";
        let range = !matches!(
            query.kind,
            crate::d3d11::object::query::Kind::Event | crate::d3d11::object::query::Kind::Timestamp
        );
        let recording = recording_mut(env);
        recording.queries.insert(pointer);
        if begin && range {
            recording.open_queries.insert(pointer);
        } else if !begin {
            recording.open_queries.remove(&pointer);
        }
    }
    if method.starts_with("draw") || method.starts_with("dispatch") {
        command.state = Some(state(env)?.snapshot());
    }
    retain(env, references)?;
    recording_mut(env).list.push(Operation::Call(command));
    Ok(())
}

impl Command {
    fn capture(&mut self, env: &Env<'_>, slot: usize, bytes: u32) -> Result<(), Error> {
        let pointer = self.arguments[slot];
        if pointer != 0 && bytes != 0 {
            self.data.push((slot, array(env, pointer, bytes)?));
        }
        Ok(())
    }

    fn capture_update(&mut self, env: &Env<'_>) -> Result<(), Error> {
        let resource = owned(env, self.arguments[1])
            .and_then(Object::resource)
            .ok_or(Error::InvalidArgument)?;
        let subresource = self.arguments[2];
        if subresource >= resource.subresources() {
            return Err(Error::InvalidArgument);
        }
        let extent = if self.arguments[3] != 0 {
            let area: D3D11_BOX = read(env, self.arguments[3])?;
            self.capture(env, 3, size_of::<D3D11_BOX>() as u32)?;
            [
                area.right.saturating_sub(area.left),
                area.bottom.saturating_sub(area.top),
                area.back.saturating_sub(area.front),
            ]
        } else {
            match resource {
                Resource::Buffer(buffer) => [buffer.size(), 1, 1],
                Resource::Texture(texture) => texture.extent(subresource % texture.levels()),
            }
        };
        if extent.contains(&0) {
            return Ok(());
        }
        let bytes = match resource {
            Resource::Buffer(_) => extent[0],
            Resource::Texture(texture) => {
                let format = texture.format();
                let row = u64::from(extent[0].div_ceil(u32::from(format.alignment[0])))
                    * u64::from(format.aligned_unit_bytes);
                let rows = extent[1].div_ceil(u32::from(format.alignment[1]));
                let bytes = row
                    + u64::from(rows - 1) * u64::from(self.arguments[5])
                    + u64::from(extent[2] - 1) * u64::from(self.arguments[6]);
                u32::try_from(bytes).map_err(|_| Error::InvalidArgument)?
            }
        };
        self.capture(env, 4, bytes)
    }

    fn run(&self, env: &mut Env<'_>, context: u32) -> Result<(), Error> {
        let mut arguments = self.arguments.to_vec();
        arguments[0] = context;
        let scratch_size: usize = self
            .data
            .iter()
            .map(|(_, bytes)| bytes.len().next_multiple_of(4))
            .sum();
        let scratch = if scratch_size == 0 {
            0
        } else {
            let heaps = env.ext.get_mut::<adv_os::Heaps>();
            heaps
                .alloc(
                    env.cpu,
                    &mut env.sys.vm,
                    heaps.process_heap,
                    u32::try_from(scratch_size).map_err(|_| ObjectError::Exhausted)?,
                    false,
                )
                .ok_or(ObjectError::Exhausted)?
        };
        let result = (|| {
            let mut address = scratch;
            for (slot, bytes) in &self.data {
                env.cpu
                    .write(address, bytes)
                    .map_err(|_| Error::InvalidArgument)?;
                arguments[*slot] = address;
                address += bytes.len().next_multiple_of(4) as u32;
            }
            let previous = self.state.as_ref().map(|snapshot| {
                state_mut(env)
                    .ok()
                    .unwrap()
                    .replace_groups(snapshot.clone())
            });
            let result = env.invoke_host_u32(self.binding, arguments, |_, result| {
                Outcome::Return(result.value())
            });
            if let Some(previous) = previous {
                state_mut(env).ok().unwrap().replace_groups(previous);
            }
            match result {
                Outcome::Return(_) => Ok(()),
                _ => Err(Error::InvalidArgument),
            }
        })();
        if scratch != 0 {
            free(env, scratch);
        }
        result
    }
}

fn recording_mut<'a>(env: &'a mut Env<'_>) -> &'a mut Recording {
    let context = env.arg(0);
    table(env)
        .get_mut(&mut env.sys.objects, context)
        .unwrap()
        .context_data_mut()
        .unwrap()
        .deferred
        .as_mut()
        .map(|list| &mut list.recording)
        .unwrap()
}

fn retain(env: &mut Env<'_>, references: BTreeSet<u32>) -> Result<(), Error> {
    for pointer in references {
        retain_binding(env, pointer)?;
    }
    Ok(())
}

pub(in crate::d3d11::glue::context) fn retain_binding(
    env: &mut Env<'_>,
    pointer: u32,
) -> Result<(), Error> {
    if recording_mut(env).references.insert(pointer) {
        table(env).retain_bound(&mut env.sys.objects, pointer)?;
    }
    Ok(())
}

pub(super) fn retain_current_bindings(env: &mut Env<'_>) -> Result<(), Error> {
    let mut references = BTreeSet::new();
    groups(env)?.for_each_binding(|pointer| {
        references.insert(pointer);
    });
    retain(env, references)
}

pub(in crate::d3d11::glue::context) fn replay(
    env: &mut Env<'_>,
    pointer: u32,
) -> Result<(), Error> {
    let list = owned(env, pointer).unwrap().command_list().unwrap();
    if current(env).is_some() {
        let commands = list.recording.list.clone();
        let references = list.recording.references.clone();
        let queries = list.recording.queries.clone();
        if !recording_mut(env).open_queries.is_disjoint(&queries) {
            return Ok(());
        }
        retain(env, references)?;
        let recording = recording_mut(env);
        recording.queries.extend(queries);
        recording.list.splice(&commands);
    } else {
        if list.recording.queries.iter().any(|&pointer| {
            owned(env, pointer)
                .and_then(Object::query)
                .is_some_and(|query| query.active.is_some())
        }) {
            return Ok(());
        }
        let commands = list.recording.list.clone();
        let context = env.arg(0);
        for operation in commands.iter() {
            match operation {
                Operation::Call(command) => command.run(env, context)?,
                Operation::ResetCounters(counters) => {
                    gpu::reset_counter_values(env, counters.iter().copied())?
                }
                Operation::Write {
                    resource,
                    subresource,
                    bytes,
                } => {
                    gpu::write_dynamic(env, *resource, *subresource, bytes)?;
                }
            }
        }
    }
    Ok(())
}

pub(in crate::d3d11::glue::context) fn record_counters(
    env: &mut Env<'_>,
    pointers: &[u32],
    initial: u32,
) -> Result<(), Error> {
    let values = array::<u32>(env, initial, pointers.len() as u32)?;
    let counters: Vec<_> = pointers
        .iter()
        .copied()
        .zip(values)
        .filter(|&(pointer, value)| {
            value != u32::MAX
                && owned(env, pointer)
                    .and_then(Object::view)
                    .is_some_and(|(_, view)| view.has_counter())
        })
        .collect();
    if !counters.is_empty() {
        let references = counters.iter().map(|(pointer, _)| *pointer).collect();
        retain(env, references)?;
        recording_mut(env)
            .list
            .push(Operation::ResetCounters(counters));
    }
    Ok(())
}

impl Recording {
    pub(in crate::d3d11) fn release_mappings(&mut self) -> impl Iterator<Item = u32> + use<> {
        std::mem::take(&mut self.mappings)
            .into_values()
            .map(|mapping| mapping.storage.pointer())
    }
}

pub(in crate::d3d11::glue::context) fn map_resource(
    env: &mut Env<'_>,
    pointer: u32,
    subresource: u32,
    mode: u32,
    flags: u32,
) -> Result<D3D11_MAPPED_SUBRESOURCE, Error> {
    let resource = owned(env, pointer)
        .and_then(Object::resource)
        .ok_or(Error::InvalidArgument)?;
    if !matches!(
        map::Map::admit(resource, subresource, mode, flags)?,
        map::Map::Dynamic
    ) {
        return Err(Error::InvalidArgument);
    }
    if !recording_mut(env).mappings.contains_key(&pointer) {
        if mode != 4 {
            return Err(Error::InitialDiscardRequired);
        }
        let layouts = map::Layouts::new(resource, NonZeroU32::new(128).unwrap())
            .ok_or(ObjectError::Exhausted)?;
        let heaps = env.ext.get_mut::<adv_os::Heaps>();
        let address = heaps
            .alloc(
                env.cpu,
                &mut env.sys.vm,
                heaps.process_heap,
                layouts.size(),
                true,
            )
            .ok_or(ObjectError::Exhausted)?;
        retain(env, [pointer].into())?;
        recording_mut(env).mappings.insert(
            pointer,
            Mapping {
                storage: layouts.attach(address),
                mapped: None,
            },
        );
    }
    let mapping = recording_mut(env).mappings.get_mut(&pointer).unwrap();
    let mapped = mapping
        .storage
        .map(subresource, mode, flags, false, |_, _, _| Ok(()))?;
    mapping.mapped = Some(subresource);
    Ok(mapped)
}

pub(in crate::d3d11::glue::context) fn unmap_resource(
    env: &mut Env<'_>,
    pointer: u32,
    subresource: u32,
) -> Result<(), Error> {
    let resource = owned(env, pointer)
        .and_then(Object::resource)
        .ok_or(Error::InvalidArgument)?;
    let Some(mapping) = recording_mut(env).mappings.get_mut(&pointer) else {
        return Ok(());
    };
    if mapping.mapped != Some(subresource) {
        return Ok(());
    }
    mapping.mapped = None;
    let layout = mapping.storage.layout(subresource);
    let base = mapping.storage.pointer();
    let depth = match resource {
        Resource::Buffer(_) => 1,
        Resource::Texture(texture) => texture.extent(subresource % texture.levels())[2],
    };
    let bytes = array(env, layout.slice(base, 0), layout.depth_pitch() * depth)?;
    recording_mut(env).list.push(Operation::Write {
        resource: pointer,
        subresource,
        bytes,
    });
    Ok(())
}

pub(super) fn finish(env: &mut Env<'_>) -> Result<(), Error> {
    let pending: Vec<_> = recording_mut(env)
        .mappings
        .iter()
        .filter_map(|(&pointer, mapping)| mapping.mapped.map(|subresource| (pointer, subresource)))
        .collect();
    for (pointer, subresource) in pending {
        unmap_resource(env, pointer, subresource)?;
    }
    let queries = std::mem::take(&mut recording_mut(env).open_queries);
    if !queries.is_empty() {
        let binding = env
            .sys
            .thunks
            .published_index(DLL, "ID3D11DeviceContext2::End")
            .unwrap();
        for pointer in queries {
            recording_mut(env).list.push(Operation::Call(Command {
                binding,
                arguments: Box::new([0, pointer]),
                data: Vec::new(),
                state: None,
            }));
        }
    }
    let mappings = recording_mut(env).release_mappings();
    for pointer in mappings {
        free(env, pointer);
    }
    Ok(())
}

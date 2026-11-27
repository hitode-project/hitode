//! Context state and guest array marshaling.
use super::*;
use crate::d3d11::{
    desc::Bind,
    state::{
        BoundView, CONSTANTS, ConstantBuffer, ContextState, IndexBuffer, RENDER_TARGETS, SAMPLERS,
        SRVS, STREAM_OUTPUTS, STREAMS, Snapshot, Stage, StreamOutput, UAVS, VIEWPORTS,
        VertexBuffer, ViewSlot,
    },
};
use std::ops::Range;

pub(in crate::d3d11) mod deferred;
use deferred::{execute_list, finish_list, get_context_flags, get_type, swap_state};

macro_rules! methods {
    ($($method:ident => $handler:ident $level:ident),+ $(,)?) => {
        ("ID3D11DeviceContext1", &[
            $((concat!("ID3D11DeviceContext1::", stringify!($method)), $handler, ImplementationLevel::$level)),+
        ])
    };
}

pub(super) const FACE: (&str, &[Slot]) = methods!(
    QueryInterface => query Implement,
    AddRef => add_ref Implement,
    Release => release Implement,
    GetDevice => get_device Implement,
    GetPrivateData => get_private Implement,
    SetPrivateData => set_private Implement,
    SetPrivateDataInterface => set_interface Implement,
    VSSetConstantBuffers => vs_set_constants State,
    PSSetShaderResources => ps_set_srvs State,
    PSSetShader => ps_set_shader State,
    PSSetSamplers => ps_set_samplers State,
    VSSetShader => vs_set_shader State,
    DrawIndexed => draw_indexed Implement,
    Draw => draw Implement,
    Map => map_resource Implement,
    Unmap => unmap_resource Implement,
    PSSetConstantBuffers => ps_set_constants State,
    IASetInputLayout => ia_set_layout State,
    IASetVertexBuffers => ia_set_vertices State,
    IASetIndexBuffer => ia_set_index State,
    DrawIndexedInstanced => draw_indexed_instanced Implement,
    DrawInstanced => draw_instanced Implement,
    GSSetConstantBuffers => gs_set_constants State,
    GSSetShader => gs_set_shader State,
    IASetPrimitiveTopology => ia_set_topology State,
    VSSetShaderResources => vs_set_srvs State,
    VSSetSamplers => vs_set_samplers State,
    Begin => begin_query Implement,
    End => end_query Implement,
    GetData => get_data Implement,
    SetPredication => set_predication State,
    GSSetShaderResources => gs_set_srvs State,
    GSSetSamplers => gs_set_samplers State,
    OMSetRenderTargets => om_set_targets State,
    OMSetRenderTargetsAndUnorderedAccessViews => om_set_uavs State,
    OMSetBlendState => om_set_blend State,
    OMSetDepthStencilState => om_set_depth State,
    SOSetTargets => so_set_targets State,
    DrawAuto => draw_auto Implement,
    DrawIndexedInstancedIndirect => draw_indexed_indirect Implement,
    DrawInstancedIndirect => draw_indirect Implement,
    Dispatch => dispatch Implement,
    DispatchIndirect => dispatch_indirect Implement,
    RSSetState => rs_set_state State,
    RSSetViewports => rs_set_viewports State,
    RSSetScissorRects => rs_set_scissors State,
    CopySubresourceRegion => copy_region Implement,
    CopyResource => copy_resource Implement,
    UpdateSubresource => update_resource Implement,
    CopyStructureCount => copy_structure_count Implement,
    ClearRenderTargetView => clear_color Implement,
    ClearUnorderedAccessViewUint => clear_uav_uint Implement,
    ClearUnorderedAccessViewFloat => clear_uav_float Implement,
    ClearDepthStencilView => clear_depth Implement,
    GenerateMips => generate_mips Implement,
    SetResourceMinLOD => set_min_lod State,
    GetResourceMinLOD => min_lod State,
    ResolveSubresource => resolve Implement,
    ExecuteCommandList => execute_list Implement,
    HSSetShaderResources => hs_set_srvs State,
    HSSetShader => hs_set_shader State,
    HSSetSamplers => hs_set_samplers State,
    HSSetConstantBuffers => hs_set_constants State,
    DSSetShaderResources => ds_set_srvs State,
    DSSetShader => ds_set_shader State,
    DSSetSamplers => ds_set_samplers State,
    DSSetConstantBuffers => ds_set_constants State,
    CSSetShaderResources => cs_set_srvs State,
    CSSetUnorderedAccessViews => cs_set_uavs State,
    CSSetShader => cs_set_shader State,
    CSSetSamplers => cs_set_samplers State,
    CSSetConstantBuffers => cs_set_constants State,
    VSGetConstantBuffers => vs_get_constants State,
    PSGetShaderResources => ps_get_srvs State,
    PSGetShader => ps_get_shader State,
    PSGetSamplers => ps_get_samplers State,
    VSGetShader => vs_get_shader State,
    PSGetConstantBuffers => ps_get_constants State,
    IAGetInputLayout => ia_get_layout State,
    IAGetVertexBuffers => ia_get_vertices State,
    IAGetIndexBuffer => ia_get_index State,
    GSGetConstantBuffers => gs_get_constants State,
    GSGetShader => gs_get_shader State,
    IAGetPrimitiveTopology => ia_get_topology State,
    VSGetShaderResources => vs_get_srvs State,
    VSGetSamplers => vs_get_samplers State,
    GetPredication => get_predication State,
    GSGetShaderResources => gs_get_srvs State,
    GSGetSamplers => gs_get_samplers State,
    OMGetRenderTargets => om_get_targets State,
    OMGetRenderTargetsAndUnorderedAccessViews => om_get_uavs State,
    OMGetBlendState => om_get_blend State,
    OMGetDepthStencilState => om_get_depth State,
    SOGetTargets => so_get_targets State,
    RSGetState => rs_get_state State,
    RSGetViewports => rs_get_viewports State,
    RSGetScissorRects => rs_get_scissors State,
    HSGetShaderResources => hs_get_srvs State,
    HSGetShader => hs_get_shader State,
    HSGetSamplers => hs_get_samplers State,
    HSGetConstantBuffers => hs_get_constants State,
    DSGetShaderResources => ds_get_srvs State,
    DSGetShader => ds_get_shader State,
    DSGetSamplers => ds_get_samplers State,
    DSGetConstantBuffers => ds_get_constants State,
    CSGetShaderResources => cs_get_srvs State,
    CSGetUnorderedAccessViews => cs_get_uavs State,
    CSGetShader => cs_get_shader State,
    CSGetSamplers => cs_get_samplers State,
    CSGetConstantBuffers => cs_get_constants State,
    ClearState => clear State,
    Flush => flush Implement,
    GetType => get_type State,
    GetContextFlags => get_context_flags State,
    FinishCommandList => finish_list Implement,
    CopySubresourceRegion1 => copy_region Implement,
    UpdateSubresource1 => update_resource Implement,
    DiscardResource => discard State,
    DiscardView => discard State,
    VSSetConstantBuffers1 => vs_set_constants1 State,
    HSSetConstantBuffers1 => hs_set_constants1 State,
    DSSetConstantBuffers1 => ds_set_constants1 State,
    GSSetConstantBuffers1 => gs_set_constants1 State,
    PSSetConstantBuffers1 => ps_set_constants1 State,
    CSSetConstantBuffers1 => cs_set_constants1 State,
    VSGetConstantBuffers1 => vs_get_constants1 State,
    HSGetConstantBuffers1 => hs_get_constants1 State,
    DSGetConstantBuffers1 => ds_get_constants1 State,
    GSGetConstantBuffers1 => gs_get_constants1 State,
    PSGetConstantBuffers1 => ps_get_constants1 State,
    CSGetConstantBuffers1 => cs_get_constants1 State,
    SwapDeviceContextState => swap_state Implement,
    ClearView => clear_view Implement,
    DiscardView1 => discard State,
);

macro_rules! stage_methods {
    ($stage:ident, $($name:ident => $method:ident),+ $(,)?) => {
        $(fn $name(env: &mut Env<'_>) -> Outcome { $method(env, Stage::$stage) })+
    };
}
stage_methods!(Vertex,
    vs_set_shader => set_shader,
    vs_get_shader => get_shader,
    vs_set_constants => set_constants,
    vs_get_constants => get_constants,
    vs_set_srvs => set_srvs,
    vs_get_srvs => get_srvs,
    vs_set_samplers => set_samplers,
    vs_get_samplers => get_samplers,
    vs_set_constants1 => set_constants1,
    vs_get_constants1 => get_constants1,
);
stage_methods!(Pixel,
    ps_set_shader => set_shader,
    ps_get_shader => get_shader,
    ps_set_constants => set_constants,
    ps_get_constants => get_constants,
    ps_set_srvs => set_srvs,
    ps_get_srvs => get_srvs,
    ps_set_samplers => set_samplers,
    ps_get_samplers => get_samplers,
    ps_set_constants1 => set_constants1,
    ps_get_constants1 => get_constants1,
);
stage_methods!(Geometry,
    gs_set_shader => set_shader,
    gs_get_shader => get_shader,
    gs_set_constants => set_constants,
    gs_get_constants => get_constants,
    gs_set_srvs => set_srvs,
    gs_get_srvs => get_srvs,
    gs_set_samplers => set_samplers,
    gs_get_samplers => get_samplers,
    gs_set_constants1 => set_constants1,
    gs_get_constants1 => get_constants1,
);
stage_methods!(Hull,
    hs_set_shader => set_shader,
    hs_get_shader => get_shader,
    hs_set_constants => set_constants,
    hs_get_constants => get_constants,
    hs_set_srvs => set_srvs,
    hs_get_srvs => get_srvs,
    hs_set_samplers => set_samplers,
    hs_get_samplers => get_samplers,
    hs_set_constants1 => set_constants1,
    hs_get_constants1 => get_constants1,
);
stage_methods!(Domain,
    ds_set_shader => set_shader,
    ds_get_shader => get_shader,
    ds_set_constants => set_constants,
    ds_get_constants => get_constants,
    ds_set_srvs => set_srvs,
    ds_get_srvs => get_srvs,
    ds_set_samplers => set_samplers,
    ds_get_samplers => get_samplers,
    ds_set_constants1 => set_constants1,
    ds_get_constants1 => get_constants1,
);
stage_methods!(Compute,
    cs_set_shader => set_shader,
    cs_get_shader => get_shader,
    cs_set_constants => set_constants,
    cs_get_constants => get_constants,
    cs_set_srvs => set_srvs,
    cs_get_srvs => get_srvs,
    cs_set_samplers => set_samplers,
    cs_get_samplers => get_samplers,
    cs_set_constants1 => set_constants1,
    cs_get_constants1 => get_constants1,
);

pub(super) use deferred::list_flags;

fn state<'a>(env: &'a Env<'_>) -> Result<&'a ContextState, Error> {
    ops::object(env, table(env), env.arg(0))?
        .context()
        .ok_or(Error::InvalidArgument)
}

pub(super) fn groups<'a>(env: &'a Env<'_>) -> Result<&'a Snapshot, Error> {
    Ok(state(env)?.groups())
}

fn state_mut<'a>(env: &'a mut Env<'_>) -> Result<&'a mut ContextState, Error> {
    let context = env.arg(0);
    ops::object_mut(env, table(env), context)?
        .context_mut()
        .ok_or(Error::InvalidArgument)
}

pub(super) fn owner(env: &Env<'_>) -> Result<u32, Error> {
    ops::object(env, table(env), env.arg(0))?
        .device()
        .ok_or(Error::InvalidArgument)
}

pub(super) fn report<'a>(env: &'a Env<'_>) -> Result<&'a crate::d3d11::caps::Report, Error> {
    let level = ops::object(env, table(env), env.arg(0))?
        .context_data()
        .and_then(|context| context.level)
        .ok_or(Error::InvalidArgument)?;
    let reports = &env.ext.get::<State>().devices.reports;
    reports[level.column()]
        .as_deref()
        .map_or_else(|| device::report(env, owner(env)?), Ok)
}

pub(super) fn owned<'a>(env: &'a Env<'_>, pointer: u32) -> Option<&'a Object> {
    let object = table(env).get(&env.sys.objects, pointer)?;
    (object.device() == Some(owner(env).ok()?)).then_some(object)
}

fn object_pointer(env: &Env<'_>, pointer: u32, valid: impl FnOnce(&Object) -> bool) -> u32 {
    owned(env, pointer)
        .filter(|object| valid(object))
        .and_then(|_| table(env).canonical(&env.sys.objects, pointer))
        .unwrap_or(0)
}

fn buffer_pointer(env: &Env<'_>, pointer: u32, bind: u32) -> u32 {
    object_pointer(env, pointer, |object| {
        matches!(object.resource(), Some(Resource::Buffer(_)))
            && object.resource().unwrap().bind().contains(bind)
    })
}

fn range(first: u32, count: u32, limit: usize) -> Result<Range<usize>, Error> {
    let end = first
        .checked_add(count)
        .filter(|&end| end as usize <= limit)
        .ok_or(Error::InvalidArgument)?;
    Ok(first as usize..end as usize)
}

pub(super) fn output_array<T: Pod>(
    env: &mut Env<'_>,
    address: u32,
    values: &[T],
) -> Result<(), Error> {
    if address != 0 {
        env.cpu
            .write(address, bytemuck::cast_slice(values))
            .map_err(|_| Error::InvalidArgument)?;
    }
    Ok(())
}

pub(super) fn optional<T: Pod>(env: &mut Env<'_>, address: u32, value: T) -> Result<(), Error> {
    if address != 0 {
        write(env, address, value)?;
    }
    Ok(())
}

fn reference_array(env: &mut Env<'_>, address: u32, pointers: &[u32]) -> Result<(), Error> {
    if address != 0 {
        let table = table(env);
        output_array(env, address, pointers)?;
        for &pointer in pointers {
            if pointer != 0 {
                table.add_ref(&mut env.sys.objects, pointer)?;
            }
        }
    }
    Ok(())
}

fn replace(
    env: &mut Env<'_>,
    changes: impl IntoIterator<Item = (u32, u32)>,
    apply: impl FnOnce(&mut ContextState),
) -> Outcome {
    let table = table(env);
    let mut released = Release::default();
    let result = (|| {
        state(env)?;
        for (old, new) in changes {
            if old != new {
                if new != 0 {
                    table.retain_bound(&mut env.sys.objects, new)?;
                    #[cfg(feature = "gpu")]
                    if deferred::current(env).is_some() {
                        deferred::retain_binding(env, new)?;
                    }
                }
                table.drop_binding(&mut env.sys.objects, old, &mut released);
            }
        }
        apply(state_mut(env)?);
        Ok(())
    })();
    released.references = result.map_or_else(Error::hresult, |()| 0);
    retire(env, released)
}

fn clear(env: &mut Env<'_>) -> Outcome {
    let mut released = Release::default();
    released.references = clear_bindings(env, &mut released).map_or_else(Error::hresult, |()| 0);
    retire(env, released)
}

fn clear_bindings(env: &mut Env<'_>, released: &mut Release) -> Result<(), Error> {
    let table = table(env);
    let old = state_mut(env)?.clear();
    old.for_each_binding(|pointer| table.drop_binding(&mut env.sys.objects, pointer, released));
    Ok(())
}

fn ia_set_layout(env: &mut Env<'_>) -> Outcome {
    let new = object_pointer(env, env.arg(1), |object| object.input_layout().is_some());
    match groups(env).map(|state| state.ia.layout) {
        Ok(old) => replace(env, [(old, new)], |state| state.set_input_layout(new)),
        Err(error) => Outcome::Return(error.hresult()),
    }
}

fn ia_get_layout(env: &mut Env<'_>) -> Outcome {
    let out = env.arg(1);
    call((|| {
        let layout = groups(env)?.ia.layout;
        reference_array(env, out, &[layout])
    })())
}

fn ia_set_vertices(env: &mut Env<'_>) -> Outcome {
    let [first, count, buffers, strides, offsets] =
        std::array::from_fn(|index| env.arg(index as u32 + 1));
    let result = (|| {
        let slots = range(first, count, STREAMS)?;
        let buffers = array::<u32>(env, buffers, count)?;
        let strides = array::<u32>(env, strides, count)?;
        let offsets = array::<u32>(env, offsets, count)?;
        let output = groups(env)?.so.buffers;
        let vertices: Vec<_> = buffers
            .into_iter()
            .zip(strides)
            .zip(offsets)
            .map(|((buffer, stride), offset)| {
                if output.contains(&buffer) {
                    VertexBuffer::default()
                } else {
                    VertexBuffer {
                        buffer: buffer_pointer(env, buffer, Bind::VERTEX),
                        stride,
                        offset,
                    }
                }
            })
            .collect();
        let old = groups(env)?.ia.clone();
        Ok((slots, old, vertices))
    })();
    match result {
        Ok((slots, old, vertices)) => replace(
            env,
            old.vertices[slots.clone()]
                .iter()
                .zip(&vertices)
                .map(|(old, new)| (old.buffer, new.buffer)),
            |state| state.set_vertices(slots.start, &vertices),
        ),
        Err(error) => Outcome::Return(Error::hresult(error)),
    }
}

fn ia_get_vertices(env: &mut Env<'_>) -> Outcome {
    let [first, count, buffers, strides, offsets] =
        std::array::from_fn(|index| env.arg(index as u32 + 1));
    call((|| {
        let slots = range(first, count, STREAMS)?;
        let vertices = &groups(env)?.ia.vertices[slots];
        let refs: Vec<_> = vertices.iter().map(|entry| entry.buffer).collect();
        let strides_data: Vec<_> = vertices.iter().map(|entry| entry.stride).collect();
        let offsets_data: Vec<_> = vertices.iter().map(|entry| entry.offset).collect();
        reference_array(env, buffers, &refs)?;
        output_array(env, strides, &strides_data)?;
        output_array(env, offsets, &offsets_data)
    })())
}

fn ia_set_index(env: &mut Env<'_>) -> Outcome {
    let format = env.arg(2);
    let old = match groups(env) {
        Ok(groups) => groups.ia.indices.buffer,
        Err(error) => return Outcome::Return(error.hresult()),
    };
    if !matches!(format, 0 | 42 | 57) {
        return Outcome::Return(0);
    }
    let new = IndexBuffer {
        buffer: buffer_pointer(env, env.arg(1), Bind::INDEX),
        format,
        offset: env.arg(3),
    };
    replace(env, [(old, new.buffer)], |state| state.set_indices(new))
}

fn ia_get_index(env: &mut Env<'_>) -> Outcome {
    let (buffer, format, offset) = (env.arg(1), env.arg(2), env.arg(3));
    call((|| {
        let index = groups(env)?.ia.indices;
        reference_array(env, buffer, &[index.buffer])?;
        optional(env, format, index.format)?;
        optional(env, offset, index.offset)
    })())
}

fn ia_set_topology(env: &mut Env<'_>) -> Outcome {
    let topology = env.arg(1);
    call(state_mut(env).map(|state| {
        if matches!(topology, 0..=5 | 10..=13 | 33..=64) {
            state.set_topology(topology);
        }
    }))
}

fn ia_get_topology(env: &mut Env<'_>) -> Outcome {
    let out = env.arg(1);
    call((|| {
        let topology = groups(env)?.ia.topology;
        write(env, out, topology)
    })())
}

fn set_shader(env: &mut Env<'_>, stage: Stage) -> Outcome {
    use crate::shader::ir::Stage as ShaderStage;
    let result = (|| {
        let old = groups(env)?.stages[stage as usize].clone();
        let new = object_pointer(env, env.arg(1), |object| {
            object.shader().is_some_and(|shader| {
                matches!(
                    (stage, shader.stage),
                    (Stage::Vertex, ShaderStage::Vertex)
                        | (Stage::Geometry, ShaderStage::Geometry)
                        | (Stage::Hull, ShaderStage::Hull)
                        | (Stage::Domain, ShaderStage::Domain)
                        | (Stage::Pixel, ShaderStage::Pixel)
                        | (Stage::Compute, ShaderStage::Compute)
                )
            })
        });
        if env.arg(3) > 256 {
            return Err(Error::InvalidArgument);
        }
        let classes = if new == 0 {
            Vec::new()
        } else {
            array::<u32>(env, env.arg(2), env.arg(3))?
        };
        if old.shader == new && old.classes == classes {
            return Ok(None);
        }
        let specialized = if let Some(object) = owned(env, new) {
            let shader = object.shader().unwrap();
            if let Some(class_tables) = &shader.linkage {
                let linkage = object.shader_linkage().unwrap();
                let bindings = classes
                    .iter()
                    .map(|&pointer| {
                        let instance = owned(env, pointer)
                            .and_then(Object::class_instance)
                            .filter(|instance| instance.linkage == linkage)
                            .ok_or(Error::InvalidArgument)?;
                        let desc = instance.desc;
                        if desc.Created != 0 {
                            return Ok((
                                instance.class.as_slice(),
                                [
                                    desc.ConstantBuffer,
                                    desc.BaseConstantBufferOffset,
                                    desc.BaseTexture,
                                    desc.BaseSampler,
                                ],
                            ));
                        }
                        let index = desc.InstanceIndex;
                        let named = class_tables
                            .instances
                            .iter()
                            .find(|named| {
                                named.name == instance.name && index < u32::from(named.count)
                            })
                            .ok_or(Error::InvalidArgument)?;
                        let class = class_tables
                            .class(named.class)
                            .ok_or(Error::InvalidArgument)?;
                        let [cb, vector, texture, sampler] = named.offsets;
                        let texture = if class.textures == 0 {
                            127
                        } else {
                            texture + index * u32::from(class.textures)
                        };
                        let sampler = if class.samplers == 0 {
                            15
                        } else {
                            sampler + index * u32::from(class.samplers)
                        };
                        Ok((
                            class.name.as_slice(),
                            [
                                cb,
                                vector + index * u32::from(class.constants),
                                texture,
                                sampler,
                            ],
                        ))
                    })
                    .collect::<Result<Vec<_>, Error>>()?;
                Some(std::sync::Arc::new(shader.specialize(&bindings)?))
            } else {
                if !classes.is_empty() {
                    return Err(Error::InvalidArgument);
                }
                None
            }
        } else {
            None
        };
        Ok(Some((old, new, classes, specialized)))
    })();
    match result {
        Ok(Some((old, new, classes, specialized))) => {
            let changed = std::iter::once((old.shader, new))
                .chain((0..old.classes.len().max(classes.len())).map(|index| {
                    (
                        old.classes.get(index).copied().unwrap_or(0),
                        classes.get(index).copied().unwrap_or(0),
                    )
                }))
                .collect::<smallvec::SmallVec<[_; 1]>>();
            replace(env, changed, |state| {
                state.set_shader(stage, new, classes, specialized)
            })
        }
        Ok(None) => Outcome::Return(0),
        Err(error) => Outcome::Return(error.hresult()),
    }
}

fn get_shader(env: &mut Env<'_>, stage: Stage) -> Outcome {
    let (out, instances, count) = (env.arg(1), env.arg(2), env.arg(3));
    call((|| {
        let stage = groups(env)?.stages[stage as usize].clone();
        reference_array(env, out, &[stage.shader])?;
        if instances != 0 {
            let capacity = read::<u32>(env, count)? as usize;
            reference_array(
                env,
                instances,
                &stage.classes[..stage.classes.len().min(capacity)],
            )?;
        }
        optional(env, count, stage.classes.len() as u32)
    })())
}

fn set_constants(env: &mut Env<'_>, stage: Stage) -> Outcome {
    constants(env, stage, None)
}

fn set_constants1(env: &mut Env<'_>, stage: Stage) -> Outcome {
    constants(env, stage, Some((env.arg(4), env.arg(5))))
}

fn constants(env: &mut Env<'_>, stage: Stage, windows: Option<(u32, u32)>) -> Outcome {
    let (first, count, buffers) = (env.arg(1), env.arg(2), env.arg(3));
    let result = (|| {
        let slots = range(first, count, CONSTANTS)?;
        let pointers = array::<u32>(env, buffers, count)?;
        let (first, sizes) = windows.unwrap_or_default();
        let first = (first != 0)
            .then(|| array::<u32>(env, first, count))
            .transpose()?;
        let sizes = (sizes != 0)
            .then(|| array::<u32>(env, sizes, count))
            .transpose()?;
        let constants: Vec<_> = pointers
            .iter()
            .enumerate()
            .map(|(index, &buffer)| ConstantBuffer {
                buffer: buffer_pointer(env, buffer, Bind::CONSTANT),
                first: first.as_ref().map_or(0, |first| first[index]),
                count: sizes.as_ref().map_or(4096, |sizes| sizes[index]),
            })
            .collect();
        if constants
            .iter()
            .any(|slot| slot.first % 16 != 0 || slot.count % 16 != 0 || slot.count > 4096)
        {
            return Err(Error::InvalidArgument);
        }
        Ok((
            slots,
            groups(env)?.stages[stage as usize].clone(),
            constants,
        ))
    })();
    match result {
        Ok((slots, old, new)) => replace(
            env,
            old.constants[slots.clone()]
                .iter()
                .zip(&new)
                .map(|(old, new)| (old.buffer, new.buffer)),
            |state| state.set_constants(stage, slots.start, &new),
        ),
        Err(error) => Outcome::Return(Error::hresult(error)),
    }
}

fn get_constants(env: &mut Env<'_>, stage: Stage) -> Outcome {
    constant_outputs(env, stage, (0, 0))
}

fn get_constants1(env: &mut Env<'_>, stage: Stage) -> Outcome {
    constant_outputs(env, stage, (env.arg(4), env.arg(5)))
}

fn constant_outputs(env: &mut Env<'_>, stage: Stage, windows: (u32, u32)) -> Outcome {
    let (first, count, buffers) = (env.arg(1), env.arg(2), env.arg(3));
    call((|| {
        let slots = range(first, count, CONSTANTS)?;
        let constants = &groups(env)?.stages[stage as usize].constants[slots];
        let refs: Vec<_> = constants.iter().map(|entry| entry.buffer).collect();
        let first: Vec<_> = constants.iter().map(|entry| entry.first).collect();
        let sizes: Vec<_> = constants.iter().map(|entry| entry.count).collect();
        reference_array(env, buffers, &refs)?;
        output_array(env, windows.0, &first)?;
        output_array(env, windows.1, &sizes)
    })())
}

fn set_samplers(env: &mut Env<'_>, stage: Stage) -> Outcome {
    let (first, count, input) = (env.arg(1), env.arg(2), env.arg(3));
    let result = (|| {
        let slots = range(first, count, SAMPLERS)?;
        let mut pointers = array::<u32>(env, input, count)?;
        for pointer in &mut pointers {
            *pointer = object_pointer(env, *pointer, |object| {
                matches!(object.pipeline(), Some(PipelineState::Sampler(_)))
            });
        }
        Ok((slots, groups(env)?.stages[stage as usize].clone(), pointers))
    })();
    match result {
        Ok((slots, old, new)) => replace(
            env,
            old.samplers[slots.clone()]
                .iter()
                .copied()
                .zip(new.iter().copied()),
            |state| state.set_samplers(stage, slots.start, &new),
        ),
        Err(error) => Outcome::Return(Error::hresult(error)),
    }
}

fn get_samplers(env: &mut Env<'_>, stage: Stage) -> Outcome {
    let (first, count, output) = (env.arg(1), env.arg(2), env.arg(3));
    call((|| {
        let slots = range(first, count, SAMPLERS)?;
        let stage = groups(env)?.stages[stage as usize].clone();
        reference_array(env, output, &stage.samplers[slots])
    })())
}

fn as_view(env: &Env<'_>, pointer: u32) -> Option<BoundView> {
    let (resource, desc) = owned(env, pointer)?.view()?;
    let pointer = table(env).canonical(&env.sys.objects, pointer)?;
    Some(BoundView {
        pointer,
        resource,
        allocation: table(env).allocation_key(&env.sys.objects, resource)?,
        desc: desc.clone(),
    })
}

fn bind_views(env: &mut Env<'_>, binds: impl IntoIterator<Item = (ViewSlot, u32)>) -> Outcome {
    let mut changes = smallvec::SmallVec::<[(u32, u32); 8]>::new();
    let result = (|| {
        for (slot, pointer) in binds {
            let view = as_view(env, pointer);
            state_mut(env)?.bind_view(slot, view, |_, old, now| {
                changes.push((old.map_or(0, |view| view.pointer), now.unwrap_or(0)));
            });
        }
        Ok(())
    })();
    match result {
        Ok(()) => replace(env, changes, |_| ()),
        Err(error) => Outcome::Return(Error::hresult(error)),
    }
}

fn view_inputs<F: Fn(usize) -> Option<ViewSlot>>(
    env: &Env<'_>,
    range: Range<usize>,
    address: u32,
    slot: F,
) -> Result<impl Iterator<Item = (ViewSlot, u32)> + use<F>, Error> {
    let pointers = array::<u32>(env, address, range.len() as u32)?;
    Ok(range.map(move |index| slot(index).unwrap()).zip(pointers))
}

fn view_outputs(
    env: &mut Env<'_>,
    range: Range<usize>,
    output: u32,
    slot: impl Fn(usize) -> Option<ViewSlot>,
) -> Result<(), Error> {
    let groups = groups(env)?;
    let pointers: Vec<_> = range
        .map(|index| {
            groups
                .view(slot(index).unwrap())
                .map_or(0, |view| view.pointer)
        })
        .collect();
    reference_array(env, output, &pointers)
}

fn set_srvs(env: &mut Env<'_>, stage: Stage) -> Outcome {
    let (first, count, pointers) = (env.arg(1), env.arg(2), env.arg(3));
    let result = range(first, count, SRVS).and_then(|slots| {
        view_inputs(env, slots, pointers, move |index| {
            ViewSlot::srv(stage, index)
        })
    });
    match result {
        Ok(binds) => bind_views(env, binds),
        Err(error) => Outcome::Return(error.hresult()),
    }
}

fn get_srvs(env: &mut Env<'_>, stage: Stage) -> Outcome {
    let (first, count, output) = (env.arg(1), env.arg(2), env.arg(3));
    call(
        range(first, count, SRVS).and_then(|slots| {
            view_outputs(env, slots, output, |index| ViewSlot::srv(stage, index))
        }),
    )
}

fn cs_set_uavs(env: &mut Env<'_>) -> Outcome {
    let (first, count, input) = (env.arg(1), env.arg(2), env.arg(3));
    let result = (|| {
        let length = report(env)?
            .level
            .limit(crate::d3d11::caps::Requirement::ComputeUavSlots) as usize;
        let slots = range(first, count, UAVS)?;
        view_inputs(env, slots, input, ViewSlot::cs_uav).map(|binds| {
            binds.enumerate().map(move |(index, (slot, pointer))| {
                (
                    slot,
                    if first as usize + index < length {
                        pointer
                    } else {
                        0
                    },
                )
            })
        })
    })();
    match result {
        Ok(binds) => {
            let binds: smallvec::SmallVec<[_; UAVS]> = binds.collect();
            let values = env.arg(4);
            if let Err(error) =
                reset_counters(env, binds.iter().map(|&(_, pointer)| pointer), values)
            {
                return Outcome::Return(error.hresult());
            }
            bind_views(env, binds)
        }
        Err(error) => Outcome::Return(Error::hresult(error)),
    }
}

fn cs_get_uavs(env: &mut Env<'_>) -> Outcome {
    let (first, count, output) = (env.arg(1), env.arg(2), env.arg(3));
    call(
        range(first, count, UAVS)
            .and_then(|slots| view_outputs(env, slots, output, ViewSlot::cs_uav)),
    )
}

fn om_targets(
    env: &Env<'_>,
    count: u32,
    input: u32,
    depth: u32,
) -> Result<Vec<(ViewSlot, u32)>, Error> {
    let slots = range(0, count, RENDER_TARGETS)?;
    let assigned = view_inputs(env, slots, input, ViewSlot::rtv)?;
    let empty = (count as usize..RENDER_TARGETS).map(|slot| (ViewSlot::rtv(slot).unwrap(), 0));
    Ok(assigned
        .chain(empty)
        .chain([(ViewSlot::DSV, depth)])
        .collect())
}

fn om_set_targets(env: &mut Env<'_>) -> Outcome {
    match om_targets(env, env.arg(1), env.arg(2), env.arg(3)) {
        Ok(binds) => bind_views(env, binds),
        Err(error) => Outcome::Return(error.hresult()),
    }
}

fn om_set_uavs(env: &mut Env<'_>) -> Outcome {
    let [count, targets, depth, first, uavs, pointers] =
        std::array::from_fn(|index| env.arg(index as u32 + 1));
    let result = (|| {
        let mut binds = if count == u32::MAX {
            Vec::new()
        } else {
            om_targets(env, count, targets, depth)?
        };
        let counters = binds.len();
        if uavs != u32::MAX {
            let length = device::report(env, owner(env)?)?
                .level
                .limit(crate::d3d11::caps::Requirement::OutputMergerUavSlots)
                as usize;
            let slots = range(first, uavs, length)?;
            binds.extend(view_inputs(env, slots, pointers, ViewSlot::om_uav)?);
        }
        Ok((binds, counters))
    })();
    match result {
        Ok((binds, counters)) => {
            let values = env.arg(7);
            if let Err(error) = reset_counters(
                env,
                binds[counters..].iter().map(|&(_, pointer)| pointer),
                values,
            ) {
                return Outcome::Return(error.hresult());
            }
            bind_views(env, binds)
        }
        Err(error) => Outcome::Return(Error::hresult(error)),
    }
}

fn om_target_outputs(env: &mut Env<'_>, count: u32, output: u32, depth: u32) -> Result<(), Error> {
    view_outputs(env, range(0, count, RENDER_TARGETS)?, output, ViewSlot::rtv)?;
    let view = groups(env)?
        .view(ViewSlot::DSV)
        .map_or(0, |view| view.pointer);
    reference_array(env, depth, &[view])
}

fn om_get_targets(env: &mut Env<'_>) -> Outcome {
    let (count, output, depth) = (env.arg(1), env.arg(2), env.arg(3));
    call(om_target_outputs(env, count, output, depth))
}

fn om_get_uavs(env: &mut Env<'_>) -> Outcome {
    let [count, output, depth, first, uavs, pointers] =
        std::array::from_fn(|index| env.arg(index as u32 + 1));
    call((|| {
        om_target_outputs(env, count, output, depth)?;
        view_outputs(env, range(first, uavs, UAVS)?, pointers, ViewSlot::om_uav)
    })())
}

fn rs_set_state(env: &mut Env<'_>) -> Outcome {
    let new = object_pointer(env, env.arg(1), |object| {
        matches!(object.pipeline(), Some(PipelineState::Rasterizer(_)))
    });
    match groups(env).map(|state| state.rs.state) {
        Ok(old) => replace(env, [(old, new)], |state| state.set_rasterizer(new)),
        Err(error) => Outcome::Return(error.hresult()),
    }
}

fn rs_get_state(env: &mut Env<'_>) -> Outcome {
    let output = env.arg(1);
    call((|| {
        let state = groups(env)?.rs.state;
        reference_array(env, output, &[state])
    })())
}

fn rs_set_viewports(env: &mut Env<'_>) -> Outcome {
    let (count, input) = (env.arg(1), env.arg(2));
    call((|| {
        range(0, count, VIEWPORTS)?;
        let viewports = array::<D3D11_VIEWPORT>(env, input, count)?;
        state_mut(env)?.set_viewports(&viewports);
        Ok(())
    })())
}

fn rs_set_scissors(env: &mut Env<'_>) -> Outcome {
    let (count, input) = (env.arg(1), env.arg(2));
    call((|| {
        range(0, count, VIEWPORTS)?;
        let scissors = array::<RECT>(env, input, count)?;
        state_mut(env)?.set_scissors(&scissors);
        Ok(())
    })())
}

fn rs_get_viewports(env: &mut Env<'_>) -> Outcome {
    let (count, output) = (env.arg(1), env.arg(2));
    call((|| {
        let state = groups(env)?.rs.clone();
        counted_output(env, count, output, &state.viewports[..state.viewport_count])
    })())
}

fn rs_get_scissors(env: &mut Env<'_>) -> Outcome {
    let (count, output) = (env.arg(1), env.arg(2));
    call((|| {
        let state = groups(env)?.rs.clone();
        counted_output(env, count, output, &state.scissors[..state.scissor_count])
    })())
}

fn counted_output<T: Pod>(
    env: &mut Env<'_>,
    count: u32,
    output: u32,
    values: &[T],
) -> Result<(), Error> {
    if output != 0 {
        let capacity = (read::<u32>(env, count)? as usize).min(VIEWPORTS);
        let mut data = [T::zeroed(); VIEWPORTS];
        let len = values.len().min(capacity);
        data[..len].copy_from_slice(&values[..len]);
        output_array(env, output, &data[..capacity])?;
    }
    write(env, count, values.len() as u32)
}

fn om_set_blend(env: &mut Env<'_>) -> Outcome {
    let (blend, factor, mask) = (env.arg(1), env.arg(2), env.arg(3));
    let result = (|| {
        let new = object_pointer(env, blend, |object| {
            matches!(object.pipeline(), Some(PipelineState::Blend(_)))
        });
        let factor = if factor == 0 {
            [1.; 4]
        } else {
            read(env, factor)?
        };
        Ok((groups(env)?.om.blend, new, factor))
    })();
    match result {
        Ok((old, new, factor)) => replace(env, [(old, new)], |state| {
            state.set_blend(new, factor, mask)
        }),
        Err(error) => Outcome::Return(Error::hresult(error)),
    }
}

fn om_get_blend(env: &mut Env<'_>) -> Outcome {
    let (output, factor, mask) = (env.arg(1), env.arg(2), env.arg(3));
    call((|| {
        let om = groups(env)?.om.clone();
        reference_array(env, output, &[om.blend])?;
        optional(env, factor, om.blend_factor)?;
        optional(env, mask, om.sample_mask)
    })())
}

fn om_set_depth(env: &mut Env<'_>) -> Outcome {
    let (depth, reference) = (env.arg(1), env.arg(2));
    let new = object_pointer(env, depth, |object| {
        matches!(object.pipeline(), Some(PipelineState::DepthStencil(_)))
    });
    match groups(env).map(|state| state.om.depth_stencil) {
        Ok(old) => replace(env, [(old, new)], |state| {
            state.set_depth_stencil(new, reference)
        }),
        Err(error) => Outcome::Return(error.hresult()),
    }
}

fn om_get_depth(env: &mut Env<'_>) -> Outcome {
    let (output, reference) = (env.arg(1), env.arg(2));
    call((|| {
        let om = groups(env)?.om.clone();
        reference_array(env, output, &[om.depth_stencil])?;
        optional(env, reference, om.stencil_reference)
    })())
}

fn set_predication(env: &mut Env<'_>) -> Outcome {
    let value = env.arg(2) != 0;
    let predicate = object_pointer(env, env.arg(1), |object| {
        object.query().is_some_and(|query| query.predicate)
    });
    match groups(env).map(|state| state.predication.predicate) {
        Ok(old) => replace(env, [(old, predicate)], |state| {
            state.set_predication(predicate, value)
        }),
        Err(error) => Outcome::Return(error.hresult()),
    }
}

fn get_predication(env: &mut Env<'_>) -> Outcome {
    let (pointer, value) = (env.arg(1), env.arg(2));
    call((|| {
        let predication = *groups(env)?.predication;
        reference_array(env, pointer, &[predication.predicate])?;
        optional(env, value, i32::from(predication.value))
    })())
}

fn so_set_targets(env: &mut Env<'_>) -> Outcome {
    let (count, targets, offsets) = (env.arg(1), env.arg(2), env.arg(3));
    let result = (|| {
        let slots = range(0, count, STREAM_OUTPUTS)?;
        let pointers = array::<u32>(env, targets, count)?;
        let offsets = array::<u32>(env, offsets, count)?;
        let mut new = StreamOutput::default();
        for ((slot, pointer), offset) in slots.zip(pointers).zip(offsets) {
            let pointer = buffer_pointer(env, pointer, Bind::STREAM_OUTPUT);
            if let Some(Resource::Buffer(buffer)) = owned(env, pointer).and_then(Object::resource) {
                if offset != u32::MAX && (offset % 4 != 0 || offset > buffer.size()) {
                    return Err(Error::InvalidArgument);
                }
                if new.buffers.contains(&pointer) {
                    return Err(Error::InvalidArgument);
                }
                new.buffers[slot] = pointer;
                new.offsets[slot] = offset;
            }
        }
        let old = groups(env)?.clone();
        let mut vertices = old.ia.vertices;
        for vertex in &mut vertices {
            if vertex.buffer != 0 && new.buffers.contains(&vertex.buffer) {
                *vertex = VertexBuffer::default();
            }
        }
        let index = if new.buffers.contains(&old.ia.indices.buffer) {
            IndexBuffer::default()
        } else {
            old.ia.indices
        };
        #[cfg(feature = "gpu")]
        gpu::reset_stream_cursors(env, &new)?;
        Ok((old, new, vertices, index))
    })();
    match result {
        Ok((old, new, vertices, index)) => {
            let changes = old
                .so
                .buffers
                .into_iter()
                .zip(new.buffers)
                .chain(
                    old.ia
                        .vertices
                        .iter()
                        .zip(&vertices)
                        .map(|(old, new)| (old.buffer, new.buffer)),
                )
                .chain([(old.ia.indices.buffer, index.buffer)]);
            replace(env, changes, |state| {
                state.set_vertices(0, &vertices);
                state.set_indices(index);
                state.set_stream_output(new);
            })
        }
        Err(error) => Outcome::Return(error.hresult()),
    }
}

fn so_get_targets(env: &mut Env<'_>) -> Outcome {
    let (count, output) = (env.arg(1), env.arg(2));
    call((|| {
        let slots = range(0, count, STREAM_OUTPUTS)?;
        let buffers = groups(env)?.so.buffers;
        reference_array(env, output, &buffers[slots])
    })())
}

fn map_resource(env: &mut Env<'_>) -> Outcome {
    let [pointer, subresource, mode, flags, out] =
        std::array::from_fn(|index| env.arg(index as u32 + 1));
    call((|| {
        if owned(env, pointer).is_none() {
            return Err(Error::InvalidArgument);
        }
        let mapped = if deferred::current(env).is_some() {
            #[cfg(feature = "gpu")]
            {
                let mapped = deferred::map_resource(env, pointer, subresource, mode, flags)?;
                #[cfg(feature = "d3d9-counters")]
                gpu::count_map(env.ext, false);
                mapped
            }
            #[cfg(not(feature = "gpu"))]
            return Err(Error::InvalidArgument);
        } else {
            map_subresource(env, pointer, subresource, mode, flags)?
        };
        write(env, out, mapped)
    })())
}

pub(super) fn map_subresource(
    env: &mut Env<'_>,
    pointer: u32,
    subresource: u32,
    mode: u32,
    flags: u32,
) -> Result<D3D11_MAPPED_SUBRESOURCE, Error> {
    let table = table(env);
    let resource = ops::object(env, table, pointer)?
        .resource()
        .ok_or(Error::InvalidArgument)?;
    let request = map::Map::admit(resource, subresource, mode, flags)?;
    #[cfg(feature = "gpu")]
    if let map::Map::Staging { wait, .. } = request {
        gpu::wait_staging(env, pointer, subresource, wait)?;
    }
    #[cfg(not(feature = "gpu"))]
    let _ = request;
    let object = table.get_mut(&mut env.sys.objects, pointer).unwrap();
    #[cfg(feature = "gpu")]
    let backing = object.backing().and_then(|backing| backing.clone());
    let storage = object.mapping().ok_or(Error::InvalidArgument)?;
    let base = storage.pointer();
    #[cfg(all(feature = "gpu", feature = "d3d9-counters"))]
    let constant_time = request == map::Map::Dynamic && !storage.dynamic_shadow_stale(subresource);
    let mapped = storage.map(subresource, mode, flags, false, |layout, slice, parts| {
        #[cfg(feature = "gpu")]
        if let Some(backing) = &backing {
            gpu::download(
                env.ext,
                env.cpu,
                backing,
                resource,
                subresource,
                gpu::Shadow {
                    base,
                    layout,
                    slice,
                },
                parts,
            )
            .map_err(|_| map::Error::InvalidArgument)?;
        }
        #[cfg(not(feature = "gpu"))]
        let _ = (base, layout, slice, parts);
        Ok(())
    })?;
    #[cfg(all(feature = "gpu", feature = "d3d9-counters"))]
    gpu::count_map(env.ext, constant_time);
    Ok(mapped)
}

fn unmap_resource(env: &mut Env<'_>) -> Outcome {
    #[cfg(feature = "gpu")]
    {
        let (resource, subresource) = (env.arg(1), env.arg(2));
        call(if deferred::current(env).is_some() {
            deferred::unmap_resource(env, resource, subresource)
        } else {
            gpu::unmap_constants(env, resource, subresource)
        })
    }
    #[cfg(not(feature = "gpu"))]
    {
        let _ = env;
        Outcome::Return(0)
    }
}

macro_rules! gpu_methods {
    ($($name:ident),+ $(,)?) => {$(
        fn $name(env: &mut Env<'_>) -> Outcome {
            #[cfg(feature = "gpu")]
            return call(if deferred::current(env).is_some() {
                deferred::record(env, stringify!($name))
            } else {
                gpu::$name(env)
            });
            #[cfg(not(feature = "gpu"))]
            let _ = env;
            #[cfg(not(feature = "gpu"))]
            Outcome::Return(0)
        }
    )+};
}

gpu_methods!(
    copy_region,
    copy_resource,
    update_resource,
    clear_color,
    clear_depth,
    resolve,
    generate_mips,
    flush
);

gpu_methods!(
    draw,
    draw_indexed,
    draw_instanced,
    draw_indexed_instanced,
    draw_indirect,
    draw_indexed_indirect,
    dispatch,
    dispatch_indirect,
    clear_uav_uint,
    clear_uav_float,
);

gpu_methods!(begin_query, end_query);

fn get_data(env: &mut Env<'_>) -> Outcome {
    if deferred::current(env).is_some() {
        return Outcome::Return(Error::InvalidArgument.hresult());
    }
    #[cfg(feature = "gpu")]
    return status(gpu::get_data(env));
    #[cfg(not(feature = "gpu"))]
    {
        let _ = env;
        Outcome::Return(1)
    }
}

fn set_min_lod(env: &mut Env<'_>) -> Outcome {
    #[cfg(feature = "gpu")]
    if deferred::current(env).is_some() {
        return call(deferred::record(env, "set_min_lod"));
    }
    let (resource, lod) = (env.arg(1), f32::from_bits(env.arg(2)));
    call((|| {
        use crate::d3d11::desc::Misc;
        if owned(env, resource)
            .and_then(Object::resource)
            .is_some_and(|resource| resource.misc().contains(Misc::RESOURCE_CLAMP))
        {
            ops::object_mut(env, table(env), resource)?.set_min_lod(lod.max(0.));
        }
        Ok(())
    })())
}

fn min_lod(env: &mut Env<'_>) -> Outcome {
    let lod = owned(env, env.arg(1))
        .and_then(Object::min_lod)
        .unwrap_or_default();
    Outcome::ReturnF32(lod)
}

fn discard(_: &mut Env<'_>) -> Outcome {
    // Undefined contents can retain their previous value until the next write.
    Outcome::Return(0)
}

gpu_methods!(clear_view, copy_structure_count, draw_auto);

fn reset_counters(
    env: &mut Env<'_>,
    pointers: impl Iterator<Item = u32>,
    initial: u32,
) -> Result<(), Error> {
    #[cfg(feature = "gpu")]
    {
        let pointers: smallvec::SmallVec<[_; UAVS]> = pointers.collect();
        if initial != 0 && deferred::current(env).is_some() {
            return deferred::record_counters(env, &pointers, initial);
        }
        gpu::reset_counters(env, &pointers, initial)
    }
    #[cfg(not(feature = "gpu"))]
    {
        let _ = (env, pointers, initial);
        Ok(())
    }
}

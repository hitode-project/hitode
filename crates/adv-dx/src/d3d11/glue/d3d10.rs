//! Direct3D 10 and 10.1 COM faces over the Direct3D 11 objects.
use super::*;
use adv_abi::{gfx_iids::*, types::*};

pub(super) const CREATE_FLAGS: u32 = 0xc000_0000;
const DEVICE: &str = "ID3D10Device1";

macro_rules! slots {
    ($interface:literal, $($method:ident => $handler:expr),+ $(,)?) => {
        ($interface, &[$((
            concat!($interface, "::", stringify!($method)),
            $handler,
            ImplementationLevel::Implement,
        )),+])
    };
}

macro_rules! child {
    ($interface:literal $(, $method:ident => $handler:expr)* $(,)?) => {
        slots!($interface,
            QueryInterface => query,
            AddRef => add_ref,
            Release => release,
            GetDevice => child_device,
            GetPrivateData => get_private,
            SetPrivateData => set_private,
            SetPrivateDataInterface => set_interface
            $(, $method => $handler)*
        )
    };
}

macro_rules! resource_face {
    ($interface:literal) => {
        child!($interface,
            GetType => get_type,
            SetEvictionPriority => set_priority,
            GetEvictionPriority => get_priority,
            Map => map_resource,
            Unmap => unmap_resource,
            GetDesc => resource_description,
        )
    };
}

macro_rules! query_face {
    ($interface:literal) => {
        child!($interface,
            Begin => |env| query_call(env, "Begin"),
            End => |env| query_call(env, "End"),
            GetData => |env| query_call(env, "GetData"),
            GetDataSize => query_size,
            GetDesc => query_desc,
        )
    };
}

pub(super) const FACES: &[(&str, &[Slot])] = &[
    slots!("ID3D10Device1",
        QueryInterface => query,
        AddRef => add_ref,
        Release => release,
        VSSetConstantBuffers => |env| context_call(env, "VSSetConstantBuffers"),
        PSSetShaderResources => |env| context_call(env, "PSSetShaderResources"),
        PSSetShader => |env| context_call(env, "PSSetShader"),
        PSSetSamplers => |env| context_call(env, "PSSetSamplers"),
        VSSetShader => |env| context_call(env, "VSSetShader"),
        DrawIndexed => |env| context_call(env, "DrawIndexed"),
        Draw => |env| context_call(env, "Draw"),
        PSSetConstantBuffers => |env| context_call(env, "PSSetConstantBuffers"),
        IASetInputLayout => |env| context_call(env, "IASetInputLayout"),
        IASetVertexBuffers => |env| context_call(env, "IASetVertexBuffers"),
        IASetIndexBuffer => |env| context_call(env, "IASetIndexBuffer"),
        DrawIndexedInstanced => |env| context_call(env, "DrawIndexedInstanced"),
        DrawInstanced => |env| context_call(env, "DrawInstanced"),
        GSSetConstantBuffers => |env| context_call(env, "GSSetConstantBuffers"),
        GSSetShader => |env| context_call(env, "GSSetShader"),
        IASetPrimitiveTopology => |env| context_call(env, "IASetPrimitiveTopology"),
        VSSetShaderResources => |env| context_call(env, "VSSetShaderResources"),
        VSSetSamplers => |env| context_call(env, "VSSetSamplers"),
        SetPredication => |env| context_call(env, "SetPredication"),
        GSSetShaderResources => |env| context_call(env, "GSSetShaderResources"),
        GSSetSamplers => |env| context_call(env, "GSSetSamplers"),
        OMSetRenderTargets => |env| context_call(env, "OMSetRenderTargets"),
        OMSetBlendState => |env| context_call(env, "OMSetBlendState"),
        OMSetDepthStencilState => |env| context_call(env, "OMSetDepthStencilState"),
        SOSetTargets => |env| context_call(env, "SOSetTargets"),
        DrawAuto => |env| context_call(env, "DrawAuto"),
        RSSetState => |env| context_call(env, "RSSetState"),
        RSSetViewports => |env| context_call(env, "RSSetViewports"),
        RSSetScissorRects => |env| context_call(env, "RSSetScissorRects"),
        CopySubresourceRegion => |env| context_call(env, "CopySubresourceRegion"),
        CopyResource => |env| context_call(env, "CopyResource"),
        UpdateSubresource => |env| context_call(env, "UpdateSubresource"),
        ClearRenderTargetView => |env| context_call(env, "ClearRenderTargetView"),
        ClearDepthStencilView => |env| context_call(env, "ClearDepthStencilView"),
        GenerateMips => |env| context_call(env, "GenerateMips"),
        ResolveSubresource => |env| context_call(env, "ResolveSubresource"),
        VSGetConstantBuffers => |env| context_call(env, "VSGetConstantBuffers"),
        PSGetShaderResources => |env| context_call(env, "PSGetShaderResources"),
        PSGetShader => |env| context_call(env, "PSGetShader"),
        PSGetSamplers => |env| context_call(env, "PSGetSamplers"),
        VSGetShader => |env| context_call(env, "VSGetShader"),
        PSGetConstantBuffers => |env| context_call(env, "PSGetConstantBuffers"),
        IAGetInputLayout => |env| context_call(env, "IAGetInputLayout"),
        IAGetVertexBuffers => |env| context_call(env, "IAGetVertexBuffers"),
        IAGetIndexBuffer => |env| context_call(env, "IAGetIndexBuffer"),
        GSGetConstantBuffers => |env| context_call(env, "GSGetConstantBuffers"),
        GSGetShader => |env| context_call(env, "GSGetShader"),
        IAGetPrimitiveTopology => |env| context_call(env, "IAGetPrimitiveTopology"),
        VSGetShaderResources => |env| context_call(env, "VSGetShaderResources"),
        VSGetSamplers => |env| context_call(env, "VSGetSamplers"),
        GetPredication => |env| context_call(env, "GetPredication"),
        GSGetShaderResources => |env| context_call(env, "GSGetShaderResources"),
        GSGetSamplers => |env| context_call(env, "GSGetSamplers"),
        OMGetRenderTargets => |env| context_call(env, "OMGetRenderTargets"),
        OMGetBlendState => |env| context_call(env, "OMGetBlendState"),
        OMGetDepthStencilState => |env| context_call(env, "OMGetDepthStencilState"),
        SOGetTargets => |env| context_call(env, "SOGetTargets"),
        RSGetState => |env| context_call(env, "RSGetState"),
        RSGetViewports => |env| context_call(env, "RSGetViewports"),
        RSGetScissorRects => |env| context_call(env, "RSGetScissorRects"),
        GetDeviceRemovedReason => |env| device_call(env, "GetDeviceRemovedReason"),
        SetExceptionMode => |env| device_call(env, "SetExceptionMode"),
        GetExceptionMode => |env| device_call(env, "GetExceptionMode"),
        GetPrivateData => get_private,
        SetPrivateData => set_private,
        SetPrivateDataInterface => set_interface,
        ClearState => |env| context_call(env, "ClearState"),
        Flush => |env| context_call(env, "Flush"),
        CreateBuffer => |env| device_call(env, "CreateBuffer"),
        CreateTexture1D => |env| device_call(env, "CreateTexture1D"),
        CreateTexture2D => |env| device_call(env, "CreateTexture2D"),
        CreateTexture3D => |env| device_call(env, "CreateTexture3D"),
        CreateShaderResourceView => |env| device_call(env, "CreateShaderResourceView"),
        CreateRenderTargetView => |env| device_call(env, "CreateRenderTargetView"),
        CreateDepthStencilView => |env| device_call(env, "CreateDepthStencilView"),
        CreateInputLayout => |env| device_call(env, "CreateInputLayout"),
        CreateVertexShader => |env| device_call(env, "CreateVertexShader"),
        CreateGeometryShader => |env| device_call(env, "CreateGeometryShader"),
        CreateGeometryShaderWithStreamOutput => |env| device_call(env, "CreateGeometryShaderWithStreamOutput"),
        CreatePixelShader => |env| device_call(env, "CreatePixelShader"),
        CreateBlendState => |env| device_call(env, "CreateBlendState"),
        CreateDepthStencilState => |env| device_call(env, "CreateDepthStencilState"),
        CreateRasterizerState => |env| device_call(env, "CreateRasterizerState"),
        CreateSamplerState => |env| device_call(env, "CreateSamplerState"),
        CreateQuery => |env| device_call(env, "CreateQuery"),
        CreatePredicate => |env| device_call(env, "CreatePredicate"),
        CreateCounter => |env| device_call(env, "CreateCounter"),
        CheckFormatSupport => |env| device_call(env, "CheckFormatSupport"),
        CheckMultisampleQualityLevels => |env| device_call(env, "CheckMultisampleQualityLevels"),
        CheckCounterInfo => |env| device_call(env, "CheckCounterInfo"),
        CheckCounter => |env| device_call(env, "CheckCounter"),
        GetCreationFlags => |env| device_call(env, "GetCreationFlags"),
        OpenSharedResource => |env| device_call(env, "OpenSharedResource"),
        SetTextFilterSize => |env| device_call(env, "SetTextFilterSize"),
        GetTextFilterSize => |env| device_call(env, "GetTextFilterSize"),
        CreateShaderResourceView1 => |env| device_call(env, "CreateShaderResourceView1"),
        CreateBlendState1 => |env| device_call(env, "CreateBlendState1"),
        GetFeatureLevel => |env| device_call(env, "GetFeatureLevel"),
    ),
    resource_face!("ID3D10Buffer"),
    resource_face!("ID3D10Texture1D"),
    resource_face!("ID3D10Texture2D"),
    resource_face!("ID3D10Texture3D"),
    child!("ID3D10ShaderResourceView1", GetResource => child_resource, GetDesc => view_description, GetDesc1 => view_description),
    child!("ID3D10RenderTargetView", GetResource => child_resource, GetDesc => view_description),
    child!("ID3D10DepthStencilView", GetResource => child_resource, GetDesc => view_description),
    child!("ID3D10InputLayout"),
    child!("ID3D10VertexShader"),
    child!("ID3D10GeometryShader"),
    child!("ID3D10PixelShader"),
    child!("ID3D10BlendState1", GetDesc => blend_description, GetDesc1 => blend_description1),
    child!("ID3D10DepthStencilState", GetDesc => pipeline_desc),
    child!("ID3D10SamplerState", GetDesc => pipeline_desc),
    child!("ID3D10RasterizerState", GetDesc => pipeline_desc),
    query_face!("ID3D10Query"),
    query_face!("ID3D10Predicate"),
];

pub(super) fn attach_device(env: &mut Env<'_>, device: u32) -> Result<(), ObjectError> {
    let face = alloc(env, DEVICE)?;
    table(env).register_separate_face(
        &mut env.sys.objects,
        device,
        face,
        &[IID_ID3D10DEVICE, IID_ID3D10DEVICE1],
    );
    table(env)
        .get_mut(&mut env.sys.objects, device)
        .unwrap()
        .d3d10_face = face;
    Ok(())
}

pub(super) fn attach_child(env: &mut Env<'_>, pointer: u32) -> Result<(), ObjectError> {
    let table = table(env);
    let object = table.get(&env.sys.objects, pointer).unwrap();
    let device = object.device().unwrap();
    if object.d3d10_face != 0 || table.get(&env.sys.objects, device).unwrap().d3d10_face == 0 {
        return Ok(());
    }
    let (name, interfaces): (&str, &[Iid]) = if let Some(resource) = object.resource() {
        match resource {
            Resource::Buffer(_) => ("ID3D10Buffer", &[IID_ID3D10BUFFER, IID_ID3D10RESOURCE]),
            Resource::Texture(texture) => match texture.kind() {
                TextureKind::D1 => (
                    "ID3D10Texture1D",
                    &[IID_ID3D10TEXTURE1D, IID_ID3D10RESOURCE],
                ),
                TextureKind::D2 => (
                    "ID3D10Texture2D",
                    &[IID_ID3D10TEXTURE2D, IID_ID3D10RESOURCE],
                ),
                TextureKind::D3 => (
                    "ID3D10Texture3D",
                    &[IID_ID3D10TEXTURE3D, IID_ID3D10RESOURCE],
                ),
            },
        }
    } else if let Some((_, view)) = object.view() {
        match view.kind() {
            ViewKind::ShaderResource => (
                "ID3D10ShaderResourceView1",
                &[
                    IID_ID3D10SHADERRESOURCEVIEW1,
                    IID_ID3D10SHADERRESOURCEVIEW,
                    IID_ID3D10VIEW,
                ],
            ),
            ViewKind::RenderTarget => (
                "ID3D10RenderTargetView",
                &[IID_ID3D10RENDERTARGETVIEW, IID_ID3D10VIEW],
            ),
            ViewKind::DepthStencil => (
                "ID3D10DepthStencilView",
                &[IID_ID3D10DEPTHSTENCILVIEW, IID_ID3D10VIEW],
            ),
            ViewKind::UnorderedAccess => return Ok(()),
        }
    } else if let Some(state) = object.pipeline() {
        match state {
            PipelineState::Blend(_) => (
                "ID3D10BlendState1",
                &[IID_ID3D10BLENDSTATE1, IID_ID3D10BLENDSTATE],
            ),
            PipelineState::DepthStencil(_) => {
                ("ID3D10DepthStencilState", &[IID_ID3D10DEPTHSTENCILSTATE])
            }
            PipelineState::Rasterizer(_) => ("ID3D10RasterizerState", &[IID_ID3D10RASTERIZERSTATE]),
            PipelineState::Sampler(_) => ("ID3D10SamplerState", &[IID_ID3D10SAMPLERSTATE]),
        }
    } else if object.input_layout().is_some() {
        ("ID3D10InputLayout", &[IID_ID3D10INPUTLAYOUT])
    } else if let Some(shader) = object.shader() {
        use crate::shader::ir::Stage;
        match shader.module().stage {
            Stage::Vertex => ("ID3D10VertexShader", &[IID_ID3D10VERTEXSHADER]),
            Stage::Geometry => ("ID3D10GeometryShader", &[IID_ID3D10GEOMETRYSHADER]),
            Stage::Pixel => ("ID3D10PixelShader", &[IID_ID3D10PIXELSHADER]),
            _ => return Ok(()),
        }
    } else if let Some(query) = object.query() {
        if query.predicate {
            (
                "ID3D10Predicate",
                &[IID_ID3D10PREDICATE, IID_ID3D10QUERY, IID_ID3D10ASYNCHRONOUS],
            )
        } else {
            ("ID3D10Query", &[IID_ID3D10QUERY, IID_ID3D10ASYNCHRONOUS])
        }
    } else {
        return Ok(());
    };
    let face = alloc(env, name)?;
    table.register_separate_face(&mut env.sys.objects, pointer, face, interfaces);
    table.register_faces(
        &mut env.sys.objects,
        pointer,
        face,
        &[IID_ID3D10DEVICECHILD],
    );
    table
        .get_mut(&mut env.sys.objects, pointer)
        .unwrap()
        .d3d10_face = face;
    Ok(())
}

fn arguments(env: &Env<'_>) -> Vec<u32> {
    let count = u32::from(env.sys.thunks.get(env.thunk).f.conv.argc());
    (0..count).map(|i| env.arg(i)).collect()
}

fn invoke(
    env: &mut Env<'_>,
    interface: &str,
    method: &str,
    arguments: Vec<u32>,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    let name = format!("{interface}::{method}");
    let binding = env.sys.thunks.published_index(DLL, &name).unwrap();
    env.invoke_host_u32(binding, arguments, move |env, result| {
        then(env, result.value())
    })
}

fn context_call(env: &mut Env<'_>, method: &'static str) -> Outcome {
    let mut args = arguments(env);
    let table = table(env);
    args[0] = table
        .get(&env.sys.objects, args[0])
        .unwrap()
        .immediate()
        .unwrap();
    let mut outputs = [(0, 0); 2];
    let mut scratch = 0;
    let immediate = table
        .get(&env.sys.objects, args[0])
        .unwrap()
        .context_data()
        .unwrap();
    if immediate.active_state != 0 && !immediate.d3d10 {
        return Outcome::Return(0);
    }
    match method {
        "VSSetShader" | "PSSetShader" | "GSSetShader" | "VSGetShader" | "PSGetShader"
        | "GSGetShader" => {
            args.extend_from_slice(&[0, 0]);
            if method.contains("Get") {
                outputs[0] = (args[1], 1);
            }
        }
        "VSGetConstantBuffers"
        | "PSGetConstantBuffers"
        | "GSGetConstantBuffers"
        | "VSGetShaderResources"
        | "PSGetShaderResources"
        | "GSGetShaderResources"
        | "VSGetSamplers"
        | "PSGetSamplers"
        | "GSGetSamplers"
        | "IAGetVertexBuffers" => {
            outputs[0] = (args[3], args[2]);
        }
        "IAGetInputLayout"
        | "IAGetIndexBuffer"
        | "GetPredication"
        | "OMGetBlendState"
        | "OMGetDepthStencilState"
        | "RSGetState" => outputs[0] = (args[1], 1),
        "OMGetRenderTargets" => outputs = [(args[2], args[1]), (args[3], 1)],
        "SOGetTargets" => {
            outputs[0] = (args[2], args[1]);
            let offsets = args.pop().unwrap();
            if offsets != 0 {
                let values = immediate.state.groups().so.offsets;
                let _ = context::output_array(env, offsets, &values[..args[1].min(4) as usize]);
            }
        }
        "RSSetViewports" => {
            if args[1] > super::super::state::VIEWPORTS as u32 {
                return Outcome::Return(0);
            }
            let Ok(viewports) = array::<D3D10_VIEWPORT>(env, args[2], args[1]) else {
                return Outcome::Return(0);
            };
            let viewports: Vec<_> = viewports
                .into_iter()
                .map(|viewport| D3D11_VIEWPORT {
                    TopLeftX: viewport.TopLeftX as f32,
                    TopLeftY: viewport.TopLeftY as f32,
                    Width: viewport.Width as f32,
                    Height: viewport.Height as f32,
                    MinDepth: viewport.MinDepth,
                    MaxDepth: viewport.MaxDepth,
                })
                .collect();
            if !viewports.is_empty() {
                scratch = match temporary(env, bytemuck::cast_slice(&viewports)) {
                    Ok(pointer) => pointer,
                    Err(error) => return Outcome::Return(error.hresult()),
                };
                args[2] = scratch;
            }
        }
        _ => (),
    }
    let viewport_output = if method == "RSGetViewports" && args[2] != 0 {
        Some((
            args[1],
            args[2],
            read::<u32>(env, args[1])
                .unwrap_or(0)
                .min(super::super::state::VIEWPORTS as u32),
        ))
    } else {
        None
    };
    invoke(
        env,
        "ID3D11DeviceContext2",
        method,
        args,
        move |env, result| {
            if scratch != 0 {
                free(env, scratch);
            }
            if result == 0 {
                if let Some((count_address, address, count)) = viewport_output {
                    let count = count.min(read::<u32>(env, count_address).unwrap_or(0));
                    for i in 0..count {
                        let address = address + i * size_of::<D3D10_VIEWPORT>() as u32;
                        if let Ok(viewport) = read::<D3D11_VIEWPORT>(env, address) {
                            let _ = write(
                                env,
                                address,
                                D3D10_VIEWPORT {
                                    TopLeftX: viewport.TopLeftX as i32,
                                    TopLeftY: viewport.TopLeftY as i32,
                                    Width: viewport.Width as u32,
                                    Height: viewport.Height as u32,
                                    MinDepth: viewport.MinDepth,
                                    MaxDepth: viewport.MaxDepth,
                                },
                            );
                        }
                    }
                }
                return translate_outputs(env, outputs, result);
            }
            Outcome::Return(result)
        },
    )
}

fn child_device(env: &mut Env<'_>) -> Outcome {
    let table = table(env);
    let pointer = env.arg(0);
    let out = env.arg(1);
    call((|| {
        let device = table
            .get(&env.sys.objects, pointer)
            .unwrap()
            .device()
            .unwrap();
        let device = table.query(&mut env.sys.objects, device, IID_ID3D10DEVICE)?;
        write(env, out, device)
    })())
}

fn child_resource(env: &mut Env<'_>) -> Outcome {
    let table = table(env);
    let pointer = env.arg(0);
    let out = env.arg(1);
    call((|| {
        let (resource, _) = table
            .get(&env.sys.objects, pointer)
            .unwrap()
            .view()
            .unwrap();
        let resource = table.query(&mut env.sys.objects, resource, IID_ID3D10RESOURCE)?;
        write(env, out, resource)
    })())
}

fn translate_outputs(
    env: &mut Env<'_>,
    outputs: impl IntoIterator<Item = (u32, u32)>,
    result: u32,
) -> Outcome {
    let table = table(env);
    let mut retired = Release::default();
    for (address, count) in outputs {
        if address == 0 {
            continue;
        }
        for index in 0..count {
            let address = address + index * 4;
            let Ok(pointer) = read::<u32>(env, address) else {
                break;
            };
            if pointer == 0 {
                continue;
            }
            let target = table.get(&env.sys.objects, pointer).unwrap().d3d10_face;
            if target == pointer {
                continue;
            }
            if target != 0 && table.add_ref(&mut env.sys.objects, target).is_err() {
                continue;
            }
            let _ = write(env, address, target);
            let released = table.release(&mut env.sys.objects, pointer);
            retired.allocations.extend(released.allocations);
            retired
                .private_interfaces
                .extend(released.private_interfaces);
            #[cfg(feature = "gpu")]
            {
                retired.shared.extend(released.shared);
                retired.keyed_wakes.extend(released.keyed_wakes);
            }
        }
    }
    retired.references = result;
    retire(env, retired)
}

fn query_call(env: &mut Env<'_>, method: &str) -> Outcome {
    let table = table(env);
    let mut args = arguments(env);
    let device = table
        .get(&env.sys.objects, args[0])
        .unwrap()
        .device()
        .unwrap();
    let immediate = table
        .get(&env.sys.objects, device)
        .unwrap()
        .immediate()
        .unwrap();
    args.insert(0, immediate);
    invoke(env, "ID3D11DeviceContext2", method, args, |_, result| {
        Outcome::Return(result)
    })
}

fn temporary(env: &mut Env<'_>, bytes: &[u8]) -> Result<u32, Error> {
    let heaps = env.ext.get_mut::<adv_os::Heaps>();
    let pointer = heaps
        .alloc(
            env.cpu,
            &mut env.sys.vm,
            heaps.process_heap,
            bytes.len() as u32,
            false,
        )
        .ok_or(ObjectError::Exhausted)?;
    if env.cpu.write(pointer, bytes).is_err() {
        free(env, pointer);
        return Err(Error::InvalidArgument);
    }
    Ok(pointer)
}

fn device_call(env: &mut Env<'_>, method: &'static str) -> Outcome {
    let mut args = arguments(env);
    let table = table(env);
    let device = table.canonical(&env.sys.objects, args[0]).unwrap();
    args[0] = device;
    if matches!(method, "SetTextFilterSize" | "GetTextFilterSize") {
        return call((|| {
            let state = table
                .get_mut(&mut env.sys.objects, device)
                .unwrap()
                .device_state_mut()
                .unwrap();
            if method == "SetTextFilterSize" {
                state.text_filter = [args[1], args[2]];
            } else {
                let [width, height] = state.text_filter;
                context::optional(env, args[1], width)?;
                context::optional(env, args[2], height)?;
            }
            Ok(())
        })());
    }
    let descriptor = (|| {
        Ok(match method {
            "CreateBuffer" => {
                let desc: D3D10_BUFFER_DESC = read(env, args[1])?;
                Some((
                    1,
                    temporary(
                        env,
                        bytemuck::bytes_of(&D3D11_BUFFER_DESC {
                            ByteWidth: desc.ByteWidth,
                            Usage: desc.Usage,
                            BindFlags: desc.BindFlags,
                            CPUAccessFlags: desc.CPUAccessFlags,
                            MiscFlags: resource_flags_11(desc.MiscFlags),
                            StructureByteStride: 0,
                        }),
                    )?,
                ))
            }
            "CreateTexture1D" => {
                let mut desc: D3D11_TEXTURE1D_DESC = read(env, args[1])?;
                desc.MiscFlags = resource_flags_11(desc.MiscFlags);
                Some((1, temporary(env, bytemuck::bytes_of(&desc))?))
            }
            "CreateTexture2D" => {
                let mut desc: D3D11_TEXTURE2D_DESC = read(env, args[1])?;
                desc.MiscFlags = resource_flags_11(desc.MiscFlags);
                Some((1, temporary(env, bytemuck::bytes_of(&desc))?))
            }
            "CreateTexture3D" => {
                let mut desc: D3D11_TEXTURE3D_DESC = read(env, args[1])?;
                desc.MiscFlags = resource_flags_11(desc.MiscFlags);
                Some((1, temporary(env, bytemuck::bytes_of(&desc))?))
            }
            "CreateDepthStencilView" if args[2] != 0 => {
                let desc: D3D10_DEPTH_STENCIL_VIEW_DESC = read(env, args[2])?;
                Some((
                    2,
                    temporary(
                        env,
                        bytemuck::bytes_of(&D3D11_DEPTH_STENCIL_VIEW_DESC {
                            Format: desc.Format,
                            ViewDimension: desc.ViewDimension,
                            Flags: 0,
                            Anonymous: desc.Anonymous,
                        }),
                    )?,
                ))
            }
            "CreateBlendState" | "CreateBlendState1" => {
                let desc = if method == "CreateBlendState" {
                    let desc: D3D10_BLEND_DESC = read(env, args[1])?;
                    D3D11_BLEND_DESC {
                        AlphaToCoverageEnable: desc.AlphaToCoverageEnable,
                        IndependentBlendEnable: 1,
                        RenderTarget: std::array::from_fn(|i| D3D11_RENDER_TARGET_BLEND_DESC {
                            BlendEnable: desc.BlendEnable[i],
                            SrcBlend: desc.SrcBlend,
                            DestBlend: desc.DestBlend,
                            BlendOp: desc.BlendOp,
                            SrcBlendAlpha: desc.SrcBlendAlpha,
                            DestBlendAlpha: desc.DestBlendAlpha,
                            BlendOpAlpha: desc.BlendOpAlpha,
                            RenderTargetWriteMask: desc.RenderTargetWriteMask[i],
                            _padding0: [0; 3],
                        }),
                    }
                } else {
                    bytemuck::cast::<D3D10_BLEND_DESC1, D3D11_BLEND_DESC>(read(env, args[1])?)
                };
                Some((1, temporary(env, bytemuck::bytes_of(&desc))?))
            }
            "CreateGeometryShaderWithStreamOutput" => {
                let entries = array::<D3D10_SO_DECLARATION_ENTRY>(env, args[3], args[4])?;
                let mut retained = Vec::with_capacity((entries.len() + 1) * 4);
                retained.push(args[5]);
                for entry in entries {
                    retained.extend_from_slice(bytemuck::cast_slice(std::slice::from_ref(
                        &D3D11_SO_DECLARATION_ENTRY {
                            Stream: 0,
                            SemanticName: entry.SemanticName,
                            SemanticIndex: entry.SemanticIndex,
                            StartComponent: entry.StartComponent,
                            ComponentCount: entry.ComponentCount,
                            OutputSlot: entry.OutputSlot,
                            _padding0: [0],
                        },
                    )));
                }
                let allocation = temporary(env, bytemuck::cast_slice(&retained))?;
                let count = args[4];
                args.splice(3..6, [allocation + 4, count, allocation, 1, 0, 0]);
                Some((5, allocation))
            }
            "CreateVertexShader" | "CreateGeometryShader" | "CreatePixelShader" => {
                args.insert(3, 0);
                None
            }
            _ => None,
        })
    })();
    let scratch = match descriptor {
        Ok(Some((slot, pointer))) => {
            args[slot] = pointer;
            pointer
        }
        Ok(None) => 0,
        Err(error) => return Outcome::Return(Error::hresult(error)),
    };
    let output = *args.last().unwrap();
    let core_method = match method {
        "CreateShaderResourceView1" => "CreateShaderResourceView",
        "CreateBlendState1" => "CreateBlendState",
        _ => method,
    };
    invoke(
        env,
        "ID3D11Device2",
        core_method,
        args,
        move |env, result| {
            if scratch != 0 {
                free(env, scratch);
            }
            if result == 0 && method.starts_with("Create") && output != 0 {
                return translate_outputs(env, [(output, 1)], result);
            }
            match method {
                "GetCreationFlags" => Outcome::Return(result & !CREATE_FLAGS),
                "CheckFormatSupport" if result == 0 => call((|| {
                    let flags: u32 = read(env, output)?;
                    write(env, output, flags & 0x01ff_ffff)
                })()),
                _ => Outcome::Return(result),
            }
        },
    )
}

fn map_resource(env: &mut Env<'_>) -> Outcome {
    let pointer = env.arg(0);
    let desc = table(env)
        .get(&env.sys.objects, pointer)
        .and_then(Object::resource)
        .unwrap();
    let kind = match desc {
        Resource::Buffer(_) => None,
        Resource::Texture(texture) => Some(texture.kind()),
    };
    let (subresource, mode, flags, out) = if kind.is_none() {
        (0, env.arg(1), env.arg(2), env.arg(3))
    } else {
        (env.arg(1), env.arg(2), env.arg(3), env.arg(4))
    };
    call((|| {
        let mapped = context::map_subresource(env, pointer, subresource, mode, flags)?;
        match kind {
            None | Some(TextureKind::D1) => write(env, out, mapped.pData),
            Some(TextureKind::D2) => write(
                env,
                out,
                D3D10_MAPPED_TEXTURE2D {
                    pData: mapped.pData,
                    RowPitch: mapped.RowPitch,
                },
            ),
            Some(TextureKind::D3) => write(
                env,
                out,
                D3D10_MAPPED_TEXTURE3D {
                    pData: mapped.pData,
                    RowPitch: mapped.RowPitch,
                    DepthPitch: mapped.DepthPitch,
                },
            ),
        }
    })())
}

fn unmap_resource(_: &mut Env<'_>) -> Outcome {
    Outcome::Return(0)
}

fn resource_description(env: &mut Env<'_>) -> Outcome {
    let (pointer, out) = (env.arg(0), env.arg(1));
    let resource = table(env)
        .get(&env.sys.objects, pointer)
        .unwrap()
        .resource()
        .unwrap();
    call(match resource {
        Resource::Buffer(buffer) => {
            let desc = buffer.desc();
            write(
                env,
                out,
                D3D10_BUFFER_DESC {
                    ByteWidth: desc.ByteWidth,
                    Usage: desc.Usage,
                    BindFlags: desc.BindFlags,
                    CPUAccessFlags: desc.CPUAccessFlags,
                    MiscFlags: resource_flags_10(desc.MiscFlags),
                },
            )
        }
        Resource::Texture(texture) => match texture.kind() {
            TextureKind::D1 => {
                let mut desc = texture.desc_1d();
                desc.MiscFlags = resource_flags_10(desc.MiscFlags);
                write(env, out, desc)
            }
            TextureKind::D2 => {
                let mut desc = texture.desc_2d();
                desc.MiscFlags = resource_flags_10(desc.MiscFlags);
                write(env, out, desc)
            }
            TextureKind::D3 => {
                let mut desc = texture.desc_3d();
                desc.MiscFlags = resource_flags_10(desc.MiscFlags);
                write(env, out, desc)
            }
        },
    })
}

fn view_description(env: &mut Env<'_>) -> Outcome {
    let (pointer, out) = (env.arg(0), env.arg(1));
    let (_, view) = table(env)
        .get(&env.sys.objects, pointer)
        .unwrap()
        .view()
        .unwrap();
    if view.kind() == ViewKind::DepthStencil {
        let desc = view.dsv_desc();
        call(write(
            env,
            out,
            D3D10_DEPTH_STENCIL_VIEW_DESC {
                Format: desc.Format,
                ViewDimension: desc.ViewDimension,
                Anonymous: desc.Anonymous,
            },
        ))
    } else {
        view_desc(env)
    }
}

fn blend_description(env: &mut Env<'_>) -> Outcome {
    let (pointer, out) = (env.arg(0), env.arg(1));
    let object = table(env).get(&env.sys.objects, pointer).unwrap();
    let Some(PipelineState::Blend(desc)) = object.pipeline() else {
        return Outcome::Return(Error::InvalidArgument.hresult());
    };
    let first = &desc.RenderTarget[0];
    call(write(
        env,
        out,
        D3D10_BLEND_DESC {
            AlphaToCoverageEnable: desc.AlphaToCoverageEnable,
            BlendEnable: desc.RenderTarget.map(|target| target.BlendEnable),
            SrcBlend: first.SrcBlend,
            DestBlend: first.DestBlend,
            BlendOp: first.BlendOp,
            SrcBlendAlpha: first.SrcBlendAlpha,
            DestBlendAlpha: first.DestBlendAlpha,
            BlendOpAlpha: first.BlendOpAlpha,
            RenderTargetWriteMask: desc.RenderTarget.map(|target| target.RenderTargetWriteMask),
        },
    ))
}

fn blend_description1(env: &mut Env<'_>) -> Outcome {
    let (pointer, out) = (env.arg(0), env.arg(1));
    let object = table(env).get(&env.sys.objects, pointer).unwrap();
    let Some(PipelineState::Blend(desc)) = object.pipeline() else {
        return Outcome::Return(Error::InvalidArgument.hresult());
    };
    let targets = desc
        .RenderTarget
        .map(|target| D3D10_RENDER_TARGET_BLEND_DESC1 {
            BlendEnable: target.BlendEnable,
            SrcBlend: target.SrcBlend,
            DestBlend: target.DestBlend,
            BlendOp: target.BlendOp,
            SrcBlendAlpha: target.SrcBlendAlpha,
            DestBlendAlpha: target.DestBlendAlpha,
            BlendOpAlpha: target.BlendOpAlpha,
            RenderTargetWriteMask: target.RenderTargetWriteMask,
            _padding0: [0; 3],
        });
    call(write(
        env,
        out,
        D3D10_BLEND_DESC1 {
            AlphaToCoverageEnable: desc.AlphaToCoverageEnable,
            IndependentBlendEnable: desc.IndependentBlendEnable,
            RenderTarget: targets,
        },
    ))
}

fn resource_flags_11(flags: u32) -> u32 {
    (flags & !0x30) | ((flags & 0x30) << 4)
}

fn resource_flags_10(flags: u32) -> u32 {
    (flags & 7) | ((flags & 0x300) >> 4)
}

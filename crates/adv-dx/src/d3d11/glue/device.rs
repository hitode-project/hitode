//! Device creation and resource factory entry points.
use super::*;
use crate::d3d11::{
    caps::{Coverage, DeviceAdapter, DeviceRequest, Driver, FeatureLevel, Report},
    desc::{Buffer, InputLayout, Shader, Texture, ViewDesc},
};
use crate::shader::ir::Stage;
use adv_abi::types::{D3D11_COUNTER_DESC, D3D11_INPUT_ELEMENT_DESC, D3D11_SUBRESOURCE_DATA};
use std::sync::Arc;

macro_rules! methods {
    ($($method:ident => $handler:ident $level:ident),+ $(,)?) => {
        ("ID3D11Device1", &[
            $((concat!("ID3D11Device1::", stringify!($method)), $handler, ImplementationLevel::$level)),+
        ])
    };
}

pub(super) const FACE: (&str, &[Slot]) = methods!(
    QueryInterface => query Implement,
    AddRef => add_ref Implement,
    Release => release Implement,
    CreateBuffer => buffer Implement,
    CreateTexture1D => texture_1d Implement,
    CreateTexture2D => texture_2d Implement,
    CreateTexture3D => texture_3d Implement,
    CreateShaderResourceView => srv Implement,
    CreateUnorderedAccessView => uav Implement,
    CreateRenderTargetView => rtv Implement,
    CreateDepthStencilView => dsv Implement,
    CreateInputLayout => input_layout Implement,
    CreateVertexShader => vertex_shader Implement,
    CreateGeometryShader => excluded_stage SafeFail,
    CreateGeometryShaderWithStreamOutput => excluded_so_shader SafeFail,
    CreatePixelShader => pixel_shader Implement,
    CreateHullShader => excluded_stage SafeFail,
    CreateDomainShader => excluded_stage SafeFail,
    CreateComputeShader => compute_shader Implement,
    CreateClassLinkage => excluded_linkage SafeFail,
    CreateBlendState => blend Implement,
    CreateDepthStencilState => depth_stencil Implement,
    CreateRasterizerState => rasterizer Implement,
    CreateSamplerState => sampler Implement,
    CreateQuery => create_query Implement,
    CreatePredicate => create_predicate Implement,
    CreateCounter => check_counter Implement,
    CreateDeferredContext => excluded_deferred SafeFail,
    OpenSharedResource => open_shared SafeFail,
    CheckFormatSupport => format_support Implement,
    CheckMultisampleQualityLevels => multisample_quality Implement,
    CheckCounterInfo => counter_info Implement,
    CheckCounter => check_counter Implement,
    CheckFeatureSupport => feature_support Implement,
    GetPrivateData => get_private Implement,
    SetPrivateData => set_private Implement,
    SetPrivateDataInterface => set_interface Implement,
    GetFeatureLevel => feature_level State,
    GetCreationFlags => creation_flags State,
    GetDeviceRemovedReason => removed_reason State,
    GetImmediateContext => immediate_context Implement,
    SetExceptionMode => set_exception_mode State,
    GetExceptionMode => exception_mode State,
    GetImmediateContext1 => immediate_context Implement,
    CreateDeferredContext1 => excluded_deferred SafeFail,
    CreateBlendState1 => blend1 Implement,
    CreateRasterizerState1 => rasterizer1 Implement,
    CreateDeviceContextState => excluded_context_state SafeFail,
    OpenSharedResource1 => open_shared SafeFail,
    OpenSharedResourceByName => open_shared_by_name SafeFail,
);

#[derive(Default)]
pub(super) struct CreateDevices {
    pub(super) reports: [Option<Arc<Report>>; FeatureLevel::ALL.len()],
    pub(super) max_buffer_size: u64,
}

impl CreateDevices {
    pub(super) fn derive(hardware: &adv_gpu::Caps, paths: Coverage<'_>) -> Self {
        Self {
            reports: FeatureLevel::ALL.map(|level| {
                Report::derive_at_most(hardware, paths, level)
                    .filter(|report| report.level == level)
                    .map(Arc::new)
            }),
            max_buffer_size: hardware.limits.max_buffer_size,
        }
    }
}

pub(super) fn create(env: &mut Env<'_>) -> Outcome {
    let arguments = std::array::from_fn(|index| env.arg(index as u32));
    create_device(env, arguments, None)
}

pub(super) fn create_and_swap(env: &mut Env<'_>) -> Outcome {
    let arguments = std::array::from_fn(|i| env.arg(if i < 7 { i as u32 } else { i as u32 + 2 }));
    create_device(env, arguments, Some((env.arg(7), env.arg(8))))
}

fn create_device(env: &mut Env<'_>, arguments: [u32; 10], swap: Option<(u32, u32)>) -> Outcome {
    let [
        adapter,
        driver,
        module,
        flags,
        levels,
        count,
        sdk,
        device_out,
        level_out,
        context_out,
    ] = arguments;
    status((|| {
        if let Some((desc, out)) = swap
            && out != 0
        {
            write(env, out, 0u32)?;
            if desc == 0 {
                return Err(Error::InvalidArgument);
            }
        }

        for pointer in [device_out, context_out] {
            if pointer != 0 {
                write(env, pointer, 0u32)?;
            }
        }
        let driver = match driver {
            0 => Driver::Unknown,
            1 => Driver::Hardware,
            2 => Driver::Reference,
            3 => Driver::Null,
            4 => Driver::Software,
            5 => Driver::Warp,
            _ => return Err(Error::InvalidArgument),
        };
        let levels = (levels != 0)
            .then(|| array::<u32>(env, levels, count))
            .transpose()?;
        let request = DeviceRequest {
            driver,
            has_adapter: adapter != 0,
            has_software_module: module != 0,
            levels: levels.as_deref(),
            flags,
            sdk_version: sdk,
            level_count: count,
            wants_device: device_out != 0 || swap.is_some_and(|(_, out)| out != 0),
            wants_context: context_out != 0,
        };
        let devices = &env.ext.get::<State>().devices;
        let maximum = devices
            .reports
            .iter()
            .rev()
            .flatten()
            .next()
            .map(|report| report.level);
        let choice = request.choose(maximum)?;
        let report = devices.reports[choice.level.column()]
            .clone()
            .ok_or(crate::d3d11::caps::Error::Unsupported)?;
        let adapter_kind = match choice.adapter {
            DeviceAdapter::Explicit => crate::dxgi::adapter_kind(env, adapter)?,
            DeviceAdapter::Hardware => crate::dxgi::caps::Adapter::Hardware,
            DeviceAdapter::Software => crate::dxgi::caps::Adapter::Software,
            DeviceAdapter::Null => crate::dxgi::caps::Adapter::Null,
        };
        if level_out != 0 {
            write(env, level_out, choice.level as u32)?;
        }
        if choice.status != 0 {
            return Ok(choice.status);
        }

        let device = alloc(env, FACE.0)?;
        let table = table(env);
        if let Err(error) = table.attach_device(&mut env.sys.objects, device, report) {
            free(env, device);
            return Err(error.into());
        }
        let context = match (|| {
            dxgi::attach_device_face(env, device, adapter_kind)?;
            attach_multithread(env, device, flags)?;
            attach_context(env, device, flags)
        })() {
            Ok(context) => context,
            Err(error) => {
                let released = table.release(&mut env.sys.objects, device);
                retire(env, released);
                return Err(error);
            }
        };
        if let Some((desc, output)) = swap.filter(|(_, output)| *output != 0) {
            let adapter = ops::object(env, table, device)?
                .device_state()
                .unwrap()
                .adapter;
            if let Err(error) =
                crate::dxgi::create_device_swap_chain(env, adapter, device, desc, output)
            {
                let released = table.release(&mut env.sys.objects, context);
                retire(env, released);
                let released = table.release(&mut env.sys.objects, device);
                retire(env, released);
                return Err(error.into());
            }
        }
        if context_out != 0 {
            write(env, context_out, context)?;
        } else {
            let released = table.release(&mut env.sys.objects, context);
            retire(env, released);
        }
        if device_out != 0 {
            write(env, device_out, device)?;
        } else {
            let released = table.release(&mut env.sys.objects, device);
            retire(env, released);
        }
        Ok(0)
    })())
}

fn attach_context(env: &mut Env<'_>, device: u32, flags: u32) -> Result<u32, Error> {
    let table = table(env);
    let context = alloc(env, context::FACE.0)?;
    if let Err(error) = table.attach_context(&mut env.sys.objects, context, device) {
        free(env, context);
        return Err(error.into());
    }
    table.retain_bound(&mut env.sys.objects, context)?;
    let state = ops::object_mut(env, table, device)?
        .device_state_mut()
        .unwrap();
    state.immediate = context;
    state.flags = flags;
    Ok(context)
}

pub(super) fn report<'a>(env: &'a Env<'_>, device: u32) -> Result<&'a Report, Error> {
    ops::object(env, table(env), device)?
        .report()
        .ok_or(Error::InvalidArgument)
}

fn resource<T: Pod>(
    env: &mut Env<'_>,
    validate: impl FnOnce(T, bool, &Report, u64) -> Result<Resource, super::super::desc::Error>,
) -> Outcome {
    let (device, desc, initial, out) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    status((|| {
        let max_size = env.ext.get::<State>().devices.max_buffer_size;
        let resource = validate(
            read(env, desc)?,
            initial != 0,
            report(env, device)?,
            max_size,
        )?;
        let initial = if initial == 0 {
            Vec::new()
        } else {
            array::<D3D11_SUBRESOURCE_DATA>(env, initial, resource.subresources())?
        };
        output(env, out, |env| {
            attach_initialized_resource(env, device, resource, &initial)
        })
    })())
}

fn attach_initialized_resource(
    env: &mut Env<'_>,
    device: u32,
    resource: Resource,
    initial: &[D3D11_SUBRESOURCE_DATA],
) -> Result<u32, Error> {
    let data = initial
        .iter()
        .enumerate()
        .map(|(subresource, initial)| {
            let (row, rows, depth) = match resource {
                Resource::Buffer(buffer) => (buffer.size(), 1, 1),
                Resource::Texture(texture) => {
                    let [width, height, depth] =
                        texture.extent(subresource as u32 % texture.levels());
                    let format = texture.format();
                    (
                        width.div_ceil(u32::from(format.alignment[0]))
                            * u32::from(format.aligned_unit_bytes),
                        height.div_ceil(u32::from(format.alignment[1])),
                        depth,
                    )
                }
            };
            let pitch = if rows == 1 { row } else { initial.SysMemPitch };
            let last_row = pitch
                .checked_mul(rows - 1)
                .and_then(|bytes| bytes.checked_add(row))
                .ok_or(Error::InvalidArgument)?;
            let slice = if depth == 1 {
                last_row
            } else {
                initial.SysMemSlicePitch
            };
            if pitch < row || slice < last_row {
                return Err(Error::InvalidArgument);
            }
            let bytes = slice
                .checked_mul(depth - 1)
                .and_then(|bytes| bytes.checked_add(last_row))
                .ok_or(Error::InvalidArgument)?;
            let source = array::<u8>(env, initial.pSysMem, bytes)?;
            let mut packed = Vec::with_capacity(row as usize * rows as usize * depth as usize);
            for z in 0..depth {
                for y in 0..rows {
                    let start = (z * slice + y * pitch) as usize;
                    packed.extend_from_slice(&source[start..start + row as usize]);
                }
            }
            Ok(packed)
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let pointer = attach_resource(env, device, resource)?;
    let object = table(env).get_mut(&mut env.sys.objects, pointer).unwrap();
    if let Some(storage) = object.mapping() {
        for (subresource, bytes) in data.iter().enumerate() {
            storage.initialize(subresource as u32, bytes, |address, data| {
                env.cpu
                    .write(address, data)
                    .map_err(|_| Error::InvalidArgument)
            })?;
        }
    } else {
        object.set_initial_data(data);
    }
    Ok(pointer)
}

fn buffer(env: &mut Env<'_>) -> Outcome {
    resource(env, |desc, initial, report, max_size| {
        Buffer::new(desc, initial, report, max_size).map(Resource::Buffer)
    })
}

macro_rules! texture {
    ($method:ident, $constructor:ident) => {
        fn $method(env: &mut Env<'_>) -> Outcome {
            resource(env, |desc, initial, report, _| {
                Texture::$constructor(desc, initial, report).map(Resource::Texture)
            })
        }
    };
}
texture!(texture_1d, new_1d);
texture!(texture_2d, new_2d);
texture!(texture_3d, new_3d);

fn input_layout(env: &mut Env<'_>) -> Outcome {
    let [device, elements, count, bytecode, size, out] =
        std::array::from_fn(|index| env.arg(index as u32));
    status((|| {
        if count > 32 {
            return Err(Error::InvalidArgument);
        }
        let entries = array::<D3D11_INPUT_ELEMENT_DESC>(env, elements, count)?
            .into_iter()
            .map(|entry| {
                let mut name = Vec::new();
                for offset in 0..256 {
                    let address = entry
                        .SemanticName
                        .checked_add(offset)
                        .ok_or(Error::InvalidArgument)?;
                    let byte: u8 = read(env, address)?;
                    if byte == 0 {
                        return Ok((name, entry));
                    }
                    name.push(byte);
                }
                Err(Error::InvalidArgument)
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let signature = array::<u8>(env, bytecode, size)?;
        let layout = InputLayout::new(entries, &signature, report(env, device)?)?;
        output(env, out, |env| {
            let pointer = alloc(env, "ID3D11InputLayout")?;
            if let Err(error) =
                table(env).attach_input_layout(&mut env.sys.objects, pointer, device, layout)
            {
                free(env, pointer);
                return Err(error.into());
            }
            Ok(pointer)
        })
    })())
}

fn view<T: Pod + Into<ViewDesc>>(env: &mut Env<'_>, kind: ViewKind) -> Outcome {
    let (device, resource, desc, out) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    status((|| {
        let object = ops::object(env, table(env), resource)?;
        if object.device() != Some(device) {
            return Err(Error::InvalidArgument);
        }
        let description = object.resource().ok_or(Error::InvalidArgument)?;
        let desc = (desc != 0)
            .then(|| read::<T>(env, desc).map(Into::into))
            .transpose()?;
        let view = View::new(kind, description, desc, report(env, device)?)?;
        output(env, out, |env| Ok(attach_view(env, resource, view)?))
    })())
}

fn srv(env: &mut Env<'_>) -> Outcome {
    view::<D3D11_SHADER_RESOURCE_VIEW_DESC>(env, ViewKind::ShaderResource)
}
fn uav(env: &mut Env<'_>) -> Outcome {
    view::<D3D11_UNORDERED_ACCESS_VIEW_DESC>(env, ViewKind::UnorderedAccess)
}
fn rtv(env: &mut Env<'_>) -> Outcome {
    view::<D3D11_RENDER_TARGET_VIEW_DESC>(env, ViewKind::RenderTarget)
}
fn dsv(env: &mut Env<'_>) -> Outcome {
    view::<D3D11_DEPTH_STENCIL_VIEW_DESC>(env, ViewKind::DepthStencil)
}

fn shader(env: &mut Env<'_>, stage: Stage, face: &str) -> Outcome {
    let (device, bytes, length, linkage, out) =
        (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    status((|| {
        let bytecode = array::<u8>(env, bytes, length)?;
        let shader = Shader::new(stage, &bytecode, linkage != 0, report(env, device)?)?;
        output(env, out, |env| {
            let pointer = alloc(env, face)?;
            if let Err(error) =
                table(env).attach_shader(&mut env.sys.objects, pointer, device, shader)
            {
                free(env, pointer);
                return Err(error.into());
            }
            Ok(pointer)
        })
    })())
}

fn vertex_shader(env: &mut Env<'_>) -> Outcome {
    shader(env, Stage::Vertex, "ID3D11VertexShader")
}
fn pixel_shader(env: &mut Env<'_>) -> Outcome {
    shader(env, Stage::Pixel, "ID3D11PixelShader")
}
fn compute_shader(env: &mut Env<'_>) -> Outcome {
    shader(env, Stage::Compute, "ID3D11ComputeShader")
}

fn pipeline<T: Pod>(
    env: &mut Env<'_>,
    validate: impl FnOnce(T, FeatureLevel) -> Result<PipelineState, super::super::desc::Error>,
) -> Outcome {
    let (device, description, out) = (env.arg(0), env.arg(1), env.arg(2));
    status((|| {
        let state = validate(read(env, description)?, report(env, device)?.level)?;
        output(env, out, |env| Ok(attach_pipeline(env, device, state)?))
    })())
}

fn blend(env: &mut Env<'_>) -> Outcome {
    pipeline(env, PipelineState::blend)
}
fn blend1(env: &mut Env<'_>) -> Outcome {
    pipeline(env, PipelineState::blend1)
}
fn depth_stencil(env: &mut Env<'_>) -> Outcome {
    pipeline(env, |desc, _| PipelineState::depth_stencil(desc))
}
fn rasterizer(env: &mut Env<'_>) -> Outcome {
    pipeline(env, PipelineState::rasterizer)
}
fn rasterizer1(env: &mut Env<'_>) -> Outcome {
    pipeline(env, PipelineState::rasterizer1)
}
fn sampler(env: &mut Env<'_>) -> Outcome {
    pipeline(env, PipelineState::sampler)
}

fn format_support(env: &mut Env<'_>) -> Outcome {
    let (device, format, out) = (env.arg(0), env.arg(1), env.arg(2));
    call((|| {
        let support = report(env, device)?.check_format(format);
        write(env, out, support.unwrap_or(0))?;
        Ok(support.map(|_| ())?)
    })())
}

fn multisample_quality(env: &mut Env<'_>) -> Outcome {
    let (device, format, count, out) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    call((|| {
        let quality = report(env, device)?.multisample_quality(format, count)?;
        write(env, out, quality)
    })())
}

fn counter_info(env: &mut Env<'_>) -> Outcome {
    let (device, out) = (env.arg(0), env.arg(1));
    call((|| {
        let info = report(env, device)?.counter_info();
        write(env, out, info)
    })())
}

fn check_counter(env: &mut Env<'_>) -> Outcome {
    call((|| {
        read::<D3D11_COUNTER_DESC>(env, env.arg(1))?;
        Ok(report(env, env.arg(0))?.check_counter()?)
    })())
}

fn feature_support(env: &mut Env<'_>) -> Outcome {
    let (device, feature, data, size) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    call((|| {
        let format = if matches!(feature, 2 | 3) {
            read(env, data)?
        } else {
            0
        };
        let answer = report(env, device)?.check_feature(feature, size as usize, format)?;
        env.cpu
            .write(data, bytemuck::cast_slice(answer.words()))
            .map_err(|_| Error::InvalidArgument)
    })())
}

fn feature_level(env: &mut Env<'_>) -> Outcome {
    status(report(env, env.arg(0)).map(|report| report.level as u32))
}

fn creation_flags(env: &mut Env<'_>) -> Outcome {
    status(device_state(env).map(|state| state.flags))
}

fn removed_reason(env: &mut Env<'_>) -> Outcome {
    call((|| {
        device_state(env)?;
        #[cfg(feature = "gpu")]
        gpu::check(env.ext)?;
        Ok(())
    })())
}

fn immediate_context(env: &mut Env<'_>) -> Outcome {
    related(env, Object::immediate)
}

fn set_exception_mode(env: &mut Env<'_>) -> Outcome {
    let (device, flags) = (env.arg(0), env.arg(1));
    call((|| {
        if flags & !1 != 0 {
            return Err(Error::InvalidArgument);
        }
        let state = ops::object_mut(env, table(env), device)?
            .device_state_mut()
            .ok_or(Error::InvalidArgument)?;
        state.exception_mode = flags;
        Ok(())
    })())
}

fn exception_mode(env: &mut Env<'_>) -> Outcome {
    status(device_state(env).map(|state| state.exception_mode))
}

fn device_state<'a>(env: &'a Env<'_>) -> Result<&'a super::super::object::Device, Error> {
    ops::object(env, table(env), env.arg(0))?
        .device_state()
        .ok_or(Error::InvalidArgument)
}

fn unsupported_output(env: &mut Env<'_>, slot: u32, error: u32) -> Outcome {
    let output = env.arg(slot);
    if output != 0 {
        let _ = write(env, output, 0u32);
    }
    Outcome::Return(error)
}

fn excluded_stage(env: &mut Env<'_>) -> Outcome {
    unsupported_output(env, 4, 0x80070057)
}
fn excluded_so_shader(env: &mut Env<'_>) -> Outcome {
    unsupported_output(env, 9, 0x80070057)
}
fn excluded_linkage(env: &mut Env<'_>) -> Outcome {
    unsupported_output(env, 1, 0x80004001)
}
fn excluded_deferred(env: &mut Env<'_>) -> Outcome {
    unsupported_output(env, 2, 0x80004001)
}
fn open_shared(env: &mut Env<'_>) -> Outcome {
    unsupported_output(env, 3, 0x80070057)
}
fn excluded_context_state(env: &mut Env<'_>) -> Outcome {
    adv_abi::stub::warn_unavailable(env, "CreateDeviceContextState", "device context state");
    unsupported_output(env, 7, 0x80004001)
}

fn open_shared_by_name(env: &mut Env<'_>) -> Outcome {
    unsupported_output(env, 4, 0x80070057)
}

fn create_query(env: &mut Env<'_>) -> Outcome {
    new_query(env, false)
}

fn create_predicate(env: &mut Env<'_>) -> Outcome {
    new_query(env, true)
}

fn new_query(env: &mut Env<'_>, predicate: bool) -> Outcome {
    use super::super::object::query::Query;
    let (device, desc, out) = (env.arg(0), env.arg(1), env.arg(2));
    status((|| {
        report(env, device)?;
        let query = Query::new(read(env, desc)?, predicate)?;
        let face = if predicate {
            "ID3D11Predicate"
        } else {
            "ID3D11Query"
        };
        output(env, out, |env| {
            let pointer = alloc(env, face)?;
            if let Err(error) =
                table(env).attach_query(&mut env.sys.objects, pointer, device, query)
            {
                free(env, pointer);
                return Err(error.into());
            }
            Ok(pointer)
        })
    })())
}

pub(super) fn query_size(env: &mut Env<'_>) -> Outcome {
    status(ops::object(env, table(env), env.arg(0)).and_then(|object| {
        object
            .query()
            .map(|query| query.kind.size())
            .ok_or(Error::InvalidArgument)
    }))
}

pub(super) fn query_desc(env: &mut Env<'_>) -> Outcome {
    let (pointer, out) = (env.arg(0), env.arg(1));
    call((|| {
        let query = ops::object(env, table(env), pointer)?
            .query()
            .ok_or(Error::InvalidArgument)?;
        write(env, out, query.desc)
    })())
}

pub(super) const MULTITHREAD: (&str, &[Slot]) = (
    "ID3D11Multithread",
    &[
        (
            "ID3D11Multithread::QueryInterface",
            query,
            ImplementationLevel::Implement,
        ),
        (
            "ID3D11Multithread::AddRef",
            add_ref,
            ImplementationLevel::Implement,
        ),
        (
            "ID3D11Multithread::Release",
            release,
            ImplementationLevel::Implement,
        ),
        (
            "ID3D11Multithread::Enter",
            enter,
            ImplementationLevel::Implement,
        ),
        (
            "ID3D11Multithread::Leave",
            leave,
            ImplementationLevel::Implement,
        ),
        (
            "ID3D11Multithread::SetMultithreadProtected",
            set_protected,
            ImplementationLevel::State,
        ),
        (
            "ID3D11Multithread::GetMultithreadProtected",
            protected,
            ImplementationLevel::State,
        ),
    ],
);

fn attach_multithread(env: &mut Env<'_>, device: u32, flags: u32) -> Result<(), Error> {
    use adv_abi::gfx_iids::{IID_ID3D10MULTITHREAD, IID_ID3D11MULTITHREAD};
    let face = alloc(env, MULTITHREAD.0)?;
    let table = table(env);
    table.register_faces(
        &mut env.sys.objects,
        device,
        face,
        &[IID_ID3D11MULTITHREAD, IID_ID3D10MULTITHREAD],
    );
    let handle = env.sys.objects.insert(adv_machine::Object::Mutex {
        owner: None,
        recursion: 0,
    });
    let state = table
        .get_mut(&mut env.sys.objects, device)
        .unwrap()
        .device_state_mut()
        .unwrap();
    state.thread_lock = handle;
    state.multithreaded = flags & 1 == 0;
    Ok(())
}

fn enter(env: &mut Env<'_>) -> Outcome {
    match device_state(env) {
        Ok(state) => Outcome::block_then_return(
            adv_machine::Wait::Object {
                handle: state.thread_lock,
                deadline: None,
            },
            0,
        ),
        Err(error) => Outcome::Return(error.hresult()),
    }
}
fn leave(env: &mut Env<'_>) -> Outcome {
    call((|| {
        let mutex = device_state(env)?.thread_lock;
        if matches!(env.sys.objects.get(mutex), Some(adv_machine::Object::Mutex { owner: Some(owner), .. }) if *owner == env.context)
        {
            env.sys.release_mutex(mutex);
        }
        Ok(())
    })())
}
fn set_protected(env: &mut Env<'_>) -> Outcome {
    let (pointer, value) = (env.arg(0), env.arg(1) != 0);
    status((|| {
        let state = ops::object_mut(env, table(env), pointer)?
            .device_state_mut()
            .ok_or(Error::InvalidArgument)?;
        Ok(u32::from(std::mem::replace(
            &mut state.multithreaded,
            value,
        )))
    })())
}
fn protected(env: &mut Env<'_>) -> Outcome {
    status(device_state(env).map(|state| u32::from(state.multithreaded)))
}

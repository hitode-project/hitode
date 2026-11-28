//! IDXGIDevice shares the owning D3D11 object's COM identity.
use super::*;
use crate::dxgi::{self, caps::Adapter};
use adv_abi::gfx_iids::*;

macro_rules! methods {
    ($($method:ident => $handler:ident $level:ident),+ $(,)?) => {
        ("IDXGIDevice3", &[
            $((concat!("IDXGIDevice3::", stringify!($method)), $handler, ImplementationLevel::$level)),+
        ])
    };
}

pub(super) const FACE: (&str, &[Slot]) = methods!(
    QueryInterface => query Implement,
    AddRef => add_ref Implement,
    Release => release Implement,
    SetPrivateData => set_private Implement,
    SetPrivateDataInterface => set_interface Implement,
    GetPrivateData => get_private Implement,
    GetParent => parent Implement,
    GetAdapter => adapter Implement,
    CreateSurface => create_surface Implement,
    QueryResourceResidency => residency Implement,
    SetGPUThreadPriority => set_priority State,
    GetGPUThreadPriority => get_priority State,
    SetMaximumFrameLatency => set_latency State,
    GetMaximumFrameLatency => get_latency State,
    OfferResources => offer_resources Implement,
    ReclaimResources => reclaim_resources Implement,
    EnqueueSetEvent => enqueue_event Implement,
    Trim => trim State,
);

macro_rules! resource_methods {
    ($($method:ident => $handler:ident $level:ident),+ $(,)?) => {
        ("IDXGIResource1", &[
            $((concat!("IDXGIResource1::", stringify!($method)), $handler, ImplementationLevel::$level)),+
        ])
    };
}

pub(super) const RESOURCE: (&str, &[Slot]) = resource_methods!(
    QueryInterface => query Implement,
    AddRef => add_ref Implement,
    Release => release Implement,
    SetPrivateData => set_private Implement,
    SetPrivateDataInterface => set_interface Implement,
    GetPrivateData => get_private Implement,
    GetParent => surface_device Implement,
    GetDevice => surface_device Implement,
    GetSharedHandle => shared_handle Implement,
    GetUsage => resource_usage State,
    SetEvictionPriority => set_priority State,
    GetEvictionPriority => eviction_priority State,
    CreateSubresourceSurface => subresource_surface Implement,
    CreateSharedHandle => create_shared_handle Implement,
);

pub(super) const KEYED_MUTEX: (&str, &[Slot]) = (
    "IDXGIKeyedMutex",
    &[
        (
            "IDXGIKeyedMutex::QueryInterface",
            query,
            ImplementationLevel::Implement,
        ),
        (
            "IDXGIKeyedMutex::AddRef",
            add_ref,
            ImplementationLevel::Implement,
        ),
        (
            "IDXGIKeyedMutex::Release",
            release,
            ImplementationLevel::Implement,
        ),
        (
            "IDXGIKeyedMutex::SetPrivateData",
            set_private,
            ImplementationLevel::Implement,
        ),
        (
            "IDXGIKeyedMutex::SetPrivateDataInterface",
            set_interface,
            ImplementationLevel::Implement,
        ),
        (
            "IDXGIKeyedMutex::GetPrivateData",
            get_private,
            ImplementationLevel::Implement,
        ),
        (
            "IDXGIKeyedMutex::GetParent",
            surface_device,
            ImplementationLevel::Implement,
        ),
        (
            "IDXGIKeyedMutex::GetDevice",
            surface_device,
            ImplementationLevel::Implement,
        ),
        (
            "IDXGIKeyedMutex::AcquireSync",
            acquire_sync,
            ImplementationLevel::Implement,
        ),
        (
            "IDXGIKeyedMutex::ReleaseSync",
            release_sync,
            ImplementationLevel::Implement,
        ),
    ],
);

pub(super) fn attach_resource_face(env: &mut Env<'_>, resource: u32) -> Result<(), ObjectError> {
    use super::super::desc::Misc;
    let face = alloc(env, RESOURCE.0)?;
    let table = table(env);
    let description = table
        .get(&env.sys.objects, resource)
        .unwrap()
        .resource()
        .unwrap();
    let has_surface = matches!(description,
        Resource::Texture(texture) if texture.kind() == TextureKind::D2
            && texture.levels() == 1 && texture.layers() == 1);
    let iids: &[_] = if has_surface {
        &[IID_IDXGIRESOURCE, IID_IDXGIRESOURCE1]
    } else {
        &[
            IID_IDXGIRESOURCE,
            IID_IDXGIRESOURCE1,
            IID_IDXGIOBJECT,
            IID_IDXGIDEVICESUBOBJECT,
        ]
    };
    table.register_faces(&mut env.sys.objects, resource, face, iids);
    if description.misc().contains(Misc::SHARED_KEYEDMUTEX) {
        let face = alloc(env, KEYED_MUTEX.0)?;
        table.register_faces(&mut env.sys.objects, resource, face, &[IID_IDXGIKEYEDMUTEX]);
    }
    Ok(())
}

fn subresource_surface(env: &mut Env<'_>) -> Outcome {
    let (resource, subresource, out) = (env.arg(0), env.arg(1), env.arg(2));
    call((|| {
        write(env, out, 0u32)?;
        if subresource != 0 {
            return Err(dxgi::Error::InvalidCall.into());
        }
        let surface = table(env).query(&mut env.sys.objects, resource, IID_IDXGISURFACE2)?;
        write(env, out, surface)
    })())
}

macro_rules! sharing_methods {
    ($($name:ident),+ $(,)?) => {$(
        fn $name(env: &mut Env<'_>) -> Outcome {
            #[cfg(feature = "gpu")]
            return shared::$name(env);
            #[cfg(not(feature = "gpu"))]
            {
                let _ = env;
                Outcome::Return(dxgi::Error::InvalidCall.hresult())
            }
        }
    )+};
}
sharing_methods!(create_shared_handle, acquire_sync, release_sync);

fn resource_usage(env: &mut Env<'_>) -> Outcome {
    use super::super::desc::Bind;
    let (pointer, out) = (env.arg(0), env.arg(1));
    call((|| {
        let desc = ops::object(env, table(env), pointer)?.resource().unwrap();
        let mut usage = 0;
        for (bind, flag) in [
            (Bind::SHADER, 16),
            (Bind::RENDER_TARGET, 32),
            (Bind::UNORDERED, 1024),
        ] {
            if desc.bind().contains(bind) {
                usage |= flag;
            }
        }
        if desc.misc().shared() {
            usage |= 128;
        }
        write(env, out, usage)
    })())
}

fn eviction_priority(env: &mut Env<'_>) -> Outcome {
    let (pointer, out) = (env.arg(0), env.arg(1));
    call((|| {
        let priority = ops::object(env, table(env), pointer)?.priority().unwrap();
        write(env, out, priority)
    })())
}

fn shared_handle(env: &mut Env<'_>) -> Outcome {
    let (pointer, out) = (env.arg(0), env.arg(1));
    call((|| {
        use super::super::desc::Misc;
        let resource = ops::object(env, table(env), pointer)?.resource().unwrap();
        if resource.misc().contains(Misc::SHARED_NTHANDLE) {
            return Err(Error::InvalidArgument);
        }
        if !resource.misc().shared() {
            return write(env, out, 0u32);
        }
        #[cfg(feature = "gpu")]
        {
            let handle = shared::legacy_handle(env, pointer)?;
            write(env, out, handle)
        }
        #[cfg(not(feature = "gpu"))]
        Err(dxgi::Error::InvalidCall.into())
    })())
}

pub(super) fn attach_device_face(
    env: &mut Env<'_>,
    device: u32,
    adapter: Adapter,
) -> Result<(), Error> {
    let face = alloc(env, FACE.0)?;
    let table = table(env);
    table.register_faces(
        &mut env.sys.objects,
        device,
        face,
        &[
            IID_IDXGIOBJECT,
            IID_IDXGIDEVICE,
            IID_IDXGIDEVICE1,
            IID_IDXGIDEVICE2,
            IID_IDXGIDEVICE3,
        ],
    );
    let pointer = dxgi::create_device_adapter(env, adapter)?;
    let state = ops::object_mut(env, table, device)?
        .device_state_mut()
        .unwrap();
    state.adapter = pointer;
    state.null_device = adapter == Adapter::Null;
    Ok(())
}

fn get_state<'a>(env: &'a Env<'_>) -> Result<&'a super::super::object::Device, Error> {
    ops::object(env, table(env), env.arg(0))?
        .device_state()
        .ok_or(Error::InvalidArgument)
}

fn adapter(env: &mut Env<'_>) -> Outcome {
    let out = env.arg(1);
    match get_state(env).map(|state| state.adapter) {
        Ok(pointer) => retain_adapter(env, pointer, IID_IDXGIADAPTER, out),
        Err(error) => Outcome::Return(error.hresult()),
    }
}

fn parent(env: &mut Env<'_>) -> Outcome {
    let out = env.arg(2);
    match (get_state(env), read(env, env.arg(1))) {
        (Ok(state), Ok(iid)) => retain_adapter(env, state.adapter, iid, out),
        (Err(error), _) | (_, Err(error)) => Outcome::Return(error.hresult()),
    }
}

fn retain_adapter(env: &mut Env<'_>, adapter: u32, iid: Iid, out: u32) -> Outcome {
    call((|| {
        write(env, out, 0u32)?;
        let pointer = dxgi::query_adapter(env, adapter, iid)?;
        write(env, out, pointer)
    })())
}

fn residency(env: &mut Env<'_>) -> Outcome {
    let (this, pointers, output, count) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    call((|| {
        validate_resources(env, this, pointers, count)?;
        for index in 0..count {
            write(env, output + 4 * index, 1u32)?;
        }
        Ok(())
    })())
}

fn validate_resources(env: &Env<'_>, device: u32, pointers: u32, count: u32) -> Result<(), Error> {
    let table = table(env);
    let owner = table
        .canonical(&env.sys.objects, device)
        .ok_or(Error::InvalidArgument)?;
    for resource in array::<u32>(env, pointers, count)? {
        let object = ops::object(env, table, resource)?;
        if object.device() != Some(owner) || object.resource().is_none() {
            return Err(Error::InvalidArgument);
        }
    }
    Ok(())
}

fn offer_resources(env: &mut Env<'_>) -> Outcome {
    let [device, count, pointers, priority] = std::array::from_fn(|i| env.arg(i as u32));
    // Offering permits reclamation of the backing. Retain its contents.
    call(if (1..=3).contains(&priority) {
        validate_resources(env, device, pointers, count)
    } else {
        Err(Error::InvalidArgument)
    })
}

fn reclaim_resources(env: &mut Env<'_>) -> Outcome {
    let [device, count, pointers, output] = std::array::from_fn(|i| env.arg(i as u32));
    call((|| {
        validate_resources(env, device, pointers, count)?;
        if output != 0 {
            for index in 0..count {
                write(env, output + 4 * index, 0u32)?;
            }
        }
        Ok(())
    })())
}

fn enqueue_event(env: &mut Env<'_>) -> Outcome {
    #[cfg(feature = "gpu")]
    return call((|| {
        let event = env.arg(1);
        if event == 0 {
            return Err(Error::InvalidArgument);
        }
        if get_state(env)?.null_device {
            let signal = env.ext.get::<State>().signal_event;
            return signal(env, event)
                .then_some(())
                .ok_or(Error::InvalidArgument);
        }
        gpu::enqueue_event(env.ext, event)
    })());
    #[cfg(not(feature = "gpu"))]
    {
        let _ = env;
        Outcome::Return(dxgi::Error::Unavailable.hresult())
    }
}

fn trim(env: &mut Env<'_>) -> Outcome {
    #[cfg(feature = "gpu")]
    if let Ok(execution) = gpu::execution(env.ext) {
        execution.trim();
    }
    #[cfg(not(feature = "gpu"))]
    let _ = env;
    Outcome::Return(0)
}

fn set_priority(env: &mut Env<'_>) -> Outcome {
    let (this, value) = (env.arg(0), env.arg(1) as i32);
    call((|| {
        if !(-7..=7).contains(&value) {
            return Err(Error::InvalidArgument);
        }
        let state = ops::object_mut(env, table(env), this)?
            .device_state_mut()
            .ok_or(Error::InvalidArgument)?;
        state.priority = value;
        Ok(())
    })())
}

fn get_priority(env: &mut Env<'_>) -> Outcome {
    let out = env.arg(1);
    call((|| {
        let value = get_state(env)?.priority;
        write(env, out, value)
    })())
}

fn set_latency(env: &mut Env<'_>) -> Outcome {
    let (this, value) = (env.arg(0), env.arg(1));
    call((|| {
        if value > 16 {
            return Err(Error::InvalidArgument);
        }
        let state = ops::object_mut(env, table(env), this)?
            .device_state_mut()
            .ok_or(Error::InvalidArgument)?;
        state.latency = if value == 0 { 3 } else { value };
        Ok(())
    })())
}

fn get_latency(env: &mut Env<'_>) -> Outcome {
    let out = env.arg(1);
    call((|| {
        let value = get_state(env)?.latency;
        write(env, out, value)
    })())
}

fn create_surface(env: &mut Env<'_>) -> Outcome {
    use super::super::desc::{Bind, Texture};
    use adv_abi::types::DXGI_SURFACE_DESC;
    let [face, desc, count, usage, shared, out] =
        std::array::from_fn(|index| env.arg(index as u32));
    call((|| {
        if count == 0 || shared != 0 || usage & !(16 | 32 | 256 | 1024) != 0 {
            return Err(Error::Dxgi(dxgi::Error::InvalidCall));
        }
        let desc: DXGI_SURFACE_DESC = read(env, desc)?;
        let table = table(env);
        let device = table
            .canonical(&env.sys.objects, face)
            .ok_or(Error::InvalidArgument)?;
        let report = device::report(env, device)?;
        let staging = usage & 256 != 0;
        let bind = (if usage & 16 != 0 { Bind::SHADER } else { 0 })
            | (if usage & 32 != 0 {
                Bind::RENDER_TARGET
            } else {
                0
            })
            | (if usage & 1024 != 0 {
                Bind::UNORDERED
            } else {
                0
            });
        let texture = Texture::new_2d(
            D3D11_TEXTURE2D_DESC {
                Width: desc.Width,
                Height: desc.Height,
                MipLevels: 1,
                ArraySize: 1,
                Format: desc.Format,
                SampleDesc: desc.SampleDesc,
                Usage: if staging { 3 } else { 0 },
                BindFlags: bind,
                CPUAccessFlags: if staging { 0x20000 } else { 0 },
                ..Default::default()
            },
            false,
            report,
        )?;
        let bytes = count.checked_mul(4).ok_or(Error::InvalidArgument)?;
        adv_abi::guest::check_range(env.cpu, out, bytes, Prot::WRITE)
            .map_err(|_| Error::InvalidArgument)?;
        let mut surfaces = Vec::new();
        for _ in 0..count {
            let resource = match attach_resource(env, device, Resource::Texture(texture)) {
                Ok(pointer) => pointer,
                Err(error) => {
                    for pointer in surfaces {
                        let released = table.release(&mut env.sys.objects, pointer);
                        retire(env, released);
                    }
                    return Err(error.into());
                }
            };
            let face = table.query(&mut env.sys.objects, resource, IID_IDXGISURFACE)?;
            let released = table.release(&mut env.sys.objects, resource);
            retire(env, released);
            surfaces.push(face);
        }
        env.cpu
            .write(out, bytemuck::cast_slice(&surfaces))
            .map_err(|_| Error::InvalidArgument)
    })())
}

#[cfg(feature = "gpu")]
pub(super) struct SwapBuffers;

#[cfg(feature = "gpu")]
impl dxgi::swap::Buffers for SwapBuffers {
    fn identity(&self, env: &Env<'_>, device: u32) -> Result<u32, dxgi::Error> {
        device_state(env, device)?;
        table(env)
            .canonical(&env.sys.objects, device)
            .ok_or(dxgi::Error::InvalidArgument)
    }

    fn adapter(&self, env: &Env<'_>, device: u32) -> Result<Adapter, dxgi::Error> {
        dxgi::adapter_kind(env, device_state(env, device)?.adapter)
    }

    fn surface_owner(&self, env: &Env<'_>, pointer: u32) -> Result<(u32, u32), dxgi::Error> {
        let table = table(env);
        let surface = table
            .canonical(&env.sys.objects, pointer)
            .ok_or(dxgi::Error::InvalidCall)?;
        let object = table.get(&env.sys.objects, surface).unwrap();
        if !matches!(object.resource(), Some(Resource::Texture(texture)) if texture.kind() == TextureKind::D2)
        {
            return Err(dxgi::Error::InvalidCall);
        }
        Ok((surface, object.device().unwrap()))
    }

    fn scanout(
        &self,
        env: &mut Env<'_>,
        surface: u32,
        serial: u64,
    ) -> Result<Option<u64>, dxgi::Error> {
        let Some(Resource::Texture(texture)) = table(env)
            .get(&env.sys.objects, surface)
            .and_then(Object::resource)
        else {
            return Err(dxgi::Error::InvalidCall);
        };
        let [width, height, _] = texture.extent(0);
        self.present(env, surface, [width, height], serial, true, false)
            .map(|(_, completed)| completed)
    }

    fn capture(&self, env: &mut Env<'_>) -> Result<adv_device::frame::Waker, dxgi::Error> {
        gpu::capture(env.ext).map_err(|_| dxgi::Error::Unavailable)
    }

    fn copy_display(&self, env: &mut Env<'_>, surface: u32) -> Result<(), dxgi::Error> {
        let resource = table(env)
            .get(&env.sys.objects, surface)
            .unwrap()
            .resource()
            .unwrap();
        if !resource.cpu_access().write() {
            return Err(dxgi::Error::InvalidCall);
        }
        let frame = self
            .capture(env)?
            .capture()
            .recv()
            .map_err(|_| dxgi::Error::Unavailable)?
            .map_err(|_| dxgi::Error::Unavailable)?;
        gpu::copy_capture(env, surface, resource, &frame).map_err(capture_error)
    }

    fn desktop_resource(
        &self,
        env: &mut Env<'_>,
        device: u32,
        mode: adv_abi::types::DXGI_MODE_DESC,
        frame: &adv_device::frame::Captured,
    ) -> Result<u32, dxgi::Error> {
        let report = device::report(env, device).map_err(capture_error)?;
        let texture = super::super::desc::Texture::new_2d(
            D3D11_TEXTURE2D_DESC {
                Width: mode.Width,
                Height: mode.Height,
                MipLevels: 1,
                ArraySize: 1,
                Format: mode.Format,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BindFlags: super::super::desc::Bind::SHADER,
                ..Default::default()
            },
            false,
            report,
        )
        .map_err(Error::from)
        .map_err(capture_error)?;
        let description = Resource::Texture(texture);
        let resource = attach_resource(env, device, description).map_err(com_error)?;
        let table = table(env);
        let result = gpu::copy_capture(env, resource, description, frame)
            .map_err(capture_error)
            .and_then(|()| {
                table
                    .query(&mut env.sys.objects, resource, IID_IDXGIRESOURCE)
                    .map_err(com_error)
            });
        let released = table.release(&mut env.sys.objects, resource);
        retire(env, released);
        result
    }

    fn drop_device_ref(&self, env: &mut Env<'_>, device: u32) {
        let released = table(env).release(&mut env.sys.objects, device);
        retire(env, released);
    }

    fn caps<'a>(&self, ext: &'a adv_machine::Ext) -> Result<&'a adv_gpu::Caps, dxgi::Error> {
        gpu::caps(ext).ok_or(dxgi::Error::Unsupported)
    }

    fn retain_device(&self, env: &mut Env<'_>, device: u32) -> Result<(), dxgi::Error> {
        table(env)
            .add_ref(&mut env.sys.objects, device)
            .map(|_| ())
            .map_err(com_error)
    }

    fn create(
        &self,
        env: &mut Env<'_>,
        device: u32,
        desc: adv_abi::types::DXGI_SWAP_CHAIN_DESC1,
    ) -> Result<Vec<u32>, dxgi::Error> {
        use super::super::desc::{Bind, Misc, Texture};
        let device = table(env)
            .canonical(&env.sys.objects, device)
            .ok_or(dxgi::Error::InvalidArgument)?;
        let report = device::report(env, device).map_err(|_| dxgi::Error::InvalidArgument)?;
        let texture = Texture::new_2d(
            D3D11_TEXTURE2D_DESC {
                Width: desc.Width,
                Height: desc.Height,
                MipLevels: 1,
                ArraySize: 1,
                Format: desc.Format,
                SampleDesc: desc.SampleDesc,
                BindFlags: Bind::RENDER_TARGET
                    | if desc.BufferUsage & 16 != 0 {
                        Bind::SHADER
                    } else {
                        0
                    },
                MiscFlags: if desc.Flags & 4 != 0 {
                    Misc::GDI_COMPATIBLE
                } else {
                    0
                },
                ..Default::default()
            },
            false,
            report,
        )
        .map_err(|_| dxgi::Error::InvalidArgument)?;
        let mut result = Vec::new();
        for _ in 0..desc.BufferCount {
            let pointer = match attach_resource(env, device, Resource::Texture(texture)) {
                Ok(pointer) => pointer,
                Err(error) => {
                    for pointer in result {
                        let released = table(env).release_bound(&mut env.sys.objects, pointer);
                        retire(env, released);
                    }
                    return Err(com_error(error));
                }
            };
            let table = table(env);
            table
                .retain_bound(&mut env.sys.objects, pointer)
                .map_err(com_error)?;
            let released = table.release(&mut env.sys.objects, pointer);
            retire(env, released);
            result.push(pointer);
        }
        Ok(result)
    }

    fn query(&self, env: &mut Env<'_>, pointer: u32, iid: Iid) -> Result<u32, dxgi::Error> {
        table(env)
            .query(&mut env.sys.objects, pointer, iid)
            .map_err(com_error)
    }

    fn referenced(&self, env: &Env<'_>, pointer: u32) -> bool {
        table(env).buffer_referenced(&env.sys.objects, pointer)
    }

    fn release(&self, env: &mut Env<'_>, pointer: u32, interfaces: &mut Vec<u32>) {
        gpu::release_presentation(env.ext, pointer);
        let released = table(env).release_bound(&mut env.sys.objects, pointer);
        for pointer in released.allocations {
            free(env, pointer);
        }
        interfaces.extend(released.private_interfaces);
    }

    fn present(
        &self,
        env: &mut Env<'_>,
        resource: u32,
        size: [u32; 2],
        serial: u64,
        first: bool,
        composition: bool,
    ) -> Result<(bool, Option<u64>), dxgi::Error> {
        if device_state(
            env,
            table(env)
                .get(&env.sys.objects, resource)
                .and_then(Object::device)
                .unwrap(),
        )?
        .null_device
        {
            return Ok((false, None));
        }
        if composition {
            return gpu::flush_shared(env)
                .map(|()| (false, None))
                .map_err(|_| dxgi::Error::Unavailable);
        }
        gpu::present(env, resource, size, serial, first).map_err(|error| match error {
            adv_device::gpu::Error::Busy => dxgi::Error::StillDrawing,
            _ => dxgi::Error::Unavailable,
        })
    }

    fn text_store(&self, env: &Env<'_>, buffer: u32) -> Option<adv_machine::text_records::Store> {
        #[cfg(feature = "gpu")]
        return Some(gpu::text_store(env, buffer));
        #[cfg(not(feature = "gpu"))]
        {
            let _ = (env, buffer);
            None
        }
    }

    fn latency(&self, env: &Env<'_>, device: u32) -> u32 {
        device_state(env, device).unwrap().latency
    }

    fn present_wake_generation(&self, ext: &adv_machine::Ext) -> Option<u64> {
        Some(
            ext.get::<State>()
                .execution
                .as_ref()?
                .wake
                .as_ref()?
                .generation(),
        )
    }

    fn wait_for_present(
        &self,
        env: &mut Env<'_>,
        generation: u64,
        then: adv_machine::Continuation,
    ) -> Outcome {
        gpu::wait_for_present(env, generation, then)
    }

    fn shared_adapter(&self, env: &Env<'_>, handle: u32) -> Result<Adapter, dxgi::Error> {
        shared::adapter(env, handle).ok_or(dxgi::Error::InvalidSharedHandle)
    }

    fn output(&self, env: &mut Env<'_>, device: u32) -> Result<u32, dxgi::Error> {
        let adapter = device_state(env, device)?.adapter;
        dxgi::device_output(env, adapter)
    }
}

#[cfg(feature = "gpu")]
fn capture_error(error: Error) -> dxgi::Error {
    match error {
        Error::Device => dxgi::Error::Unavailable,
        _ => dxgi::Error::InvalidCall,
    }
}

#[cfg(feature = "gpu")]
fn device_state<'a>(
    env: &'a Env<'_>,
    pointer: u32,
) -> Result<&'a super::super::object::Device, dxgi::Error> {
    table(env)
        .get(&env.sys.objects, pointer)
        .and_then(Object::device_state)
        .ok_or(dxgi::Error::InvalidArgument)
}

#[cfg(feature = "gpu")]
fn com_error(error: ObjectError) -> dxgi::Error {
    match error {
        ObjectError::InvalidObject => dxgi::Error::InvalidArgument,
        ObjectError::NoInterface => dxgi::Error::NoInterface,
        ObjectError::Exhausted | ObjectError::TooManyUniqueStates => dxgi::Error::OutOfMemory,
    }
}

pub(super) const SURFACE: (&str, &[Slot]) = (
    "IDXGISurface2",
    &[
        (
            "IDXGISurface2::QueryInterface",
            query,
            ImplementationLevel::Implement,
        ),
        (
            "IDXGISurface2::AddRef",
            add_ref,
            ImplementationLevel::Implement,
        ),
        (
            "IDXGISurface2::Release",
            release,
            ImplementationLevel::Implement,
        ),
        (
            "IDXGISurface2::SetPrivateData",
            set_private,
            ImplementationLevel::Implement,
        ),
        (
            "IDXGISurface2::SetPrivateDataInterface",
            set_interface,
            ImplementationLevel::Implement,
        ),
        (
            "IDXGISurface2::GetPrivateData",
            get_private,
            ImplementationLevel::Implement,
        ),
        (
            "IDXGISurface2::GetParent",
            surface_device,
            ImplementationLevel::Implement,
        ),
        (
            "IDXGISurface2::GetDevice",
            surface_device,
            ImplementationLevel::Implement,
        ),
        (
            "IDXGISurface2::GetDesc",
            surface_desc,
            ImplementationLevel::State,
        ),
        (
            "IDXGISurface2::Map",
            surface_map,
            ImplementationLevel::Implement,
        ),
        (
            "IDXGISurface2::Unmap",
            surface_unmap,
            ImplementationLevel::State,
        ),
        (
            "IDXGISurface2::GetDC",
            get_dc,
            ImplementationLevel::Implement,
        ),
        (
            "IDXGISurface2::ReleaseDC",
            release_dc,
            ImplementationLevel::Implement,
        ),
        (
            "IDXGISurface2::GetResource",
            surface_resource,
            ImplementationLevel::Implement,
        ),
    ],
);

pub(super) fn attach_surface_face(env: &mut Env<'_>, resource: u32) -> Result<(), ObjectError> {
    let face = alloc(env, SURFACE.0)?;
    table(env).register_faces(
        &mut env.sys.objects,
        resource,
        face,
        &[
            IID_IDXGIOBJECT,
            IID_IDXGIDEVICESUBOBJECT,
            IID_IDXGISURFACE,
            IID_IDXGISURFACE1,
            IID_IDXGISURFACE2,
        ],
    );
    Ok(())
}

fn surface_resource(env: &mut Env<'_>) -> Outcome {
    let (surface, iid, out, subresource) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    call((|| {
        write(env, out, 0u32)?;
        context::optional(env, subresource, 0u32)?;
        let iid = read(env, iid)?;
        let resource = table(env).query(&mut env.sys.objects, surface, iid)?;
        write(env, out, resource)
    })())
}

fn surface_device(env: &mut Env<'_>) -> Outcome {
    let table = table(env);
    let (surface, iid, out) = (env.arg(0), read::<Iid>(env, env.arg(1)), env.arg(2));
    call((|| {
        write(env, out, 0u32)?;
        let device = ops::object(env, table, surface)?
            .device()
            .ok_or(Error::InvalidArgument)?;
        let pointer = table.query(&mut env.sys.objects, device, iid?)?;
        write(env, out, pointer)
    })())
}

fn surface_desc(env: &mut Env<'_>) -> Outcome {
    let (surface, out) = (env.arg(0), env.arg(1));
    call((|| {
        let Resource::Texture(texture) = ops::object(env, table(env), surface)?
            .resource()
            .ok_or(Error::InvalidArgument)?
        else {
            return Err(Error::InvalidArgument);
        };
        let desc = texture.desc_2d();
        write(
            env,
            out,
            adv_abi::types::DXGI_SURFACE_DESC {
                Width: desc.Width,
                Height: desc.Height,
                Format: desc.Format,
                SampleDesc: desc.SampleDesc,
            },
        )
    })())
}

fn surface_map(env: &mut Env<'_>) -> Outcome {
    let (surface, out, flags) = (env.arg(0), env.arg(1), env.arg(2));
    call((|| {
        let mode = match flags {
            1 => 1,
            2 => 2,
            3 => 3,
            6 => 4,
            _ => return Err(dxgi::Error::InvalidCall.into()),
        };
        let pointer = table(env)
            .canonical(&env.sys.objects, surface)
            .ok_or(Error::InvalidArgument)?;
        let mapped = context::map_subresource(env, pointer, 0, mode, 0)?;
        write(
            env,
            out,
            adv_abi::types::DXGI_MAPPED_RECT {
                Pitch: mapped.RowPitch as i32,
                pBits: mapped.pData,
            },
        )
    })())
}

fn surface_unmap(_: &mut Env<'_>) -> Outcome {
    Outcome::Return(0)
}

fn get_dc(env: &mut Env<'_>) -> Outcome {
    #[cfg(feature = "gpu")]
    return dc::get(env);
    #[cfg(not(feature = "gpu"))]
    {
        let _ = env;
        Outcome::Return(dxgi::Error::Unavailable.hresult())
    }
}

fn release_dc(env: &mut Env<'_>) -> Outcome {
    #[cfg(feature = "gpu")]
    return dc::release(env);
    #[cfg(not(feature = "gpu"))]
    {
        let _ = env;
        Outcome::Return(dxgi::Error::Unavailable.hresult())
    }
}

#[cfg(feature = "gpu")]
mod dc;
#[cfg(feature = "gpu")]
pub(super) mod shared;

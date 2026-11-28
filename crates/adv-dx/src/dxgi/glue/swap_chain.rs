use super::*;
use crate::dxgi::{
    caps::SwapCoverage,
    object::Kind,
    swap::{Chain, Description, Resize, Services, Window},
};
use adv_abi::types::{DXGI_MATRIX_3X2_F, DXGI_PRESENT_PARAMETERS};
use adv_machine::Wait;

pub(super) const SLOTS: &[Slot] = vtable!("IDXGISwapChain2",
    QueryInterface => query Implement,
    AddRef => add_ref Implement,
    Release => release Implement,
    SetPrivateData => set_private Implement,
    SetPrivateDataInterface => set_interface Implement,
    GetPrivateData => get_private Implement,
    GetParent => parent Implement,
    GetDevice => device Implement,
    Present => present Implement,
    GetBuffer => buffer Implement,
    SetFullscreenState => set_fullscreen Implement,
    GetFullscreenState => fullscreen State,
    GetDesc => description State,
    ResizeBuffers => resize_buffers Implement,
    ResizeTarget => resize_target Implement,
    GetContainingOutput => containing_output Implement,
    GetFrameStatistics => statistics State,
    GetLastPresentCount => present_count State,
    GetDesc1 => description1 State,
    GetFullscreenDesc => fullscreen_description State,
    GetHwnd => hwnd State,
    GetCoreWindow => core_window State,
    Present1 => present1 Implement,
    IsTemporaryMonoSupported => zero State,
    GetRestrictToOutput => restrict_output State,
    SetBackgroundColor => set_background State,
    GetBackgroundColor => background State,
    SetRotation => set_rotation State,
    GetRotation => rotation State,
    SetSourceSize => set_source_size State,
    GetSourceSize => source_size State,
    SetMaximumFrameLatency => set_latency State,
    GetMaximumFrameLatency => latency State,
    GetFrameLatencyWaitableObject => waitable Implement,
    SetMatrixTransform => set_matrix State,
    GetMatrixTransform => matrix State,
);

pub(super) fn services(env: &Env<'_>) -> Result<&'static dyn crate::dxgi::swap::Buffers, Error> {
    Ok(env
        .ext
        .try_get::<Services>()
        .ok_or(Error::InvalidArgument)?
        .buffers)
}

fn window_state(env: &Env<'_>, window: u32, size: [u32; 2]) -> Result<Window, Error> {
    let services = env
        .ext
        .try_get::<Services>()
        .ok_or(Error::InvalidArgument)?;
    if window == 0 {
        return Ok(Window {
            client: size,
            minimized: false,
        });
    }
    (services.window)(env.ext, &env.sys.objects, window).ok_or(Error::InvalidCall)
}

fn coverage(build: u32) -> SwapCoverage {
    SwapCoverage {
        bitblt: true,
        flip: true,
        srgb: true,
        multisample: true,
        flags: 2 | 4 | if build >= 9600 { 64 } else { 0 },
    }
}

pub(super) fn create(env: &mut Env<'_>, invocation: DeviceCall) -> Option<Outcome> {
    use DeviceOperation::*;
    let [device, window, desc, fullscreen, _, out] = invocation.arguments;
    let (window, desc, fullscreen, out) = match invocation.operation {
        CreateSwapChain => {
            return Some(call(|| {
                create_legacy(env, invocation.this, device, window, desc)
            }));
        }
        CreateSwapChainForHwnd => (
            window,
            read(env, desc),
            if fullscreen == 0 {
                Ok(DXGI_SWAP_CHAIN_FULLSCREEN_DESC {
                    Windowed: 1,
                    ..Default::default()
                })
            } else {
                read(env, fullscreen)
            },
            out,
        ),
        CreateSwapChainForCoreWindow => {
            let build = env.ext.get::<Services>().reported_build;
            return Some(Outcome::Return(if build < 9200 {
                Error::NotImplemented.hresult()
            } else {
                // A Win32 caller has no CoreWindow to associate with the chain.
                Error::InvalidCall.hresult()
            }));
        }
        CreateSwapChainForComposition => {
            let [device, desc, restrict, out, ..] = invocation.arguments;
            return Some(call(|| {
                if env.ext.get::<Services>().reported_build < 9200 {
                    return Err(Error::NotImplemented);
                }
                let backend = services(env)?;
                if backend.adapter(env, device)? == Adapter::Null {
                    return Err(Error::Unsupported);
                }
                let desc: DXGI_SWAP_CHAIN_DESC1 = read(env, desc)?;
                if desc.SwapEffect != 3 || desc.Scaling != 0 || desc.Width == 0 || desc.Height == 0
                {
                    return Err(Error::InvalidCall);
                }
                if restrict != 0 {
                    ops::output_display(env, restrict)?;
                }
                create_chain(
                    env,
                    invocation.this,
                    device,
                    Destination::Composition(restrict),
                    desc,
                    DXGI_SWAP_CHAIN_FULLSCREEN_DESC {
                        Windowed: 1,
                        ..Default::default()
                    },
                    out,
                )
            }));
        }
        GetSharedResourceAdapterLuid
        | TakeOwnership
        | ReleaseOwnership
        | GetGammaControlCapabilities
        | SetGammaControl
        | GetGammaControl
        | SetDisplaySurface
        | GetDisplaySurfaceData
        | GetDisplaySurfaceData1
        | DuplicateOutput
        | DuplicateOutput1 => return None,
    };
    Some(call(|| {
        create_chain(
            env,
            invocation.this,
            device,
            Destination::Window(window),
            desc?,
            fullscreen?,
            out,
        )
    }))
}

pub(super) fn create_legacy(
    env: &mut Env<'_>,
    factory: u32,
    device: u32,
    desc: u32,
    output: u32,
) -> Result<(), Error> {
    let legacy: DXGI_SWAP_CHAIN_DESC = read(env, desc)?;
    let modern = DXGI_SWAP_CHAIN_DESC1 {
        Width: legacy.BufferDesc.Width,
        Height: legacy.BufferDesc.Height,
        Format: legacy.BufferDesc.Format,
        SampleDesc: legacy.SampleDesc,
        BufferUsage: legacy.BufferUsage,
        BufferCount: legacy.BufferCount,
        SwapEffect: legacy.SwapEffect,
        Flags: legacy.Flags,
        ..Default::default()
    };
    let fullscreen = DXGI_SWAP_CHAIN_FULLSCREEN_DESC {
        RefreshRate: legacy.BufferDesc.RefreshRate,
        ScanlineOrdering: legacy.BufferDesc.ScanlineOrdering,
        Scaling: legacy.BufferDesc.Scaling,
        Windowed: legacy.Windowed,
    };
    create_chain(
        env,
        factory,
        device,
        Destination::Window(legacy.OutputWindow),
        modern,
        fullscreen,
        output,
    )
}

enum Destination {
    Window(u32),
    Composition(u32),
}

fn create_chain(
    env: &mut Env<'_>,
    factory: u32,
    device: u32,
    destination: Destination,
    desc: DXGI_SWAP_CHAIN_DESC1,
    fullscreen: DXGI_SWAP_CHAIN_FULLSCREEN_DESC,
    output: u32,
) -> Result<(), Error> {
    let (window, composition) = match destination {
        Destination::Window(window) => (window, None),
        Destination::Composition(restrict) => (0, Some(restrict)),
    };
    let backend = services(env)?;
    backend.validate_device(env, device)?;
    let build = env.ext.get::<Services>().reported_build;
    let window_state = window_state(env, window, [desc.Width, desc.Height])?;
    if desc.Flags & 64 != 0 && (desc.SwapEffect < 3 || fullscreen.Windowed == 0) {
        return Err(Error::InvalidCall);
    }
    let description =
        Description::new(desc, window_state, backend.caps(env.ext)?, coverage(build))?;
    let buffers = backend.create(env, device, description.desc())?;
    backend.retain_device(env, device)?;
    let vtable = env.ext.get::<State>().swap_chain_vtable;
    let waitable = if desc.Flags & 64 != 0 {
        env.sys.objects.insert(adv_machine::Object::Timer {
            manual_reset: false,
            signaled: true,
            deadline: None,
            period_us: 0,
        })
    } else {
        0
    };
    let actual = description.desc();
    let chain = Chain {
        device,
        buffers,
        description,
        window,
        composition,
        fullscreen: DXGI_SWAP_CHAIN_FULLSCREEN_DESC {
            Windowed: 1,
            ..fullscreen
        },
        target: 0,
        generation: 0,
        queue: Default::default(),
        background: [0.0; 4],
        rotation: 1,
        matrix: DXGI_MATRIX_3X2_F {
            _11: 1.0,
            _22: 1.0,
            ..Default::default()
        },
        source: [actual.Width, actual.Height],
        waitable,
        latency: 1,
    };
    let base = [
        IID_IDXGIDEVICESUBOBJECT,
        IID_IDXGISWAPCHAIN,
        IID_IDXGISWAPCHAIN1,
    ];
    let extended = (build >= 9600).then_some(IID_IDXGISWAPCHAIN2);
    let pointer = ops::create(
        env,
        vtable,
        Kind::SwapChain(Box::new(chain)),
        Some(factory),
        base.into_iter().chain(extended),
    )?;
    if let Some(restrict) = composition.filter(|&restrict| restrict != 0) {
        ops::add_ref(env, restrict);
    }
    if window != 0 {
        env.ext
            .get_mut::<State>()
            .chains
            .entry(window)
            .or_default()
            .push(pointer);
    }
    if fullscreen.Windowed == 0
        && let Err(error) = change_fullscreen(env, pointer, true, 0)
    {
        ops::release(env, pointer);
        return Err(error);
    }
    write(env, output, pointer)
}

fn chain<'a>(env: &'a Env<'_>, this: u32) -> Result<&'a Chain, Error> {
    match &ops::object(env, this)?.kind {
        Kind::SwapChain(chain) => Ok(chain),
        Kind::Factory(_) | Kind::Adapter(_) | Kind::Output | Kind::Duplication(_) => {
            Err(Error::InvalidCall)
        }
    }
}
fn chain_mut<'a>(env: &'a mut Env<'_>, this: u32) -> Result<&'a mut Chain, Error> {
    match &mut ops::object_mut(env, this)?.kind {
        Kind::SwapChain(chain) => Ok(chain),
        Kind::Factory(_) | Kind::Adapter(_) | Kind::Output | Kind::Duplication(_) => {
            Err(Error::InvalidCall)
        }
    }
}

fn device(env: &mut Env<'_>) -> Outcome {
    let (this, iid, out) = (env.arg(0), read(env, env.arg(1)), env.arg(2));
    interface_out(env, out, |env| {
        let pointer = chain(env, this)?.device;
        services(env)?.query(env, pointer, iid?)
    })
}
fn buffer(env: &mut Env<'_>) -> Outcome {
    let (this, index, iid, out) = (env.arg(0), env.arg(1), read(env, env.arg(2)), env.arg(3));
    interface_out(env, out, |env| {
        let chain = chain(env, this)?;
        let index = chain.description.buffer(index)?;
        let pointer = chain.buffers[index as usize];
        services(env)?.query(env, pointer, iid?)
    })
}

fn present(env: &mut Env<'_>) -> Outcome {
    present_call(env, env.arg(0), env.arg(1), env.arg(2))
}
fn present1(env: &mut Env<'_>) -> Outcome {
    let params = read::<DXGI_PRESENT_PARAMETERS>(env, env.arg(3));
    if let Err(error) = params {
        return Outcome::Return(error.hresult());
    }
    present(env)
}

fn present_call(env: &mut Env<'_>, this: u32, interval: u32, flags: u32) -> Outcome {
    let result = (|| {
        let chain = chain(env, this)?;
        let desc = chain.description;
        let raw = desc.desc();
        let window = window_state(env, chain.window, [raw.Width, raw.Height])?;
        if interval > 4 || flags & !(1 | 2 | 4 | 8 | 16 | 32 | 0x100 | 0x200) != 0 {
            return Err(Error::InvalidCall);
        }
        let status = desc.present_status(window);
        if status != 0 || flags & 1 != 0 {
            return Ok(Outcome::Return(status));
        }
        let backend = services(env)?;
        let device = chain.device;
        let buffer = chain.buffers[0];
        let hwnd = chain.window;
        let first = chain.generation == 0;
        let composition = chain.composition.is_some();
        let hz = chain.fullscreen.RefreshRate;
        let hz = if hz.Numerator == 0 || hz.Denominator == 0 {
            60
        } else {
            (hz.Numerator / hz.Denominator).max(1)
        };
        let source = chain.source;
        let latency = if chain.waitable == 0 {
            backend.latency(env, device)
        } else {
            chain.latency
        };
        let latency = latency.max(1);
        let now = env.sys.clock.now_us();
        if let Some(deadline) = chain_mut(env, this)?.queue.available(now, latency) {
            if flags & 8 != 0 {
                return Ok(Outcome::Return(0x887a000a));
            }
            env.sys.keep_exact_deadline();
            return Ok(Outcome::block(Wait::Until(deadline), move |env, _| {
                present_call(env, this, interval, flags)
            }));
        }
        let serial = env.ext.get::<State>().presentation + 1;
        #[cfg(feature = "gpu")]
        let generation = backend.present_wake_generation(env.ext);
        let completed_serial =
            match backend.present(env, buffer, source, serial, first, composition) {
                Ok((published, completed)) => {
                    if published {
                        env.ext.get_mut::<State>().presentation = serial;
                    }
                    completed
                }
                Err(Error::StillDrawing) if flags & 8 == 0 => {
                    #[cfg(feature = "gpu")]
                    if let Some(generation) = generation {
                        return Ok(backend.wait_for_present(
                            env,
                            generation,
                            Box::new(move |env, _| present_call(env, this, interval, flags)),
                        ));
                    }
                    let deadline = now + 1000;
                    env.sys.keep_exact_deadline();
                    return Ok(Outcome::block(Wait::Until(deadline), move |env, _| {
                        present_call(env, this, interval, flags)
                    }));
                }
                Err(error) => return Err(error),
            };
        if crate::text::on(env)
            && let Some(store) = backend.text_store(env, buffer)
        {
            let rect = |[w, h]: [u32; 2]| adv_device::surface::Rect { x: 0, y: 0, w, h };
            let (client, read) = (rect(window.client), rect(source));
            if let Some(map) = crate::text::rect_map(client, read) {
                crate::text::presented(env, store, hwnd, client, map);
            }
        }
        let chain = chain_mut(env, this)?;
        chain.queue.submit(now, interval, hz);
        chain.generation = serial;
        if raw.SwapEffect >= 3 {
            chain.buffers.rotate_left(1);
        }
        update_latency_timer(env, this)?;
        Ok(complete_present(env, hwnd, completed_serial))
    })();
    result.unwrap_or_else(|error: Error| Outcome::Return(error.hresult()))
}

pub(super) fn complete_present(env: &mut Env<'_>, hwnd: u32, serial: Option<u64>) -> Outcome {
    env.ext
        .get::<Services>()
        .completed
        .zip(serial)
        .and_then(|(completed, serial)| completed(env, hwnd, serial))
        .unwrap_or(Outcome::Return(0))
}

fn set_fullscreen(env: &mut Env<'_>) -> Outcome {
    let (this, full, target) = (env.arg(0), env.arg(1) != 0, env.arg(2));
    call(|| change_fullscreen(env, this, full, target))
}

pub fn alt_enter(env: &mut Env<'_>, window: u32) -> bool {
    let state = env.ext.get::<State>();
    let Some(pointer) = state
        .chains
        .get(&window)
        .and_then(|chains| chains.last())
        .copied()
    else {
        return false;
    };
    let object = ops::object(env, pointer).unwrap();
    let Kind::SwapChain(chain) = &object.kind else {
        unreachable!()
    };
    let Kind::Factory(factory) = &ops::object(env, object.parent.unwrap()).unwrap().kind else {
        unreachable!()
    };
    if factory
        .window
        .is_some_and(|associated| associated != window)
        || factory.window_flags & 3 != 0
    {
        return false;
    }
    change_fullscreen(env, pointer, chain.fullscreen.Windowed != 0, 0).is_ok()
}

fn change_fullscreen(env: &mut Env<'_>, this: u32, full: bool, target: u32) -> Result<(), Error> {
    let chain = chain(env, this)?;
    if chain.composition.is_some() || full && (chain.window == 0 || chain.waitable != 0) {
        return Err(Error::InvalidCall);
    }
    let old = chain.target;
    if target != 0 && !matches!(ops::object(env, target)?.kind, Kind::Output) {
        return Err(Error::InvalidCall);
    }
    if !full && target != 0 {
        return Err(Error::InvalidCall);
    }
    let backend = services(env)?;
    let device = backend.identity(env, chain.device)?;
    if full {
        if backend.adapter(env, device)? == Adapter::Software {
            return Err(if target == 0 {
                Error::Unsupported
            } else {
                Error::InvalidCall
            });
        }
        ops::claim_output(env, this, device, false)?;
    } else {
        ops::release_output(env, this, &mut Vec::new());
    }
    let state = chain_mut(env, this)?;
    state.fullscreen.Windowed = i32::from(!full);
    state.target = target;
    if target != 0 {
        ops::add_ref(env, target);
    }
    if old != 0 {
        ops::release(env, old);
    }
    Ok(())
}

fn fullscreen(env: &mut Env<'_>) -> Outcome {
    let (this, status, target) = (env.arg(0), env.arg(1), env.arg(2));
    call(|| {
        let chain = chain(env, this)?;
        let full = i32::from(chain.fullscreen.Windowed == 0);
        let (pointer, device) = (chain.target, chain.device);
        if status != 0 {
            write(env, status, full)?;
        }
        if target != 0 {
            let pointer = if pointer == 0 && full != 0 {
                services(env)?.output(env, device)?
            } else {
                if pointer != 0 {
                    ops::add_ref(env, pointer);
                }
                pointer
            };
            write(env, target, pointer)?;
        }
        Ok(())
    })
}

fn description(env: &mut Env<'_>) -> Outcome {
    let (this, out) = (env.arg(0), env.arg(1));
    call(|| {
        let chain = chain(env, this)?;
        let raw = chain.description.desc();
        write(
            env,
            out,
            DXGI_SWAP_CHAIN_DESC {
                BufferDesc: DXGI_MODE_DESC {
                    Width: raw.Width,
                    Height: raw.Height,
                    Format: raw.Format,
                    RefreshRate: chain.fullscreen.RefreshRate,
                    ScanlineOrdering: chain.fullscreen.ScanlineOrdering,
                    Scaling: chain.fullscreen.Scaling,
                },
                SampleDesc: raw.SampleDesc,
                BufferUsage: raw.BufferUsage,
                BufferCount: raw.BufferCount,
                OutputWindow: chain.window,
                Windowed: chain.fullscreen.Windowed,
                SwapEffect: raw.SwapEffect,
                Flags: raw.Flags,
            },
        )
    })
}

fn description1(env: &mut Env<'_>) -> Outcome {
    let (this, out) = (env.arg(0), env.arg(1));
    call(|| write(env, out, chain(env, this)?.description.desc()))
}
fn fullscreen_description(env: &mut Env<'_>) -> Outcome {
    let (this, out) = (env.arg(0), env.arg(1));
    call(|| write(env, out, chain(env, this)?.fullscreen))
}
fn hwnd(env: &mut Env<'_>) -> Outcome {
    let (this, out) = (env.arg(0), env.arg(1));
    call(|| {
        let window = chain(env, this)?.window;
        if window == 0 {
            return Err(Error::InvalidCall);
        }
        write(env, out, window)
    })
}
fn core_window(env: &mut Env<'_>) -> Outcome {
    let out = env.arg(2);
    call(|| {
        write(env, out, 0u32)?;
        if chain(env, env.arg(0))?.composition.is_some() {
            return Err(Error::InvalidCall);
        }
        Err(Error::NoInterface)
    })
}
fn restrict_output(env: &mut Env<'_>) -> Outcome {
    let out = env.arg(1);
    interface_out(env, out, |env| {
        let pointer = chain(env, env.arg(0))?.composition.unwrap_or(0);
        if pointer != 0 {
            ops::add_ref(env, pointer);
        }
        Ok(pointer)
    })
}

fn resize_buffers(env: &mut Env<'_>) -> Outcome {
    let [this, buffers, width, height, format, flags] = std::array::from_fn(|i| env.arg(i as u32));
    let result = (|| {
        let backend = services(env)?;
        let chain = chain(env, this)?;
        let device = chain.device;
        let raw = chain.description.desc();
        let window = window_state(env, chain.window, [raw.Width, raw.Height])?;
        let resize = Resize {
            buffers,
            size: [width, height],
            format: format as i32,
            flags,
            referenced: chain
                .buffers
                .iter()
                .any(|&pointer| backend.referenced(env, pointer)),
        };
        if (raw.Flags ^ flags) & 64 != 0 {
            return Err(Error::InvalidCall);
        }
        let caps = coverage(env.ext.get::<Services>().reported_build);
        let desc = chain
            .description
            .resize(resize, window, backend.caps(env.ext)?, caps)?;
        let buffers = backend.create(env, device, desc.desc())?;
        let chain = chain_mut(env, this)?;
        let previous = std::mem::replace(&mut chain.buffers, buffers);
        chain.description = desc;
        chain.source = [desc.desc().Width, desc.desc().Height];
        chain.generation = 0;
        let mut release = Vec::new();
        for pointer in previous {
            backend.release(env, pointer, &mut release);
        }
        Ok(release)
    })();
    result.map_or_else(
        |error: Error| Outcome::Return(error.hresult()),
        |pointers| ops::release_interfaces(env, pointers, 0),
    )
}

fn resize_target(env: &mut Env<'_>) -> Outcome {
    let (this, raw) = (env.arg(0), read::<DXGI_MODE_DESC>(env, env.arg(1)));
    let result = (|| {
        let raw = raw?;
        let chain = chain(env, this)?;
        if chain.composition.is_some() {
            return Err(Error::InvalidCall);
        }
        Ok::<_, Error>((raw, chain.window))
    })();
    let (raw, window) = match result {
        Ok(args) => args,
        Err(error) => return Outcome::Return(error.hresult()),
    };
    let resize = env.ext.get::<Services>().resize;
    resize(
        env,
        window,
        raw,
        Box::new(move |env, result| {
            call(|| {
                if result.value() == 0 {
                    return Err(Error::InvalidCall);
                }
                let fullscreen = &mut chain_mut(env, this)?.fullscreen;
                fullscreen.RefreshRate = raw.RefreshRate;
                fullscreen.ScanlineOrdering = raw.ScanlineOrdering;
                fullscreen.Scaling = raw.Scaling;
                if fullscreen.Windowed == 0 {
                    ops::output_mode_changed(env.ext);
                }
                Ok(())
            })
        }),
    )
}

fn containing_output(env: &mut Env<'_>) -> Outcome {
    let (this, out) = (env.arg(0), env.arg(1));
    interface_out(env, out, |env| {
        let chain = chain(env, this)?;
        if chain.composition.is_some() {
            return Err(Error::InvalidCall);
        }
        services(env)?.output(env, chain.device)
    })
}

fn statistics(env: &mut Env<'_>) -> Outcome {
    let (this, out) = (env.arg(0), env.arg(1));
    call(|| {
        let chain = chain(env, this)?;
        let count = chain.queue.count;
        if count == 0 {
            return Err(Error::FrameStatisticsDisjoint);
        }
        let refresh = (chain.queue.sync * 60 / 1_000_000) as u32;
        write(
            env,
            out,
            DXGI_FRAME_STATISTICS {
                PresentCount: count,
                PresentRefreshCount: refresh,
                SyncRefreshCount: refresh,
                SyncQPCTime: (chain.queue.sync * 10) as i64,
                SyncGPUTime: 0,
                ..Default::default()
            },
        )
    })
}
fn present_count(env: &mut Env<'_>) -> Outcome {
    let (this, out) = (env.arg(0), env.arg(1));
    call(|| write(env, out, chain(env, this)?.queue.count))
}

fn set_background(env: &mut Env<'_>) -> Outcome {
    let (this, raw) = (env.arg(0), read::<[f32; 4]>(env, env.arg(1)));
    call(|| {
        chain_mut(env, this)?.background = raw?;
        Ok(())
    })
}
fn background(env: &mut Env<'_>) -> Outcome {
    let (this, out) = (env.arg(0), env.arg(1));
    call(|| write(env, out, chain(env, this)?.background))
}
fn set_rotation(env: &mut Env<'_>) -> Outcome {
    let (this, value) = (env.arg(0), env.arg(1));
    call(|| {
        if !(1..=4).contains(&value) {
            return Err(Error::InvalidArgument);
        }
        chain_mut(env, this)?.rotation = value as i32;
        Ok(())
    })
}
fn rotation(env: &mut Env<'_>) -> Outcome {
    let (this, out) = (env.arg(0), env.arg(1));
    call(|| write(env, out, chain(env, this)?.rotation))
}

fn set_source_size(env: &mut Env<'_>) -> Outcome {
    let (this, width, height) = (env.arg(0), env.arg(1), env.arg(2));
    call(|| {
        let chain = chain_mut(env, this)?;
        let raw = chain.description.desc();
        if raw.SwapEffect < 3
            || width == 0
            || height == 0
            || width > raw.Width
            || height > raw.Height
        {
            return Err(Error::InvalidCall);
        }
        chain.source = [width, height];
        Ok(())
    })
}

fn source_size(env: &mut Env<'_>) -> Outcome {
    let (this, width, height) = (env.arg(0), env.arg(1), env.arg(2));
    call(|| {
        let [w, h] = chain(env, this)?.source;
        write(env, width, w)?;
        write(env, height, h)
    })
}

fn set_latency(env: &mut Env<'_>) -> Outcome {
    let (this, latency) = (env.arg(0), env.arg(1));
    call(|| {
        let chain = chain_mut(env, this)?;
        if chain.waitable == 0 || !(1..=16).contains(&latency) {
            return Err(Error::InvalidCall);
        }
        chain.latency = latency;
        update_latency_timer(env, this)
    })
}

fn latency(env: &mut Env<'_>) -> Outcome {
    let (this, out) = (env.arg(0), env.arg(1));
    call(|| {
        let chain = chain(env, this)?;
        if chain.waitable == 0 {
            return Err(Error::InvalidCall);
        }
        write(env, out, chain.latency)
    })
}

fn update_latency_timer(env: &mut Env<'_>, this: u32) -> Result<(), Error> {
    let now = env.sys.clock.now_us();
    let chain = chain_mut(env, this)?;
    let handle = chain.waitable;
    if handle != 0 {
        let deadline = chain.queue.available(now, chain.latency).unwrap_or(now);
        env.sys.arm_timer(handle, deadline, 0);
    }
    Ok(())
}

fn waitable(env: &mut Env<'_>) -> Outcome {
    let this = env.arg(0);
    let handle = chain(env, this).map_or(0, |chain| chain.waitable);
    Outcome::Return(if handle != 0 {
        env.sys.objects.duplicate(handle).unwrap_or(0)
    } else {
        0
    })
}

fn set_matrix(env: &mut Env<'_>) -> Outcome {
    call(|| {
        let matrix = read::<DXGI_MATRIX_3X2_F>(env, env.arg(1))?;
        let chain = chain_mut(env, env.arg(0))?;
        if chain.composition.is_none() || matrix._12 != 0.0 || matrix._21 != 0.0 {
            return Err(Error::InvalidCall);
        }
        chain.matrix = matrix;
        Ok(())
    })
}

fn matrix(env: &mut Env<'_>) -> Outcome {
    call(|| {
        let chain = chain(env, env.arg(0))?;
        if chain.composition.is_none() {
            return Err(Error::InvalidCall);
        }
        write(env, env.arg(1), chain.matrix)
    })
}

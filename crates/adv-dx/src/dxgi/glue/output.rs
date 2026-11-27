//! DXGI operations over the virtual display and its device-backed scanout.
use super::*;
use crate::dxgi::{
    object::{Duplication, Kind},
    swap::Services,
};
use adv_abi::types::DXGI_OUTDUPL_DESC;
#[cfg(feature = "gpu")]
use adv_abi::types::DXGI_OUTDUPL_FRAME_INFO;
#[cfg(feature = "gpu")]
use adv_machine::Wait;
#[cfg(feature = "gpu")]
use std::sync::mpsc::TryRecvError;
#[cfg(feature = "gpu")]
use {crate::dxgi::object::Publication, adv_device::surface::SurfaceStore};

fn output_adapter(env: &Env<'_>, this: u32) -> Result<Adapter, Error> {
    let output = ops::object(env, this)?;
    if !matches!(output.kind, Kind::Output) {
        return Err(Error::InvalidCall);
    }
    ops::adapter(env, output.parent.unwrap())
}

pub(super) struct OutputDevices;

impl crate::dxgi::Devices for OutputDevices {
    fn invoke(&self, env: &mut Env<'_>, invocation: DeviceCall) -> Outcome {
        use DeviceOperation::*;
        let this = invocation.this;
        let [first, second, third, fourth, fifth, _] = invocation.arguments;
        match invocation.operation {
            TakeOwnership => call(|| {
                if first == 0 {
                    return Ok(());
                }
                let backend = swap_chain::services(env)?;
                let device = backend.identity(env, first)?;
                let adapter = backend.adapter(env, device)?;
                if adapter == Adapter::Software {
                    return Ok(());
                }
                if adapter != output_adapter(env, this)? {
                    return Err(Error::InvalidArgument);
                }
                if second == 0 {
                    return Ok(());
                }
                ops::claim_output(env, this, device, true)
            }),
            ReleaseOwnership => {
                let mut interfaces = Vec::new();
                if let Some(claimant) = env
                    .ext
                    .get::<State>()
                    .output
                    .owner
                    .as_ref()
                    .map(|o| o.claimant)
                {
                    ops::release_output(env, claimant, &mut interfaces);
                }
                ops::release_interfaces(env, interfaces, 0)
            }
            GetGammaControlCapabilities => call(|| {
                fullscreen_owner(env)?;
                let mut positions = [0.0; 1025];
                for (i, point) in positions[..256].iter_mut().enumerate() {
                    *point = i as f32 / 255.0;
                }
                let caps = DXGI_GAMMA_CONTROL_CAPABILITIES {
                    ScaleAndOffsetSupported: 0,
                    MaxConvertedValue: 1.0,
                    MinConvertedValue: 0.0,
                    NumGammaControlPoints: 256,
                    ControlPointPositions: positions,
                };
                let written =
                    std::mem::offset_of!(DXGI_GAMMA_CONTROL_CAPABILITIES, ControlPointPositions)
                        + caps.NumGammaControlPoints as usize * size_of::<f32>();
                env.cpu
                    .write(first, &bytemuck::bytes_of(&caps)[..written])
                    .map_err(|_| Error::InvalidCall)
            }),
            SetGammaControl => call(|| {
                fullscreen_owner(env)?;
                let gamma = read(env, first)?;
                let output = &mut env.ext.get_mut::<State>().output;
                output.gamma = Some(Box::new(gamma));
                output.gamma_version += 1;
                Ok(())
            }),
            GetGammaControl => call(|| {
                fullscreen_owner(env)?;
                let output = &env.ext.get::<State>().output;
                write(
                    env,
                    first,
                    *output.gamma.as_deref().ok_or(Error::InvalidCall)?,
                )
            }),
            SetDisplaySurface => call(|| {
                let device = owned(env)?;
                let backend = swap_chain::services(env)?;
                let (surface, parent) = backend.surface_owner(env, first)?;
                let state = env.ext.get::<State>();
                if parent != device {
                    return Err(Error::InvalidCall);
                }
                let chain = state
                    .chains
                    .values()
                    .flatten()
                    .find_map(|&pointer| {
                        let Kind::SwapChain(chain) = &ops::object(env, pointer).ok()?.kind else {
                            return None;
                        };
                        chain.buffers.contains(&surface).then_some(chain)
                    })
                    .ok_or(Error::InvalidCall)?;
                let serial = state.presentation + 1;
                let (window, raw, fullscreen) =
                    (chain.window, chain.description.desc(), chain.fullscreen);
                let current = mode(env, this)?;
                let resized = (raw.Width, raw.Height) != (current.Width, current.Height);
                if resized {
                    let resize = env.ext.get::<Services>().resize;
                    resize(
                        env,
                        window,
                        DXGI_MODE_DESC {
                            Width: raw.Width,
                            Height: raw.Height,
                            Format: raw.Format,
                            RefreshRate: fullscreen.RefreshRate,
                            ScanlineOrdering: fullscreen.ScanlineOrdering,
                            Scaling: fullscreen.Scaling,
                        },
                    )?;
                    ops::output_mode_changed(env.ext);
                }
                backend.scanout(env, surface, serial)?;
                env.ext.get_mut::<State>().presentation = serial;
                Ok(())
            }),
            GetDisplaySurfaceData | GetDisplaySurfaceData1 => {
                #[cfg(feature = "gpu")]
                return call(|| display_surface(env, first));
                #[cfg(not(feature = "gpu"))]
                return ops::device_unavailable(invocation.operation);
            }
            DuplicateOutput => call(|| duplicate(env, this, first, 87, second)),
            DuplicateOutput1 => call(|| {
                if second & !1 != 0 {
                    return Err(Error::InvalidArgument);
                }
                let mut format = None;
                for index in 0..third {
                    let candidate: i32 = read(env, fourth + 4 * index)?;
                    if candidate == 87 {
                        format = Some(87);
                        break;
                    }
                    if matches!(candidate, 10 | 24 | 28) {
                        format.get_or_insert(candidate);
                    }
                }
                duplicate(
                    env,
                    this,
                    first,
                    format.ok_or(Error::InvalidArgument)?,
                    fifth,
                )
            }),
            CreateSwapChain
            | CreateSwapChainForHwnd
            | CreateSwapChainForCoreWindow
            | CreateSwapChainForComposition
            | GetSharedResourceAdapterLuid => ops::device_unavailable(invocation.operation),
        }
    }
}

#[cfg(feature = "gpu")]
pub(crate) fn gamma(ext: &adv_machine::Ext, device: u32) -> Option<(u64, &DXGI_GAMMA_CONTROL)> {
    let output = &ext.get::<State>().output;
    output
        .owner
        .as_ref()
        .filter(|owner| owner.device == device)?;
    output
        .gamma
        .as_deref()
        .map(|gamma| (output.gamma_version, gamma))
}

#[cfg(feature = "gpu")]
fn display_surface(env: &mut Env<'_>, surface: u32) -> Result<(), Error> {
    let device = owned(env)?;
    let backend = swap_chain::services(env)?;
    let (surface, parent) = backend.surface_owner(env, surface)?;
    if parent != device {
        return Err(Error::InvalidCall);
    }
    backend.copy_display(env, surface)
}
fn owned(env: &Env<'_>) -> Result<u32, Error> {
    env.ext
        .get::<State>()
        .output
        .owner
        .as_ref()
        .map(|owner| owner.device)
        .ok_or(Error::InvalidCall)
}

fn fullscreen_owner(env: &Env<'_>) -> Result<(), Error> {
    env.ext
        .get::<State>()
        .output
        .owner
        .as_ref()
        .filter(|owner| !owner.retained)
        .map(|_| ())
        .ok_or(Error::InvalidCall)
}

pub(super) const SLOTS: &[Slot] = vtable!("IDXGIOutputDuplication",
    QueryInterface => query Implement,
    AddRef => add_ref Implement,
    Release => release Implement,
    SetPrivateData => set_private Implement,
    SetPrivateDataInterface => set_interface Implement,
    GetPrivateData => get_private Implement,
    GetParent => parent Implement,
    GetDesc => description State,
    AcquireNextFrame => acquire Implement,
    GetFrameDirtyRects => dirty_rects Implement,
    GetFrameMoveRects => move_rects State,
    GetFramePointerShape => pointer_shape State,
    MapDesktopSurface => map_desktop Implement,
    UnMapDesktopSurface => unmap_desktop State,
    ReleaseFrame => release_frame State,
);

fn mode(env: &Env<'_>, output: u32) -> Result<DXGI_MODE_DESC, Error> {
    let display = ops::output_display(env, output)?;
    let [left, top, right, bottom] = display.physical_rect;
    let size = ((right - left) as u32, (bottom - top) as u32);
    let mode = display.modes.iter().find(|mode| mode.size == size);
    let (numerator, denominator) =
        mode.map_or((60, 1), |mode| (mode.refresh.0, mode.refresh.1.get()));
    Ok(DXGI_MODE_DESC {
        Width: size.0,
        Height: size.1,
        RefreshRate: DXGI_RATIONAL {
            Numerator: numerator,
            Denominator: denominator,
        },
        Format: 87,
        ScanlineOrdering: mode.map_or(1, |mode| mode.scanline),
        ..Default::default()
    })
}

fn duplicate(
    env: &mut Env<'_>,
    this: u32,
    device: u32,
    format: i32,
    out: u32,
) -> Result<(), Error> {
    if env.ext.get::<Services>().reported_build < 9200 {
        return Err(Error::NotImplemented);
    }
    let backend = swap_chain::services(env)?;
    let device = backend.identity(env, device)?;
    if backend.adapter(env, device)? != output_adapter(env, this)? {
        return Err(Error::InvalidArgument);
    }
    let state = env.ext.get::<State>();
    if state.output.duplicating {
        return Err(Error::InvalidArgument);
    }
    let duplicate = Duplication {
        device,
        mode: DXGI_MODE_DESC {
            Format: format,
            ..mode(env, this)?
        },
        epoch: state.output.epoch,
        held: false,
        #[cfg(feature = "gpu")]
        last: None,
        #[cfg(feature = "gpu")]
        capture: None,
    };
    let vtable = state.duplication_vtable;
    backend.retain_device(env, device)?;
    let pointer = ops::create(
        env,
        vtable,
        Kind::Duplication(Box::new(duplicate)),
        Some(this),
        [IID_IDXGIOUTPUTDUPLICATION],
    );
    match pointer {
        Ok(pointer) => {
            env.ext.get_mut::<State>().output.duplicating = true;
            write(env, out, pointer)
        }
        Err(error) => {
            backend.drop_device_ref(env, device);
            Err(error)
        }
    }
}

fn duplication<'a>(env: &'a Env<'_>, this: u32) -> Result<&'a Duplication, Error> {
    let object = ops::object(env, this)?;
    let Kind::Duplication(duplicate) = &object.kind else {
        return Err(Error::InvalidCall);
    };
    let display = mode(env, object.parent.unwrap()).map_err(|_| Error::AccessLost)?;
    if duplicate.epoch != env.ext.get::<State>().output.epoch
        || (
            duplicate.mode.Width,
            duplicate.mode.Height,
            duplicate.mode.RefreshRate.Numerator,
            duplicate.mode.RefreshRate.Denominator,
        ) != (
            display.Width,
            display.Height,
            display.RefreshRate.Numerator,
            display.RefreshRate.Denominator,
        )
    {
        return Err(Error::AccessLost);
    }
    Ok(duplicate)
}

fn duplication_mut<'a>(env: &'a mut Env<'_>, this: u32) -> Result<&'a mut Duplication, Error> {
    let Kind::Duplication(duplicate) = &mut ops::object_mut(env, this)?.kind else {
        return Err(Error::InvalidCall);
    };
    Ok(duplicate)
}

fn description(env: &mut Env<'_>) -> Outcome {
    let (this, out) = (env.arg(0), env.arg(1));
    if let Ok(object) = ops::object(env, this)
        && let Kind::Duplication(duplicate) = &object.kind
    {
        let _ = write(
            env,
            out,
            DXGI_OUTDUPL_DESC {
                ModeDesc: duplicate.mode,
                Rotation: 1,
                DesktopImageInSystemMemory: 0,
            },
        );
    }
    Outcome::Return(0)
}

#[cfg(not(feature = "gpu"))]
fn acquire(_: &mut Env<'_>) -> Outcome {
    Outcome::Return(Error::Unsupported.hresult())
}

#[cfg(feature = "gpu")]
fn acquire(env: &mut Env<'_>) -> Outcome {
    let (this, timeout, info, out) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    if validate_output(env, info, size_of::<DXGI_OUTDUPL_FRAME_INFO>() as u32).is_err()
        || write(env, out, 0u32).is_err()
    {
        return Outcome::Return(Error::InvalidArgument.hresult());
    }
    let deadline =
        (timeout != u32::MAX).then(|| env.sys.clock.now_us() + u64::from(timeout) * 1000);
    acquire_frame(env, this, deadline, info, out)
}

#[cfg(feature = "gpu")]
fn publication(env: &Env<'_>) -> Publication {
    (
        env.ext.get::<State>().presentation,
        env.ext
            .try_get::<SurfaceStore>()
            .map_or(0, |store| store.stats.presentations),
    )
}

#[cfg(feature = "gpu")]
fn acquire_frame(
    env: &mut Env<'_>,
    this: u32,
    deadline: Option<u64>,
    info: u32,
    out: u32,
) -> Outcome {
    let result = (|| {
        let duplicate = duplication(env, this)?;
        if duplicate.held {
            return Err(Error::InvalidCall);
        }
        let (device, mode, last) = (duplicate.device, duplicate.mode, duplicate.last);
        let backend = swap_chain::services(env)?;
        let publication = publication(env);
        if duplicate.capture.is_none() && last != Some(publication) {
            let capture = backend.capture(env)?.capture();
            duplication_mut(env, this)?.capture = Some((publication, capture));
        }
        let duplicate = duplication_mut(env, this)?;
        let Some((captured, capture)) = &duplicate.capture else {
            return Ok(false);
        };
        let captured = *captured;
        let frame = match capture.try_recv() {
            Err(TryRecvError::Empty) => return Ok(false),
            Ok(Ok(frame)) => frame,
            Ok(Err(_)) | Err(TryRecvError::Disconnected) => {
                duplicate.capture = None;
                return Ok(false);
            }
        };
        duplicate.capture = None;
        let resource = backend.desktop_resource(env, device, mode, &frame)?;
        let age = last.map_or(1, |(modern, shared)| {
            (captured.0.saturating_sub(modern) + captured.1.saturating_sub(shared))
                .clamp(1, u64::from(u32::MAX)) as u32
        });
        write(
            env,
            info,
            DXGI_OUTDUPL_FRAME_INFO {
                LastPresentTime: env.sys.clock.qpc() as i64,
                AccumulatedFrames: age,
                RectsCoalesced: 1,
                TotalMetadataBufferSize: size_of::<RECT>() as u32,
                ..Default::default()
            },
        )?;
        write(env, out, resource)?;
        let duplicate = duplication_mut(env, this)?;
        duplicate.held = true;
        duplicate.last = Some(captured);
        Ok(true)
    })();
    match result {
        Ok(true) => Outcome::Return(0),
        Ok(false) => {
            let now = env.sys.clock.now_us();
            if deadline.is_some_and(|deadline| deadline <= now) {
                return Outcome::Return(Error::WaitTimeout.hresult());
            }
            let next = deadline.map_or(now + 1000, |deadline| deadline.min(now + 1000));
            env.sys.keep_exact_deadline();
            Outcome::block(Wait::Until(next), move |env, _| {
                acquire_frame(env, this, deadline, info, out)
            })
        }
        Err(error) => Outcome::Return(error.hresult()),
    }
}

fn held(env: &Env<'_>, this: u32) -> Result<DXGI_MODE_DESC, Error> {
    let duplicate = duplication(env, this)?;
    if !duplicate.held {
        return Err(Error::InvalidCall);
    }
    Ok(duplicate.mode)
}

fn dirty_rects(env: &mut Env<'_>) -> Outcome {
    call(|| {
        let mode = held(env, env.arg(0))?;
        if env.arg(2) == 0 {
            return Err(Error::InvalidArgument);
        }
        write(env, env.arg(3), size_of::<RECT>() as u32)?;
        if env.arg(1) < size_of::<RECT>() as u32 {
            return Err(Error::MoreData);
        }
        write(
            env,
            env.arg(2),
            RECT {
                left: 0,
                top: 0,
                right: mode.Width as i32,
                bottom: mode.Height as i32,
            },
        )
    })
}

fn move_rects(env: &mut Env<'_>) -> Outcome {
    call(|| {
        held(env, env.arg(0))?;
        if env.arg(2) == 0 {
            return Err(Error::InvalidArgument);
        }
        write(env, env.arg(3), 0u32)
    })
}

fn pointer_shape(env: &mut Env<'_>) -> Outcome {
    call(|| {
        held(env, env.arg(0))?;
        if env.arg(2) == 0 || env.arg(4) == 0 {
            return Err(Error::InvalidArgument);
        }
        write(env, env.arg(3), 0u32)
    })
}

fn map_desktop(env: &mut Env<'_>) -> Outcome {
    call(|| {
        held(env, env.arg(0))?;
        Err(Error::Unsupported)
    })
}

fn unmap_desktop(env: &mut Env<'_>) -> Outcome {
    call(|| {
        duplication(env, env.arg(0))?;
        Err(Error::InvalidCall)
    })
}

fn release_frame(env: &mut Env<'_>) -> Outcome {
    call(|| {
        let this = env.arg(0);
        held(env, this)?;
        duplication_mut(env, this)?.held = false;
        Ok(())
    })
}

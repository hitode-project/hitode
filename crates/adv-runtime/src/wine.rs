//! Assembly of the graphics fronts on the Wine userland. Wine's loader maps
//! their PE images over the process surface store. DirectDraw and Direct3D 9
//! also supply memory DCs that Wine's GDI draws into.
//!
//! A surface's DC comes from `D3DKMTCreateDCFromMemory` over the surface's
//! own pixels, called through the IAT of the front's image so that a guest
//! hook sees it. Window geometry comes from the user driver in `adv_drv`.
use std::collections::HashMap;
use std::path::Path;

use adv_abi::Registry;
use adv_abi::dll::{Export, Import, ModuleImage};
use adv_cpu::backend::{Backend, GuestExt};
use adv_device::surface::PixelFormat;
use adv_drv::{free_slot, slot, write_words};
use adv_dx::interop::{DcCreated, DcFinished, DcSurface, MemoryDc};
use adv_machine::{Env, Machine, Outcome};
use adv_pe::{ImportModule, ImportName};

#[cfg(feature = "gpu")]
use crate::graphics::{Snapshot, Surfaces};
use crate::graphics::{guest_memory, install_store};

pub use adv_drv::codec::vorbisfile;

mod class;
mod dmsynth;
mod fonts;
#[cfg(any(target_vendor = "apple", target_os = "android"))]
pub mod session;
/// Codec library acceleration on the image-mapping path.
#[cfg(any(target_vendor = "apple", target_os = "android"))]
pub use adv_drv::codec;
mod sound;
mod video;
pub mod winmm;
mod xaudio2;

/// The display driver to name in the process's startup parameters.
pub const GRAPHICS_DRIVER: &str = adv_drv::user::MODULE;

/// The fronts' images, by the index their memory DCs are bound to.
const IMAGES: [&str; 6] = [
    "ddraw.dll",
    "d3d9.dll",
    "dxgi.dll",
    "d3d11.dll",
    "d3d10core.dll",
    "d3d10_1core.dll",
];
const DDRAW: usize = 0;
#[cfg(feature = "gpu")]
const D3D9: usize = 1;
#[cfg(feature = "gpu")]
const DXGI: usize = 2;
const GDI32: &str = "gdi32.dll";
const DDRAW_EXPORTS: [&str; 7] = [
    "DirectDrawCreate",
    "DirectDrawCreateClipper",
    "DirectDrawCreateEx",
    "DirectDrawEnumerateA",
    "DirectDrawEnumerateExA",
    "DirectDrawEnumerateExW",
    "DirectDrawEnumerateW",
];
#[cfg(feature = "gpu")]
const D3D9_EXPORTS: [&str; 1] = ["Direct3DCreate9"];
const GDI_IMPORTS: [&str; 4] = [
    "CreateCompatibleDC",
    "D3DKMTCreateDCFromMemory",
    "D3DKMTDestroyDCFromMemory",
    "GdiFlush",
];
/// `D3DKMT_CREATEDCFROMMEMORY` (i386): pMemory, Format, Width, Height,
/// Pitch, hDeviceDc, pColorTable, then the hDc and hBitmap it returns.
const CREATE_DESC: u32 = 36;
const CREATED_DC: u32 = 28;
const CREATED_BITMAP: u32 = 32;

pub fn register(registry: &mut Registry) {
    adv_dx::ddraw::register(registry);
    #[cfg(feature = "gpu")]
    adv_dx::ddraw::d3d::register(registry);
    class::register(registry);
    dmsynth::register(registry);
    sound::register(registry);
    video::register(registry);
    winmm::register(registry);
    xaudio2::register(registry);
    #[cfg(feature = "gpu")]
    {
        adv_dx::d3d9::register(registry);
        adv_dx::dxgi::register(registry);
        adv_dx::d3d11::register_children(registry);
    }
    adv_drv::register(registry);
}

/// Install the surface store, the graphics fronts and the user driver, and
/// put their images on drive C: at `drive`, in place of Wine's. Call before
/// the process starts with [`GRAPHICS_DRIVER`].
pub fn install<B: Backend>(
    machine: &mut Machine<B>,
    drive: &Path,
    width: u32,
    height: u32,
) -> Result<(), String> {
    install_store(machine, width, height)?;
    machine.sys.context_wait = Some(flush_on_wait);
    adv_dx::ddraw::install(
        machine,
        adv_dx::ddraw::Provider {
            dc: memory_dc::<DDRAW>(),
            memory: guest_memory,
            window: |ext, objects, hwnd| {
                let hwnd = if hwnd == 0 {
                    adv_drv::user::input::presented_window(ext)?
                } else {
                    hwnd
                };
                geometry(ext, objects, hwnd)
            },
            completed: frame_completed,
            submitted: |ext, hwnd, surface, shared| {
                if shared {
                    adv_drv::user::input::shared_front_frame(ext, hwnd, surface);
                } else {
                    adv_drv::user::input::front_frame(ext, hwnd, surface);
                }
            },
            released: adv_drv::user::input::front_released,
            vsync: |ext| {
                ext.try_get::<std::sync::Arc<adv_sync::vsync::Vsync>>()
                    .and_then(|vsync| vsync.phase(std::time::Instant::now()))
            },
        },
    )?;
    #[cfg(feature = "gpu")]
    {
        adv_dx::ddraw::d3d::install(machine)?;
        machine
            .ext
            .get_mut::<adv_dx::ddraw::front::State>()
            .set_activation_provider(|ext, hwnd| {
                (adv_drv::user::input::presented_window(ext) == Some(hwnd))
                    .then(|| !adv_drv::user::project::minimized(ext))
            });
    }
    class::install(machine)?;
    let ddraw: Vec<_> = DDRAW_EXPORTS.into_iter().chain(class::EXPORTS).collect();
    install_image(machine, drive, DDRAW, &ddraw)?;
    let heap = machine
        .ext
        .get_or_default::<adv_os::Heaps>()
        .create(&mut machine.cpu, &mut machine.sys.vm)
        .map_err(|e| e.to_string())?;
    machine.ext.insert(Heap(heap));
    sound::install(machine, drive, allocator())?;
    dmsynth::install(machine, drive, allocator())?;
    video::install(machine, drive)?;
    winmm::install(machine, drive)?;
    xaudio2::install(machine, drive)?;
    #[cfg(feature = "gpu")]
    {
        adv_dx::d3d9::install(machine, d3d9_provider())?;
        install_image(machine, drive, D3D9, &D3D9_EXPORTS)?;
        install_modern_graphics(machine, drive)?;
    }
    let provider = adv_drv::Provider {
        memory: guest_memory,
        completed: crate::graphics::frame_completed,
    };
    adv_drv::media::install(machine)?;
    adv_drv::install(machine, drive, provider)
}

#[cfg(feature = "gpu")]
fn install_modern_graphics<B: Backend>(
    machine: &mut Machine<B>,
    drive: &Path,
) -> Result<(), String> {
    adv_dx::dxgi::install(
        machine,
        adv_dx::dxgi::Provider {
            adapter: Default::default(),
            display: adv_dx::dxgi::shared_display,
            window_exists: |ext, _, hwnd| adv_drv::user::window(ext, hwnd).is_some(),
            next_vblank: |ext| {
                let vsync = ext.try_get::<std::sync::Arc<adv_sync::vsync::Vsync>>()?;
                let (phase, period) = vsync.phase(std::time::Instant::now())?;
                Some(period - phase)
            },
            devices: None,
        },
    )?;
    adv_dx::d3d11::install_children(machine)?;
    static EXPORTS: std::sync::OnceLock<Vec<Vec<(String, u16)>>> = std::sync::OnceLock::new();
    let exports = EXPORTS.get_or_init(|| {
        let mut registry = Registry::default();
        adv_dx::dxgi::register(&mut registry);
        adv_dx::d3d11::register_children(&mut registry);
        IMAGES[DXGI..]
            .iter()
            .map(|module| graphics_exports(&registry, module))
            .collect()
    });
    for (&module, exports) in IMAGES[DXGI..].iter().zip(exports) {
        let exports: Vec<_> = exports
            .iter()
            .map(|(name, ordinal)| Export {
                name,
                ordinal: *ordinal,
            })
            .collect();
        let built = ModuleImage::build(machine, module, &exports, &[])?;
        adv_unix::builtin::install(drive, &built).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(feature = "gpu")]
fn graphics_exports(registry: &Registry, module: &str) -> Vec<(String, u16)> {
    let names: Vec<_> = registry
        .exports(module)
        .into_iter()
        .filter(|name| registry.is_implemented(module, name))
        .collect();
    let ordinals: HashMap<_, _> = adv_abi::ordinals::exports(module)
        .map(|(ordinal, name)| (name, ordinal))
        .collect();
    let mut named_ordinals = (ordinals.values().copied().max().unwrap_or(0) + 1)..;
    names
        .into_iter()
        .map(|name| {
            let ordinal = ordinals
                .get(name.as_str())
                .copied()
                .unwrap_or_else(|| named_ordinals.next().unwrap());
            (name, ordinal)
        })
        .collect()
}

/// A thread that waits has ended its frame: publish the software frame it
/// drew, as the built-in stack's GDI does. With presentation deferred to a
/// UI, nothing else publishes it while the guest idles.
fn flush_on_wait(env: &mut Env<'_>) -> Option<Outcome> {
    if let Some(outcome) = adv_drv::user::flush_on_wait(env) {
        return Some(outcome);
    }
    let memory = guest_memory(env.cpu);
    let store = env.ext.try_get_mut::<adv_device::surface::SurfaceStore>()?;
    match store.flush_context(env.context, &memory, adv_device::surface::FlushReason::Wait) {
        Ok(true) => {
            env.sys.poll_work();
            let serial = store.stats.presentations;
            frame_completed(env, serial)
        }
        Ok(false) => None,
        Err(error) => Some(Outcome::Fail(error)),
    }
}

fn frame_completed(env: &mut Env<'_>, serial: u64) -> Option<Outcome> {
    adv_drv::user::input::presented(env.ext);
    crate::graphics::frame_completed(env, serial)
}

/// The geometry DirectDraw sees for `hwnd`: its client area, as the user
/// driver last heard of it.
fn geometry(
    ext: &adv_machine::Ext,
    _: &adv_machine::Objects,
    hwnd: u32,
) -> Option<adv_dx::ddraw::WindowGeometry> {
    let [left, top, right, bottom] = adv_drv::user::window(ext, hwnd)?.client;
    let (width, height) = (right.checked_sub(left)?, bottom.checked_sub(top)?);
    let (origin, _) = adv_drv::user::client_geometry(ext, hwnd)?;
    (width > 0 && height > 0).then_some(adv_dx::ddraw::WindowGeometry {
        hwnd,
        width: width as u32,
        height: height as u32,
        origin,
    })
}

/// Build the image of front `image`, which imports GDI for its memory DCs.
fn install_image<B: Backend>(
    machine: &mut Machine<B>,
    drive: &Path,
    image: usize,
    names: &[&str],
) -> Result<(), String> {
    let exports: Vec<_> = names
        .iter()
        .zip(1..)
        .map(|(&name, ordinal)| Export { name, ordinal })
        .collect();
    let imports = [ImportModule {
        dll: GDI32.into(),
        imports: GDI_IMPORTS
            .into_iter()
            .map(|name| adv_pe::Import {
                name: ImportName::Name {
                    hint: 0,
                    name: name.into(),
                },
                iat_rva: 0,
            })
            .collect(),
    }];
    let built = ModuleImage::build(machine, IMAGES[image], &exports, &imports)?;
    adv_unix::builtin::install(drive, &built).map_err(|e| e.to_string())?;
    Ok(())
}

/// The heap of guest storage that the old stack's fronts allocate.
struct Heap(u32);

/// Large-block threshold matching `adv_os::heap::LARGE`.
const LARGE_POOL_MIN: u32 = 0x8_0000;
const LARGE_POOL_BYTES: usize = 32 * 1024 * 1024;
const LARGE_POOL_COUNT: usize = 16;

/// Bounded host-only pool of freed large blocks on [`Heap`] so texture churn
/// avoids unmapping and remapping guest VM pages every frame.
#[derive(Default)]
struct LargePool {
    live: HashMap<u32, u32>,
    free: Vec<(u32, u32)>,
    free_bytes: usize,
}

fn zero_guest(cpu: &mut dyn adv_cpu::backend::Guest, addr: u32, len: u32) -> bool {
    if len == 0 {
        return true;
    }
    if let Some(bytes) = cpu.writable_bytes(addr, len as usize) {
        bytes.fill(0);
        return true;
    }
    let zeros = [0u8; 4096];
    let mut offset = 0;
    while offset < len {
        let chunk = (len - offset).min(zeros.len() as u32);
        if cpu.write(addr + offset, &zeros[..chunk as usize]).is_err() {
            return false;
        }
        offset += chunk;
    }
    true
}

/// Allocation from [`Heap`], as the fronts' Win32 heap.
fn allocator() -> adv_dx::interop::GuestAllocator {
    adv_dx::interop::GuestAllocator {
        allocate: |env, length| {
            let heap = env.ext.get::<Heap>().0;
            if length >= LARGE_POOL_MIN {
                let max_cap = length.saturating_add(length / 4).max(length + 0x1_0000);
                let pool = env.ext.get_or_default::<LargePool>();
                if let Some((index, _)) = pool
                    .free
                    .iter()
                    .enumerate()
                    .filter(|(_, (_, cap))| *cap >= length && *cap <= max_cap)
                    .min_by_key(|(_, (_, cap))| *cap)
                {
                    let (addr, cap) = pool.free.remove(index);
                    pool.free_bytes -= cap as usize;
                    if zero_guest(env.cpu, addr, length) {
                        pool.live.insert(addr, cap);
                        return Some(addr);
                    }
                    let heaps = env.ext.get_mut::<adv_os::Heaps>();
                    heaps.free(env.cpu, &mut env.sys.vm, heap, addr);
                }
            }
            let heaps = env.ext.get_mut::<adv_os::Heaps>();
            let addr = heaps.alloc(env.cpu, &mut env.sys.vm, heap, length, true)?;
            if length >= LARGE_POOL_MIN {
                env.ext
                    .get_or_default::<LargePool>()
                    .live
                    .insert(addr, length);
            }
            Some(addr)
        },
        free: |env, pointer| {
            let heap = env.ext.get::<Heap>().0;
            if let Some(cap) = env.ext.get_or_default::<LargePool>().live.remove(&pointer) {
                let mut evicted = Vec::new();
                {
                    let pool = env.ext.get_mut::<LargePool>();
                    if cap as usize <= LARGE_POOL_BYTES {
                        while (pool.free.len() >= LARGE_POOL_COUNT
                            || pool.free_bytes + cap as usize > LARGE_POOL_BYTES)
                            && !pool.free.is_empty()
                        {
                            let (old_addr, old_cap) = pool.free.remove(0);
                            pool.free_bytes -= old_cap as usize;
                            evicted.push(old_addr);
                        }
                        pool.free.push((pointer, cap));
                        pool.free_bytes += cap as usize;
                    } else {
                        evicted.push(pointer);
                    }
                }
                if !evicted.is_empty() {
                    let heaps = env.ext.get_mut::<adv_os::Heaps>();
                    for addr in evicted {
                        heaps.free(env.cpu, &mut env.sys.vm, heap, addr);
                    }
                }
                return;
            }
            let heaps = env.ext.get_mut::<adv_os::Heaps>();
            heaps.free(env.cpu, &mut env.sys.vm, heap, pointer);
        },
    }
}

/// The Direct3D 9 front's assembly, with what it takes from USER and the heap
/// answered on the Wine userland.
#[cfg(feature = "gpu")]
fn d3d9_provider() -> adv_dx::d3d9::SurfaceProvider {
    adv_dx::d3d9::SurfaceProvider {
        allocator: allocator(),
        dc: memory_dc::<D3D9>(),
        // Windowed devices only; full-screen modes come with display modes.
        mode_changed: |_, _, _| Outcome::Return(0),
        device_released: |_, _| {},
        display: |_| {
            let (width, height) = adv_drv::user::DESKTOP;
            (width, height, 60)
        },
        // win32u numbers monitors from 1, and the driver reports one.
        monitor: 1,
        window: |ext, objects, hwnd| {
            geometry(ext, objects, hwnd).map(|window| (window.width, window.height))
        },
        visible: |ext, _, hwnd, surface| {
            if ext.get::<Surfaces<Snapshot>>().base_source() == Some(surface) {
                return Some(true);
            }
            adv_drv::user::window(ext, hwnd).map(|window| window.visible)
        },
        window_presented: |ext, hwnd| {
            if let Some(id) = ext.get::<Surfaces<Snapshot>>().base_source() {
                adv_drv::user::input::front_frame(ext, hwnd, id);
            }
        },
        completed: frame_completed,
        fast_forward: |_| false,
        guest_frame_wait: winmm::frame_timer::take,
        insert: |ext, target| ext.get_mut::<Surfaces<Snapshot>>().insert_native(target),
        target: |ext, id| ext.get::<Surfaces<Snapshot>>().native_target(id),
        cpu_access: |ext, id, rect| ext.get::<Surfaces<Snapshot>>().cpu_readback(id, rect),
        gpu_access: |env, id, rect, access| {
            let store = env.ext.get_mut::<Surfaces<Snapshot>>();
            let mut target = store.native_target(id)?;
            let rect = rect.unwrap_or_else(|| store[id].rect());
            target.image = store
                .gpu_access(id, rect, access, &guest_memory(env.cpu))
                .ok()?;
            Some(target)
        },
        remove: |ext, _, id| {
            if ext.get_mut::<Surfaces<Snapshot>>().delete(id) {
                Ok(())
            } else {
                Err("D3D9 surface identity no longer exists".into())
            }
        },
        replace: |ext, id, target| {
            ext.get_mut::<Surfaces<Snapshot>>()
                .replace_native(id, target)
                .map(|_| ())
        },

        present: native_present,

        present_copy: native_present,

        repeat: |ext, id| ext.get_mut::<Surfaces<Snapshot>>().repeat_native_frame(id),
        presentation_serial: |ext| ext.get::<Surfaces<Snapshot>>().output_serial(),
        vsync: |ext| {
            ext.try_get::<std::sync::Arc<adv_sync::vsync::Vsync>>()
                .and_then(|vsync| vsync.phase(std::time::Instant::now()))
        },
    }
}

#[cfg(feature = "gpu")]
fn native_present(
    ext: &mut adv_machine::Ext,
    id: u32,
    damage: Option<adv_device::surface::Region>,
    gpu: &mut adv_device::gpu::Gpu,
) -> Result<u64, adv_device::gpu::Error> {
    let surfaces = ext.get_mut::<Surfaces<Snapshot>>();
    surfaces
        .present_native(id, damage, gpu)
        .map_err(adv_device::gpu::Error::Invalid)?;
    Ok(surfaces.stats.presentations)
}

/// Memory DCs lent to the fronts' surfaces.
#[derive(Default)]
struct Dcs {
    /// Where the loader mapped each front's image.
    images: [Option<u32>; IMAGES.len()],
    /// The DC that memory DCs are made compatible with.
    device: u32,
    /// The bitmap of each lent DC.
    bitmaps: HashMap<u32, u32>,
}

fn import<const IMAGE: usize>(env: &mut Env<'_>, name: &str) -> Result<Import, String> {
    let image = IMAGES[IMAGE];
    let base = match env.ext.get_or_default::<Dcs>().images[IMAGE] {
        Some(base) => base,
        None => {
            let base = adv_nt::process::module_base(env.cpu, env.sys.peb, image)
                .ok_or_else(|| format!("{image} is not loaded"))?;
            env.ext.get_mut::<Dcs>().images[IMAGE] = Some(base);
            base
        }
    };
    Import::bind_in(env, image, base, GDI32, name)
        .ok_or_else(|| format!("{image} does not import {name}"))
}

/// Memory DCs whose GDI calls go through front `IMAGE`'s IAT.
fn memory_dc<const IMAGE: usize>() -> MemoryDc {
    MemoryDc {
        create: create::<IMAGE>,
        flush: |env, _, _, then| match import::<IMAGE>(env, "GdiFlush") {
            Ok(flush) => flush.call(env, [], |env, _| then(env, Ok(()))),
            Err(error) => then(env, Err(error)),
        },
        release: release::<IMAGE>,
        // Without guest code the GDI objects stay until the process ends.
        discard: |env, dc| {
            env.ext.get_or_default::<Dcs>().bitmaps.remove(&dc);
        },
    }
}

fn create<const IMAGE: usize>(env: &mut Env<'_>, surface: DcSurface, then: DcCreated) -> Outcome {
    // D3DDDIFORMAT values for the store's direct-color formats.
    let format = match surface.format {
        PixelFormat::Bgra32 => 22,
        PixelFormat::Bgr24 => 20,
        PixelFormat::Rgb565 => 23,
        PixelFormat::Rgb555 => 24,
        other => return then(env, Err(format!("no memory DC for {other:?} surfaces"))),
    };
    let device = env.ext.get_or_default::<Dcs>().device;
    if device != 0 {
        return create_from_memory::<IMAGE>(env, device, format, surface, then);
    }
    let compatible = match import::<IMAGE>(env, "CreateCompatibleDC") {
        Ok(compatible) => compatible,
        Err(error) => return then(env, Err(error)),
    };
    compatible.call(env, [0], move |env, created| {
        let device = created.value();
        if device == 0 {
            return then(env, Err("CreateCompatibleDC failed".into()));
        }
        env.ext.get_mut::<Dcs>().device = device;
        create_from_memory::<IMAGE>(env, device, format, surface, then)
    })
}

fn create_from_memory<const IMAGE: usize>(
    env: &mut Env<'_>,
    device: u32,
    format: u32,
    surface: DcSurface,
    then: DcCreated,
) -> Outcome {
    let desc = match slot(env) {
        Ok(desc) => desc,
        Err(error) => return then(env, Err(error)),
    };
    let prepared = {
        let words = [
            surface.pixels,
            format,
            surface.width,
            surface.height,
            surface.pitch,
            device,
            0,
            0,
            0,
        ];
        debug_assert_eq!(words.len() as u32 * 4, CREATE_DESC);
        write_words(env, desc, &words)
            .and_then(|()| import::<IMAGE>(env, "D3DKMTCreateDCFromMemory"))
    };
    let create = match prepared {
        Ok(create) => create,
        Err(error) => {
            free_slot(env, desc);
            return then(env, Err(error));
        }
    };
    create.call(env, [desc], move |env, status| {
        let created = match status.value() {
            0 => env
                .cpu
                .read_u32(desc + CREATED_DC)
                .and_then(|dc| Ok((dc, env.cpu.read_u32(desc + CREATED_BITMAP)?)))
                .map_err(|e| e.to_string()),
            status => Err(format!("D3DKMTCreateDCFromMemory: {status:#x}")),
        };
        free_slot(env, desc);
        match created {
            Ok((dc, bitmap)) => {
                env.ext.get_mut::<Dcs>().bitmaps.insert(dc, bitmap);
                then(env, Ok(dc))
            }
            Err(error) => then(env, Err(error)),
        }
    })
}

fn release<const IMAGE: usize>(env: &mut Env<'_>, dc: u32, then: DcFinished) -> Outcome {
    let Some(bitmap) = env.ext.get_or_default::<Dcs>().bitmaps.remove(&dc) else {
        return then(env, Err(format!("{dc:#x} is not a lent memory DC")));
    };
    let desc = match slot(env) {
        Ok(desc) => desc,
        Err(error) => return then(env, Err(error)),
    };
    let destroy = match write_words(env, desc, &[dc, bitmap])
        .and_then(|()| import::<IMAGE>(env, "D3DKMTDestroyDCFromMemory"))
    {
        Ok(destroy) => destroy,
        Err(error) => {
            free_slot(env, desc);
            return then(env, Err(error));
        }
    };
    destroy.call(env, [desc], move |env, status| {
        free_slot(env, desc);
        match status.value() {
            0 => then(env, Ok(())),
            status => then(env, Err(format!("D3DKMTDestroyDCFromMemory: {status:#x}"))),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::Cpu;
    use adv_machine::{Clock, layout::PARK_ADDR};

    #[test]
    fn large_pool_reuses_zeroed_blocks_and_evicts_past_cap() {
        let mut machine = Machine::new(
            Cpu::new(PARK_ADDR, false).unwrap(),
            Registry::default(),
            Clock::virtual_time(),
        )
        .unwrap();
        let heap = machine
            .ext
            .get_or_default::<adv_os::Heaps>()
            .create(&mut machine.cpu, &mut machine.sys.vm)
            .unwrap();
        machine.ext.insert(Heap(heap));
        let alloc = allocator();
        let size = 6 * 1024 * 1024;
        let mut env = Env {
            context: machine.sys.current,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        };
        let first = (alloc.allocate)(&mut env, size).unwrap();
        env.cpu.write(first, &[0x5a; 4096]).unwrap();
        (alloc.free)(&mut env, first);
        let reused = (alloc.allocate)(&mut env, size).unwrap();
        assert_eq!(reused, first, "freed large block should be reused in place");
        let mut head = [0u8; 4096];
        env.cpu.read(reused, &mut head).unwrap();
        assert!(head.iter().all(|&b| b == 0), "reused block must be zeroed");
        (alloc.free)(&mut env, reused);
        // Exceed the 32 MiB pool cap with distinct live allocations, then free all.
        let mut blocks = Vec::new();
        for _ in 0..7 {
            blocks.push((alloc.allocate)(&mut env, size).unwrap());
        }
        for b in blocks {
            (alloc.free)(&mut env, b);
        }
        let pool = env.ext.get::<LargePool>();
        assert!(pool.free_bytes <= LARGE_POOL_BYTES);
        assert!(pool.live.is_empty());
    }
}

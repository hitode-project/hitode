//! Assembly of the graphics fronts on the Wine userland (issue #58): our
//! DirectDraw and Direct3D 9 as PE images that Wine's loader maps, over the
//! process surface store, with memory DCs that Wine's GDI draws into.
//!
//! A surface's DC comes from `D3DKMTCreateDCFromMemory` over the surface's
//! own pixels, called through the IAT of the front's image so that a guest
//! hook sees it. Window geometry comes from the user driver in [`driver`].
use std::collections::HashMap;
use std::path::Path;

use adv_abi::Registry;
use adv_abi::dll::{Export, Import, ModuleImage};
use adv_cpu::backend::{Backend, GuestExt, Prot};
use adv_device::surface::PixelFormat;
use adv_dx::interop::{DcCreated, DcFinished, DcSurface, MemoryDc};
use adv_machine::layout::PAGE;
use adv_machine::{Env, Machine, Outcome};
use adv_pe::{ImportModule, ImportName};

use crate::graphics::{frame_completed, guest_memory, install_store};

pub mod driver;
#[cfg(any(target_vendor = "apple", target_os = "android"))]
pub mod session;
mod surface;

/// The display driver to name in the process's startup parameters.
pub const GRAPHICS_DRIVER: &str = driver::MODULE;

/// The fronts' images, by the index their memory DCs are bound to.
const IMAGES: [&str; 2] = ["ddraw.dll", "d3d9.dll"];
const DDRAW: usize = 0;
#[cfg(feature = "gpu")]
const D3D9: usize = 1;
const GDI32: &str = "gdi32.dll";
const DDRAW_EXPORTS: [&str; 4] = [
    "DirectDrawCreate",
    "DirectDrawCreateEx",
    "DirectDrawEnumerateA",
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
/// Room for one descriptor; `D3DKMT_DESTROYDCFROMMEMORY` is hDc, hBitmap.
const SLOT: u32 = 64;

pub fn register(registry: &mut Registry) {
    adv_dx::ddraw::register(registry);
    #[cfg(feature = "gpu")]
    adv_dx::d3d9::register(registry);
    driver::register(registry);
    surface::register(registry);
}

/// Install the surface store, the graphics fronts and the user driver, and
/// put their images in `prefix`, in place of Wine's. Call before the process
/// starts with [`GRAPHICS_DRIVER`].
pub fn install<B: Backend>(
    machine: &mut Machine<B>,
    prefix: &Path,
    width: u32,
    height: u32,
) -> Result<(), String> {
    install_store(machine, width, height)?;
    machine.sys.context_wait = Some(flush_on_wait);
    machine
        .ext
        .insert(adv_nt::process::Desktop(driver::start_desktop));
    adv_dx::ddraw::install(
        machine,
        adv_dx::ddraw::Provider {
            dc: memory_dc::<DDRAW>(),
            memory: guest_memory,
            window: driver::geometry,
            completed: frame_completed,
        },
    )?;
    install_image(machine, prefix, DDRAW, &DDRAW_EXPORTS)?;
    #[cfg(feature = "gpu")]
    {
        let heap = machine
            .ext
            .get_or_default::<adv_os::Heaps>()
            .create(&mut machine.cpu, &mut machine.sys.vm)
            .map_err(|e| e.to_string())?;
        machine.ext.insert(Heap(heap));
        adv_dx::d3d9::install(machine, d3d9_provider())?;
        install_image(machine, prefix, D3D9, &D3D9_EXPORTS)?;
    }
    let driver = driver::image(machine)?;
    adv_unix::builtin::install(prefix, &driver).map_err(|e| e.to_string())?;
    Ok(())
}

/// A thread that waits has ended its frame: publish the software frame it
/// drew, as the built-in stack's GDI does. With presentation deferred to a
/// UI, nothing else publishes it while the guest idles.
fn flush_on_wait(env: &mut Env<'_>) -> Option<Outcome> {
    let memory = guest_memory(env.cpu);
    let store = env.ext.try_get_mut::<adv_device::surface::SurfaceStore>()?;
    match store.flush_context(env.context, &memory, adv_device::surface::FlushReason::Wait) {
        Ok(true) => {
            let serial = store.stats.presentations;
            frame_completed(env, serial)
        }
        Ok(false) => None,
        Err(error) => Some(Outcome::Fail(error)),
    }
}

/// Build the image of front `image`, which imports GDI for its memory DCs.
fn install_image<B: Backend>(
    machine: &mut Machine<B>,
    prefix: &Path,
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
    adv_unix::builtin::install(prefix, &built).map_err(|e| e.to_string())?;
    Ok(())
}

/// The heap of guest storage that Direct3D 9 allocates.
#[cfg(feature = "gpu")]
struct Heap(u32);

/// The old stack's Direct3D 9 assembly, with what it takes from USER and the
/// Win32 heap answered on the Wine userland.
#[cfg(feature = "gpu")]
fn d3d9_provider() -> adv_dx::d3d9::SurfaceProvider {
    adv_dx::d3d9::SurfaceProvider {
        allocator: adv_dx::interop::GuestAllocator {
            allocate: |env, length| {
                let heap = env.ext.get::<Heap>().0;
                let heaps = env.ext.get_mut::<adv_os::Heaps>();
                heaps.alloc(env.cpu, &mut env.sys.vm, heap, length, true)
            },
            free: |env, pointer| {
                let heap = env.ext.get::<Heap>().0;
                let heaps = env.ext.get_mut::<adv_os::Heaps>();
                heaps.free(env.cpu, &mut env.sys.vm, heap, pointer);
            },
        },
        dc: memory_dc::<D3D9>(),
        // Windowed devices only; full-screen modes come with display modes.
        mode_changed: |_, _, _| Outcome::Return(0),
        device_released: |_, _| {},
        display: |ext| {
            let store = ext.get::<adv_device::surface::SurfaceStore>();
            (store.width, store.height, 60)
        },
        window: |ext, objects, hwnd| {
            driver::geometry(ext, objects, hwnd).map(|window| (window.width, window.height))
        },
        visible: |ext, _, hwnd| driver::visible(ext, hwnd),
        fast_forward: |_| false,
        unpaced: |_| {},
        ..crate::graphics::d3d9_provider()
    }
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
    /// Free descriptor slots in guest memory.
    slots: Vec<u32>,
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

/// A 64-byte block of guest memory for a call's in and out parameters.
pub(super) fn slot(env: &mut Env<'_>) -> Result<u32, String> {
    if let Some(slot) = env.ext.get_or_default::<Dcs>().slots.pop() {
        return Ok(slot);
    }
    let page = env
        .sys
        .vm
        .alloc(env.cpu, PAGE, Prot::RW)
        .map_err(|e| e.to_string())?;
    let slots = &mut env.ext.get_mut::<Dcs>().slots;
    slots.extend((1..PAGE / SLOT).map(|index| page + index * SLOT));
    Ok(page)
}

/// Give back a block from [`slot`].
pub(super) fn free_slot(env: &mut Env<'_>, slot: u32) {
    env.ext.get_or_default::<Dcs>().slots.push(slot);
}

pub(super) fn write_words(env: &mut Env<'_>, at: u32, words: &[u32]) -> Result<(), String> {
    let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    env.cpu.write(at, &bytes).map_err(|e| e.to_string())
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
            env.ext.get_mut::<Dcs>().slots.push(desc);
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
        env.ext.get_mut::<Dcs>().slots.push(desc);
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
            env.ext.get_mut::<Dcs>().slots.push(desc);
            return then(env, Err(error));
        }
    };
    destroy.call(env, [desc], move |env, status| {
        env.ext.get_mut::<Dcs>().slots.push(desc);
        match status.value() {
            0 => then(env, Ok(())),
            status => then(env, Err(format!("D3DKMTDestroyDCFromMemory: {status:#x}"))),
        }
    })
}

//! Assembly of the graphics fronts on the Wine userland (issue #58): our
//! DirectDraw as a PE image that Wine's loader maps, over the process surface
//! store, with memory DCs that Wine's GDI draws into.
//!
//! A DirectDraw surface's DC comes from `D3DKMTCreateDCFromMemory` over the
//! surface's own pixels, called through the image's IAT so that a guest hook
//! sees it. Window geometry comes from the user driver in [`driver`].
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
mod surface;

/// The display driver to name in the process's startup parameters.
pub const GRAPHICS_DRIVER: &str = driver::MODULE;

const DDRAW: &str = "ddraw.dll";
const GDI32: &str = "gdi32.dll";
const EXPORTS: [&str; 4] = [
    "DirectDrawCreate",
    "DirectDrawCreateEx",
    "DirectDrawEnumerateA",
    "DirectDrawEnumerateW",
];
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
    driver::register(registry);
    surface::register(registry);
}

/// Install the surface store, DirectDraw and the user driver, and put their
/// images in `prefix`, DirectDraw's in place of Wine's. Call before the
/// process starts with [`GRAPHICS_DRIVER`].
pub fn install<B: Backend>(
    machine: &mut Machine<B>,
    prefix: &Path,
    width: u32,
    height: u32,
) -> Result<(), String> {
    install_store(machine, width, height)?;
    machine
        .ext
        .insert(adv_nt::process::Desktop(driver::start_desktop));
    adv_dx::ddraw::install(
        machine,
        adv_dx::ddraw::Provider {
            dc: memory_dc(),
            memory: guest_memory,
            window: driver::geometry,
            completed: frame_completed,
        },
    )?;
    let exports: Vec<_> = EXPORTS
        .into_iter()
        .zip(1..)
        .map(|(name, ordinal)| Export { name, ordinal })
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
    let image = ModuleImage::build(machine, DDRAW, &exports, &imports)?;
    adv_unix::builtin::install(prefix, &image).map_err(|e| e.to_string())?;
    let driver = driver::image(machine)?;
    adv_unix::builtin::install(prefix, &driver).map_err(|e| e.to_string())?;
    Ok(())
}

/// Memory DCs lent to DirectDraw surfaces.
#[derive(Default)]
struct Dcs {
    /// Where the loader mapped our DirectDraw image.
    image: Option<u32>,
    /// The DC that memory DCs are made compatible with.
    device: u32,
    /// The bitmap of each lent DC.
    bitmaps: HashMap<u32, u32>,
    /// Free descriptor slots in guest memory.
    slots: Vec<u32>,
}

fn import(env: &mut Env<'_>, name: &str) -> Result<Import, String> {
    let base = match env.ext.get_or_default::<Dcs>().image {
        Some(base) => base,
        None => {
            let base = adv_nt::process::module_base(env.cpu, env.sys.peb, DDRAW)
                .ok_or("DirectDraw is not loaded")?;
            env.ext.get_mut::<Dcs>().image = Some(base);
            base
        }
    };
    Import::bind_in(env, DDRAW, base, GDI32, name)
        .ok_or_else(|| format!("{DDRAW} does not import {name}"))
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

fn memory_dc() -> MemoryDc {
    MemoryDc {
        create,
        flush: |env, _, _, then| match import(env, "GdiFlush") {
            Ok(flush) => flush.call(env, [], |env, _| then(env, Ok(()))),
            Err(error) => then(env, Err(error)),
        },
        release,
        // Without guest code the GDI objects stay until the process ends.
        discard: |env, dc| {
            env.ext.get_or_default::<Dcs>().bitmaps.remove(&dc);
        },
    }
}

fn create(env: &mut Env<'_>, surface: DcSurface, then: DcCreated) -> Outcome {
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
        return create_from_memory(env, device, format, surface, then);
    }
    let compatible = match import(env, "CreateCompatibleDC") {
        Ok(compatible) => compatible,
        Err(error) => return then(env, Err(error)),
    };
    compatible.call(env, [0], move |env, created| {
        let device = created.value();
        if device == 0 {
            return then(env, Err("CreateCompatibleDC failed".into()));
        }
        env.ext.get_mut::<Dcs>().device = device;
        create_from_memory(env, device, format, surface, then)
    })
}

fn create_from_memory(
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
        write_words(env, desc, &words).and_then(|()| import(env, "D3DKMTCreateDCFromMemory"))
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

fn release(env: &mut Env<'_>, dc: u32, then: DcFinished) -> Outcome {
    let Some(bitmap) = env.ext.get_or_default::<Dcs>().bitmaps.remove(&dc) else {
        return then(env, Err(format!("{dc:#x} is not a lent memory DC")));
    };
    let desc = match slot(env) {
        Ok(desc) => desc,
        Err(error) => return then(env, Err(error)),
    };
    let destroy = match write_words(env, desc, &[dc, bitmap])
        .and_then(|()| import(env, "D3DKMTDestroyDCFromMemory"))
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

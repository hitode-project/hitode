//! Read-only bitmap views reuse GDI metadata, surface palettes and guest bytes.
use crate::Win32;
use adv_cpu::backend::{Guest, Prot};
use adv_device::surface::{MemoryOperations, MemoryView, PixelFormat, Surface, SurfaceStore};
use adv_machine::Env;
use adv_os::gdi::GdiObj;
use std::{borrow::Cow, sync::Arc};
pub(crate) fn memory(cpu: &mut dyn Guest) -> MemoryView<'_, dyn Guest + '_> {
    MemoryView::new(
        cpu,
        MemoryOperations {
            read: |cpu, address, length| {
                let mut bytes = vec![0; length];
                cpu.read(address, &mut bytes).map_err(|e| e.to_string())?;
                Ok(Cow::Owned(bytes))
            },
            write: |cpu, address, bytes| cpu.write(address, bytes).map_err(|e| e.to_string()),
            writable: |cpu, address, length| {
                adv_abi::guest::check_range(cpu, address, length as u32, Prot::WRITE)
                    .map_err(|e| e.to_string())
            },
            copy: |cpu, source, destination, length| {
                cpu.copy_memory(source, destination, length)
                    .map_err(|e| e.to_string())
            },
        },
    )
}
pub(crate) struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub depth: u16,
    pub rgba: Vec<u8>,
}
pub(crate) fn snapshot(env: &mut Env<'_>, handle: u32) -> Result<Bitmap, u32> {
    let Some(GdiObj::Bitmap {
        width,
        height,
        bpp,
        bits,
        stride,
        owned,
        ..
    }) = env.ext.get::<Win32>().gdi.get(&env.sys.objects, handle)
    else {
        return Err(6);
    };
    let (width, height, depth, bits, stride, owned) =
        (*width, *height, *bpp, *bits, *stride, *owned);
    let bottom_up = !owned && height > 0;
    if width <= 0 || height == 0 || bits == 0 {
        return Err(87);
    }
    let width = width as u32;
    let height = height.unsigned_abs();
    let count = u64::from(width) * u64::from(height) * 4;
    if count > adv_device::image::LIMIT as u64 {
        return Err(50);
    }
    let stored = env
        .ext
        .try_get::<SurfaceStore>()
        .and_then(|s| s.get(handle))
        .cloned();
    let surface = if let Some(surface) = stored {
        surface
    } else {
        let format = match depth {
            1 => PixelFormat::Indexed1,
            16 => PixelFormat::Rgb555,
            24 => PixelFormat::Bgr24,
            32 => PixelFormat::Bgra32,
            _ => return Err(50),
        };
        let palette = if depth == 1 {
            let mut colors = [[0; 4]; 256];
            colors[1] = [255, 255, 255, 0];
            Some(Arc::new(colors))
        } else {
            None
        };
        Surface {
            pixels: bits,
            width,
            height,
            pitch: stride,
            bottom_up,
            locked: None,
            version: 0,
            format,
            palette,
        }
    };
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(count as usize).map_err(|_| 8u32)?;
    // Flush buffered GDI writes through their existing device owner. Reading a
    // bitmap has no frame-pacing return value and cannot skip copying its bytes.
    if let Some(store) = env.ext.try_get_mut::<SurfaceStore>() {
        store
            .flush(
                &memory(env.cpu),
                adv_device::surface::FlushReason::Immediate,
            )
            .map_err(|_| 998u32)?;
    }
    let memory = memory(env.cpu);
    for y in 0..height {
        let row = surface
            .read_pixels(&memory, 0, y, width)
            .map_err(|_| 998u32)?;
        for raw in row.chunks_exact(surface.format.bytes() as usize) {
            let [b, g, r, a] = surface.decode_pixel(raw);
            rgba.extend_from_slice(&[r, g, b, if depth == 32 { a } else { 255 }]);
        }
    }
    Ok(Bitmap {
        width,
        height,
        depth,
        rgba,
    })
}

//! Borrowed CPU canvases for graphics frontends. The store owns their arena
//! allocation and the common GDI namespace owns their DC/bitmap metadata.
use super::*;
use adv_device::surface::{PixelFormat, SurfaceMemory};
use adv_os::gdi::GdiObj;

pub fn acquire(
    env: &mut Env<'_>,
    width: u32,
    height: u32,
    source: u32,
    pitch: u32,
) -> Result<u32, String> {
    if !available(env) {
        return Err("GDI surface provider is not installed".into());
    }
    adv_device::surface::size(width, height)?;
    let row = width.checked_mul(4).ok_or("canvas row overflow")?;
    let bytes = row.checked_mul(height).ok_or("canvas size overflow")?;
    if pitch < row {
        return Err("canvas source pitch is too small".into());
    }
    let source_len = (height - 1)
        .checked_mul(pitch)
        .and_then(|n| n.checked_add(row))
        .ok_or("canvas source overflow")?;
    adv_abi::guest::check_range(env.cpu, source, source_len, Prot::READ)
        .map_err(|e| e.to_string())?;
    let growth = env
        .ext
        .get::<SurfaceStore>()
        .arena_growth(bytes)
        .ok_or("surface storage exhausted")?;
    if growth != 0 {
        let address = env
            .sys
            .vm
            .alloc(env.cpu, growth, Prot::RW)
            .map_err(|e| e.to_string())?;
        let length = env.sys.vm.region(address).unwrap();
        if let Err(error) = env
            .ext
            .get_mut::<SurfaceStore>()
            .extend_arena(address, length)
        {
            let _ = env.sys.vm.free(env.cpu, address);
            return Err(error);
        }
    }
    let provider = env.ext.get::<State>().provider.unwrap();
    let (state, store) = env.ext.get2_installed_mut::<State, SurfaceStore>();
    let mut memory = (provider.memory)(env.cpu);
    let bitmap = store.create(width, height, false, PixelFormat::Bgra32, &mut memory)?;
    let surface = store.get(bitmap).unwrap();
    let bits = surface.pixels;
    for y in 0..height {
        if let Err(error) = memory.copy(source + y * pitch, bits + y * row, row as usize) {
            store.delete(bitmap);
            return Err(error);
        }
    }
    if !state.dcs.register_bitmap(
        &mut env.sys.objects,
        bitmap,
        GdiObj::Bitmap {
            width: width as i32,
            height: -(height as i32),
            bpp: 32,
            bits,
            stride: row,
            owned: false,
            dib_colors: 0,
        },
    ) {
        store.delete(bitmap);
        return Err("GDI bitmap namespace exhausted".into());
    }
    let dc = store.new_id();
    if !state.dcs.create(&mut env.sys.objects, dc, bitmap) {
        state.dcs.remove_bitmap(&mut env.sys.objects, bitmap);
        store.delete(bitmap);
        return Err("GDI DC namespace exhausted".into());
    }
    state.surface_dcs.insert(dc, bitmap);
    Ok(dc)
}

pub fn writeback(env: &mut Env<'_>, dc: u32, destination: u32, pitch: u32) -> Result<(), String> {
    let state = env.ext.get::<State>();
    let bitmap = *state
        .surface_dcs
        .get(&dc)
        .ok_or("DC is not a borrowed surface canvas")?;
    // The original canvas is authoritative even if guest GDI state is changed.
    let surface = env
        .ext
        .get::<SurfaceStore>()
        .get(bitmap)
        .ok_or("borrowed canvas is missing")?
        .clone();
    let row = surface.width * 4;
    if pitch < row {
        return Err("canvas destination pitch is too small".into());
    }
    let length = (surface.height - 1)
        .checked_mul(pitch)
        .and_then(|n| n.checked_add(row))
        .ok_or("canvas destination overflow")?;
    adv_abi::guest::check_range(
        env.cpu,
        surface.pixels,
        surface.pitch * surface.height,
        Prot::READ,
    )
    .map_err(|e| e.to_string())?;
    adv_abi::guest::check_range(env.cpu, destination, length, Prot::WRITE)
        .map_err(|e| e.to_string())?;
    for y in 0..surface.height {
        env.cpu
            .copy_memory(
                surface.pixels + y * surface.pitch,
                destination + y * pitch,
                row as usize,
            )
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
pub fn release(env: &mut Env<'_>, dc: u32) {
    let (state, store) = env.ext.get2_installed_mut::<State, SurfaceStore>();
    if let Some(bitmap) = state.surface_dcs.remove(&dc) {
        state.dcs.remove(&mut env.sys.objects, dc);
        state.dcs.remove_bitmap(&mut env.sys.objects, bitmap);
        store.delete(bitmap);
    }
}

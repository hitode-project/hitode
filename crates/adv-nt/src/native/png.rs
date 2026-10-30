//! Pinned libpng row ABIs. Device math is supplied by the codec assembly.
use adv_cpu::backend::Prot;
use adv_machine::{Conv, Env};

#[derive(Clone, Copy)]
pub enum PngFilter {
    Sub,
    Up,
    Average,
    Paeth,
}

/// The existing device kernels; returning false retains guest execution.
pub struct PngRows(pub fn(PngFilter, &mut [u8], &[u8], usize) -> bool);

pub(super) fn bodies() -> Vec<super::Body> {
    let entries: &[(&str, super::BodyFn)] = &[
        ("png_read_filter_row_sub", sub),
        ("png_read_filter_row_up", up),
        ("png_read_filter_row_avg", average),
        ("png_read_filter_row_paeth_1byte_pixel", paeth_one),
        ("png_read_filter_row_paeth_multibyte_pixel", paeth),
    ];
    entries
        .iter()
        .map(|(name, body)| super::Body {
            name: format!("windowscodecs.dll!{name}").into(),
            conv: Conv::Cdecl { argc: 3 },
            body: Some(*body),
            handler: super::resume_original,
        })
        .collect()
}
fn sub(env: &mut Env<'_>) -> Option<u64> {
    filter(env, PngFilter::Sub, false)
}
fn up(env: &mut Env<'_>) -> Option<u64> {
    filter(env, PngFilter::Up, false)
}
fn average(env: &mut Env<'_>) -> Option<u64> {
    filter(env, PngFilter::Average, false)
}
fn paeth_one(env: &mut Env<'_>) -> Option<u64> {
    filter(env, PngFilter::Paeth, true)
}
fn paeth(env: &mut Env<'_>) -> Option<u64> {
    filter(env, PngFilter::Paeth, false)
}
fn overlaps(a: u32, b: u32, len: u32) -> bool {
    u64::from(a) < u64::from(b) + u64::from(len) && u64::from(b) < u64::from(a) + u64::from(len)
}
// spec: NAT-BODY-9
fn filter(env: &mut Env<'_>, kind: PngFilter, one: bool) -> Option<u64> {
    let kernel = env.ext.try_get::<PngRows>()?.0;
    let (info, row, previous) = (env.arg(0), env.arg(1), env.arg(2));
    // Wine's i386 png_row_info: rowbytes at 4, pixel_depth at 11 (DWARF).
    let mut metadata = [0; 12];
    env.cpu.read(info, &mut metadata).ok()?;
    let len = u32::from_le_bytes(metadata[4..8].try_into().ok()?);
    let bpp = if one {
        1
    } else {
        usize::from(metadata[11]).div_ceil(8)
    };
    if bpp == 0
        || len < bpp as u32
        || !env.cpu.accessible(row, len, Prot::RW)
        || (u64::from(row) < u64::from(info) + 12
            && u64::from(info) < u64::from(row) + u64::from(len))
    {
        return None;
    }
    let above = if matches!(kind, PngFilter::Sub) {
        &[][..]
    } else {
        if !env.cpu.accessible(previous, len, Prot::READ) || overlaps(row, previous, len) {
            return None;
        }
        env.cpu.shared_bytes(previous, len as usize)?
    };
    let mut stack = [0; 8192];
    let mut heap = Vec::new();
    let output = if len as usize <= stack.len() {
        &mut stack[..len as usize]
    } else {
        heap.resize(len as usize, 0);
        &mut heap[..]
    };
    env.cpu.read(row, output).ok()?;
    if !kernel(kind, output, above, bpp) {
        return None;
    }
    env.cpu.write(row, output).ok()?;
    Some(0)
}

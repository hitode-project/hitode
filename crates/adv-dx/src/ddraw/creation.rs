//! CreateSurface descriptors: read either descriptor version, check them in
//! the order Windows does, and keep what each surface reports back.
use super::*;

pub(super) const DDSD_CAPS: u32 = 1;
pub(super) const DDSD_HEIGHT: u32 = 2;
pub(super) const DDSD_WIDTH: u32 = 4;
pub(super) const DDSD_PITCH: u32 = 8;
pub(super) const DDSD_BACKBUFFERCOUNT: u32 = 0x20;
const DDSD_ZBUFFERBITDEPTH: u32 = 0x40;
pub(super) const DDSD_LPSURFACE: u32 = 0x800;
pub(super) const DDSD_PIXELFORMAT: u32 = 0x1000;
pub(super) const DDSD_MIPMAPCOUNT: u32 = 0x20000;
/// DDSD_CKDESTOVERLAY, DDSD_CKDESTBLT, DDSD_CKSRCOVERLAY and DDSD_CKSRCBLT,
/// in descriptor order, with the DDCKEY kind each one carries.
pub(super) const KEYS: [(u32, u32); 4] = [(0x2000, 4), (0x4000, 2), (0x8000, 0x10), (0x10000, 8)];

pub(super) const BACKBUFFER: u32 = 4;
pub(super) const COMPLEX: u32 = 8;
pub(super) const FLIP: u32 = 0x10;
pub(super) const FRONTBUFFER: u32 = 0x20;
pub(super) const OVERLAY: u32 = 0x80;
pub(super) const PRIMARY: u32 = 0x200;
pub(super) const SYSTEM_MEMORY: u32 = 0x800;
pub(super) const TEXTURE: u32 = 0x1000;
pub(super) const VIDEO_MEMORY: u32 = 0x4000;
pub(super) const VISIBLE: u32 = 0x8000;
const WRITE_ONLY: u32 = 0x10000;
pub(super) const ZBUFFER: u32 = 0x20000;
pub(super) const MIPMAP: u32 = 0x40_0000;
const ALLOC_ON_LOAD: u32 = 0x400_0000;
const LOCAL_VIDEO_MEMORY: u32 = 0x1000_0000;
pub(super) const CUBEMAP: u32 = 0x200;
const CUBEMAP_ALL_FACES: u32 = 0xfc00;
pub(super) const MANAGED: u32 = 0x10 | 0x2_0000;

const DDPF_PALETTEINDEXED8: u32 = 0x20;
const DDPF_RGB: u32 = 0x40;
const DDPF_ZBUFFER: u32 = 0x400;

pub(super) const INVALID_CAPS: u32 = 0x8876_0064;
pub(super) const NO_COOPERATIVE_LEVEL: u32 = 0x8876_00d4;
pub(super) const CANNOT_ATTACH: u32 = 0x8876_000a;
pub(super) const CANNOT_DETACH: u32 = 0x8876_0014;
pub(super) const ALREADY_ATTACHED: u32 = 0x8876_019a;
pub(super) const NOT_ATTACHED: u32 = 0x8876_01cc;
pub(super) const NOT_FLIPPABLE: u32 = 0x8876_0246;
pub(super) const INVALID_SURFACE_TYPE: u32 = 0x8876_0250;
pub(super) const OFFSCREEN_PLAIN: u32 = 0x40;
pub(super) const THREE_D_DEVICE: u32 = 0x2000;
pub(super) const MIPMAP_SUBLEVEL: u32 = 0x1_0000;
const NO_OVERLAY_HW: u32 = 0x8876_0104;
const NO_COLOR_KEY_HW: u32 = 0x8876_00dc;
const INVALID_PIXEL_FORMAT: u32 = 0x8876_0091;
const NO_AGGREGATION: u32 = 0x8004_0110;

/// A surface descriptor as DDSURFACEDESC2, which DDSURFACEDESC converts to.
#[derive(Clone, Copy, Default)]
pub(super) struct Described {
    pub flags: u32,
    pub width: u32,
    pub height: u32,
    pub backbuffers: u32,
    pub mipmaps: u32,
    /// lPitch and lpSurface as given; reports take both from the store.
    pub pitch: u32,
    pub memory: u32,
    /// Low and high values of each key in [`KEYS`] order.
    pub keys: [(u32, u32); 4],
    /// DDPIXELFORMAT.
    pub format: [u8; 32],
    pub caps: [u32; 4],
}

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

impl Described {
    /// Read the `len`-byte descriptor at `at` (108 or 124 bytes).
    pub fn read(m: &dyn SurfaceMemory, at: u32, len: u32) -> Result<Self, CallError> {
        Ok(Self::parse(&m.arg_read(at, len as usize)?))
    }

    /// Parse a DDSURFACEDESC (108 bytes) or DDSURFACEDESC2 (124 bytes).
    fn parse(raw: &[u8]) -> Self {
        let flags = word(raw, 4);
        let mut d = Self {
            flags,
            height: word(raw, 8),
            width: word(raw, 12),
            pitch: word(raw, 16),
            backbuffers: word(raw, 20),
            mipmaps: word(raw, 24),
            memory: word(raw, 36),
            keys: std::array::from_fn(|i| (word(raw, 40 + i * 8), word(raw, 44 + i * 8))),
            format: raw[72..104].try_into().unwrap(),
            caps: [word(raw, 104), 0, 0, 0],
        };
        if raw.len() == 124 {
            d.caps = std::array::from_fn(|i| word(raw, 104 + i * 4));
        } else if flags & DDSD_ZBUFFERBITDEPTH != 0 {
            // DDSURFACEDESC names Z buffers by depth; DDSURFACEDESC2 by format.
            d.flags = flags & !DDSD_ZBUFFERBITDEPTH;
            if flags & DDSD_PIXELFORMAT == 0 {
                d.flags |= DDSD_PIXELFORMAT;
                d.format = z_format(d.mipmaps);
            }
        }
        d
    }

    /// Write the descriptor as `len` bytes (108 or 124) for a surface whose
    /// pixels are at `pitch` and `pixels`, zero outside a lock. Like Windows,
    /// a locked descriptor carries the address without DDSD_LPSURFACE.
    pub fn bytes(&self, len: u32, pitch: u32, pixels: u32) -> Vec<u8> {
        let mut flags = self.flags | DDSD_PITCH;
        let mut depth = self.mipmaps;
        if len == 108 && self.caps[0] & ZBUFFER != 0 && flags & DDSD_PIXELFORMAT != 0 {
            flags = flags & !DDSD_PIXELFORMAT | DDSD_ZBUFFERBITDEPTH;
            depth = word(&self.format, 12);
        }
        let mut out = vec![0u8; len as usize];
        let mut put = |at: usize, value: u32| out[at..at + 4].copy_from_slice(&value.to_le_bytes());
        put(0, len);
        put(4, flags);
        put(8, self.height);
        put(12, self.width);
        put(16, pitch);
        put(20, self.backbuffers);
        put(24, depth);
        put(36, pixels);
        for (i, &(low, high)) in self.keys.iter().enumerate() {
            put(40 + i * 8, low);
            put(44 + i * 8, high);
        }
        let caps = if len == 124 { 4 } else { 1 };
        for (i, &value) in self.caps[..caps].iter().enumerate() {
            put(104 + i * 4, value);
        }
        out[72..104].copy_from_slice(&self.format);
        out
    }
}

/// DDPIXELFORMAT of a Z buffer `depth` bits deep.
fn z_format(depth: u32) -> [u8; 32] {
    let mut f = [0u8; 32];
    f[0] = 32;
    f[4..8].copy_from_slice(&DDPF_ZBUFFER.to_le_bytes());
    f[12..16].copy_from_slice(&depth.to_le_bytes());
    let mask = u32::MAX.checked_shr(32u32.wrapping_sub(depth)).unwrap_or(0);
    f[20..24].copy_from_slice(&mask.to_le_bytes());
    f
}

/// DDPIXELFORMAT of a display or storage `format`.
pub(super) fn pixel_format(format: PixelFormat) -> [u8; 32] {
    let mut f = [0u8; 32];
    let (r, g, b) = format.masks();
    let flags = if matches!(format, PixelFormat::Indexed8) {
        DDPF_RGB | DDPF_PALETTEINDEXED8
    } else {
        DDPF_RGB
    };
    for (at, value) in [
        (0, 32),
        (4, flags),
        (12, format.bytes() * 8),
        (16, r),
        (20, g),
        (24, b),
    ] {
        f[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    f
}

/// The storage holding pixels of DDPIXELFORMAT `format`, if supported.
pub(super) fn storage(format: &[u8; 32]) -> Option<PixelFormat> {
    let (flags, bits) = (word(format, 4), word(format, 12));
    let masks = (word(format, 16), word(format, 20), word(format, 24));
    if flags & DDPF_ZBUFFER != 0 {
        return match bits {
            16 => Some(PixelFormat::Rgb565),
            24 | 32 => Some(PixelFormat::Bgra32),
            _ => None,
        };
    }
    if flags & DDPF_RGB == 0 {
        return None;
    }
    match (bits, masks) {
        (8, (0, 0, 0)) if flags & DDPF_PALETTEINDEXED8 != 0 => Some(PixelFormat::Indexed8),
        (16, (0xf800, 0x7e0, 0x1f)) => Some(PixelFormat::Rgb565),
        (16, (0x7c00, 0x3e0, 0x1f)) => Some(PixelFormat::Rgb555),
        (24, (0xff_0000, 0xff00, 0xff)) => Some(PixelFormat::Bgr24),
        (32, (0xff_0000, 0xff00, 0xff)) => Some(PixelFormat::Bgra32),
        _ => None,
    }
}

/// Check `d` as Windows does for a DirectDraw `version` object, completing
/// what the caller left out from the display mode.
pub(super) fn validate(
    d: &mut Described,
    outer: u32,
    version: u8,
    exclusive: bool,
    (width, height, display): (u32, u32, PixelFormat),
) -> Result<(), u32> {
    let [caps, caps2, ..] = d.caps;
    let has = |bits: u32| caps & bits != 0;
    let cube = caps2 & CUBEMAP != 0;
    let managed = caps2 & MANAGED != 0;
    if outer != 0 {
        return Err(NO_AGGREGATION);
    }
    d.flags |= DDSD_CAPS;
    if has(COMPLEX) && !has(FLIP | MIPMAP) && !cube {
        return Err(INVALID_CAPS);
    }
    if d.flags & DDSD_MIPMAPCOUNT != 0 && !has(COMPLEX) {
        return Err(INVALID_CAPS);
    }
    if has(FLIP) {
        if d.flags & DDSD_BACKBUFFERCOUNT == 0 || d.backbuffers == 0 || !has(COMPLEX) {
            return Err(INVALID_CAPS);
        }
        // Unlike Wine, 32-bit Windows also makes flippable textures.
        if cube {
            return Err(INVALID);
        }
    } else if d.flags & DDSD_BACKBUFFERCOUNT != 0 {
        return Err(if cube { INVALID } else { INVALID_CAPS });
    }
    if has(PRIMARY) {
        if has(TEXTURE) || (has(COMPLEX) && !has(FLIP)) {
            return Err(INVALID_CAPS);
        }
        if has(FLIP) && !exclusive {
            return Err(NO_EXCLUSIVE_MODE);
        }
    }
    if caps & (VIDEO_MEMORY | SYSTEM_MEMORY) == VIDEO_MEMORY | SYSTEM_MEMORY
        || (has(ALLOC_ON_LOAD | MIPMAP) && !has(TEXTURE))
        || (caps2 & CUBEMAP_ALL_FACES != 0 && !cube)
    {
        return Err(INVALID_CAPS);
    }
    if cube && caps2 & CUBEMAP_ALL_FACES == 0 {
        return Err(INVALID);
    }
    if managed && (!has(TEXTURE) || has(VIDEO_MEMORY | SYSTEM_MEMORY)) {
        return Err(INVALID_CAPS);
    }
    if has(WRITE_ONLY) && !managed {
        return Err(INVALID_CAPS);
    }
    if d.flags & DDSD_PIXELFORMAT == 0 {
        d.flags |= DDSD_PIXELFORMAT;
        d.format = pixel_format(display);
    }
    if d.flags & (DDSD_WIDTH | DDSD_HEIGHT) != DDSD_WIDTH | DDSD_HEIGHT {
        if !has(PRIMARY) {
            return Err(INVALID);
        }
        d.flags |= DDSD_WIDTH | DDSD_HEIGHT;
        (d.width, d.height) = (width, height);
    }
    if d.width == 0 || d.height == 0 {
        return Err(INVALID);
    }
    let mut caps = caps;
    if has(FLIP) {
        caps |= FRONTBUFFER;
    }
    if has(PRIMARY) {
        caps |= VISIBLE;
    }
    if has(MIPMAP) {
        if !has(COMPLEX) {
            d.mipmaps = 1;
        } else if d.flags & DDSD_MIPMAPCOUNT == 0 {
            let side = if version == 7 {
                d.width.max(d.height)
            } else {
                d.width.min(d.height)
            };
            d.mipmaps = side.ilog2() + 1;
        } else if d.mipmaps == 0 {
            return Err(INVALID);
        }
        d.flags |= DDSD_MIPMAPCOUNT;
    }
    if !has(VIDEO_MEMORY | SYSTEM_MEMORY) && !managed {
        caps |= VIDEO_MEMORY;
    }
    if has(OVERLAY) {
        // The virtual adapter has no overlay hardware.
        return Err(NO_OVERLAY_HW);
    }
    if managed {
        caps |= SYSTEM_MEMORY;
    }
    if caps & VIDEO_MEMORY != 0 {
        caps |= LOCAL_VIDEO_MEMORY;
    }
    d.caps[0] = caps;
    if d.flags & DDSD_LPSURFACE != 0 {
        if caps & SYSTEM_MEMORY == 0 {
            return Err(INVALID_CAPS);
        }
        if version < 4 || d.memory == 0 || d.flags & DDSD_PITCH == 0 {
            return Err(INVALID);
        }
    }
    if KEYS
        .iter()
        .zip(d.keys)
        .any(|(&(flag, _), (low, high))| d.flags & flag != 0 && low != high)
    {
        return Err(NO_COLOR_KEY_HW);
    }
    if storage(&d.format).is_none() {
        return Err(INVALID_PIXEL_FORMAT);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(flags: u32, caps: u32) -> Described {
        Described {
            flags: flags | DDSD_CAPS,
            width: 64,
            height: 32,
            caps: [caps, 0, 0, 0],
            ..Default::default()
        }
    }
    fn check(d: &mut Described, exclusive: bool) -> Result<(), u32> {
        validate(d, 0, 7, exclusive, (640, 480, PixelFormat::Bgra32))
    }

    // spec: GFX-DDRAW-15
    #[test]
    fn descriptors_fail_in_windows_order_and_complete_from_the_mode() {
        let wh = DDSD_WIDTH | DDSD_HEIGHT;
        for (flags, caps, exclusive, expected) in [
            (wh, COMPLEX, true, INVALID_CAPS),
            (wh | DDSD_MIPMAPCOUNT, TEXTURE, true, INVALID_CAPS),
            (wh, FLIP | COMPLEX, true, INVALID_CAPS),
            (wh | DDSD_BACKBUFFERCOUNT, 0x40, true, INVALID_CAPS),
            (
                DDSD_BACKBUFFERCOUNT,
                PRIMARY | FLIP | COMPLEX,
                false,
                NO_EXCLUSIVE_MODE,
            ),
            (wh, VIDEO_MEMORY | SYSTEM_MEMORY, true, INVALID_CAPS),
            (wh, MIPMAP, true, INVALID_CAPS),
            (DDSD_WIDTH, 0x40, true, INVALID),
            (wh, OVERLAY, true, NO_OVERLAY_HW),
        ] {
            let mut d = request(flags, caps);
            d.backbuffers = 1;
            assert_eq!(
                check(&mut d, exclusive),
                Err(expected),
                "{flags:#x} {caps:#x}"
            );
        }
        // Outer unknowns fail before anything else.
        let mut d = request(0, COMPLEX);
        assert_eq!(
            validate(&mut d, 1, 7, true, (640, 480, PixelFormat::Bgra32)),
            Err(NO_AGGREGATION)
        );

        // A primary takes the display mode and reports visible video memory.
        let mut d = Described {
            caps: [PRIMARY, 0, 0, 0],
            ..Default::default()
        };
        assert_eq!(check(&mut d, false), Ok(()));
        assert_eq!((d.width, d.height), (640, 480));
        assert_eq!(
            d.flags,
            DDSD_CAPS | DDSD_WIDTH | DDSD_HEIGHT | DDSD_PIXELFORMAT
        );
        assert_eq!(
            d.caps[0],
            PRIMARY | VISIBLE | VIDEO_MEMORY | LOCAL_VIDEO_MEMORY
        );
        assert_eq!(storage(&d.format), Some(PixelFormat::Bgra32));

        // Complex mipmaps count levels down from the longer side in version 7.
        let mut d = request(wh, TEXTURE | MIPMAP | COMPLEX);
        assert_eq!(check(&mut d, false), Ok(()));
        assert_eq!(d.mipmaps, 7);
        let mut d = request(wh, TEXTURE | MIPMAP | COMPLEX);
        assert_eq!(
            validate(&mut d, 0, 4, false, (640, 480, PixelFormat::Bgra32)),
            Ok(())
        );
        assert_eq!(d.mipmaps, 6);
    }

    // spec: GFX-DDRAW-15
    #[test]
    fn version_one_descriptors_name_z_buffers_by_depth() {
        let mut raw = vec![0u8; 108];
        for (at, value) in [
            (0, 108),
            (4, 0x47),
            (8, 4),
            (12, 8),
            (24, 16),
            (104, ZBUFFER),
        ] {
            raw[at..at + 4].copy_from_slice(&u32::to_le_bytes(value));
        }
        let mut d = Described::parse(&raw);
        assert_eq!(
            d.flags & (DDSD_ZBUFFERBITDEPTH | DDSD_PIXELFORMAT),
            DDSD_PIXELFORMAT
        );
        assert_eq!(
            (word(&d.format, 4), word(&d.format, 12), word(&d.format, 20)),
            (DDPF_ZBUFFER, 16, 0xffff)
        );
        assert_eq!(check(&mut d, false), Ok(()));
        assert_eq!(storage(&d.format), Some(PixelFormat::Rgb565));
        let back = d.bytes(108, 16, 0);
        assert_eq!(
            word(&back, 4) & (DDSD_ZBUFFERBITDEPTH | DDSD_PIXELFORMAT),
            DDSD_ZBUFFERBITDEPTH
        );
        assert_eq!(word(&back, 24), 16);
    }
}

//! Validation of display-mode requests, independent of guest memory.
use super::Error;
use super::abi::*;
use super::caps::PhysicalMode;

pub(super) const INTERLACED: u32 = 1;
pub(super) const SCALING: u32 = 2;
const STEREO: u32 = 4;
const DISABLED_STEREO: u32 = 8;

#[derive(Clone, Copy)]
pub(super) struct ModeRequest {
    pub format: i32,
    pub flags: u32,
}

impl ModeRequest {
    pub fn new(format: i32, flags: u32, v1: bool) -> Result<Self, Error> {
        let allowed = INTERLACED | SCALING | if v1 { STEREO | DISABLED_STEREO } else { 0 };
        if flags & !allowed != 0 {
            return Err(Error::InvalidCall);
        }
        Ok(Self { format, flags })
    }

    pub fn includes(self, mode: &PhysicalMode) -> bool {
        matches!(self.format, 28 | 87)
            && (mode.scanline <= 1 || self.flags & INTERLACED != 0)
            && (!mode.stereo || self.flags & (STEREO | DISABLED_STEREO) != 0)
    }
}

pub(super) fn mode_desc(mode: PhysicalMode, format: i32) -> DXGI_MODE_DESC1 {
    DXGI_MODE_DESC1 {
        Width: mode.size.0,
        Height: mode.size.1,
        RefreshRate: DXGI_RATIONAL {
            Numerator: mode.refresh.0,
            Denominator: mode.refresh.1.get(),
        },
        Format: format,
        ScanlineOrdering: mode.scanline,
        Scaling: mode.scaling,
        Stereo: i32::from(mode.stereo),
    }
}

pub(super) fn base_mode(mode: DXGI_MODE_DESC1) -> DXGI_MODE_DESC {
    DXGI_MODE_DESC {
        Width: mode.Width,
        Height: mode.Height,
        RefreshRate: mode.RefreshRate,
        Format: mode.Format,
        ScanlineOrdering: mode.ScanlineOrdering,
        Scaling: mode.Scaling,
    }
}

pub(super) fn closest_request(mode: DXGI_MODE_DESC1) -> Result<DXGI_MODE_DESC1, Error> {
    if (mode.Width == 0) != (mode.Height == 0)
        || (mode.RefreshRate.Numerator == 0) != (mode.RefreshRate.Denominator == 0)
        || !(0..=3).contains(&mode.ScanlineOrdering)
        || !(0..=2).contains(&mode.Scaling)
        || mode.Format == 0
    {
        return Err(Error::InvalidCall);
    }
    Ok(mode)
}

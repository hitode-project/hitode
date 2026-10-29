//! Offsets and module RVAs from the pinned Wine 11 win32u.so DWARF.
#![allow(non_upper_case_globals, dead_code)]
pub mod attr {
    pub const SIZE: u32 = 0xc0;
    pub const hdc: u32 = 0x0;
    pub const disabled: u32 = 0x4;
    pub const save_level: u32 = 0x8;
    pub const background_color: u32 = 0xc;
    pub const brush_color: u32 = 0x10;
    pub const pen_color: u32 = 0x14;
    pub const text_color: u32 = 0x18;
    pub const cur_pos: u32 = 0x1c;
    pub const graphics_mode: u32 = 0x24;
    pub const arc_direction: u32 = 0x28;
    pub const layout: u32 = 0x2c;
    pub const text_align: u32 = 0x30;
    pub const background_mode: u32 = 0x32;
    pub const poly_fill_mode: u32 = 0x34;
    pub const rop_mode: u32 = 0x36;
    pub const rel_abs_mode: u32 = 0x38;
    pub const stretch_blt_mode: u32 = 0x3a;
    pub const map_mode: u32 = 0x3c;
    pub const char_extra: u32 = 0x40;
    pub const mapper_flags: u32 = 0x44;
    pub const vis_rect: u32 = 0x48;
    pub const miter_limit: u32 = 0x58;
    pub const brush_org: u32 = 0x5c;
    pub const wnd_org: u32 = 0x64;
    pub const wnd_ext: u32 = 0x6c;
    pub const vport_org: u32 = 0x74;
    pub const vport_ext: u32 = 0x7c;
    pub const virtual_res: u32 = 0x84;
    pub const virtual_size: u32 = 0x8c;
    pub const font_code_page: u32 = 0x94;
    pub const emf_bounds: u32 = 0x98;
    pub const emf: u32 = 0xa8;
    pub const abort_proc: u32 = 0xb0;
    pub const print: u32 = 0xb8;
}
pub mod physdev {
    pub const SIZE: u32 = 0xc;
    pub const funcs: u32 = 0x0;
    pub const next: u32 = 0x4;
    pub const hdc: u32 = 0x8;
}
pub mod funcs {
    pub const SIZE: u32 = 0x164;
    pub const pAbortDoc: u32 = 0x0;
    pub const pAbortPath: u32 = 0x4;
    pub const pAlphaBlend: u32 = 0x8;
    pub const pAngleArc: u32 = 0xc;
    pub const pArc: u32 = 0x10;
    pub const pArcTo: u32 = 0x14;
    pub const pBeginPath: u32 = 0x18;
    pub const pBlendImage: u32 = 0x1c;
    pub const pChord: u32 = 0x20;
    pub const pCloseFigure: u32 = 0x24;
    pub const pCreateCompatibleDC: u32 = 0x28;
    pub const pCreateDC: u32 = 0x2c;
    pub const pDeleteDC: u32 = 0x30;
    pub const pDeleteObject: u32 = 0x34;
    pub const pEllipse: u32 = 0x38;
    pub const pEndDoc: u32 = 0x3c;
    pub const pEndPage: u32 = 0x40;
    pub const pEndPath: u32 = 0x44;
    pub const pEnumFonts: u32 = 0x48;
    pub const pExtEscape: u32 = 0x4c;
    pub const pExtFloodFill: u32 = 0x50;
    pub const pExtTextOut: u32 = 0x54;
    pub const pFillPath: u32 = 0x58;
    pub const pFillRgn: u32 = 0x5c;
    pub const pFontIsLinked: u32 = 0x60;
    pub const pFrameRgn: u32 = 0x64;
    pub const pGetBoundsRect: u32 = 0x68;
    pub const pGetCharABCWidths: u32 = 0x6c;
    pub const pGetCharABCWidthsI: u32 = 0x70;
    pub const pGetCharWidth: u32 = 0x74;
    pub const pGetCharWidthInfo: u32 = 0x78;
    pub const pGetDeviceCaps: u32 = 0x7c;
    pub const pGetDeviceGammaRamp: u32 = 0x80;
    pub const pGetFontData: u32 = 0x84;
    pub const pGetFontRealizationInfo: u32 = 0x88;
    pub const pGetFontUnicodeRanges: u32 = 0x8c;
    pub const pGetGlyphIndices: u32 = 0x90;
    pub const pGetGlyphOutline: u32 = 0x94;
    pub const pGetImage: u32 = 0x98;
    pub const pGetKerningPairs: u32 = 0x9c;
    pub const pGetNearestColor: u32 = 0xa0;
    pub const pGetOutlineTextMetrics: u32 = 0xa4;
    pub const pGetPixel: u32 = 0xa8;
    pub const pGetSystemPaletteEntries: u32 = 0xac;
    pub const pGetTextCharsetInfo: u32 = 0xb0;
    pub const pGetTextExtentExPoint: u32 = 0xb4;
    pub const pGetTextExtentExPointI: u32 = 0xb8;
    pub const pGetTextFace: u32 = 0xbc;
    pub const pGetTextMetrics: u32 = 0xc0;
    pub const pGradientFill: u32 = 0xc4;
    pub const pInvertRgn: u32 = 0xc8;
    pub const pLineTo: u32 = 0xcc;
    pub const pMoveTo: u32 = 0xd0;
    pub const pPaintRgn: u32 = 0xd4;
    pub const pPatBlt: u32 = 0xd8;
    pub const pPie: u32 = 0xdc;
    pub const pPolyBezier: u32 = 0xe0;
    pub const pPolyBezierTo: u32 = 0xe4;
    pub const pPolyDraw: u32 = 0xe8;
    pub const pPolyPolygon: u32 = 0xec;
    pub const pPolyPolyline: u32 = 0xf0;
    pub const pPolylineTo: u32 = 0xf4;
    pub const pPutImage: u32 = 0xf8;
    pub const pRealizeDefaultPalette: u32 = 0xfc;
    pub const pRealizePalette: u32 = 0x100;
    pub const pRectangle: u32 = 0x104;
    pub const pResetDC: u32 = 0x108;
    pub const pRoundRect: u32 = 0x10c;
    pub const pSelectBitmap: u32 = 0x110;
    pub const pSelectBrush: u32 = 0x114;
    pub const pSelectFont: u32 = 0x118;
    pub const pSelectPen: u32 = 0x11c;
    pub const pSetBkColor: u32 = 0x120;
    pub const pSetBoundsRect: u32 = 0x124;
    pub const pSetDCBrushColor: u32 = 0x128;
    pub const pSetDCPenColor: u32 = 0x12c;
    pub const pSetDIBitsToDevice: u32 = 0x130;
    pub const pSetDeviceClipping: u32 = 0x134;
    pub const pSetDeviceGammaRamp: u32 = 0x138;
    pub const pSetPixel: u32 = 0x13c;
    pub const pSetTextColor: u32 = 0x140;
    pub const pStartDoc: u32 = 0x144;
    pub const pStartPage: u32 = 0x148;
    pub const pStretchBlt: u32 = 0x14c;
    pub const pStretchDIBits: u32 = 0x150;
    pub const pStrokeAndFillPath: u32 = 0x154;
    pub const pStrokePath: u32 = 0x158;
    pub const pUnrealizePalette: u32 = 0x15c;
    pub const priority: u32 = 0x160;
}
pub mod dc {
    pub const SIZE: u32 = 0x128;
    pub const obj: u32 = 0x0;
    pub const hSelf: u32 = 0x8;
    pub const nulldrv: u32 = 0xc;
    pub const physDev: u32 = 0x18;
    pub const thread: u32 = 0x1c;
    pub const refcount: u32 = 0x20;
    pub const dirty: u32 = 0x24;
    pub const attr: u32 = 0x28;
    pub const saved_dc: u32 = 0x2c;
    pub const dce: u32 = 0x30;
    pub const bounds_enabled: u32 = 0x34;
    pub const path_open: u32 = 0x34;
    pub const is_display: u32 = 0x34;
    pub const opengl_drawable: u32 = 0x38;
    pub const device_rect: u32 = 0x3c;
    pub const pixel_format: u32 = 0x4c;
    pub const aa_flags: u32 = 0x50;
    pub const display: u32 = 0x54;
    pub const flags: u32 = 0x94;
    pub const hClipRgn: u32 = 0x98;
    pub const hMetaRgn: u32 = 0x9c;
    pub const hVisRgn: u32 = 0xa0;
    pub const region: u32 = 0xa4;
    pub const hPen: u32 = 0xa8;
    pub const hBrush: u32 = 0xac;
    pub const hFont: u32 = 0xb0;
    pub const hBitmap: u32 = 0xb4;
    pub const hPalette: u32 = 0xb8;
    pub const path: u32 = 0xbc;
    pub const font_gamma_ramp: u32 = 0xc0;
    pub const breakExtra: u32 = 0xc4;
    pub const breakRem: u32 = 0xc8;
    pub const xformWorld2Wnd: u32 = 0xcc;
    pub const xformWorld2Vport: u32 = 0xe4;
    pub const xformVport2World: u32 = 0xfc;
    pub const vport2WorldValid: u32 = 0x114;
    pub const bounds: u32 = 0x118;
}
pub mod region {
    pub const SIZE: u32 = 0x64;
    pub const obj: u32 = 0x0;
    pub const size: u32 = 0x8;
    pub const numRects: u32 = 0xc;
    pub const rects: u32 = 0x10;
    pub const extents: u32 = 0x14;
    pub const rects_buf: u32 = 0x24;
}
pub mod dib {
    pub const SIZE: u32 = 0x64;
    pub const bit_count: u32 = 0x0;
    pub const width: u32 = 0x4;
    pub const height: u32 = 0x8;
    pub const compression: u32 = 0xc;
    pub const rect: u32 = 0x10;
    pub const stride: u32 = 0x20;
    pub const bits: u32 = 0x24;
    pub const red_mask: u32 = 0x34;
    pub const green_mask: u32 = 0x38;
    pub const blue_mask: u32 = 0x3c;
    pub const red_shift: u32 = 0x40;
    pub const green_shift: u32 = 0x44;
    pub const blue_shift: u32 = 0x48;
    pub const red_len: u32 = 0x4c;
    pub const green_len: u32 = 0x50;
    pub const blue_len: u32 = 0x54;
    pub const color_table: u32 = 0x58;
    pub const color_table_size: u32 = 0x5c;
    pub const funcs: u32 = 0x60;
}
pub mod dibdev {
    pub const SIZE: u32 = 0x22c;
    pub const dev: u32 = 0x0;
    pub const dib: u32 = 0xc;
    pub const brush: u32 = 0x70;
    pub const clip: u32 = 0x108;
    pub const bounds: u32 = 0x10c;
    pub const font: u32 = 0x110;
    pub const pen_style: u32 = 0x114;
    pub const pen_endcap: u32 = 0x118;
    pub const pen_join: u32 = 0x11c;
    pub const pen_uses_region: u32 = 0x120;
    pub const pen_is_ext: u32 = 0x124;
    pub const pen_width: u32 = 0x128;
    pub const pen_brush: u32 = 0x12c;
    pub const pen_pattern: u32 = 0x1c4;
    pub const dash_pos: u32 = 0x20c;
    pub const dash_masks: u32 = 0x218;
    pub const pen_lines: u32 = 0x228;
}
pub mod font {
    pub const SIZE: u32 = 0x64;
    pub const obj: u32 = 0x0;
    pub const logfont: u32 = 0x8;
}
pub mod handle {
    pub const SIZE: u32 = 0x18;
    pub const Object: u32 = 0x0;
    pub const Owner: u32 = 0x8;
    pub const Type: u32 = 0xe;
    pub const Flags: u32 = 0xf;
    pub const UserPointer: u32 = 0x10;
}
pub const GDI_SHARED: u32 = 0x1f194c;
pub const FONT_FUNCS: u32 = 0x1f1884;
pub const FONT_DRIVER: u32 = 0x1d0620;
pub const DIB_DRIVER: u32 = 0x1d4be0;
pub const HKCU_KEY: u32 = 0x1e678c;
pub const ANSI_CP: u32 = 0x1e6760;
pub const OEM_CP: u32 = 0x1f1600;
pub const UTF8_CP: u32 = 0x1f1640;
/// `CPTABLEINFO`, as `RtlInitCodePageTable` fills it.
pub mod cptable {
    pub const SIZE: usize = 0x2c;
    pub const LeadByte: usize = 0xe;
    pub const MultiByteTable: usize = 0x1c;
    pub const WideCharTable: usize = 0x20;
    pub const DBCSRanges: usize = 0x24;
    pub const DBCSOffsets: usize = 0x28;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};
    #[repr(C)]
    struct PhysDev {
        funcs: u32,
        next: u32,
        hdc: u32,
    }
    #[repr(C)]
    struct Dc {
        header: [u32; 2],
        hdc: u32,
        null: PhysDev,
        phys: u32,
        thread: u32,
        refs: i32,
        dirty: i32,
        attr: u32,
        saved: u32,
        dce: u32,
        flags: u32,
        gl: u32,
        device: [i32; 4],
        format: i32,
        aa: u32,
        display: [u16; 32],
        dcflags: i32,
        clip: u32,
        meta: u32,
        vis: u32,
        region: u32,
        pen: u32,
        brush: u32,
        font: u32,
        bitmap: u32,
        palette: u32,
        path: u32,
        gamma: u32,
        extra: i32,
        remainder: i32,
        world: [f32; 6],
        transform: [f32; 6],
        inverse: [f32; 6],
        valid: i32,
        bounds: [i32; 4],
    }
    #[repr(C)]
    struct Attr {
        hdc: u32,
        disabled: i32,
        save: i32,
        background: u32,
        brush: u32,
        pen: u32,
        text: u32,
        position: [i32; 2],
        graphics: i32,
        arc: i32,
        layout: u32,
        align: u16,
        background_mode: u16,
        poly: u16,
        rop: u16,
        relative: u16,
        stretch: u16,
        map: u16,
        pad: u16,
        extra: i32,
        mapper: u32,
        visible: [i32; 4],
        miter: f32,
        brush_origin: [i32; 2],
        window_origin: [i32; 2],
        window_extent: [i32; 2],
        viewport_origin: [i32; 2],
        viewport_extent: [i32; 2],
        resolution: [i32; 2],
        physical: [i32; 2],
        codepage: u32,
        emf_bounds: [i32; 4],
        private: [u64; 3],
    }
    #[repr(C)]
    struct HandleEntry {
        object: u64,
        owner: u32,
        unique: u16,
        kind: u8,
        flags: u8,
        user: u64,
    }
    #[repr(C)]
    struct FontObject {
        header: [u32; 2],
        logical: [u8; 92],
    }
    #[repr(C)]
    struct CreateFontObject {
        data: u32,
        size: u64,
        index: u32,
        object: u32,
    }
    // spec: WINE-UNIX-6, NAT-BODY-4
    #[test]
    fn font_dc_dwarf_offsets_match_wine_i386_records() {
        assert_eq!(size_of::<PhysDev>(), physdev::SIZE as usize);
        assert_eq!(size_of::<Dc>(), dc::SIZE as usize);
        assert_eq!(size_of::<Attr>(), attr::SIZE as usize);
        assert_eq!(size_of::<HandleEntry>(), handle::SIZE as usize);
        assert_eq!(size_of::<FontObject>(), font::SIZE as usize);
        assert_eq!(offset_of!(Dc, phys), dc::physDev as usize);
        assert_eq!(offset_of!(Dc, attr), dc::attr as usize);
        assert_eq!(offset_of!(Dc, font), dc::hFont as usize);
        assert_eq!(offset_of!(Dc, extra), dc::breakExtra as usize);
        assert_eq!(offset_of!(Dc, transform), dc::xformWorld2Vport as usize);
        assert_eq!(offset_of!(Attr, align), attr::text_align as usize);
        assert_eq!(offset_of!(Attr, extra), attr::char_extra as usize);
        assert_eq!(offset_of!(Attr, codepage), attr::font_code_page as usize);
        assert_eq!(offset_of!(HandleEntry, kind), handle::Type as usize);
        assert_eq!(offset_of!(FontObject, logical), font::logfont as usize);
    }
    // spec: WINE-UNIX-6
    #[test]
    fn dwrite_dwarf_offsets_preserve_aligned_font_data_size() {
        assert_eq!(size_of::<CreateFontObject>(), 24);
        assert_eq!(offset_of!(CreateFontObject, size), 8);
        assert_eq!(offset_of!(CreateFontObject, index), 16);
        assert_eq!(offset_of!(CreateFontObject, object), 20);
    }
}

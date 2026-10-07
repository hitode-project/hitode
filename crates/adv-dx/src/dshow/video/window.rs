//! The graph's video-window distributor. Geometry is in video-window client
//! coordinates; presentation owns the conversion to the owner's canvas.
use adv_cpu::backend::{GuestExt, Prot};
use adv_device::{media::VideoInfo, surface::VideoFrame};
use adv_machine::{Env, Outcome};

use crate::dshow::{
    E_INVALIDARG, E_NOINTERFACE, E_NOTIMPL, E_OUTOFMEMORY, E_POINTER, Face, S_FALSE, S_OK, State,
    VFW_E_NOT_CONNECTED, graph, state,
};

const E_FAIL: u32 = 0x8000_4005;
const VFW_E_NOT_PAUSED: u32 = 0x8004_0225;
const VFW_E_NO_PALETTE_AVAILABLE: u32 = 0x8004_0220;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_FORBIDDEN: u32 = 0x0800_0000 | 0x2000_0000 | 0x0100_0000 | 0x0020_0000 | 0x0010_0000;
// Parent/clip/visibility and conventional nonclient style metadata. Scroll,
// disabled and iconic/maximized windows are excluded by IVideoWindow's contract.
const WS_SUPPORTED: u32 = 0x4000_0000 | WS_VISIBLE | 0x0400_0000 | 0x0200_0000 | 0x00cf_0000;
const SW_HIDE: u32 = 0;
const SW_SHOW: u32 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rect {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub(crate) fn image(info: &VideoInfo) -> Self {
        Self {
            left: 0,
            top: 0,
            width: info.width as i32,
            height: info.height as i32,
        }
    }

    fn valid(self) -> bool {
        self.width > 0
            && self.height > 0
            && self.left.checked_add(self.width).is_some()
            && self.top.checked_add(self.height).is_some()
    }

    fn in_image(self, width: u32, height: u32) -> bool {
        self.valid()
            && self.left >= 0
            && self.top >= 0
            && i64::from(self.left) + i64::from(self.width) <= i64::from(width)
            && i64::from(self.top) + i64::from(self.height) <= i64::from(height)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WindowState {
    pub owner: u32,
    pub style: u32,
    pub style_ex: u32,
    /// None follows the media's native size until an application sets geometry.
    pub position: Option<Rect>,
    /// None uses the entire source image.
    pub source: Option<Rect>,
    /// None follows the video window's entire client area on every resize.
    pub destination: Option<Rect>,
    pub visible: bool,
    pub auto_show: bool,
    pub hide_cursor: bool,
    pub border_color: u32,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            owner: 0,
            style: 0x00cf_0000,
            style_ex: 0,
            position: None,
            source: None,
            destination: None,
            visible: false,
            auto_show: true,
            hide_cursor: false,
            border_color: 0,
        }
    }
}

impl WindowState {
    pub(crate) fn effective_position(&self, info: &VideoInfo) -> Rect {
        self.position.unwrap_or_else(|| Rect::image(info))
    }

    pub(crate) fn effective_source(&self, info: &VideoInfo) -> Rect {
        self.source.unwrap_or_else(|| Rect::image(info))
    }

    pub(crate) fn effective_destination(&self, info: &VideoInfo) -> Rect {
        self.destination.unwrap_or_else(|| {
            let position = self.effective_position(info);
            Rect {
                left: 0,
                top: 0,
                ..position
            }
        })
    }

    /// Keep the visible style bit consistent with explicit and automatic shows.
    pub(crate) fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
        if visible {
            self.style |= WS_VISIBLE;
        } else {
            self.style &= !WS_VISIBLE;
        }
    }
}

fn window<'a>(env: &'a Env<'_>, graph: u32) -> &'a WindowState {
    &env.ext.get::<State>().graphs[&graph].video
}

fn writable(env: &Env<'_>, pointer: u32, length: u32) -> bool {
    pointer != 0 && env.cpu.accessible(pointer, length, Prot::WRITE)
}

/// Validate all outputs before writing the first one, including cross-page
/// scalar outputs. Host writes alone intentionally bypass guest page protection.
fn write_words(env: &mut Env<'_>, outputs: &[u32], values: &[u32]) -> u32 {
    if outputs.iter().any(|&pointer| !writable(env, pointer, 4)) {
        return E_POINTER;
    }
    for (&pointer, &value) in outputs.iter().zip(values) {
        if env.cpu.write_u32(pointer, value).is_err() {
            return E_POINTER;
        }
    }
    S_OK
}

fn write_rect(env: &mut Env<'_>, rect: Rect) -> u32 {
    let outputs = [env.arg(1), env.arg(2), env.arg(3), env.arg(4)];
    write_words(
        env,
        &outputs,
        &[
            rect.left as u32,
            rect.top as u32,
            rect.width as u32,
            rect.height as u32,
        ],
    )
}

fn write_size(env: &mut Env<'_>, width: u32, height: u32) -> u32 {
    let outputs = [env.arg(1), env.arg(2)];
    write_words(env, &outputs, &[width, height])
}

fn write_word(env: &mut Env<'_>, value: u32) -> u32 {
    let output = env.arg(1);
    write_words(env, &[output], &[value])
}

fn get_word(env: &mut Env<'_>, graph: u32, get: impl FnOnce(&WindowState) -> u32) -> u32 {
    let value = get(window(env, graph));
    write_word(env, value)
}

fn get_rect(env: &mut Env<'_>, graph: u32, get: impl FnOnce(&WindowState) -> Rect) -> u32 {
    let rect = get(window(env, graph));
    write_rect(env, rect)
}

fn oa_bool(value: u32) -> Option<bool> {
    match value {
        0 => Some(false),
        u32::MAX => Some(true),
        _ => None,
    }
}

fn bool_word(value: bool) -> u32 {
    if value { u32::MAX } else { 0 }
}

fn unsupported(env: &mut Env<'_>, face: Face, method: &str, reason: &str) -> u32 {
    adv_abi::warn_unavailable(env, &format!("{}::{method}", face.interface()), reason);
    E_NOTIMPL
}

/// Presentation errors must not publish a partially applied window property.
fn update(env: &mut Env<'_>, graph: u32, change: impl FnOnce(&mut WindowState)) -> u32 {
    let previous = window(env, graph).clone();
    change(&mut state(env).graphs.get_mut(&graph).unwrap().video);
    let result = super::refresh(env, graph);
    if result & 0x8000_0000 != 0 {
        state(env).graphs.get_mut(&graph).unwrap().video = previous;
        let _ = super::refresh(env, graph);
    }
    result
}

fn rectangle_args(env: &Env<'_>) -> Rect {
    Rect {
        left: env.arg(1) as i32,
        top: env.arg(2) as i32,
        width: env.arg(3) as i32,
        height: env.arg(4) as i32,
    }
}

fn property_rect(mut rect: Rect, method: &str, value: i32) -> Rect {
    if method.ends_with("Left") {
        rect.left = value;
    } else if method.ends_with("Top") {
        rect.top = value;
    } else if method.ends_with("Width") {
        rect.width = value;
    } else {
        rect.height = value;
    }
    rect
}

fn rect_property(rect: Rect, method: &str) -> u32 {
    if method.ends_with("Left") {
        rect.left as u32
    } else if method.ends_with("Top") {
        rect.top as u32
    } else if method.ends_with("Width") {
        rect.width as u32
    } else {
        rect.height as u32
    }
}

/// Absent renderers retain the graph distributor's E_NOINTERFACE behavior.
pub(crate) fn call(env: &mut Env<'_>, graph: u32, face: Face, method: &str) -> Option<Outcome> {
    if !super::available(env, graph) {
        return Some(Outcome::Return(E_NOINTERFACE));
    }
    if !super::connected(env, graph) {
        return Some(Outcome::Return(VFW_E_NOT_CONNECTED));
    }
    let Some(info) = super::info(env, graph) else {
        return Some(Outcome::Return(VFW_E_NOT_CONNECTED));
    };
    if info.width == 0
        || info.height == 0
        || info.width > i32::MAX as u32
        || info.height > i32::MAX as u32
    {
        return Some(Outcome::Return(E_INVALIDARG));
    }
    let result = match face {
        Face::VideoWindow => window_call(env, graph, method, &info),
        Face::BasicVideo => basic_call(env, graph, method, &info),
        _ => return None,
    };
    Some(Outcome::Return(result))
}

fn window_call(env: &mut Env<'_>, graph: u32, method: &str, info: &VideoInfo) -> u32 {
    let value = env.arg(1);
    match method {
        "put_Owner" => {
            if value != 0 && super::owner_geometry(env, value).is_none() {
                return E_INVALIDARG;
            }
            update(env, graph, |w| w.owner = value)
        }
        "get_Owner" => get_word(env, graph, |w| w.owner),
        "put_WindowStyle" => {
            if value & WS_FORBIDDEN != 0 {
                return E_INVALIDARG;
            }
            if value & !WS_SUPPORTED != 0 {
                return unsupported(
                    env,
                    Face::VideoWindow,
                    method,
                    "video window style flags are not supported",
                );
            }
            if value & WS_VISIBLE != 0 && window(env, graph).owner == 0 {
                return unsupported(
                    env,
                    Face::VideoWindow,
                    method,
                    "standalone video windows are not supported; attach a guest owner window",
                );
            }
            update(env, graph, |w| {
                w.style = value;
                w.visible = value & WS_VISIBLE != 0;
            })
        }
        "get_WindowStyle" => get_word(env, graph, |w| w.style),
        "put_WindowStyleEx" => {
            // Extended styles change nonclient rendering, input, z-order or
            // mirroring. None is represented by the embedded video layer.
            if value != 0 {
                return unsupported(
                    env,
                    Face::VideoWindow,
                    method,
                    "extended video window styles are not supported",
                );
            }
            update(env, graph, |w| w.style_ex = value)
        }
        "get_WindowStyleEx" => get_word(env, graph, |w| w.style_ex),
        "put_AutoShow" | "put_Visible" | "HideCursor" => {
            let Some(value) = oa_bool(value) else {
                return E_INVALIDARG;
            };
            if method == "put_Visible" && value && window(env, graph).owner == 0 {
                return unsupported(
                    env,
                    Face::VideoWindow,
                    method,
                    "standalone video windows are not supported; attach a guest owner window",
                );
            }
            update(env, graph, |w| match method {
                "put_AutoShow" => w.auto_show = value,
                "put_Visible" => w.set_visible(value),
                _ => w.hide_cursor = value,
            })
        }
        "get_AutoShow" => get_word(env, graph, |w| bool_word(w.auto_show)),
        "get_Visible" => get_word(env, graph, |w| bool_word(w.visible)),
        "IsCursorHidden" => get_word(env, graph, |w| bool_word(w.hide_cursor)),
        "put_WindowState" => match value {
            SW_HIDE => update(env, graph, |w| w.set_visible(false)),
            // SW_SHOWNORMAL, SW_SHOWNOACTIVATE, SW_SHOW, SW_SHOWNA, SW_RESTORE.
            1 | 4 | 5 | 8 | 9 => {
                if window(env, graph).owner == 0 {
                    return unsupported(
                        env,
                        Face::VideoWindow,
                        method,
                        "standalone video windows are not supported; attach a guest owner window",
                    );
                }
                update(env, graph, |w| w.set_visible(true))
            }
            2 | 3 | 6 | 7 | 10 | 11 => unsupported(
                env,
                Face::VideoWindow,
                method,
                "video window activation, minimization and maximization are not supported",
            ),
            _ => E_INVALIDARG,
        },
        "get_WindowState" => get_word(env, graph, |w| if w.visible { SW_SHOW } else { SW_HIDE }),
        "SetWindowPosition" => {
            let rect = rectangle_args(env);
            if !rect.valid() {
                return E_INVALIDARG;
            }
            update(env, graph, |w| w.position = Some(rect))
        }
        "put_Left" | "put_Top" | "put_Width" | "put_Height" => {
            let rect = property_rect(
                window(env, graph).effective_position(info),
                method,
                value as i32,
            );
            if !rect.valid() {
                return E_INVALIDARG;
            }
            update(env, graph, |w| w.position = Some(rect))
        }
        "get_Left" | "get_Top" | "get_Width" | "get_Height" => get_word(env, graph, |w| {
            rect_property(w.effective_position(info), method)
        }),
        "GetWindowPosition" | "GetRestorePosition" => {
            get_rect(env, graph, |w| w.effective_position(info))
        }
        "GetMinIdealImageSize" | "GetMaxIdealImageSize" => write_size(env, info.width, info.height),
        "get_BorderColor" => get_word(env, graph, |w| w.border_color),
        "put_BorderColor" => {
            if value > 0x00ff_ffff {
                return E_INVALIDARG;
            }
            update(env, graph, |w| w.border_color = value)
        }
        "get_FullScreenMode" | "get_BackgroundPalette" | "get_MessageDrain" => write_word(env, 0),
        "put_FullScreenMode" | "put_BackgroundPalette" => match oa_bool(value) {
            Some(false) => S_OK,
            Some(true) => unsupported(
                env,
                Face::VideoWindow,
                method,
                "full-screen video and palette realization are not supported",
            ),
            None => E_INVALIDARG,
        },
        "put_MessageDrain" => {
            if value == 0 {
                S_OK
            } else {
                unsupported(
                    env,
                    Face::VideoWindow,
                    method,
                    "video-window input message forwarding is not supported",
                )
            }
        }
        "get_Caption" => {
            if !writable(env, value, 4) {
                return E_POINTER;
            }
            unsupported(
                env,
                Face::VideoWindow,
                method,
                "standalone video window captions are not supported",
            )
        }
        _ => unsupported(
            env,
            Face::VideoWindow,
            method,
            "standalone video windows, activation and owner messages are not supported",
        ),
    }
}

fn basic_call(env: &mut Env<'_>, graph: u32, method: &str, info: &VideoInfo) -> u32 {
    match method {
        "get_VideoWidth" => write_word(env, info.width),
        "get_VideoHeight" => write_word(env, info.height),
        "GetVideoSize" => write_size(env, info.width, info.height),
        "get_AvgTimePerFrame" => {
            let output = env.arg(1);
            if !writable(env, output, 8) {
                return E_POINTER;
            }
            let (numerator, denominator) = info.frame_rate;
            if numerator <= 0 || denominator <= 0 {
                return E_NOTIMPL;
            }
            let seconds = f64::from(denominator) / f64::from(numerator);
            if env.cpu.write(output, &seconds.to_le_bytes()).is_err() {
                E_POINTER
            } else {
                S_OK
            }
        }
        "get_BitRate" | "get_BitErrorRate" => {
            if !writable(env, env.arg(1), 4) {
                return E_POINTER;
            }
            unsupported(
                env,
                Face::BasicVideo,
                method,
                "video stream bitrate statistics are not available",
            )
        }
        "SetSourcePosition" => {
            let rect = rectangle_args(env);
            if !rect.in_image(info.width, info.height) {
                return E_INVALIDARG;
            }
            update(env, graph, |w| w.source = Some(rect))
        }
        "put_SourceLeft" | "put_SourceTop" | "put_SourceWidth" | "put_SourceHeight" => {
            let rect = property_rect(
                window(env, graph).effective_source(info),
                method,
                env.arg(1) as i32,
            );
            if !rect.in_image(info.width, info.height) {
                return E_INVALIDARG;
            }
            update(env, graph, |w| w.source = Some(rect))
        }
        "get_SourceLeft" | "get_SourceTop" | "get_SourceWidth" | "get_SourceHeight" => {
            get_word(env, graph, |w| {
                rect_property(w.effective_source(info), method)
            })
        }
        "GetSourcePosition" => get_rect(env, graph, |w| w.effective_source(info)),
        "SetDefaultSourcePosition" => update(env, graph, |w| w.source = None),
        "IsUsingDefaultSource" => {
            if window(env, graph).source.is_none() {
                S_OK
            } else {
                S_FALSE
            }
        }
        "SetDestinationPosition" => {
            let rect = rectangle_args(env);
            if !rect.valid() {
                return E_INVALIDARG;
            }
            update(env, graph, |w| w.destination = Some(rect))
        }
        "put_DestinationLeft"
        | "put_DestinationTop"
        | "put_DestinationWidth"
        | "put_DestinationHeight" => {
            let rect = property_rect(
                window(env, graph).effective_destination(info),
                method,
                env.arg(1) as i32,
            );
            if !rect.valid() {
                return E_INVALIDARG;
            }
            update(env, graph, |w| w.destination = Some(rect))
        }
        "get_DestinationLeft"
        | "get_DestinationTop"
        | "get_DestinationWidth"
        | "get_DestinationHeight" => get_word(env, graph, |w| {
            rect_property(w.effective_destination(info), method)
        }),
        "GetDestinationPosition" => get_rect(env, graph, |w| w.effective_destination(info)),
        "SetDefaultDestinationPosition" => update(env, graph, |w| w.destination = None),
        "IsUsingDefaultDestination" => {
            if window(env, graph).destination.is_none() {
                S_OK
            } else {
                S_FALSE
            }
        }
        "GetCurrentImage" => current_image(env, graph, info),
        "GetVideoPaletteEntries" => {
            let (start, entries, retrieved, palette) =
                (env.arg(1) as i32, env.arg(2) as i32, env.arg(3), env.arg(4));
            if start < 0 || entries < 0 {
                return E_INVALIDARG;
            }
            let Some(length) = (entries as u32).checked_mul(4) else {
                return E_INVALIDARG;
            };
            if !writable(env, retrieved, 4) || (entries > 0 && !writable(env, palette, length)) {
                return E_POINTER;
            }
            // The renderer accepts RGB formats only, which have no palette.
            VFW_E_NO_PALETTE_AVAILABLE
        }
        _ => unsupported(
            env,
            Face::BasicVideo,
            method,
            "video Automation type information and dispatch are not supported",
        ),
    }
}

fn current_image(env: &mut Env<'_>, graph: u32, info: &VideoInfo) -> u32 {
    let (size, output) = (env.arg(1), env.arg(2));
    if !writable(env, size, 4) || (output != 0 && !env.cpu.accessible(size, 4, Prot::READ)) {
        return E_POINTER;
    }
    if env.ext.get::<State>().graphs[&graph].state != graph::PAUSED {
        return VFW_E_NOT_PAUSED;
    }
    let Some(frame) = super::latest(env, graph) else {
        return E_FAIL;
    };
    let source = window(env, graph).effective_source(info);
    let bytes = match dib(&frame, source) {
        Ok(bytes) => bytes,
        Err(error) => return error,
    };
    let needed = bytes.len() as u32;
    if output == 0 {
        return if env.cpu.write_u32(size, needed).is_err() {
            E_POINTER
        } else {
            S_OK
        };
    }
    let capacity = match env.cpu.read_u32(size) {
        Ok(capacity) if capacity <= i32::MAX as u32 => capacity,
        Ok(_) => return E_INVALIDARG,
        Err(_) => return E_POINTER,
    };
    if capacity < needed {
        return E_OUTOFMEMORY;
    }
    if !writable(env, output, needed) {
        return E_POINTER;
    }
    if env.cpu.write(output, &bytes).is_err() {
        E_POINTER
    } else {
        S_OK
    }
}

/// BI_RGB 32-bit DIB, bottom-up, including the BITMAPINFOHEADER. Captures use
/// source cropping but retain native pixels rather than destination scaling.
fn dib(frame: &VideoFrame, source: Rect) -> Result<Vec<u8>, u32> {
    if !source.in_image(frame.width, frame.height)
        || frame.pitch < frame.width.checked_mul(4).ok_or(E_FAIL)?
        || u64::from(frame.pitch) * u64::from(frame.height) > frame.bgra.len() as u64
    {
        return Err(E_FAIL);
    }
    let row = (source.width as u32).checked_mul(4).ok_or(E_OUTOFMEMORY)?;
    let pixels = row.checked_mul(source.height as u32).ok_or(E_OUTOFMEMORY)?;
    let length = 40u32
        .checked_add(pixels)
        .filter(|&n| n <= i32::MAX as u32)
        .ok_or(E_OUTOFMEMORY)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length as usize)
        .map_err(|_| E_OUTOFMEMORY)?;
    bytes.resize(length as usize, 0);
    bytes[0..4].copy_from_slice(&40u32.to_le_bytes());
    bytes[4..8].copy_from_slice(&source.width.to_le_bytes());
    bytes[8..12].copy_from_slice(&source.height.to_le_bytes());
    bytes[12..14].copy_from_slice(&1u16.to_le_bytes());
    bytes[14..16].copy_from_slice(&32u16.to_le_bytes());
    bytes[20..24].copy_from_slice(&pixels.to_le_bytes());
    for y in 0..source.height as usize {
        let source_y = source.top as usize + source.height as usize - 1 - y;
        let start = source_y * frame.pitch as usize + source.left as usize * 4;
        let target = 40 + y * row as usize;
        bytes[target..target + row as usize]
            .copy_from_slice(&frame.bgra[start..start + row as usize]);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;

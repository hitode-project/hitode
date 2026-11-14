//! Video Mixing Renderer (VMR-7) config/windowless interfaces and push-mode
//! `IMemInputPin` reception on host `Role::VideoRenderer` filters.

use adv_cpu::backend::Prot;
use adv_device::media::{RawVideoOutput, VideoInfo};
use adv_machine::{Conv, Env, Handle, Object, Outcome, Wait};

use super::filter::INPUT;
use super::video::window::Rect;
use super::*;

pub(crate) const CLSID_VIDEO_MIXING_RENDERER: Iid = guid(
    0xb87b_eb7b,
    0x8d29,
    0x423f,
    [0xae, 0x4d, 0x65, 0x82, 0xc1, 0x01, 0x75, 0xac],
);
pub(crate) const IID_IVMRFILTERCONFIG: Iid = guid(
    0x9e55_30c5,
    0x7034,
    0x48b4,
    [0xbb, 0x46, 0x0b, 0x8a, 0x6e, 0xfc, 0x8e, 0x36],
);
pub(crate) const IID_IVMRWINDOWLESSCONTROL: Iid = guid(
    0x0eb1_088c,
    0x4dcd,
    0x46f0,
    [0x87, 0x8f, 0x39, 0xda, 0xe8, 0x6a, 0x51, 0xb7],
);
pub(crate) const MEDIASUBTYPE_ARGB32: Iid = guid(
    0x773c_9ac0,
    0x3274,
    0x11d0,
    [0xb7, 0x24, 0x00, 0xaa, 0x00, 0x6c, 0x1a, 0x01],
);
pub(crate) const FORMAT_VIDEOINFO2: Iid = guid(
    0xf72a_76a0,
    0xeb0a,
    0x11d0,
    [0xac, 0xe4, 0x00, 0x00, 0xc0, 0xcc, 0x16, 0xba],
);

const VMR_MODE_WINDOWED: u32 = 1;
const VMR_MODE_WINDOWLESS: u32 = 2;
const VMR_MODE_RENDERLESS: u32 = 4;
const VMR_MAX_STREAMS: u32 = 16;
const VMR_ARMODE_NONE: u32 = 0;
const VMR_ARMODE_LETTER_BOX: u32 = 1;
const EC_COMPLETE: u32 = 1;
const EC_ERRORABORT: u32 = 3;
const E_FAIL: u32 = 0x8000_4005;
const VFW_S_NO_STOP_TIME: u32 = 0x0004_0270;
const VFW_E_INVALIDMEDIATYPE: u32 = 0x8004_0200;
const WS_CHILD: u32 = 0x4000_0000;
const WS_CLIPSIBLINGS: u32 = 0x0400_0000;
const POLL_US: u64 = 10_000;
const MAX_VIDEO_DIMENSION: u32 = 8192;

pub(crate) struct Vmr {
    pub mode: u32,
    pub streams: u32,
    pub prefs: u32,
    pub aspect_mode: u32,
    pub color_key: u32,
    pub wake: Handle,
    pub flushing: bool,
    pub stopping: bool,
    pub eos: bool,
    pub complete_sent: bool,
    pub prerolled: bool,
    pub allocator: u32,
    pub output: Option<RawVideoOutput>,
}

impl Vmr {
    pub fn new(env: &mut Env<'_>) -> Self {
        let wake = env.sys.objects.insert(Object::Event {
            manual_reset: true,
            signaled: false,
        });
        Self {
            mode: VMR_MODE_WINDOWED,
            streams: 1,
            prefs: 0,
            aspect_mode: VMR_ARMODE_NONE,
            color_key: 0,
            wake,
            flushing: false,
            stopping: false,
            eos: false,
            complete_sent: false,
            prerolled: false,
            allocator: 0,
            output: None,
        }
    }
}

pub(super) const FILTER_CONFIG_METHODS: &[(&str, Conv, u16)] = &[
    ("QueryInterface", Conv::Stdcall { argc: 3 }, 0),
    ("AddRef", Conv::Stdcall { argc: 1 }, 1),
    ("Release", Conv::Stdcall { argc: 1 }, 2),
    ("SetImageCompositor", Conv::Stdcall { argc: 2 }, 3),
    ("SetNumberOfStreams", Conv::Stdcall { argc: 2 }, 4),
    ("GetNumberOfStreams", Conv::Stdcall { argc: 2 }, 5),
    ("SetRenderingPrefs", Conv::Stdcall { argc: 2 }, 6),
    ("GetRenderingPrefs", Conv::Stdcall { argc: 2 }, 7),
    ("SetRenderingMode", Conv::Stdcall { argc: 2 }, 8),
    ("GetRenderingMode", Conv::Stdcall { argc: 2 }, 9),
];

pub(super) const WINDOWLESS_METHODS: &[(&str, Conv, u16)] = &[
    ("QueryInterface", Conv::Stdcall { argc: 3 }, 0),
    ("AddRef", Conv::Stdcall { argc: 1 }, 1),
    ("Release", Conv::Stdcall { argc: 1 }, 2),
    ("GetNativeVideoSize", Conv::Stdcall { argc: 5 }, 3),
    ("GetMinIdealVideoSize", Conv::Stdcall { argc: 3 }, 4),
    ("GetMaxIdealVideoSize", Conv::Stdcall { argc: 3 }, 5),
    ("SetVideoPosition", Conv::Stdcall { argc: 3 }, 6),
    ("GetVideoPosition", Conv::Stdcall { argc: 3 }, 7),
    ("GetAspectRatioMode", Conv::Stdcall { argc: 2 }, 8),
    ("SetAspectRatioMode", Conv::Stdcall { argc: 2 }, 9),
    ("SetVideoClippingWindow", Conv::Stdcall { argc: 2 }, 10),
    ("RepaintVideo", Conv::Stdcall { argc: 3 }, 11),
    ("DisplayModeChanged", Conv::Stdcall { argc: 1 }, 12),
    ("GetCurrentImage", Conv::Stdcall { argc: 2 }, 13),
    ("SetBorderColor", Conv::Stdcall { argc: 2 }, 14),
    ("GetBorderColor", Conv::Stdcall { argc: 2 }, 15),
    ("SetColorKey", Conv::Stdcall { argc: 2 }, 16),
    ("GetColorKey", Conv::Stdcall { argc: 2 }, 17),
];

pub(super) fn interface_methods(interface: &str) -> Option<&'static [(&'static str, Conv, u16)]> {
    match interface {
        "IVMRFilterConfig" => Some(FILTER_CONFIG_METHODS),
        "IVMRWindowlessControl" => Some(WINDOWLESS_METHODS),
        _ => None,
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct PixelFormat {
    sub: Iid,
    width: u32,
    height: u32,
    top_down: bool,
    frame_100ns: i64,
}

fn parse_video_format(mt: &MediaType) -> Option<PixelFormat> {
    if mt.major != MEDIATYPE_VIDEO {
        return None;
    }
    if ![
        MEDIASUBTYPE_RGB32,
        MEDIASUBTYPE_ARGB32,
        MEDIASUBTYPE_RGB24,
        MEDIASUBTYPE_RGB565,
        MEDIASUBTYPE_RGB555,
    ]
    .contains(&mt.sub)
    {
        return None;
    }
    let header_offset = if mt.format_type == FORMAT_VIDEOINFO && mt.format.len() >= 88 {
        48
    } else if mt.format_type == FORMAT_VIDEOINFO2 && mt.format.len() >= 112 {
        72
    } else {
        return None;
    };
    let word = |at: usize| u32::from_le_bytes(mt.format[at..at + 4].try_into().unwrap());
    let bmi_size = word(header_offset);
    if bmi_size < 40 {
        return None;
    }
    let width = word(header_offset + 4) as i32;
    let raw_height = word(header_offset + 8) as i32;
    if width <= 0 || raw_height == 0 {
        return None;
    }
    let height = raw_height.unsigned_abs();
    if width as u32 > MAX_VIDEO_DIMENSION || height > MAX_VIDEO_DIMENSION {
        return None;
    }
    let frame_100ns = i64::from_le_bytes(mt.format[40..48].try_into().unwrap());
    Some(PixelFormat {
        sub: mt.sub,
        width: width as u32,
        height,
        top_down: raw_height < 0,
        frame_100ns,
    })
}

pub(crate) fn accepts_input(mt: &MediaType) -> bool {
    parse_video_format(mt).is_some()
}

pub(crate) fn video_info_from_media_type(mt: &MediaType) -> Option<VideoInfo> {
    let fmt = parse_video_format(mt)?;
    let frame_rate = if fmt.frame_100ns > 0 {
        (10_000_000, fmt.frame_100ns.min(i64::from(i32::MAX)) as i32)
    } else {
        (30, 1)
    };
    Some(VideoInfo {
        codec: "rgb".into(),
        width: fmt.width,
        height: fmt.height,
        frame_rate,
    })
}

fn filter_vmr<'a>(env: &'a mut Env<'_>, filter: u32) -> Option<&'a mut Vmr> {
    state(env).filters.get_mut(&filter)?.vmr.as_mut()
}

fn writable(env: &Env<'_>, pointer: u32, len: u32) -> bool {
    pointer != 0 && env.cpu.accessible(pointer, len, Prot::WRITE)
}

fn write_word(env: &mut Env<'_>, pointer: u32, value: u32) -> u32 {
    if !writable(env, pointer, 4) || env.cpu.write_u32(pointer, value).is_err() {
        E_POINTER
    } else {
        S_OK
    }
}

fn read_rect(env: &Env<'_>, pointer: u32) -> Result<Rect, u32> {
    if pointer == 0 {
        return Err(E_POINTER);
    }
    let mut raw = [0u8; 16];
    env.cpu.read(pointer, &mut raw).map_err(|_| E_POINTER)?;
    let word = |at: usize| i32::from_le_bytes(raw[at..at + 4].try_into().unwrap());
    let (left, top, right, bottom) = (word(0), word(4), word(8), word(12));
    let (Some(width), Some(height)) = (right.checked_sub(left), bottom.checked_sub(top)) else {
        return Err(E_INVALIDARG);
    };
    Ok(Rect {
        left,
        top,
        width,
        height,
    })
}

fn write_rect(env: &mut Env<'_>, pointer: u32, rect: Rect) -> u32 {
    if !writable(env, pointer, 16) {
        return E_POINTER;
    }
    let Some(right) = rect.left.checked_add(rect.width) else {
        return E_INVALIDARG;
    };
    let Some(bottom) = rect.top.checked_add(rect.height) else {
        return E_INVALIDARG;
    };
    let bytes: Vec<u8> = [rect.left, rect.top, right, bottom]
        .into_iter()
        .flat_map(i32::to_le_bytes)
        .collect();
    if env.cpu.write(pointer, &bytes).is_err() {
        E_POINTER
    } else {
        S_OK
    }
}

pub(crate) fn call(env: &mut Env<'_>, face: Face, method: &str, this: u32) -> Option<Outcome> {
    let filter = canonical(env, this)?;
    if !state(env).filters.contains_key(&filter) {
        return None;
    }
    let result = match face {
        Face::VmrFilterConfig => config_call(env, filter, method),
        Face::VmrWindowlessControl => windowless_call(env, filter, method),
        _ => return None,
    };
    Some(Outcome::Return(result))
}

fn config_call(env: &mut Env<'_>, filter: u32, method: &str) -> u32 {
    let a1 = env.arg(1);
    let graph = state(env).filters[&filter].graph;
    match method {
        "SetImageCompositor" => {
            if a1 == 0 {
                S_OK
            } else {
                adv_abi::warn_unavailable(
                    env,
                    "IVMRFilterConfig::SetImageCompositor",
                    "custom VMR image compositors are not supported",
                );
                E_NOTIMPL
            }
        }
        "SetNumberOfStreams" => {
            if a1 == 1 {
                filter_vmr(env, filter).unwrap().streams = 1;
                S_OK
            } else if (2..=VMR_MAX_STREAMS).contains(&a1) {
                adv_abi::warn_unavailable(
                    env,
                    "IVMRFilterConfig::SetNumberOfStreams",
                    "multiple VMR input streams are not supported",
                );
                E_NOTIMPL
            } else {
                E_INVALIDARG
            }
        }
        "GetNumberOfStreams" => {
            let value = filter_vmr(env, filter).unwrap().streams;
            write_word(env, a1, value)
        }
        "SetRenderingPrefs" => {
            filter_vmr(env, filter).unwrap().prefs = a1;
            S_OK
        }
        "GetRenderingPrefs" => {
            let value = filter_vmr(env, filter).unwrap().prefs;
            write_word(env, a1, value)
        }
        "SetRenderingMode" => match a1 {
            VMR_MODE_WINDOWED => {
                filter_vmr(env, filter).unwrap().mode = a1;
                S_OK
            }
            VMR_MODE_WINDOWLESS => {
                filter_vmr(env, filter).unwrap().mode = a1;
                // Windowless VMR renders directly onto the clipping HWND
                // instead of auto-showing a standalone renderer window.
                if let Some(g) = state(env).graphs.get_mut(&graph) {
                    g.video.auto_show = false;
                }
                S_OK
            }
            VMR_MODE_RENDERLESS => {
                adv_abi::warn_unavailable(
                    env,
                    "IVMRFilterConfig::SetRenderingMode",
                    "VMRMode_Renderless is not supported",
                );
                E_NOTIMPL
            }
            _ => E_INVALIDARG,
        },
        "GetRenderingMode" => {
            let value = filter_vmr(env, filter).unwrap().mode;
            write_word(env, a1, value)
        }
        _ => E_NOTIMPL,
    }
}

fn connected_info(env: &Env<'_>, filter: u32) -> (u32, Option<VideoInfo>) {
    let state = env.ext.get::<State>();
    let graph = state.filters[&filter].graph;
    let info = if graph != 0 {
        video::info(env, graph)
    } else {
        state.filters[&filter]
            .pins
            .iter()
            .find_map(|id| state.pins.get(id)?.mt.as_ref())
            .and_then(video_info_from_media_type)
    };
    (graph, info)
}

fn letterbox_destination(position: Rect, info: &VideoInfo) -> Option<Rect> {
    if position.width <= 0 || position.height <= 0 || info.width == 0 || info.height == 0 {
        return None;
    }
    let pw = i64::from(position.width);
    let ph = i64::from(position.height);
    let vw = i64::from(info.width);
    let vh = i64::from(info.height);
    let (w, h) = if pw * vh <= ph * vw {
        (pw, (pw * vh / vw).max(1))
    } else {
        ((ph * vw / vh).max(1), ph)
    };
    Some(Rect {
        left: ((pw - w) / 2) as i32,
        top: ((ph - h) / 2) as i32,
        width: w as i32,
        height: h as i32,
    })
}

fn windowless_call(env: &mut Env<'_>, filter: u32, method: &str) -> u32 {
    let (graph, info) = connected_info(env, filter);
    match method {
        "GetNativeVideoSize" => {
            let (lp_w, lp_h, lp_ar_w, lp_ar_h) = (env.arg(1), env.arg(2), env.arg(3), env.arg(4));
            let (w, h) = info.map_or((0, 0), |i| (i.width, i.height));
            for &(ptr, val) in &[(lp_w, w), (lp_h, h), (lp_ar_w, w), (lp_ar_h, h)] {
                if ptr != 0 && write_word(env, ptr, val) != S_OK {
                    return E_POINTER;
                }
            }
            S_OK
        }
        "GetMinIdealVideoSize" | "GetMaxIdealVideoSize" => {
            let (lp_w, lp_h) = (env.arg(1), env.arg(2));
            if lp_w == 0 && lp_h == 0 {
                return E_POINTER;
            }
            let (w, h) = info.map_or((0, 0), |i| (i.width, i.height));
            for &(ptr, val) in &[(lp_w, w), (lp_h, h)] {
                if ptr != 0 && write_word(env, ptr, val) != S_OK {
                    return E_POINTER;
                }
            }
            S_OK
        }
        "SetVideoPosition" => {
            if graph == 0 {
                return E_UNEXPECTED;
            }
            let (src_ptr, dst_ptr) = (env.arg(1), env.arg(2));
            let src = if src_ptr != 0 {
                let r = match read_rect(env, src_ptr) {
                    Ok(r) => r,
                    Err(e) => return e,
                };
                if r.width <= 0 || r.height <= 0 {
                    return E_INVALIDARG;
                }
                if let Some(ref i) = info
                    && (r.left < 0
                        || r.top < 0
                        || i64::from(r.left) + i64::from(r.width) > i64::from(i.width)
                        || i64::from(r.top) + i64::from(r.height) > i64::from(i.height))
                {
                    return E_INVALIDARG;
                }
                Some(r)
            } else {
                None
            };
            let dst = if dst_ptr != 0 {
                let r = match read_rect(env, dst_ptr) {
                    Ok(r) => r,
                    Err(e) => return e,
                };
                if r.width <= 0 || r.height <= 0 {
                    return E_INVALIDARG;
                }
                Some(r)
            } else {
                None
            };
            let aspect = filter_vmr(env, filter).unwrap().aspect_mode;
            let Some(g) = state(env).graphs.get_mut(&graph) else {
                return E_UNEXPECTED;
            };
            if let Some(src) = src {
                g.video.source = Some(src);
            }
            if let Some(dst) = dst {
                g.video.position = Some(dst);
                g.video.destination = (aspect == VMR_ARMODE_LETTER_BOX)
                    .then(|| info.as_ref().and_then(|i| letterbox_destination(dst, i)))
                    .flatten();
            }
            if info.is_some() {
                video::refresh(env, graph)
            } else {
                S_OK
            }
        }
        "GetVideoPosition" => {
            if graph == 0 {
                return E_UNEXPECTED;
            }
            let (src_ptr, dst_ptr) = (env.arg(1), env.arg(2));
            let Some(info) = info else {
                return VFW_E_NOT_CONNECTED;
            };
            let (src_rect, abs_dst) = {
                let w = &state(env).graphs[&graph].video;
                let pos = w.effective_position(&info);
                let dst = w.effective_destination(&info);
                (
                    w.effective_source(&info),
                    Rect {
                        left: pos.left + dst.left,
                        top: pos.top + dst.top,
                        width: dst.width,
                        height: dst.height,
                    },
                )
            };
            if src_ptr != 0 && write_rect(env, src_ptr, src_rect) != S_OK {
                return E_POINTER;
            }
            if dst_ptr != 0 && write_rect(env, dst_ptr, abs_dst) != S_OK {
                return E_POINTER;
            }
            S_OK
        }
        "GetAspectRatioMode" => {
            let value = filter_vmr(env, filter).unwrap().aspect_mode;
            write_word(env, env.arg(1), value)
        }
        "SetAspectRatioMode" => match env.arg(1) {
            VMR_ARMODE_NONE | VMR_ARMODE_LETTER_BOX => {
                filter_vmr(env, filter).unwrap().aspect_mode = env.arg(1);
                S_OK
            }
            _ => E_INVALIDARG,
        },
        "SetVideoClippingWindow" => {
            let hwnd = env.arg(1);
            if graph == 0 {
                return E_UNEXPECTED;
            }
            if hwnd != 0 && video::owner_geometry(env, hwnd).is_none() {
                return E_INVALIDARG;
            }
            let Some(g) = state(env).graphs.get_mut(&graph) else {
                return E_UNEXPECTED;
            };
            g.video.owner = hwnd;
            if hwnd != 0 {
                g.video.style = WS_CHILD | WS_CLIPSIBLINGS;
                g.video.visible = true;
                g.video.auto_show = false;
            } else {
                g.video.visible = false;
            }
            if info.is_some() {
                video::refresh(env, graph)
            } else {
                S_OK
            }
        }
        "RepaintVideo" | "DisplayModeChanged" => {
            if graph != 0 && info.is_some() {
                video::refresh(env, graph)
            } else {
                S_OK
            }
        }
        "GetCurrentImage" => {
            let output = env.arg(1);
            if !writable(env, output, 4) {
                return E_POINTER;
            }
            if graph == 0 {
                return E_UNEXPECTED;
            }
            let Some(info) = info else {
                return VFW_E_NOT_CONNECTED;
            };
            let bytes = match super::video::window::current_image_dib(env, graph, &info) {
                Ok(bytes) => bytes,
                Err(error) => return error,
            };
            let dib = adv_os::com::task_alloc(env, bytes.len() as u32);
            if dib == 0 {
                return E_OUTOFMEMORY;
            }
            if env.cpu.write(dib, &bytes).is_err() || env.cpu.write_u32(output, dib).is_err() {
                adv_os::com::task_free(env, dib);
                return E_POINTER;
            }
            S_OK
        }
        "SetBorderColor" => {
            if graph == 0 {
                return E_UNEXPECTED;
            }
            let color = env.arg(1);
            if color > 0x00ff_ffff {
                return E_INVALIDARG;
            }
            state(env)
                .graphs
                .get_mut(&graph)
                .unwrap()
                .video
                .border_color = color;
            if info.is_some() {
                video::refresh(env, graph)
            } else {
                S_OK
            }
        }
        "GetBorderColor" => {
            if graph == 0 {
                return E_UNEXPECTED;
            }
            let color = state(env).graphs[&graph].video.border_color;
            write_word(env, env.arg(1), color)
        }
        "SetColorKey" => {
            filter_vmr(env, filter).unwrap().color_key = env.arg(1);
            S_OK
        }
        "GetColorKey" => {
            let key = filter_vmr(env, filter).unwrap().color_key;
            write_word(env, env.arg(1), key)
        }
        _ => E_NOTIMPL,
    }
}

pub(crate) fn state_changed(env: &mut Env<'_>, filter: u32, value: u32) {
    let Some(v) = filter_vmr(env, filter) else {
        return;
    };
    let (wake, allocator) = (v.wake, v.allocator);
    v.stopping = false;
    let mut deliver_eos = false;
    if value == graph::STOPPED {
        v.prerolled = false;
        v.flushing = false;
        v.eos = false;
        v.complete_sent = false;
    } else if value == graph::RUNNING && v.eos && !v.complete_sent && !v.flushing {
        v.complete_sent = true;
        deliver_eos = true;
    }
    if allocator != 0 {
        if value == graph::STOPPED {
            sample::decommit(env, allocator);
        } else {
            sample::commit(env, allocator);
        }
    }
    env.sys.set_event(wake);
    if deliver_eos {
        let graph = state(env).filters[&filter].graph;
        if graph != 0 && state(env).graphs.contains_key(&graph) {
            graph::notify(env, graph, EC_COMPLETE, 0, 0);
        }
    }
}

pub(crate) fn begin_stop(env: &mut Env<'_>, graph: u32) {
    let items: Vec<(Handle, u32)> = state(env)
        .filters
        .values_mut()
        .filter(|f| f.graph == graph)
        .filter_map(|f| {
            let v = f.vmr.as_mut()?;
            v.stopping = true;
            Some((v.wake, v.allocator))
        })
        .collect();
    for (wake, allocator) in items {
        if allocator != 0 {
            sample::decommit(env, allocator);
        }
        env.sys.set_event(wake);
    }
}

pub(crate) fn destroyed(env: &mut Env<'_>, vmr: Vmr) {
    if vmr.allocator != 0 {
        sample::decommit(env, vmr.allocator);
        release(env, vmr.allocator);
    }
    env.sys.objects.close(vmr.wake);
}

/// Connected guest-fed `Role::VideoRenderer` filters expected to deliver EC_COMPLETE.
pub(crate) fn renderers(env: &Env<'_>, graph: u32) -> u32 {
    let state = env.ext.get::<State>();
    state
        .filters
        .values()
        .filter(|f| f.graph == graph && f.role == super::filter::Role::VideoRenderer)
        .filter(|f| {
            f.pins.iter().any(|p| {
                let pin = &state.pins[p];
                pin.dir == INPUT && pin.peer != 0 && !state.pins.contains_key(&pin.peer)
            })
        })
        .count() as u32
}

/// Guest sources connected directly or indirectly to a host video renderer.
pub(crate) fn upstream_filters(env: &Env<'_>, graph: u32) -> Vec<u32> {
    let state = env.ext.get::<State>();
    let mut sources = Vec::new();
    for m in &state.graphs[&graph].members {
        if m.host.is_some() {
            continue;
        }
        let connected_to_host_renderer = state.filters.values().any(|f| {
            f.graph == graph
                && f.role == super::filter::Role::VideoRenderer
                && f.pins.iter().any(|p| {
                    state.pins[p].peer != 0 && !state.pins.contains_key(&state.pins[p].peer)
                })
        });
        if connected_to_host_renderer {
            sources.push(m.filter);
        }
    }
    sources
}

pub(crate) fn pin_method(env: &mut Env<'_>, pin: u32, method: &str) -> Option<Outcome> {
    let p = state(env).pins.get(&pin)?;
    if p.dir != INPUT {
        return None;
    }
    let filter = p.filter;
    let f = state(env).filters.get(&filter)?;
    let (filter_state, graph) = (f.state, f.graph);
    match method {
        "BeginFlush" => {
            let v = filter_vmr(env, filter)?;
            let wake = v.wake;
            v.flushing = true;
            env.sys.set_event(wake);
            Some(Outcome::Return(S_OK))
        }
        "EndFlush" => {
            let v = filter_vmr(env, filter)?;
            v.flushing = false;
            v.eos = false;
            v.complete_sent = false;
            v.prerolled = false;
            Some(Outcome::Return(S_OK))
        }
        "NewSegment" => {
            let v = filter_vmr(env, filter)?;
            v.eos = false;
            v.complete_sent = false;
            v.prerolled = false;
            Some(Outcome::Return(S_OK))
        }
        "EndOfStream" => {
            let v = filter_vmr(env, filter)?;
            if filter_state == graph::STOPPED || v.flushing || v.stopping {
                return Some(Outcome::Return(S_OK));
            }
            let wake = (!v.eos).then_some(v.wake);
            v.eos = true;
            let notify = filter_state == graph::RUNNING && !v.complete_sent;
            if notify {
                v.complete_sent = true;
            }
            if let Some(wake) = wake {
                env.sys.set_event(wake);
            }
            if notify && state(env).graphs.contains_key(&graph) {
                graph::notify(env, graph, EC_COMPLETE, 0, 0);
                return Some(graph::flush_posts(
                    env,
                    Box::new(|_, _| Outcome::Return(S_OK)),
                ));
            }
            Some(Outcome::Return(S_OK))
        }
        _ => None,
    }
}

pub(crate) fn input_method(env: &mut Env<'_>, pin: u32, method: &str) -> Option<Outcome> {
    let filter = state(env).pins[&pin].filter;
    let r = |value: u32| Some(Outcome::Return(value));
    match method {
        "GetAllocator" => {
            let output = env.arg(1);
            if output == 0 {
                return r(E_POINTER);
            }
            let existing = filter_vmr(env, filter)?.allocator;
            let allocator = if existing != 0 {
                existing
            } else {
                match sample::create_allocator(env, 0, 0) {
                    Ok(allocator) => {
                        filter_vmr(env, filter)?.allocator = allocator;
                        allocator
                    }
                    Err(error) => return r(error),
                }
            };
            add_ref(env, allocator);
            r(if env.cpu.write_u32(output, allocator).is_ok() {
                S_OK
            } else {
                E_POINTER
            })
        }
        "NotifyAllocator" => r(S_OK),
        "GetAllocatorRequirements" => r(E_NOTIMPL),
        "ReceiveCanBlock" => r(S_OK),
        "Receive" => Some(receive(env, pin, env.arg(1))),
        "ReceiveMultiple" => {
            let processed = env.arg(3);
            if !writable(env, processed, 4) || env.cpu.write_u32(processed, 0).is_err() {
                return r(E_POINTER);
            }
            let count = env.arg(2).min(64);
            let samples: Vec<u32> = (0..count)
                .map(|i| env.cpu.read_u32(env.arg(1) + i * 4).unwrap_or(0))
                .collect();
            Some(receive_each(env, pin, samples, processed, 0))
        }
        _ => None,
    }
}

type Done = Box<dyn FnOnce(&mut Env<'_>, u32) -> Outcome>;

fn receive_each(
    env: &mut Env<'_>,
    pin: u32,
    mut samples: Vec<u32>,
    processed: u32,
    done_count: u32,
) -> Outcome {
    if samples.is_empty() {
        return Outcome::Return(S_OK);
    }
    let sample = samples.remove(0);
    receive_then(
        env,
        pin,
        sample,
        Box::new(move |env, hr| {
            if hr != S_OK {
                return Outcome::Return(hr);
            }
            let next_count = done_count + 1;
            let _ = env.cpu.write_u32(processed, next_count);
            receive_each(env, pin, samples, processed, next_count)
        }),
    )
}

fn receive(env: &mut Env<'_>, pin: u32, sample: u32) -> Outcome {
    receive_then(env, pin, sample, Box::new(|_, hr| Outcome::Return(hr)))
}

struct ReadSample {
    bytes: Vec<u8>,
    start_100ns: Option<i64>,
    preroll: bool,
}

fn receive_then(env: &mut Env<'_>, pin: u32, sample: u32, done: Done) -> Outcome {
    if sample == 0 {
        return done(env, E_POINTER);
    }
    let Some(p) = state(env).pins.get(&pin) else {
        return done(env, E_UNEXPECTED);
    };
    let (filter, peer, mt) = (p.filter, p.peer, p.mt.clone());
    if peer == 0 {
        return done(env, VFW_E_NOT_CONNECTED);
    }
    let Some(fmt) = mt.as_ref().and_then(parse_video_format) else {
        return done(env, VFW_E_INVALIDMEDIATYPE);
    };
    let Some(f) = state(env).filters.get(&filter) else {
        return done(env, E_UNEXPECTED);
    };
    let (filter_state, graph) = (f.state, f.graph);
    let Some(v) = f.vmr.as_ref() else {
        return done(env, E_UNEXPECTED);
    };
    if v.flushing || v.stopping {
        return done(env, S_FALSE);
    }
    if filter_state == graph::STOPPED {
        return done(env, VFW_E_WRONG_STATE);
    }
    if let Some(s) = canonical(env, sample).and_then(|c| state(env).samples.get(&c).cloned()) {
        let len = s.actual as usize;
        let mut bytes = vec![0u8; len];
        if len > 0 && env.cpu.read(s.buffer, &mut bytes).is_err() {
            return done(env, E_POINTER);
        }
        return present_sample(
            env,
            filter,
            graph,
            fmt,
            ReadSample {
                bytes,
                start_100ns: s.time.map(|(start, _)| start),
                preroll: s.preroll,
            },
            done,
        );
    }
    read_guest_sample(env, sample, move |env, result| match result {
        Ok(read) => present_sample(env, filter, graph, fmt, read, done),
        Err(hr) => done(env, hr),
    })
}

fn read_guest_sample(
    env: &mut Env<'_>,
    sample: u32,
    then: impl FnOnce(&mut Env<'_>, Result<ReadSample, u32>) -> Outcome + 'static,
) -> Outcome {
    let Some(scratch) = alloc(env, 24) else {
        return then(env, Err(E_OUTOFMEMORY));
    };
    guest_call(env, sample, 3, vec![scratch], move |env, hr| {
        let ptr = env.cpu.read_u32(scratch).unwrap_or(0);
        if hr != S_OK || ptr == 0 {
            free(env, scratch);
            return then(env, Err(if hr != S_OK { hr } else { E_POINTER }));
        }
        guest_call(env, sample, 11, vec![], move |env, actual| {
            let len = (actual as i32).max(0) as usize;
            let mut bytes = vec![0u8; len];
            if len > 0 && env.cpu.read(ptr, &mut bytes).is_err() {
                free(env, scratch);
                return then(env, Err(E_POINTER));
            }
            guest_call(
                env,
                sample,
                5,
                vec![scratch + 8, scratch + 16],
                move |env, time_hr| {
                    let start_100ns = matches!(time_hr, S_OK | VFW_S_NO_STOP_TIME)
                        .then(|| read_i64(env, scratch + 8))
                        .flatten();
                    guest_call(env, sample, 9, vec![], move |env, preroll_hr| {
                        free(env, scratch);
                        then(
                            env,
                            Ok(ReadSample {
                                bytes,
                                start_100ns,
                                preroll: preroll_hr == S_OK,
                            }),
                        )
                    })
                },
            )
        })
    })
}

fn present_sample(
    env: &mut Env<'_>,
    filter: u32,
    graph: u32,
    fmt: PixelFormat,
    sample: ReadSample,
    done: Done,
) -> Outcome {
    if sample.preroll {
        return done(env, S_OK);
    }
    let bgra = match to_bgra(&sample.bytes, fmt) {
        Ok(bgra) => bgra,
        Err(hr) => return done(env, hr),
    };
    wait_due(env, filter, graph, sample.start_100ns, move |env, hr| {
        if hr != S_OK {
            return done(env, hr);
        }
        let pts_us = (sample.start_100ns.unwrap_or(0).max(0) / 10) as u64;
        publish_frame(env, filter, graph, fmt, pts_us, bgra, done)
    })
}

fn wait_due(
    env: &mut Env<'_>,
    filter: u32,
    graph: u32,
    start_100ns: Option<i64>,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    let (filter_state, flushing, stopping, prerolled, wake) = {
        let Some(f) = state(env).filters.get(&filter) else {
            return then(env, VFW_E_WRONG_STATE);
        };
        let Some(v) = f.vmr.as_ref() else {
            return then(env, VFW_E_WRONG_STATE);
        };
        (f.state, v.flushing, v.stopping, v.prerolled, v.wake)
    };
    if flushing || stopping {
        return then(env, S_FALSE);
    }
    if filter_state == graph::STOPPED || !state(env).graphs.contains_key(&graph) {
        return then(env, VFW_E_WRONG_STATE);
    }
    let now = env.sys.clock.now_us();
    let position = state(env).graphs[&graph].position(now);
    let wait_us = if filter_state == graph::RUNNING {
        let Some(due) = start_100ns else {
            return then(env, S_OK);
        };
        if position >= due {
            return then(env, S_OK);
        }
        (((due - position) / 10) as u64).min(POLL_US * 10)
    } else if !prerolled {
        return then(env, S_OK);
    } else {
        POLL_US
    };
    env.sys.reset_event(wake);
    Outcome::block(
        Wait::Object {
            handle: wake,
            deadline: Some(now + wait_us.max(1)),
        },
        move |env, _| wait_due(env, filter, graph, start_100ns, then),
    )
}

fn publish_frame(
    env: &mut Env<'_>,
    filter: u32,
    graph: u32,
    fmt: PixelFormat,
    pts_us: u64,
    bgra: Vec<u8>,
    done: Done,
) -> Outcome {
    let Some(v) = filter_vmr(env, filter) else {
        return done(env, VFW_E_WRONG_STATE);
    };
    if v.flushing || v.stopping {
        return done(env, S_FALSE);
    }
    v.prerolled = true;
    let output = v.output.get_or_insert_with(RawVideoOutput::new);
    let frame = match output.push_bgra(pts_us, fmt.width, fmt.height, bgra.clone()) {
        Ok(frame) => frame,
        Err(adv_device::media::Error::Decode(message)) if message.contains("exhausted") => {
            let wake = v.wake;
            let now = env.sys.clock.now_us();
            env.sys.reset_event(wake);
            return Outcome::block(
                Wait::Object {
                    handle: wake,
                    deadline: Some(now + 1000),
                },
                move |env, _| publish_frame(env, filter, graph, fmt, pts_us, bgra, done),
            );
        }
        Err(_) => {
            if state(env).graphs.contains_key(&graph) {
                graph::notify(env, graph, EC_ERRORABORT, E_FAIL, 0);
                return graph::flush_posts(env, Box::new(move |env, _| done(env, E_FAIL)));
            }
            return done(env, E_FAIL);
        }
    };
    match video::publish(env, graph, frame) {
        Ok(outcome) => {
            env.ext.get_or_default::<Playback>().video_frames += 1;
            match outcome {
                Some(outcome) => after(env, outcome, Box::new(move |env, _| done(env, S_OK))),
                None => done(env, S_OK),
            }
        }
        Err(hr) => {
            if state(env).graphs.contains_key(&graph) {
                graph::notify(env, graph, EC_ERRORABORT, hr, 0);
                return graph::flush_posts(env, Box::new(move |env, _| done(env, hr)));
            }
            done(env, hr)
        }
    }
}

fn to_bgra(src: &[u8], fmt: PixelFormat) -> Result<Vec<u8>, u32> {
    let width = fmt.width as usize;
    let height = fmt.height as usize;
    let bpp = if fmt.sub == MEDIASUBTYPE_RGB32 || fmt.sub == MEDIASUBTYPE_ARGB32 {
        4
    } else if fmt.sub == MEDIASUBTYPE_RGB24 {
        3
    } else {
        2
    };
    let src_stride = (width * bpp + 3) & !3;
    if src.len() < src_stride * height {
        return Err(E_INVALIDARG);
    }
    let mut dst = vec![0u8; width * height * 4];
    for y in 0..height {
        let src_y = if fmt.top_down { y } else { height - 1 - y };
        let srow = &src[src_y * src_stride..src_y * src_stride + width * bpp];
        let drow = &mut dst[y * width * 4..(y + 1) * width * 4];
        if fmt.sub == MEDIASUBTYPE_RGB32 {
            for (s, d) in srow
                .as_chunks::<4>()
                .0
                .iter()
                .zip(drow.as_chunks_mut::<4>().0.iter_mut())
            {
                d[0] = s[0];
                d[1] = s[1];
                d[2] = s[2];
                d[3] = 0xff;
            }
        } else if fmt.sub == MEDIASUBTYPE_ARGB32 {
            drow.copy_from_slice(srow);
        } else if fmt.sub == MEDIASUBTYPE_RGB24 {
            for (s, d) in srow
                .as_chunks::<3>()
                .0
                .iter()
                .zip(drow.as_chunks_mut::<4>().0.iter_mut())
            {
                d[0] = s[0];
                d[1] = s[1];
                d[2] = s[2];
                d[3] = 0xff;
            }
        } else if fmt.sub == MEDIASUBTYPE_RGB565 {
            for (s, d) in srow
                .as_chunks::<2>()
                .0
                .iter()
                .zip(drow.as_chunks_mut::<4>().0.iter_mut())
            {
                let p = u16::from_le_bytes([s[0], s[1]]);
                let r5 = ((p >> 11) & 0x1f) as u8;
                let g6 = ((p >> 5) & 0x3f) as u8;
                let b5 = (p & 0x1f) as u8;
                d[0] = (b5 << 3) | (b5 >> 2);
                d[1] = (g6 << 2) | (g6 >> 4);
                d[2] = (r5 << 3) | (r5 >> 2);
                d[3] = 0xff;
            }
        } else {
            for (s, d) in srow
                .as_chunks::<2>()
                .0
                .iter()
                .zip(drow.as_chunks_mut::<4>().0.iter_mut())
            {
                let p = u16::from_le_bytes([s[0], s[1]]);
                let r5 = ((p >> 10) & 0x1f) as u8;
                let g5 = ((p >> 5) & 0x1f) as u8;
                let b5 = (p & 0x1f) as u8;
                d[0] = (b5 << 3) | (b5 >> 2);
                d[1] = (g5 << 3) | (g5 >> 2);
                d[2] = (r5 << 3) | (r5 >> 2);
                d[3] = 0xff;
            }
        }
    }
    Ok(dst)
}

#[cfg(test)]
mod tests {
    use adv_cpu::backend::{Guest, GuestExt, Reg};
    use adv_device::surface::VideoLayer;

    use super::*;
    use crate::dshow::tests::{DATA, STACK, activate, machine, method};

    fn query(m: &mut adv_machine::Machine<adv_cpu::JitCpu>, iface: u32, iid: Iid) -> u32 {
        m.cpu.write(DATA, &iid).unwrap();
        assert_eq!(
            method(m, "IBaseFilter::QueryInterface", &[iface, DATA, DATA + 16]),
            S_OK
        );
        m.cpu.read_u32(DATA + 16).unwrap()
    }

    fn setup_windowless() -> (
        adv_machine::Machine<adv_cpu::JitCpu>,
        u32,
        u32,
        u32,
        u32,
        u32,
    ) {
        let mut m = machine();
        m.ext.insert(VideoProvider {
            window: |_, _, hwnd| {
                (hwnd == 0x200).then_some(VideoWindowGeometry {
                    width: 800,
                    height: 600,
                    origin: (10, 20),
                    canvas: (1024, 768),
                    visible: true,
                })
            },
            update: |ext, _, _, layer| {
                let updates = ext.get_or_default::<Vec<Option<VideoLayer>>>();
                updates.push(layer);
                Ok(updates.len() as u64)
            },
            completed: |_, _| None,
        });
        let graph = activate(&mut m, CLSID_FILTERGRAPH, IID_IFILTERGRAPH2);
        let control = query(&mut m, graph, IID_IMEDIACONTROL);
        let vmr = activate(&mut m, CLSID_VIDEO_MIXING_RENDERER, IID_IBASEFILTER);
        assert_eq!(
            method(&mut m, "IFilterGraph2::AddFilter", &[graph, vmr, 0]),
            S_OK
        );

        let config = query(&mut m, vmr, IID_IVMRFILTERCONFIG);
        assert_eq!(
            method(&mut m, "IVMRFilterConfig::SetRenderingMode", &[config, 2]),
            S_OK
        );
        assert_eq!(
            method(&mut m, "IVMRFilterConfig::SetNumberOfStreams", &[config, 1]),
            S_OK
        );
        let windowless = query(&mut m, vmr, IID_IVMRWINDOWLESSCONTROL);

        // Locate VMR input pin ("VMR Input0") and connect a guest output pin.
        let name: Vec<u8> = "VMR Input0"
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        m.cpu.write(DATA + 64, &name).unwrap();
        assert_eq!(
            method(&mut m, "IBaseFilter::FindPin", &[vmr, DATA + 64, DATA]),
            S_OK
        );
        let input_pin = m.cpu.read_u32(DATA).unwrap();

        // Install a dummy guest pin vtable whose AddRef (slot 1) returns S_OK.
        let guest_vt = DATA + 1024;
        let guest_pin = DATA + 1100;
        let add_ref_ret = DATA + 1200;
        m.cpu
            .write(add_ref_ret, &[0xb8, 1, 0, 0, 0, 0xc2, 4, 0])
            .unwrap();
        m.cpu.write_u32(guest_vt + 4, add_ref_ret).unwrap();
        m.cpu.write_u32(guest_pin, guest_vt).unwrap();

        let mt = MediaType::video_rgb(MEDIASUBTYPE_RGB32, 2, 2, 333_333);
        let mt_addr = DATA + 256;
        let fmt_addr = DATA + 512;
        m.cpu.write(fmt_addr, &mt.format).unwrap();
        let mut raw_mt = [0u8; MEDIA_TYPE_SIZE as usize];
        raw_mt[0..16].copy_from_slice(&mt.major);
        raw_mt[16..32].copy_from_slice(&mt.sub);
        raw_mt[32..36].copy_from_slice(&1u32.to_le_bytes());
        raw_mt[40..44].copy_from_slice(&mt.sample_size.to_le_bytes());
        raw_mt[44..60].copy_from_slice(&mt.format_type);
        raw_mt[64..68].copy_from_slice(&(mt.format.len() as u32).to_le_bytes());
        raw_mt[68..72].copy_from_slice(&fmt_addr.to_le_bytes());
        m.cpu.write(mt_addr, &raw_mt).unwrap();

        // IPin::ReceiveConnection succeeds before SetVideoClippingWindow.
        let mut env = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        let thunk = env
            .sys
            .thunks
            .iter()
            .find(|(_, t)| t.name == "IPin::ReceiveConnection")
            .unwrap()
            .0;
        env.thunk = thunk;
        env.cpu.set_reg(Reg::Esp, STACK);
        for (i, word) in [input_pin, guest_pin, mt_addr].into_iter().enumerate() {
            env.cpu.write_u32(STACK + 4 + i as u32 * 4, word).unwrap();
        }
        let hr = match filter::call(&mut env, Face::Pin, "ReceiveConnection", input_pin).unwrap() {
            Outcome::Call {
                target, args, then, ..
            } => {
                assert_eq!(target, add_ref_ret);
                assert_eq!(args, vec![guest_pin]);
                match then(&mut env, adv_machine::Resume::Returned(1)) {
                    Outcome::Return(hr) => hr,
                    _ => panic!("unexpected continuation"),
                }
            }
            Outcome::Return(hr) => hr,
            _ => panic!("unexpected ReceiveConnection outcome"),
        };
        assert_eq!(hr, S_OK);
        (m, graph, control, windowless, input_pin, config)
    }

    #[test]
    fn windowless_vmr_connects_before_clipping_window_and_maps_rects() {
        let (mut m, _graph, _control, windowless, _input, config) = setup_windowless();
        // Multi-stream requests are rejected instead of succeeding silently.
        assert_eq!(
            method(&mut m, "IVMRFilterConfig::SetNumberOfStreams", &[config, 2]),
            E_NOTIMPL
        );
        assert_eq!(
            method(&mut m, "IVMRFilterConfig::SetNumberOfStreams", &[config, 0]),
            E_INVALIDARG
        );
        assert_eq!(
            method(
                &mut m,
                "IVMRFilterConfig::GetNumberOfStreams",
                &[config, DATA]
            ),
            S_OK
        );
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), 1);

        // Calling SetVideoClippingWindow AFTER ReceiveConnection succeeds.
        assert_eq!(
            method(
                &mut m,
                "IVMRWindowlessControl::SetVideoClippingWindow",
                &[windowless, 0x200]
            ),
            S_OK
        );
        assert_eq!(
            method(
                &mut m,
                "IVMRWindowlessControl::GetNativeVideoSize",
                &[windowless, DATA, DATA + 4, DATA + 8, DATA + 12],
            ),
            S_OK
        );
        assert_eq!(
            [
                m.cpu.read_u32(DATA).unwrap(),
                m.cpu.read_u32(DATA + 4).unwrap(),
                m.cpu.read_u32(DATA + 8).unwrap(),
                m.cpu.read_u32(DATA + 12).unwrap(),
            ],
            [2, 2, 2, 2]
        );
        let dst_rect: Vec<u8> = [10i32, 20, 410, 320]
            .into_iter()
            .flat_map(i32::to_le_bytes)
            .collect();
        m.cpu.write(DATA + 32, &dst_rect).unwrap();
        assert_eq!(
            method(
                &mut m,
                "IVMRWindowlessControl::SetVideoPosition",
                &[windowless, 0, DATA + 32],
            ),
            S_OK
        );
        assert_eq!(
            method(
                &mut m,
                "IVMRWindowlessControl::GetVideoPosition",
                &[windowless, DATA + 64, DATA + 80],
            ),
            S_OK
        );
        let got = |at: u32| -> [i32; 4] {
            let mut b = [0u8; 16];
            m.cpu.read(at, &mut b).unwrap();
            [
                i32::from_le_bytes(b[0..4].try_into().unwrap()),
                i32::from_le_bytes(b[4..8].try_into().unwrap()),
                i32::from_le_bytes(b[8..12].try_into().unwrap()),
                i32::from_le_bytes(b[12..16].try_into().unwrap()),
            ]
        };
        assert_eq!(got(DATA + 64), [0, 0, 2, 2]);
        assert_eq!(got(DATA + 80), [10, 20, 410, 320]);
    }

    #[cfg(any(not(feature = "gpu"), feature = "media"))]
    #[test]
    fn pushed_rgb32_flips_rows_paces_pause_and_completes_on_eos() {
        let (mut m, graph, control, windowless, input_pin, _config) = setup_windowless();
        assert_eq!(
            method(
                &mut m,
                "IVMRWindowlessControl::SetVideoClippingWindow",
                &[windowless, 0x200]
            ),
            S_OK
        );
        let mem_input = query(&mut m, input_pin, IID_IMEMINPUTPIN);
        assert_eq!(
            method(&mut m, "IMemInputPin::GetAllocator", &[mem_input, DATA]),
            S_OK
        );
        let alloc_ptr = m.cpu.read_u32(DATA).unwrap();
        let props: Vec<u8> = [2u32, 16, 1, 0]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        m.cpu.write(DATA + 16, &props).unwrap();
        assert_eq!(
            method(
                &mut m,
                "IMemAllocator::SetProperties",
                &[alloc_ptr, DATA + 16, DATA + 32]
            ),
            S_OK
        );
        assert_eq!(method(&mut m, "IMemAllocator::Commit", &[alloc_ptr]), S_OK);
        assert_eq!(method(&mut m, "IMediaControl::Pause", &[control]), S_OK);

        // First buffer: bottom-up 2x2 RGB32 with zero alpha bytes; row 0 in buffer is image bottom.
        assert_eq!(
            method(
                &mut m,
                "IMemAllocator::GetBuffer",
                &[alloc_ptr, DATA, 0, 0, 0]
            ),
            S_OK
        );
        let sample = m.cpu.read_u32(DATA).unwrap();
        assert_eq!(
            method(&mut m, "IMediaSample::GetPointer", &[sample, DATA + 4]),
            S_OK
        );
        let buf = m.cpu.read_u32(DATA + 4).unwrap();
        m.cpu
            .write(
                buf,
                &[
                    1, 2, 3, 0, 4, 5, 6, 0, // bottom row
                    10, 20, 30, 0, 40, 50, 60, 0, // top row
                ],
            )
            .unwrap();
        assert_eq!(
            method(&mut m, "IMediaSample::SetActualDataLength", &[sample, 16]),
            S_OK
        );
        m.cpu.write(DATA + 64, &0i64.to_le_bytes()).unwrap();
        m.cpu.write(DATA + 72, &333_333i64.to_le_bytes()).unwrap();
        assert_eq!(
            method(
                &mut m,
                "IMediaSample::SetTime",
                &[sample, DATA + 64, DATA + 72]
            ),
            S_OK
        );
        assert_eq!(
            method(&mut m, "IMemInputPin::Receive", &[mem_input, sample]),
            S_OK
        );

        #[cfg(not(feature = "gpu"))]
        {
            let updates = m.ext.get::<Vec<Option<VideoLayer>>>();
            let last = updates.last().unwrap().as_ref().unwrap();
            assert_eq!(
                &*last.frame.bgra,
                &[
                    10, 20, 30, 255, 40, 50, 60, 255, // top row first
                    1, 2, 3, 255, 4, 5, 6, 255, // bottom row second
                ]
            );
        }
        assert_eq!(m.ext.get::<Playback>().video_frames, 1);
        assert_eq!(
            method(
                &mut m,
                "IVMRWindowlessControl::GetCurrentImage",
                &[windowless, DATA + 96]
            ),
            S_OK
        );
        let dib_ptr = m.cpu.read_u32(DATA + 96).unwrap();
        assert_ne!(dib_ptr, 0);

        // Flush cancels delivery while Run + EndOfStream completes the graph.
        assert_eq!(method(&mut m, "IPin::BeginFlush", &[input_pin]), S_OK);
        assert_eq!(
            method(&mut m, "IMemInputPin::Receive", &[mem_input, sample]),
            S_FALSE
        );
        assert_eq!(method(&mut m, "IPin::EndFlush", &[input_pin]), S_OK);

        assert_eq!(method(&mut m, "IMediaControl::Run", &[control]), S_OK);
        // VMR GetCurrentImage succeeds in RUNNING and STOPPED states as well as PAUSED.
        assert_eq!(
            method(
                &mut m,
                "IVMRWindowlessControl::GetCurrentImage",
                &[windowless, DATA + 100]
            ),
            S_OK
        );
        assert_ne!(m.cpu.read_u32(DATA + 100).unwrap(), 0);
        assert_eq!(method(&mut m, "IPin::EndOfStream", &[input_pin]), S_OK);
        let events = query(&mut m, graph, IID_IMEDIAEVENTEX);
        assert_eq!(
            method(
                &mut m,
                "IMediaEventEx::GetEvent",
                &[events, DATA, DATA + 4, DATA + 8, 0]
            ),
            S_OK
        );
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), 1);
        assert_eq!(method(&mut m, "IMediaControl::Stop", &[control]), S_OK);
        assert_eq!(
            method(
                &mut m,
                "IVMRWindowlessControl::GetCurrentImage",
                &[windowless, DATA + 104]
            ),
            S_OK
        );
        assert_ne!(m.cpu.read_u32(DATA + 104).unwrap(), 0);
    }

    #[test]
    fn paused_eos_defers_ec_complete_until_run_once() {
        const E_ABORT: u32 = 0x8000_4004;
        let (mut m, _graph, control, _windowless, input_pin, _config) = setup_windowless();
        let events = query(&mut m, _graph, IID_IMEDIAEVENTEX);
        assert_eq!(method(&mut m, "IMediaControl::Pause", &[control]), S_OK);
        assert_eq!(method(&mut m, "IPin::EndOfStream", &[input_pin]), S_OK);
        assert_eq!(
            method(
                &mut m,
                "IMediaEventEx::GetEvent",
                &[events, DATA, DATA + 4, DATA + 8, 0]
            ),
            E_ABORT
        );

        // Emitting EC_COMPLETE happens once when entering RUNNING.
        assert_eq!(method(&mut m, "IMediaControl::Run", &[control]), S_OK);
        assert_eq!(
            method(
                &mut m,
                "IMediaEventEx::GetEvent",
                &[events, DATA, DATA + 4, DATA + 8, 0]
            ),
            S_OK
        );
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), EC_COMPLETE);

        // Pausing and running again without flushing or a new EOS does not repeat EC_COMPLETE.
        assert_eq!(method(&mut m, "IMediaControl::Pause", &[control]), S_OK);
        assert_eq!(method(&mut m, "IMediaControl::Run", &[control]), S_OK);
        assert_eq!(
            method(
                &mut m,
                "IMediaEventEx::GetEvent",
                &[events, DATA, DATA + 4, DATA + 8, 0]
            ),
            E_ABORT
        );
    }

    #[test]
    fn guest_sample_with_no_stop_time_preserves_start_timestamp() {
        let mut m = machine();
        let sample_vt = DATA + 2048;
        let sample_obj = DATA + 2120;
        let buf = DATA + 2200;
        m.cpu.write(buf, &[0u8; 16]).unwrap();
        let slots = [
            (3u32, DATA + 2300),
            (11, DATA + 2310),
            (5, DATA + 2320),
            (9, DATA + 2330),
        ];
        m.cpu.write_u32(sample_obj, sample_vt).unwrap();
        for (slot, addr) in slots {
            m.cpu.write_u32(sample_vt + slot * 4, addr).unwrap();
        }
        let mut env = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
        let out = captured.clone();
        let mut outcome = read_guest_sample(&mut env, sample_obj, move |_, res| {
            *out.borrow_mut() = Some(res);
            Outcome::Return(0)
        });
        loop {
            match outcome {
                Outcome::Call {
                    target, args, then, ..
                } => {
                    let hr = if target == slots[0].1 {
                        env.cpu.write_u32(args[1], buf).unwrap();
                        S_OK
                    } else if target == slots[1].1 {
                        16
                    } else if target == slots[2].1 {
                        env.cpu.write(args[1], &5_000_000i64.to_le_bytes()).unwrap();
                        VFW_S_NO_STOP_TIME
                    } else {
                        assert_eq!(target, slots[3].1);
                        S_FALSE
                    };
                    outcome = then(&mut env, adv_machine::Resume::Returned(hr));
                }
                Outcome::Return(0) => break,
                _ => panic!("unexpected outcome while reading guest sample"),
            }
        }
        let sample = captured.borrow_mut().take().unwrap().unwrap();
        assert_eq!(sample.bytes.len(), 16);
        assert_eq!(sample.start_100ns, Some(5_000_000));
        assert!(!sample.preroll);
    }
}

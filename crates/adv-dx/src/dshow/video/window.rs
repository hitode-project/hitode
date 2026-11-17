//! Video-window distribution and the VMR guest USER window. Geometry is in video-window client
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
    if face == Face::VideoWindow
        && method == "put_FullScreenMode"
        && env.arg(1) == 0
        && super::renderer(env, graph).is_some_and(|f| {
            env.ext.get::<State>().filters[&f].clsid
                == crate::dshow::vmr::CLSID_VIDEO_MIXING_RENDERER
        })
    {
        return Some(Outcome::Return(S_FALSE));
    }
    let result = match face {
        Face::VideoWindow => window_call(env, graph, method, &info),
        Face::BasicVideo => basic_call(env, graph, method, &info),
        _ => return None,
    };
    Some(Outcome::Return(result))
}

pub(crate) fn user(
    env: &mut Env<'_>,
    name: &str,
    args: Vec<u32>,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    let entry = env
        .ext
        .try_get::<crate::dshow::host::Host>()
        .and_then(|host| (host.export)(env, "user32.dll", name));
    match entry {
        Some(entry) => Outcome::call(entry, args, move |env, result| then(env, result.value())),
        None => Outcome::Return(E_NOTIMPL),
    }
}

pub(crate) fn create_vmr_window(env: &mut Env<'_>, filter: u32) -> Outcome {
    use crate::dshow::{alloc, free};
    if env.ext.try_get::<crate::dshow::host::Host>().is_none() {
        return Outcome::Return(S_OK);
    }
    let Some(text) = alloc(env, 64) else {
        return Outcome::Return(E_OUTOFMEMORY);
    };
    let bytes: Vec<u8> = "static\0ActiveMovie Window\0"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let _ = env.cpu.write(text, &bytes);
    user(
        env,
        "CreateWindowExW",
        vec![0, text, text + 14, 0x06cf_0000, 0, 0, 0, 0, 0, 0, 0, 0],
        move |env, hwnd| {
            free(env, text);
            if hwnd == 0 {
                return Outcome::Return(E_FAIL);
            }
            state(env)
                .filters
                .get_mut(&filter)
                .unwrap()
                .vmr
                .as_mut()
                .unwrap()
                .hwnd = hwnd;
            let proc = env
                .sys
                .thunks
                .publish(env.cpu, "quartz.dll", "VMRWindowProc")
                .unwrap();
            user(
                env,
                "SetWindowLongW",
                vec![hwnd, (-4i32) as u32, proc],
                |_, _| Outcome::Return(S_OK),
            )
        },
    )
}

pub(crate) fn connect_vmr_window(env: &mut Env<'_>, filter: u32) -> Outcome {
    use crate::dshow::{alloc, free};
    let f = &state(env).filters[&filter];
    let (graph, hwnd, mode) = (
        f.graph,
        f.vmr.as_ref().unwrap().hwnd,
        f.vmr.as_ref().unwrap().mode,
    );
    if graph == 0 {
        return Outcome::Return(S_OK);
    }
    let windowless = mode == 2;
    if windowless {
        let window = &mut state(env).graphs.get_mut(&graph).unwrap().video;
        window.auto_show = false;
        window.position = Some(Rect {
            left: 0,
            top: 0,
            width: 0,
            height: 0,
        });
        return Outcome::Return(S_OK);
    }
    let info = super::info(env, graph).unwrap();
    let window = &mut state(env).graphs.get_mut(&graph).unwrap().video;
    window.source = None;
    window.destination = None;
    if hwnd == 0 {
        return Outcome::Return(S_OK);
    }
    window.owner = hwnd;
    let Some(rect) = alloc(env, 16) else {
        return Outcome::Return(E_OUTOFMEMORY);
    };
    let bytes: Vec<u8> = [0, 0, info.width, info.height]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    let _ = env.cpu.write(rect, &bytes);
    user(
        env,
        "AdjustWindowRectEx",
        vec![rect, 0x06cf_0000, 0, 0],
        move |env, _| {
            let width = env
                .cpu
                .read_u32(rect + 8)
                .unwrap()
                .wrapping_sub(env.cpu.read_u32(rect).unwrap());
            let height = env
                .cpu
                .read_u32(rect + 12)
                .unwrap()
                .wrapping_sub(env.cpu.read_u32(rect + 4).unwrap());
            user(
                env,
                "SetWindowPos",
                vec![hwnd, 0, 0, 0, width, height, 0x14],
                move |env, _| {
                    user(env, "GetClientRect", vec![hwnd, rect], move |env, _| {
                        let width = env.cpu.read_u32(rect + 8).unwrap() as i32;
                        let height = env.cpu.read_u32(rect + 12).unwrap() as i32;
                        free(env, rect);
                        state(env).graphs.get_mut(&graph).unwrap().video.position = Some(Rect {
                            left: 0,
                            top: 0,
                            width,
                            height,
                        });
                        state(env)
                            .filters
                            .get_mut(&filter)
                            .unwrap()
                            .vmr
                            .as_mut()
                            .unwrap()
                            .closed = false;
                        Outcome::Return(S_OK)
                    })
                },
            )
        },
    )
}

pub(crate) fn vmr_window_proc(env: &mut Env<'_>) -> Outcome {
    let args: Vec<u32> = (0..4).map(|i| env.arg(i)).collect();
    let (hwnd, message) = (args[0], args[1]);
    let filter = state(env)
        .filters
        .iter()
        .find_map(|(&id, f)| f.vmr.as_ref().filter(|v| v.hwnd == hwnd).map(|_| id));
    if let Some(filter) = filter {
        let f = &state(env).filters[&filter];
        let graph = f.graph;
        let drain = f.vmr.as_ref().unwrap().drain;
        if drain != 0 && matches!(message, 0x21 | 0xa1..=0xa9 | 0x100 | 0x101 | 0x200..=0x209) {
            let mut forwarded = args.clone();
            forwarded[0] = drain;
            return user(env, "PostMessageW", forwarded, |_, _| Outcome::Return(0));
        }
        if message == 0x10 {
            let v = state(env)
                .filters
                .get_mut(&filter)
                .unwrap()
                .vmr
                .as_mut()
                .unwrap();
            v.closed = true;
            let wake = v.wake;
            env.sys.set_event(wake);
            if state(env).graphs.contains_key(&graph) {
                graph::notify(env, graph, 2, 0, 0);
                state(env)
                    .graphs
                    .get_mut(&graph)
                    .unwrap()
                    .video
                    .set_visible(false);
                let _ = super::refresh(env, graph);
            }
            return user(env, "ShowWindow", vec![hwnd, 0], move |env, _| {
                graph::flush_posts(env, Box::new(|_, _| Outcome::Return(0)))
            });
        }
        if message == 5 && state(env).graphs.contains_key(&graph) {
            let size = args[3];
            let w = &mut state(env).graphs.get_mut(&graph).unwrap().video;
            w.position = Some(Rect {
                left: 0,
                top: 0,
                width: (size & 0xffff) as i32,
                height: (size >> 16) as i32,
            });
            let _ = super::refresh(env, graph);
        }
    }
    user(env, "DefWindowProcW", args, |_, result| {
        Outcome::Return(result)
    })
}

pub(crate) fn vmr_window_call(env: &mut Env<'_>, filter: u32, method: &str) -> Outcome {
    use crate::dshow::{alloc, free};
    let f = &state(env).filters[&filter];
    let (graph, hwnd) = (f.graph, f.vmr.as_ref().unwrap().hwnd);
    if !super::connected(env, graph) {
        return Outcome::Return(VFW_E_NOT_CONNECTED);
    }
    let (value, a2, a3, a4) = (env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    let finish = |_: &mut Env<'_>, _: u32| Outcome::Return(S_OK);
    match method {
        "get_Caption" => {
            let Some(text) = alloc(env, 1024) else {
                return Outcome::Return(E_OUTOFMEMORY);
            };
            user(
                env,
                "GetWindowTextW",
                vec![hwnd, text, 512],
                move |env, length| {
                    let mut bytes = vec![0; length as usize * 2];
                    let _ = env.cpu.read(text, &mut bytes);
                    free(env, text);
                    let wide: Vec<u16> = bytes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|b| u16::from_le_bytes([b[0], b[1]]))
                        .collect();
                    let bstr = adv_os::com::alloc_bstr(env, &wide);
                    Outcome::Return(if env.cpu.write_u32(value, bstr).is_ok() {
                        S_OK
                    } else {
                        E_POINTER
                    })
                },
            )
        }
        "put_Caption" => user(env, "SetWindowTextW", vec![hwnd, value], finish),
        "get_WindowStyle" | "get_WindowStyleEx" => {
            let index = if method.ends_with("Ex") { -20i32 } else { -16 };
            user(
                env,
                "GetWindowLongW",
                vec![hwnd, index as u32],
                move |env, style| Outcome::Return(write_word(env, style)),
            )
        }
        "put_WindowStyle" => {
            if value & WS_FORBIDDEN != 0 {
                return Outcome::Return(E_INVALIDARG);
            }
            user(env, "IsWindowVisible", vec![hwnd], move |env, visible| {
                user(
                    env,
                    "GetWindowLongW",
                    vec![hwnd, (-16i32) as u32],
                    move |env, old| {
                        let style = (value & !WS_VISIBLE)
                            | (old & 0x2100_0000)
                            | if visible != 0 {
                                WS_VISIBLE | 0x0400_0000
                            } else {
                                0
                            };
                        user(
                            env,
                            "SetWindowLongW",
                            vec![hwnd, (-16i32) as u32, style],
                            finish,
                        )
                    },
                )
            })
        }
        "put_WindowStyleEx" => user(
            env,
            "SetWindowLongW",
            vec![hwnd, (-20i32) as u32, value],
            finish,
        ),
        "get_AutoShow" => Outcome::Return(write_word(env, bool_word(window(env, graph).auto_show))),
        "put_AutoShow" => {
            state(env).graphs.get_mut(&graph).unwrap().video.auto_show = value != 0;
            Outcome::Return(S_OK)
        }
        "get_Visible" => user(env, "IsWindowVisible", vec![hwnd], move |env, visible| {
            Outcome::Return(write_word(env, bool_word(visible != 0)))
        }),
        "put_Visible" | "put_WindowState" => {
            let show = if method == "put_Visible" {
                if value == 0 { 0 } else { 5 }
            } else {
                value
            };
            state(env)
                .graphs
                .get_mut(&graph)
                .unwrap()
                .video
                .set_visible(show != 0);
            user(env, "ShowWindow", vec![hwnd, show], move |env, _| {
                Outcome::Return(super::refresh(env, graph))
            })
        }
        "get_WindowState" => user(
            env,
            "GetWindowLongW",
            vec![hwnd, (-16i32) as u32],
            move |env, style| {
                let show = if style & WS_VISIBLE == 0 {
                    0
                } else if style & 0x2000_0000 != 0 {
                    6
                } else if style & 0x0100_0000 != 0 {
                    3
                } else {
                    5
                };
                Outcome::Return(write_word(env, show))
            },
        ),
        "get_MessageDrain" => {
            let drain = state(env).filters[&filter].vmr.as_ref().unwrap().drain;
            Outcome::Return(write_word(env, drain))
        }
        "put_MessageDrain" => {
            state(env)
                .filters
                .get_mut(&filter)
                .unwrap()
                .vmr
                .as_mut()
                .unwrap()
                .drain = value;
            Outcome::Return(S_OK)
        }
        "get_Owner" => user(
            env,
            "GetWindowLongW",
            vec![hwnd, (-16i32) as u32],
            move |env, style| {
                if style & 0x4000_0000 == 0 {
                    return Outcome::Return(write_word(env, 0));
                }
                user(env, "GetParent", vec![hwnd], move |env, parent| {
                    Outcome::Return(write_word(env, parent))
                })
            },
        ),
        "put_Owner" => user(
            env,
            "GetWindowLongW",
            vec![hwnd, (-16i32) as u32],
            move |env, style| {
                let style = if value == 0 {
                    (style & !0x4000_0000) | 0x0400_0000
                } else {
                    style | 0x4000_0000
                };
                user(
                    env,
                    "SetWindowLongW",
                    vec![hwnd, (-16i32) as u32, style],
                    move |env, _| user(env, "SetParent", vec![hwnd, value], finish),
                )
            },
        ),
        "NotifyOwnerMessage" => {
            if a2 == 0x15 {
                user(env, "SendMessageW", vec![hwnd, a2, a3, a4], finish)
            } else {
                Outcome::Return(S_OK)
            }
        }
        "SetWindowForeground" => {
            if !matches!(value, 0 | u32::MAX) {
                return Outcome::Return(E_INVALIDARG);
            }
            if value == 0 {
                return user(env, "SetWindowPos", vec![hwnd, 0, 0, 0, 0, 0, 0x13], finish);
            }
            user(
                env,
                "SetWindowPos",
                vec![hwnd, 0, 0, 0, 0, 0, 3],
                move |env, _| {
                    user(env, "SetForegroundWindow", vec![hwnd], move |env, _| {
                        user(env, "SetFocus", vec![hwnd], finish)
                    })
                },
            )
        }
        "get_FullScreenMode" | "put_FullScreenMode" => Outcome::Return(E_NOTIMPL),
        "GetMinIdealImageSize" | "GetMaxIdealImageSize" => {
            Outcome::Return(crate::dshow::VFW_E_WRONG_STATE)
        }
        "GetWindowPosition" | "get_Left" | "get_Top" | "get_Width" | "get_Height"
        | "SetWindowPosition" | "put_Left" | "put_Top" | "put_Width" | "put_Height" => {
            let Some(rect) = alloc(env, 16) else {
                return Outcome::Return(E_OUTOFMEMORY);
            };
            let method = method.to_owned();
            let outputs = [value, a2, a3, a4];
            user(env, "GetWindowRect", vec![hwnd, rect], move |env, _| {
                let left = env.cpu.read_u32(rect).unwrap() as i32;
                let top = env.cpu.read_u32(rect + 4).unwrap() as i32;
                let width = (env.cpu.read_u32(rect + 8).unwrap() as i32) - left;
                let height = (env.cpu.read_u32(rect + 12).unwrap() as i32) - top;
                free(env, rect);
                let position = Rect {
                    left,
                    top,
                    width,
                    height,
                };
                if method == "GetWindowPosition" {
                    return Outcome::Return(write_words(
                        env,
                        &outputs,
                        &[left as u32, top as u32, width as u32, height as u32],
                    ));
                }
                if method.starts_with("get_") {
                    return Outcome::Return(write_word(env, rect_property(position, &method)));
                }
                let position = if method == "SetWindowPosition" {
                    Rect {
                        left: value as i32,
                        top: a2 as i32,
                        width: a3 as i32,
                        height: a4 as i32,
                    }
                } else {
                    property_rect(position, &method, value as i32)
                };
                user(
                    env,
                    "SetWindowPos",
                    vec![
                        hwnd,
                        0,
                        position.left as u32,
                        position.top as u32,
                        position.width as u32,
                        position.height as u32,
                        0x14,
                    ],
                    finish,
                )
            })
        }
        _ => {
            let info = super::info(env, graph).unwrap();
            Outcome::Return(window_call(env, graph, method, &info))
        }
    }
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

pub(crate) fn vmr_basic_call(env: &mut Env<'_>, filter: u32, method: &str) -> Outcome {
    let (a1, a2, a3, a4) = (env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    let outputs: &[u32] = match method {
        "GetSourcePosition" | "GetDestinationPosition" => &[a1, a2, a3, a4],
        "GetVideoSize" | "GetPreferredAspectRatio" => &[a1, a2],
        "GetVideoPaletteEntries" => &[a3],
        "GetCurrentImage" => &[a1],
        method if method.starts_with("get_") => &[a1],
        _ => &[],
    };
    if outputs.iter().any(|&p| !writable(env, p, 4)) {
        return Outcome::Return(E_POINTER);
    }
    let f = &env.ext.get::<State>().filters[&filter];
    let graph = f.graph;
    let Some(info) = super::info(env, graph) else {
        return Outcome::Return(VFW_E_NOT_CONNECTED);
    };
    let result = match method {
        "get_BitRate" | "get_BitErrorRate" => {
            let mt = f
                .pins
                .iter()
                .find_map(|pin| env.ext.get::<State>().pins[pin].mt.as_ref())
                .unwrap();
            let offset = if method == "get_BitRate" { 32 } else { 36 };
            let value = u32::from_le_bytes(mt.format[offset..offset + 4].try_into().unwrap());
            write_word(env, value)
        }
        "GetVideoPaletteEntries" => VFW_E_NO_PALETTE_AVAILABLE,
        "IsUsingDefaultSource" => {
            if window(env, graph).effective_source(&info) == Rect::image(&info) {
                S_OK
            } else {
                S_FALSE
            }
        }
        "GetCurrentImage" => {
            let bytes = match current_image_dib(env, graph, &info) {
                Ok(bytes) => bytes,
                Err(hr) => return Outcome::Return(hr),
            };
            if a2 == 0 {
                write_word(env, bytes.len() as u32)
            } else if env.cpu.write(a2, &bytes).is_ok() {
                S_OK
            } else {
                E_POINTER
            }
        }
        _ => basic_call(env, graph, method, &info),
    };
    Outcome::Return(result)
}

fn basic_call(env: &mut Env<'_>, graph: u32, method: &str, info: &VideoInfo) -> u32 {
    match method {
        "get_VideoWidth" => write_word(env, info.width),
        "get_VideoHeight" => write_word(env, info.height),
        "GetVideoSize" | "GetPreferredAspectRatio" => write_size(env, info.width, info.height),
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

pub(crate) fn current_image_dib(
    env: &Env<'_>,
    graph: u32,
    info: &VideoInfo,
) -> Result<Vec<u8>, u32> {
    let frame = super::latest(env, graph).ok_or(E_FAIL)?;
    let source = window(env, graph).effective_source(info);
    dib(&frame, source)
}

fn current_image(env: &mut Env<'_>, graph: u32, info: &VideoInfo) -> u32 {
    let (size, output) = (env.arg(1), env.arg(2));
    if !writable(env, size, 4) || (output != 0 && !env.cpu.accessible(size, 4, Prot::READ)) {
        return E_POINTER;
    }
    if env.ext.get::<State>().graphs[&graph].state != graph::PAUSED {
        return VFW_E_NOT_PAUSED;
    }
    let bytes = match current_image_dib(env, graph, info) {
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
    let bgra = frame.bgra();
    if !source.in_image(frame.width, frame.height)
        || frame.pitch < frame.width.checked_mul(4).ok_or(E_FAIL)?
        || u64::from(frame.pitch) * u64::from(frame.height) > bgra.len() as u64
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
        bytes[target..target + row as usize].copy_from_slice(&bgra[start..start + row as usize]);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;

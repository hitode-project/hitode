//! The system video renderer owns COM state; the device owns pixels and composition.
//! Runtime assembly resolves USER geometry without a dependency on adv-win32.
use super::{
    filter::{self, INPUT, Role},
    *,
};
use adv_device::{
    media,
    surface::{BlitRect, Rect, VideoFrame, VideoLayer},
};
use adv_machine::{Env, Ext, Objects, Outcome};

pub(crate) mod window;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoWindowGeometry {
    pub width: u32,
    pub height: u32,
    /// Owner client origin relative to the process's presented client canvas.
    pub origin: (i32, i32),
    pub canvas: (u32, u32),
    pub visible: bool,
}

#[derive(Clone, Copy)]
pub struct VideoProvider {
    pub window: fn(&Ext, &Objects, u32) -> Option<VideoWindowGeometry>,
    pub update: fn(
        &mut Ext,
        &mut dyn adv_cpu::backend::Guest,
        u64,
        Option<VideoLayer>,
    ) -> Result<u64, String>,
    pub completed: fn(&mut Env<'_>, u64) -> Option<Outcome>,
}

pub fn set_video_provider(ext: &mut Ext, provider: VideoProvider) {
    ext.insert(provider);
}

fn renderer(env: &Env<'_>, graph: u32) -> Option<u32> {
    let state = env.ext.try_get::<State>()?;
    state
        .graphs
        .get(&graph)?
        .members
        .iter()
        .filter_map(|m| m.host)
        .find(|id| {
            state
                .filters
                .get(id)
                .is_some_and(|f| f.role == Role::VideoRenderer)
        })
}

pub(crate) fn available(env: &Env<'_>, graph: u32) -> bool {
    renderer(env, graph).is_some()
}

pub(crate) fn connected(env: &Env<'_>, graph: u32) -> bool {
    info(env, graph).is_some()
}

pub(crate) fn info(env: &Env<'_>, graph: u32) -> Option<media::VideoInfo> {
    let renderer = renderer(env, graph)?;
    let state = env.ext.get::<State>();
    let input = state
        .filters
        .get(&renderer)?
        .pins
        .iter()
        .find_map(|id| state.pins.get(id).filter(|p| p.dir == INPUT))?;
    if input.peer == 0 || !state.pins.contains_key(&input.peer) {
        return None;
    }
    let source = filter::upstream_source(env, input.peer)?;
    state
        .filters
        .get(&source)?
        .source
        .as_ref()?
        .info
        .as_ref()?
        .video
        .clone()
}

pub(crate) fn latest(env: &Env<'_>, graph: u32) -> Option<VideoFrame> {
    env.ext
        .try_get::<State>()?
        .graphs
        .get(&graph)?
        .video_frame
        .clone()
}

pub(crate) fn owner_geometry(env: &Env<'_>, owner: u32) -> Option<VideoWindowGeometry> {
    let provider = env.ext.try_get::<VideoProvider>()?;
    (provider.window)(env.ext, &env.sys.objects, owner)
}

fn key(graph: u32) -> u64 {
    (1u64 << 63) | u64::from(graph)
}

fn layer(env: &Env<'_>, graph: u32) -> Option<VideoLayer> {
    let graph_state = env.ext.get::<State>().graphs.get(&graph)?;
    let window = &graph_state.video;
    if !window.visible || window.owner == 0 {
        return None;
    }
    let geometry = owner_geometry(env, window.owner)?;
    if !geometry.visible || geometry.width == 0 || geometry.height == 0 {
        return None;
    }
    let info = info(env, graph)?;
    let position = window.effective_position(&info);
    let destination = window.effective_destination(&info);
    let source = window.effective_source(&info);
    let x = i64::from(geometry.origin.0) + i64::from(position.left);
    let y = i64::from(geometry.origin.1) + i64::from(position.top);
    // The owner and the video child clip independently of the sampling origin.
    // In particular a negative destination does not reset texture coordinates.
    let left = i64::from(geometry.origin.0).max(x).max(0);
    let top = i64::from(geometry.origin.1).max(y).max(0);
    let right = (i64::from(geometry.origin.0) + i64::from(geometry.width))
        .min(x + i64::from(position.width))
        .min(i64::from(geometry.canvas.0));
    let bottom = (i64::from(geometry.origin.1) + i64::from(geometry.height))
        .min(y + i64::from(position.height))
        .min(i64::from(geometry.canvas.1));
    if right <= left || bottom <= top {
        return None;
    }
    let (frame, source) = match graph_state.video_frame.clone() {
        Some(frame) => (
            frame,
            Rect {
                x: source.left as u32,
                y: source.top as u32,
                w: source.width as u32,
                h: source.height as u32,
            },
        ),
        None => (
            VideoFrame {
                width: 1,
                height: 1,
                pitch: 4,
                bgra: std::sync::Arc::from([0u8, 0, 0, 255]),
            },
            Rect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
        ),
    };
    let color = window.border_color;
    Some(VideoLayer {
        frame,
        source,
        destination: BlitRect {
            x: x + i64::from(destination.left),
            y: y + i64::from(destination.top),
            w: destination.width as u32,
            h: destination.height as u32,
        },
        background: Some((
            BlitRect {
                x,
                y,
                w: position.width as u32,
                h: position.height as u32,
            },
            [(color >> 16) as u8, (color >> 8) as u8, color as u8, 255],
        )),
        clip: Rect {
            x: left as u32,
            y: top as u32,
            w: (right - left) as u32,
            h: (bottom - top) as u32,
        },
        canvas: geometry.canvas,
        visible: true,
        hide_cursor: window.hide_cursor,
    })
}

fn update(env: &mut Env<'_>, graph: u32) -> Result<u64, u32> {
    let layer = layer(env, graph);
    let owner = state(env).graphs.get(&graph).map_or(0, |g| g.video.owner);
    let geometry = (owner != 0).then(|| owner_geometry(env, owner)).flatten();
    let Some(provider) = env.ext.try_get::<VideoProvider>().copied() else {
        return if layer.is_none() {
            Ok(0)
        } else {
            Err(E_NOINTERFACE)
        };
    };
    let serial = (provider.update)(env.ext, env.cpu, key(graph), layer).map_err(|error| {
        adv_abi::warn_unavailable(env, "DirectShowVideoRenderer", &error);
        0x8000_4005u32
    })?;
    if let Some(g) = state(env).graphs.get_mut(&graph) {
        g.video_geometry = geometry;
    }
    Ok(serial)
}

pub(crate) fn refresh(env: &mut Env<'_>, graph: u32) -> u32 {
    update(env, graph).map_or_else(|error| error, |_| S_OK)
}

pub(crate) fn publish(
    env: &mut Env<'_>,
    graph: u32,
    frame: media::VideoFrame,
) -> Result<Option<Outcome>, u32> {
    if !connected(env, graph) {
        return Err(VFW_E_NOT_CONNECTED);
    }
    let pitch = frame.width.checked_mul(4).ok_or(E_INVALIDARG)?;
    if frame.height == 0
        || frame.width == 0
        || u64::from(pitch) * u64::from(frame.height) != frame.bgra.len() as u64
    {
        return Err(E_INVALIDARG);
    }
    state(env).graphs.get_mut(&graph).unwrap().video_frame = Some(VideoFrame {
        width: frame.width,
        height: frame.height,
        pitch,
        bgra: frame.bgra.into(),
    });
    let visible = layer(env, graph).is_some();
    let serial = update(env, graph)?;
    if !visible {
        return Ok(None);
    }
    let provider = *env.ext.get::<VideoProvider>();
    Ok((provider.completed)(env, serial))
}

pub(crate) fn auto_show(env: &mut Env<'_>, graph: u32) -> u32 {
    if !available(env, graph) {
        return S_OK;
    }
    let window = &mut state(env).graphs.get_mut(&graph).unwrap().video;
    let previous = window.clone();
    if window.auto_show {
        window.set_visible(true);
    }
    let standalone = window.owner == 0 && window.visible;
    if standalone {
        adv_abi::warn_unavailable(
            env,
            "IVideoWindow",
            "standalone video windows are unavailable; attach the renderer to a guest owner window",
        );
        state(env).graphs.get_mut(&graph).unwrap().video = previous;
        return E_NOTIMPL;
    }
    let hr = refresh(env, graph);
    if hr != S_OK {
        state(env).graphs.get_mut(&graph).unwrap().video = previous;
    }
    hr
}

pub(crate) fn detach(env: &mut Env<'_>, graph: u32) {
    if let Some(g) = state(env).graphs.get_mut(&graph) {
        g.video_frame = None;
        g.video_geometry = None;
    }
    if let Some(provider) = env.ext.try_get::<VideoProvider>().copied()
        && let Err(error) = (provider.update)(env.ext, env.cpu, key(graph), None)
    {
        adv_abi::warn_unavailable(env, "DirectShowVideoRenderer", &error);
    }
}

/// Recheck USER geometry at the existing host service boundary, including when
/// a movie is paused or stopped and no new decoded frame will arrive. Unchanged
/// layouts do not republish or wake presentation.
pub fn pump_video<B: adv_cpu::backend::Backend>(
    machine: &mut adv_machine::Machine<B>,
) -> Result<(), String> {
    let Some(state) = machine.ext.try_get::<State>() else {
        return Ok(());
    };
    let graphs: Vec<u32> = state
        .graphs
        .iter()
        .filter_map(|(&id, g)| (g.video.owner != 0 && g.video.visible).then_some(id))
        .collect();
    if graphs.is_empty() {
        return Ok(());
    }
    let mut env = Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    };
    for graph in graphs {
        if !available(&env, graph) {
            continue;
        }
        let g = &env.ext.get::<State>().graphs[&graph];
        let geometry = owner_geometry(&env, g.video.owner);
        if geometry != g.video_geometry {
            update(&mut env, graph)
                .map_err(|hr| format!("DirectShow video window update failed ({hr:#x})"))?;
        }
    }
    Ok(())
}

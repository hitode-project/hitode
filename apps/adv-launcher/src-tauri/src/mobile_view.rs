//! Game view events for the mobile overlay (AND-APP-16): the screen layout
//! as `game_layout` and three-finger taps as `game_keyboard`.
use adv_shell::mobile::{Insets, Layout, Rect};
use adv_shell::window::ViewEvent;
use serde::Serialize;
use tauri::{Emitter, WebviewWindow};

#[derive(Clone, Serialize)]
struct Area {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl From<Rect> for Area {
    fn from(rect: Rect) -> Self {
        Self {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        }
    }
}

#[derive(Clone, Serialize)]
struct Edges {
    top: f64,
    right: f64,
    bottom: f64,
    left: f64,
}

impl From<Insets> for Edges {
    fn from(insets: Insets) -> Self {
        Self {
            top: insets.top,
            right: insets.right,
            bottom: insets.bottom,
            left: insets.left,
        }
    }
}

/// The `game_layout` payload, in CSS pixels of the webview.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GameLayout {
    width: f64,
    height: f64,
    game: Area,
    cutouts: Vec<Area>,
    corner_radius: f64,
    gestures: Edges,
}

impl From<Layout> for GameLayout {
    fn from(layout: Layout) -> Self {
        Self {
            width: layout.width,
            height: layout.height,
            game: layout.game.into(),
            cutouts: layout
                .occlusion
                .cutouts
                .into_iter()
                .map(Area::from)
                .collect(),
            corner_radius: layout.occlusion.corner_radius,
            gestures: layout.occlusion.gestures.into(),
        }
    }
}

/// Forwards a game view's events to the overlay in `window`; `cursor`
/// draws the trackpad cursor where the host must.
pub fn listener(
    window: WebviewWindow,
    cursor: impl Fn(Option<(f32, f32)>) + 'static,
) -> Box<dyn Fn(ViewEvent)> {
    Box::new(move |event| match event {
        ViewEvent::Layout(layout) => {
            let _ = window.emit("game_layout", GameLayout::from(layout));
        }
        ViewEvent::Keyboard => {
            let _ = window.emit("game_keyboard", ());
        }
        ViewEvent::Cursor(point) => cursor(point),
    })
}

/// The overlay's touch mode names.
pub fn touch_mode(mode: &str) -> Result<adv_shell::mobile::TouchMode, String> {
    match mode {
        "direct" => Ok(adv_shell::mobile::TouchMode::Direct),
        "trackpad" => Ok(adv_shell::mobile::TouchMode::Trackpad),
        _ => Err(format!("unknown touch mode {mode}")),
    }
}

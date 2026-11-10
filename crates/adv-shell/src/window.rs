pub use adv_sync::WakeEvent;
use std::sync::Arc;

pub enum Event {
    Menu(adv_runtime::MenuEvent),
    Dialog(adv_runtime::DialogEvent),
    Input {
        message: u32,
        wparam: u32,
        lparam: u32,
    },
    /// Mouse input on the layer that shows guest window `window`, its point
    /// in that window's pixels (WINE-IN-1).
    WindowInput {
        window: u32,
        message: u32,
        wparam: u32,
        lparam: u32,
    },
    #[cfg(host_view)]
    Text(String, std::sync::mpsc::SyncSender<usize>),
    Close,
    #[cfg(host_view)]
    ForceClose,
    FocusLost,
    #[cfg(host_view)]
    AudioSuspended(bool),
    /// The launcher paused the game: no guest execution, frozen guest time
    /// and suspended audio until it resumes.
    #[cfg(host_view)]
    Paused(bool),
    #[cfg(host_view)]
    Volume(u8),
    Repaint,
}
impl Event {
    /// Mouse `message` at view pixel `at`, on the guest window layer there
    /// or else at canvas point `canvas`.
    pub(crate) fn mouse(
        pointer: &mut Pointer,
        message: u32,
        wparam: u32,
        at: (f64, f64),
        canvas: u32,
    ) -> Self {
        match pointer.route(wparam, at) {
            Some((window, lparam)) => Self::WindowInput {
                window,
                message,
                wparam,
                lparam,
            },
            None => Self::Input {
                message,
                wparam,
                lparam: canvas,
            },
        }
    }
}

/// The guest window layers the view shows, and where a press went: until
/// every button is up, the pointer stays with it, as a capture does.
#[derive(Default)]
pub(crate) struct Pointer {
    layers: Arc<[crate::display::WindowLayer]>,
    held: Option<Option<u32>>,
}
impl Pointer {
    pub(crate) fn set_layers(&mut self, layers: Arc<[crate::display::WindowLayer]>) {
        self.layers = layers;
    }
    /// Whether view pixel `at` shows a guest window.
    #[cfg(target_os = "macos")]
    pub(crate) fn over(&self, at: (f64, f64)) -> bool {
        self.layers
            .iter()
            .any(|layer| Self::local(layer, at).is_some())
    }
    /// Where pixel `(x, y)` of guest window `window` shows, in view pixels,
    /// if the window shows as a layer.
    #[cfg(target_os = "macos")]
    pub(crate) fn shown_at(&self, window: u32, (x, y): (i32, i32)) -> Option<(f64, f64)> {
        let layer = self.layers.iter().find(|layer| layer.id == window)?;
        let [left, top, width, height] = layer.rectangle.map(f64::from);
        Some((
            left + f64::from(x) * width / f64::from(layer.size.0.max(1)),
            top + f64::from(y) * height / f64::from(layer.size.1.max(1)),
        ))
    }
    /// View pixel `(x, y)` as a pixel of `layer`, if it shows there.
    fn local(layer: &crate::display::WindowLayer, (x, y): (f64, f64)) -> Option<(i32, i32)> {
        let point = Self::offset(layer, (x, y));
        ((0..layer.size.0 as i32).contains(&point.0) && (0..layer.size.1 as i32).contains(&point.1))
            .then_some(point)
    }
    /// View pixel `(x, y)` relative to `layer`, in its window's pixels.
    fn offset(layer: &crate::display::WindowLayer, (x, y): (f64, f64)) -> (i32, i32) {
        let [left, top, width, height] = layer.rectangle.map(f64::from);
        (
            ((x - left) * f64::from(layer.size.0) / width.max(1.)).floor() as i32,
            ((y - top) * f64::from(layer.size.1) / height.max(1.)).floor() as i32,
        )
    }
    /// The window whose layer gets mouse input at view pixel `at` with
    /// buttons `wparam` held after it, and the point as a window lParam.
    pub(crate) fn route(&mut self, wparam: u32, at: (f64, f64)) -> Option<(u32, u32)> {
        let target = self.held.unwrap_or_else(|| {
            let mut top = self.layers.iter().rev();
            top.find(|layer| Self::local(layer, at).is_some())
                .map(|layer| layer.id)
        });
        // MK_LBUTTON, MK_RBUTTON, MK_MBUTTON, MK_XBUTTON1 and MK_XBUTTON2.
        self.held = (wparam & 0x73 != 0).then_some(target);
        let layer = self.layers.iter().find(|layer| Some(layer.id) == target)?;
        let (x, y) = Self::offset(layer, at);
        Some((
            layer.id,
            u32::from(x as i16 as u16) | u32::from(y as i16 as u16) << 16,
        ))
    }
}

pub type MenuSink = Box<dyn FnMut(adv_runtime::MenuEvent) -> Result<(), String>>;
pub type DialogSink = Box<dyn FnMut(adv_runtime::DialogEvent) -> Result<(), String>>;

#[cfg(target_os = "macos")]
pub(crate) mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::Projection;
#[cfg(target_os = "macos")]
pub use macos::{GameContainer, GameView, Window, initialize_dpi};

#[cfg(mobile)]
pub(crate) mod mobile;
/// The guest dialog and menu data a mobile host's overlay draws and answers.
#[cfg(mobile)]
pub use adv_runtime::{
    DialogControl, DialogEvent, DialogFont, DialogSet, DialogTarget, DialogWindow, MenuBar,
    MenuCommand, MenuEvent, MenuItem,
};
#[cfg(target_os = "ios")]
pub use mobile::GameContainer;
#[cfg(mobile)]
pub use mobile::{GameView, TouchAction, ViewEvent};
#[cfg(any(mobile, test))]
pub(crate) mod android_epoch;
#[cfg(all(test, not(target_os = "android")))]
#[path = "window/android/input.rs"]
mod android_input_tests;

#[cfg(test)]
mod tests {
    use super::*;
    // spec: WINE-IN-1
    #[test]
    fn a_press_on_a_window_layer_keeps_the_pointer_until_release() {
        let mut pointer = Pointer::default();
        pointer.set_layers(Arc::from([
            crate::display::WindowLayer {
                id: 7,
                rectangle: [100., 50., 200., 100.],
                size: (100, 50),
            },
            crate::display::WindowLayer {
                id: 8,
                rectangle: [150., 60., 40., 20.],
                size: (20, 10),
            },
        ]));
        let point = |x: i16, y: i16| u32::from(x as u16) | u32::from(y as u16) << 16;
        // Window pixels at the layer's scale; the topmost layer wins.
        assert_eq!(pointer.route(0, (101., 51.)), Some((7, point(0, 0))));
        assert_eq!(pointer.route(0, (152., 62.)), Some((8, point(1, 1))));
        assert_eq!(pointer.route(0, (99., 51.)), None);
        // A press holds its window while dragged out, until every button is up.
        assert_eq!(pointer.route(1, (110., 60.)), Some((7, point(5, 5))));
        assert_eq!(pointer.route(1, (90., 40.)), Some((7, point(-5, -5))));
        assert_eq!(pointer.route(0, (90., 40.)), Some((7, point(-5, -5))));
        assert_eq!(pointer.route(0, (90., 40.)), None);
        // A press on the frame keeps the frame.
        assert_eq!(pointer.route(1, (90., 40.)), None);
        assert_eq!(pointer.route(1, (110., 60.)), None);
        assert_eq!(pointer.route(0, (110., 60.)), None);
        assert_eq!(pointer.route(0, (110., 60.)), Some((7, point(5, 5))));
    }

    // spec: WINE-UI-1
    #[cfg(target_os = "macos")]
    #[test]
    fn a_window_pixel_shows_at_its_layer_scale() {
        let mut pointer = Pointer::default();
        pointer.set_layers(Arc::from([crate::display::WindowLayer {
            id: 7,
            rectangle: [100., 50., 200., 100.],
            size: (100, 50),
        }]));
        assert_eq!(pointer.shown_at(7, (30, 60)), Some((160., 170.)));
        assert_eq!(pointer.shown_at(8, (30, 60)), None);
    }
}

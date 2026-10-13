//! The overlay's input claim as published by the webview (src/lib/overlay.ts),
//! kept for the native game view's hit testing and first-responder changes.
//! The routing rules themselves are in [`adv_shell::overlay`].
use adv_shell::overlay::{OverlayInput, Rect, Router};
use serde::Deserialize;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// `OverlayInput` as the webview sends it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputMessage {
    regions: Vec<RectMessage>,
    modal: bool,
    wants_keyboard: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RectMessage {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl From<InputMessage> for OverlayInput {
    fn from(message: InputMessage) -> Self {
        let regions = message
            .regions
            .into_iter()
            .map(|r| Rect {
                x: r.x,
                y: r.y,
                width: r.width,
                height: r.height,
            })
            // A rectangle that cannot contain a point claims nothing.
            .filter(|r| [r.x, r.y, r.width, r.height].iter().all(|v| v.is_finite()))
            .filter(|r| r.width > 0.0 && r.height > 0.0)
            .collect();
        OverlayInput {
            regions,
            modal: message.modal,
            wants_keyboard: message.wants_keyboard,
        }
    }
}

type Listener = Box<dyn Fn(&OverlayInput) + Send + Sync>;

/// Routing state shared between the webview commands and the native game
/// view. It starts exclusive: the webview owns all input until the game
/// screen publishes its regions.
pub struct OverlayState {
    pub(crate) router: Arc<Mutex<Router>>,
    listeners: Mutex<Vec<Listener>>,
}

impl Default for OverlayState {
    fn default() -> Self {
        Self {
            router: Arc::new(Mutex::new(Router::new(OverlayInput::exclusive()))),
            listeners: Mutex::default(),
        }
    }
}

impl OverlayState {
    /// The router the host view consults on pointer events.
    pub fn router(&self) -> MutexGuard<'_, Router> {
        self.router.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Calls `listener` after every change of the claim, on the thread that
    /// received it (not the main thread). The host view uses it to move the
    /// first responder when the keyboard target changes.
    pub fn on_change(&self, listener: impl Fn(&OverlayInput) + Send + Sync + 'static) {
        self.listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Box::new(listener));
    }

    pub fn set(&self, input: OverlayInput) {
        {
            let mut router = self.router();
            if *router.input() == input {
                return;
            }
            router.set_input(input.clone());
        }
        for listener in self
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
        {
            listener(&input);
        }
    }
}

#[tauri::command]
pub fn set_overlay_input(state: tauri::State<'_, OverlayState>, input: InputMessage) {
    state.set(input.into());
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_shell::overlay::Target;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn parse(json: &str) -> OverlayInput {
        serde_json::from_str::<InputMessage>(json).unwrap().into()
    }

    // spec: HOST-OVL-1
    #[test]
    fn decodes_the_webview_message() {
        let input = parse(
            r#"{"regions":[{"x":1224,"y":16,"width":40,"height":40}],"modal":false,"wantsKeyboard":true}"#,
        );
        assert_eq!(
            input.regions,
            [Rect {
                x: 1224.0,
                y: 16.0,
                width: 40.0,
                height: 40.0
            }]
        );
        assert!(!input.modal);
        assert_eq!(input.keyboard(), Target::Overlay);
    }

    // spec: HOST-OVL-1
    #[test]
    fn rejects_unknown_fields_and_drops_empty_regions() {
        assert!(
            serde_json::from_str::<InputMessage>(
                r#"{"regions":[],"modal":false,"wantsKeyboard":false,"x":1}"#
            )
            .is_err()
        );
        let input = parse(
            r#"{"regions":[{"x":0,"y":0,"width":0,"height":5},{"x":0,"y":0,"width":-3,"height":5}],"modal":false,"wantsKeyboard":false}"#,
        );
        assert!(input.regions.is_empty());
    }

    // spec: HOST-OVL-6
    #[test]
    fn starts_exclusive_and_notifies_only_on_change() {
        let state = OverlayState::default();
        assert_eq!(state.router().input(), &OverlayInput::exclusive());
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        state.on_change(move |_| {
            seen.fetch_add(1, Ordering::Relaxed);
        });
        state.set(OverlayInput::default());
        state.set(OverlayInput::default());
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(state.router().keyboard(), Target::Game);
    }
}

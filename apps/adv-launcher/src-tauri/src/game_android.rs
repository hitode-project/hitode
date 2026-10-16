//! Android game ownership on the activity's UI thread (AND-APP-13).
//!
//! Tauri commands run elsewhere and tao's event loop has its own thread, so
//! commands reach the UI thread through Wry's JNI pipe. The JNI callbacks in
//! `android.rs` already run there.
use crate::android;
use crate::library::Library;
pub use crate::play::Playback;
use adv_shell::{Session, window::GameView, window::TouchAction};
use ndk_sys::ANativeWindow;
use std::{cell::RefCell, ptr::NonNull, rc::Rc, sync::Mutex, time::Instant};
use tauri::{Manager, WebviewWindow};

thread_local! {
    static HOST: RefCell<Host> = RefCell::new(Host::default());
}

#[derive(Default)]
struct Host {
    surface: Option<Surface>,
    view: Option<Rc<GameView>>,
    session: Option<Session>,
    playback: Option<Playback>,
    /// Shared lifecycle, audio focus and overlay pause gates.
    lifecycle: adv_shell::mobile::Lifecycle,
    /// The game was asked to close; another request forces it.
    closing: bool,
    touch_mode: adv_shell::mobile::TouchMode,
    /// The surface's placement and the screen's occlusion, with the
    /// view's pixels per point (AND-APP-16).
    screen: Option<(adv_shell::mobile::Layout, f64)>,
}

/// The SurfaceView's window, with a reference of our own.
struct Surface {
    window: NonNull<ANativeWindow>,
    size: (u32, u32),
}
impl Surface {
    fn new(window: NonNull<ANativeWindow>, size: (u32, u32)) -> Self {
        // SAFETY: the caller borrows a live window for this call.
        unsafe { ndk_sys::ANativeWindow_acquire(window.as_ptr()) };
        Self { window, size }
    }
}
impl Drop for Surface {
    fn drop(&mut self) {
        // SAFETY: balanced with the acquire in `new`.
        unsafe { ndk_sys::ANativeWindow_release(self.window.as_ptr()) };
    }
}

impl Host {
    /// AND-APP-3: the game runs only while resumed, with audio focus and no overlay pause.
    fn apply_pause(&self) {
        if let Some(session) = &self.session {
            session.set_paused(self.lifecycle.paused());
        }
    }

    fn attach(&self) {
        if let Some(view) = &self.view {
            let (window, (width, height)) = match &self.surface {
                Some(surface) => (Some(surface.window), surface.size),
                None => (None, (0, 0)),
            };
            // SAFETY: `surface` keeps its reference until the view is detached
            // from it by the next `attach`; the view also acquires its own.
            unsafe { view.set_window(window, width, height) };
        }
    }
}

pub(crate) fn surface(window: Option<NonNull<ANativeWindow>>, width: u32, height: u32) {
    HOST.with_borrow_mut(|host| {
        let previous = host.surface.take();
        host.surface = window.map(|window| Surface::new(window, (width, height)));
        host.attach();
        drop(previous);
    });
}

/// Places the game surface clear of the screen's cutouts and returns its
/// area; `width`, `height` and `occlusion` are in points.
pub(crate) fn screen(
    width: f64,
    height: f64,
    occlusion: adv_shell::mobile::Occlusion,
    scale: f64,
) -> adv_shell::mobile::Rect {
    let game = occlusion.content(width, height);
    let layout = adv_shell::mobile::Layout {
        width,
        height,
        game,
        occlusion,
    };
    HOST.with_borrow_mut(|host| {
        if let Some(view) = &host.view {
            view.set_screen(layout.clone(), scale);
        }
        host.screen = Some((layout, scale));
    });
    game
}

pub(crate) fn touch(pointer: i32, action: i32, x: f32, y: f32) {
    let action = match action {
        0 => TouchAction::Down,
        1 => TouchAction::Up,
        2 => TouchAction::Move,
        3 => TouchAction::Cancel,
        _ => return,
    };
    HOST.with_borrow(|host| {
        if host.session.is_some()
            && let Some(view) = &host.view
        {
            view.touch(pointer, action, x, y);
        }
    });
}

pub(crate) fn lifecycle(resumed: bool) {
    HOST.with_borrow_mut(|host| {
        host.lifecycle.set_resumed(resumed);
        if let Some(session) = &host.session {
            session.set_active(resumed);
        }
        host.apply_pause();
    });
}

pub(crate) fn audio_focus(active: bool) {
    HOST.with_borrow_mut(|host| {
        host.lifecycle.set_audio_focus(active);
        host.apply_pause();
    });
}

/// Runs `f` on the UI thread and waits for its answer.
async fn on_ui<R: Send + 'static>(
    window: &WebviewWindow,
    f: impl FnOnce() -> Result<R, String> + Send + 'static,
) -> Result<R, String> {
    let (reply, result) = std::sync::mpsc::sync_channel(1);
    window
        .with_webview(move |webview| {
            webview.jni_handle().exec(move |_, _, _| {
                let _ = reply.send(f());
            });
        })
        .map_err(|error| error.to_string())?;
    tauri::async_runtime::spawn_blocking(move || result.recv().map_err(|e| e.to_string())?)
        .await
        .map_err(|error| error.to_string())?
}

fn with_session<R>(f: impl FnOnce(&mut Host, &Session) -> R) -> Result<R, String> {
    HOST.with_borrow_mut(|host| {
        let session = host.session.take().ok_or("no game is running")?;
        let result = f(host, &session);
        host.session = Some(session);
        Ok(result)
    })
}

#[tauri::command]
pub async fn start_game(
    window: WebviewWindow,
    id: String,
    filter: String,
    playback: Playback,
) -> Result<(), String> {
    let filter = crate::play::upscaler(&filter)?;
    let (exe, data_dir, codepage) = window
        .state::<Mutex<Library>>()
        .lock()
        .unwrap()
        .launch(&id)
        .map_err(|e| e.to_string())?;
    let options = playback.options(exe, data_dir, codepage)?;
    let host_window = window.clone();
    on_ui(&window, move || {
        HOST.with_borrow_mut(|host| {
            if host.session.is_some() {
                return Err("a game is already running".into());
            }
            let size = host
                .surface
                .as_ref()
                .ok_or("the game surface is not ready")?
                .size;
            let view = GameView::new(size.0, size.1)?;
            view.set_listener(crate::mobile_view::listener(
                host_window.clone(),
                android::set_cursor,
            ));
            view.set_touch_mode(host.touch_mode);
            if let Some((screen, scale)) = &host.screen {
                view.set_screen(screen.clone(), *scale);
            }
            host.view = Some(view.clone());
            host.attach();
            let finished_window = host_window.clone();
            let started_at = Instant::now();
            let session = Session::new(
                view,
                options,
                Box::new(move |result| {
                    // Dropped after the borrow: teardown must not run inside it.
                    let ended = HOST.with_borrow_mut(|host| {
                        host.playback.take();
                        (host.session.take(), host.view.take())
                    });
                    drop(ended);
                    if let Err(error) = android::set_game_mode(finished_window.app_handle(), false)
                    {
                        log::error!("leaving game mode: {error}");
                    }
                    crate::play::finished(&finished_window, id, result, started_at.elapsed());
                }),
            );
            let session = match session {
                Ok(session) => session,
                Err(error) => {
                    host.view.take();
                    return Err(error);
                }
            };
            session.set_active(host.lifecycle.foreground());
            session.set_upscaler(filter);
            playback.apply(&session)?;
            host.session = Some(session);
            host.playback = Some(playback);
            host.lifecycle.set_overlay_pause(false);
            host.closing = false;
            host.apply_pause();
            Ok(())
        })?;
        // Requesting audio focus calls back into HOST, so it follows the borrow.
        android::set_game_mode(host_window.app_handle(), true).map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub async fn stop_game(window: WebviewWindow) -> Result<(), String> {
    on_ui(&window, || {
        with_session(|host, session| {
            if std::mem::replace(&mut host.closing, true) {
                session.force_close();
            } else {
                session.request_close();
            }
        })
    })
    .await
}

/// The game declined to quit, or the player changed their mind: the next
/// stop asks again instead of forcing.
#[tauri::command]
pub async fn keep_game(window: WebviewWindow) -> Result<(), String> {
    on_ui(&window, || {
        HOST.with_borrow_mut(|host| host.closing = false);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn post_text(window: WebviewWindow, text: String) -> Result<usize, String> {
    on_ui(&window, move || {
        with_session(|_, session| session.post_text(&text))
    })
    .await
}

/// Pause or resume the running game while the overlay's pause menu or report is open.
#[tauri::command]
pub async fn pause_game(window: WebviewWindow, paused: bool) -> Result<(), String> {
    on_ui(&window, move || {
        with_session(|host, _| {
            host.lifecycle.set_overlay_pause(paused);
        })?;
        HOST.with_borrow(Host::apply_pause);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn set_game_filter(window: WebviewWindow, filter: String) -> Result<(), String> {
    let filter = crate::play::upscaler(&filter)?;
    on_ui(&window, move || {
        with_session(|_, session| session.set_upscaler(filter))
    })
    .await
}

#[tauri::command]
pub async fn set_game_playback(window: WebviewWindow, playback: Playback) -> Result<(), String> {
    on_ui(&window, move || {
        with_session(|host, session| {
            playback.apply(session)?;
            host.playback = Some(playback);
            Ok(())
        })?
    })
    .await
}

#[tauri::command]
pub async fn game_fps(window: WebviewWindow) -> Result<f64, String> {
    on_ui(&window, || {
        Ok(with_session(|_, session| session.fps()).unwrap_or(0.0))
    })
    .await
}

/// Whether the game has shown its first frame; the overlay waits for it.
#[tauri::command]
pub async fn game_presented(window: WebviewWindow) -> Result<bool, String> {
    on_ui(&window, || {
        Ok(with_session(|_, session| session.presented()).unwrap_or(false))
    })
    .await
}

/// The performance overlay's frame graph: milliseconds between the presents
/// of the last second.
#[tauri::command]
pub async fn game_frames(window: WebviewWindow) -> Result<Vec<f32>, String> {
    on_ui(&window, || {
        Ok(with_session(|_, session| session.frame_intervals()).unwrap_or_default())
    })
    .await
}

/// Direct touch or the trackpad cursor, kept for later games (AND-APP-14).
#[tauri::command]
pub async fn set_touch_mode(window: WebviewWindow, mode: String) -> Result<(), String> {
    let mode = crate::mobile_view::touch_mode(&mode)?;
    on_ui(&window, move || {
        HOST.with_borrow_mut(|host| {
            host.touch_mode = mode;
            if let Some(view) = &host.view {
                view.set_touch_mode(mode);
            }
        });
        Ok(())
    })
    .await
}

/// The overlay's mouse buttons, pressed at the touch or trackpad cursor.
#[tauri::command]
pub async fn game_mouse_button(
    window: WebviewWindow,
    right: bool,
    down: bool,
) -> Result<(), String> {
    on_ui(&window, move || {
        HOST.with_borrow(|host| {
            let view = host.view.as_ref().ok_or("no game is running")?;
            view.mouse_button(right, down);
            Ok(())
        })
    })
    .await
}

/// The overlay's keys: Enter, Esc, Space, Tab and Ctrl, as virtual keys.
#[tauri::command]
pub async fn game_key(window: WebviewWindow, key: u32, down: bool) -> Result<(), String> {
    on_ui(&window, move || {
        HOST.with_borrow(|host| {
            let view = host.view.as_ref().ok_or("no game is running")?;
            view.overlay_key(key, down);
            Ok(())
        })
    })
    .await
}

/// The overlay's wheel keys, in whole notches.
#[tauri::command]
pub async fn game_wheel(window: WebviewWindow, notches: i32) -> Result<(), String> {
    on_ui(&window, move || {
        HOST.with_borrow(|host| {
            let view = host.view.as_ref().ok_or("no game is running")?;
            view.wheel(notches);
            Ok(())
        })
    })
    .await
}

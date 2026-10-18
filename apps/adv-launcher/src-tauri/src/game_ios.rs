//! iOS game ownership on the main thread.
//!
//! Tauri commands run elsewhere, so they reach the main thread through
//! `run_on_main_thread`. The game view sits below the webview in adv-shell's
//! `GameContainer`, which routes each touch with the overlay's claim.
use crate::library::Library;
use crate::overlay::OverlayState;
pub use crate::play::Playback;
use adv_shell::{
    Session,
    window::{GameContainer, GameView},
};
use block2::RcBlock;
use objc2::{
    MainThreadMarker, class, msg_send,
    rc::{Allocated, Retained},
    runtime::{AnyObject, ProtocolObject},
};
use objc2_avf_audio::{
    AVAudioSession, AVAudioSessionCategoryPlayback, AVAudioSessionInterruptionNotification,
    AVAudioSessionInterruptionType, AVAudioSessionInterruptionTypeKey,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol, NSOperationQueue};
use objc2_ui_kit::{
    UIApplication, UIApplicationDidBecomeActiveNotification, UIApplicationState,
    UIApplicationWillResignActiveNotification, UIView,
};
use std::{cell::RefCell, ptr::NonNull, rc::Rc, sync::Mutex, time::Instant};
use tauri::{AppHandle, Manager, WebviewWindow};

thread_local! {
    static HOST: RefCell<Host> = RefCell::new(Host::default());
}

#[derive(Default)]
struct Host {
    container: Option<Retained<GameContainer>>,
    view: Option<Rc<GameView>>,
    session: Option<Session>,
    playback: Option<Playback>,
    /// Shared lifecycle, audio focus and overlay pause gates.
    lifecycle: adv_shell::mobile::Lifecycle,
    /// The game was asked to close; another request forces it.
    closing: bool,
    observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
    /// The latest overlay claim applied to the webview's visibility.
    overlay_change: u64,
    touch_mode: adv_shell::mobile::TouchMode,
}

impl Host {
    /// The game runs only while active, with audio focus and no overlay pause.
    fn apply_pause(&self) {
        if let Some(session) = &self.session {
            session.set_paused(self.lifecycle.paused());
        }
    }
}

/// Prepares the JIT code pool before anything else starts (IOS-JIT-1). A
/// debugger that launched the app is detached at once, since everything
/// runs several times slower while one is attached; one that attaches
/// later is served from a background thread.
pub fn prepare_jit() {
    fn report(result: Result<(), String>, started: Instant) {
        match result {
            Ok(()) => log::info!("JIT code pool ready in {:?}", started.elapsed()),
            Err(error) => log::warn!("JIT unavailable, guests run Tier 0: {error}"),
        }
    }
    let started = Instant::now();
    match adv_shell::prepare_code_pool_if_debugged() {
        Some(result) => report(result, started),
        None => {
            std::thread::spawn(move || {
                let timeout = std::time::Duration::from_secs(10);
                report(adv_shell::prepare_code_pool_after_attach(timeout), started);
            });
        }
    }
}

/// Moves the webview into the game container and follows app activation.
pub fn setup(app: &tauri::App) -> tauri::Result<()> {
    let window = app.get_webview_window("main").unwrap();
    let handle = app.handle().clone();
    window.with_webview(move |native| {
        // with_webview supplies the retained WKWebView on the main thread.
        let webview = unsafe { &*native.inner().cast::<UIView>() };
        let router = handle.state::<OverlayState>().router.clone();
        let container = match GameContainer::new(webview, router) {
            Ok(container) => container,
            Err(error) => {
                log::error!("game container: {error}");
                return;
            }
        };
        let center = NSNotificationCenter::defaultCenter();
        let mut observers = Vec::new();
        for (name, active) in unsafe {
            [
                (UIApplicationDidBecomeActiveNotification, true),
                (UIApplicationWillResignActiveNotification, false),
            ]
        } {
            let block = RcBlock::new(move |_: NonNull<NSNotification>| lifecycle(active));
            observers.push(unsafe {
                center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block)
            });
        }
        // Audio interruptions are posted off the main thread.
        if let Some(name) = unsafe { AVAudioSessionInterruptionNotification } {
            let block = RcBlock::new(|notification: NonNull<NSNotification>| {
                audio_interruption(unsafe { notification.as_ref() })
            });
            let queue = NSOperationQueue::mainQueue();
            observers.push(unsafe {
                center.addObserverForName_object_queue_usingBlock(
                    Some(name),
                    None,
                    Some(&queue),
                    &block,
                )
            });
        }
        HOST.with_borrow_mut(|host| {
            host.container = Some(container);
            host.observers = observers;
        });
        // Games sound like other media, whatever the silent switch says.
        if let Some(playback) = unsafe { AVAudioSessionCategoryPlayback } {
            let session = unsafe { AVAudioSession::sharedInstance() };
            if let Err(error) = unsafe { session.setCategory_error(playback) } {
                log::warn!("audio category: {}", error.localizedDescription());
            }
        }
        // Activation notifications before this point were missed.
        let mtm = MainThreadMarker::new().unwrap();
        let active =
            UIApplication::sharedApplication(mtm).applicationState() == UIApplicationState::Active;
        lifecycle(active);
    })?;
    let handle = app.handle().clone();
    app.state::<OverlayState>().on_change(move |input, change| {
        let visible = input.visible;
        let _ = handle.run_on_main_thread(move || {
            HOST.with_borrow_mut(|host| {
                if change > host.overlay_change {
                    host.overlay_change = change;
                    if let Some(container) = &host.container {
                        container.set_overlay_visible(visible);
                    }
                }
            })
        });
    });
    Ok(())
}

fn lifecycle(active: bool) {
    if active {
        claim_audio();
    }
    HOST.with_borrow_mut(|host| {
        host.lifecycle.set_resumed(active);
        if let Some(session) = &host.session {
            session.set_active(active);
        }
        host.apply_pause();
    });
}

/// Activates the audio session; audio focus follows whether it is ours. Its
/// activation also ends an interruption the system never reported ending,
/// such as another app's playback.
fn claim_audio() {
    let session = unsafe { AVAudioSession::sharedInstance() };
    let focused = match unsafe { session.setActive_error(true) } {
        Ok(()) => true,
        Err(error) => {
            log::warn!("audio session: {}", error.localizedDescription());
            false
        }
    };
    HOST.with_borrow_mut(|host| {
        host.lifecycle.set_audio_focus(focused);
        host.apply_pause();
    });
}

fn audio_interruption(notification: &NSNotification) {
    let began = notification
        .userInfo()
        .and_then(|info| unsafe { info.objectForKey(AVAudioSessionInterruptionTypeKey?) })
        .is_some_and(|kind| {
            let kind: usize = unsafe { msg_send![&*kind, unsignedIntegerValue] };
            kind == AVAudioSessionInterruptionType::Began.0
        });
    if began {
        HOST.with_borrow_mut(|host| {
            host.lifecycle.set_audio_focus(false);
            host.apply_pause();
        });
    } else {
        claim_audio();
    }
}

/// Keeps the screen awake and holds landscape while a game runs.
fn set_game_mode(active: bool) {
    if let Some(mtm) = MainThreadMarker::new() {
        UIApplication::sharedApplication(mtm).setIdleTimerDisabled(active);
    }
    HOST.with_borrow(|host| {
        if let Some(container) = &host.container {
            hold_landscape(container, active);
        }
    });
}

/// Limits tao's root view controller to landscape, or to all orientations
/// but upside down, and asks the scene to rotate now: iOS 16 and later only
/// rotate when asked. A game also hides the status bar and defers edge gestures.
fn hold_landscape(view: &UIView, landscape: bool) {
    // UIInterfaceOrientationMaskLandscape, or AllButUpsideDown.
    let mask: usize = if landscape { 24 } else { 26 };
    // SAFETY: main-thread UIKit calls; the window, its root controller (tao's,
    // which declares the orientation property) and its scene are live.
    unsafe {
        let window: *mut AnyObject = msg_send![view, window];
        let Some(window) = window.as_ref() else {
            return;
        };
        let controller: *mut AnyObject = msg_send![window, rootViewController];
        let Some(controller) = controller.as_ref() else {
            return;
        };
        let _: () = msg_send![controller, setSupportedInterfaceOrientations: mask];
        let _: () = msg_send![controller, setNeedsUpdateOfSupportedInterfaceOrientations];
        // tao's setters request the appearance updates themselves.
        let _: () = msg_send![controller, setPrefersStatusBarHidden: landscape];
        // Auto-hiding makes every touch reveal the indicator and suppresses
        // edge deferral. Keep it dim during play, with a second swipe needed
        // to activate system gestures, as in Moonlight's touch controls.
        let _: () = msg_send![controller, setPrefersHomeIndicatorAutoHidden: false];
        let edges: usize = if landscape { 15 } else { 0 };
        let _: () = msg_send![controller, setPreferredScreenEdgesDeferringSystemGestures: edges];
        let scene: *mut AnyObject = msg_send![window, windowScene];
        let Some(scene) = scene.as_ref() else {
            return;
        };
        let preferences: Allocated<AnyObject> =
            msg_send![class!(UIWindowSceneGeometryPreferencesIOS), alloc];
        let preferences: Retained<AnyObject> =
            msg_send![preferences, initWithInterfaceOrientations: mask];
        let _: () = msg_send![
            scene,
            requestGeometryUpdateWithPreferences: &*preferences,
            errorHandler: std::ptr::null::<block2::Block<dyn Fn(NonNull<AnyObject>)>>()
        ];
    }
}

/// Runs `f` on the main thread and waits for its answer.
async fn on_ui<R: Send + 'static>(
    app: &AppHandle,
    f: impl FnOnce() -> Result<R, String> + Send + 'static,
) -> Result<R, String> {
    let (reply, result) = std::sync::mpsc::sync_channel(1);
    app.run_on_main_thread(move || {
        let _ = reply.send(f());
    })
    .map_err(|error| error.to_string())?;
    tauri::async_runtime::spawn_blocking(move || result.recv().map_err(|e| e.to_string())?)
        .await
        .map_err(|error| error.to_string())?
}

/// Save operations wait for the game to exit; read on the UI thread.
pub(crate) fn is_running() -> bool {
    HOST.with_borrow(|host| host.session.is_some())
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
    let backup_data = data_dir.clone();
    let options = playback.options(exe, data_dir, codepage)?;
    let host_window = window.clone();
    on_ui(window.app_handle(), move || {
        HOST.with_borrow_mut(|host| {
            if crate::saves::busy() {
                return Err("存档操作正在进行，请稍后启动".into());
            }
            if host.session.is_some() {
                return Err("a game is already running".into());
            }
            let container = host.container.clone().ok_or("the game view is not ready")?;
            let bounds = container.bounds().size;
            let scale = container.contentScaleFactor();
            let view = GameView::new(
                (bounds.width * scale) as u32,
                (bounds.height * scale) as u32,
            )?;
            view.set_listener(crate::mobile_view::listener(host_window.clone(), |_| {}));
            view.set_touch_mode(host.touch_mode);
            container.set_game(Some(&view));
            host.view = Some(view.clone());
            let finished_window = host_window.clone();
            let started_at = Instant::now();
            let session = Session::new(
                view,
                options,
                Box::new(move |result| {
                    // Dropped after the borrow: teardown must not run inside it.
                    let ended = HOST.with_borrow_mut(|host| {
                        host.playback.take();
                        if let Some(container) = &host.container {
                            container.set_game(None);
                        }
                        (host.session.take(), host.view.take())
                    });
                    drop(ended);
                    set_game_mode(false);
                    crate::saves::after_game(&finished_window, &backup_data);
                    crate::play::finished(&finished_window, id, result, started_at.elapsed());
                }),
            );
            let session = match session {
                Ok(session) => session,
                Err(error) => {
                    container.set_game(None);
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
        set_game_mode(true);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn stop_game(window: WebviewWindow) -> Result<(), String> {
    on_ui(window.app_handle(), || {
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
    on_ui(window.app_handle(), || {
        HOST.with_borrow_mut(|host| host.closing = false);
        Ok(())
    })
    .await
}

/// Answers a guest menu the overlay listed from `game_menu`.
#[tauri::command]
pub async fn game_menu(
    window: WebviewWindow,
    input: crate::mobile_view::MenuInput,
) -> Result<(), String> {
    on_ui(window.app_handle(), move || {
        with_session(|_, session| {
            for event in crate::mobile_view::menu_events(input) {
                session.menu_event(event);
            }
        })
    })
    .await
}

/// Answers a guest dialog the overlay drew from `game_dialogs`.
#[tauri::command]
pub async fn game_dialog(
    window: WebviewWindow,
    input: crate::mobile_view::DialogInput,
) -> Result<(), String> {
    on_ui(window.app_handle(), move || {
        with_session(|_, session| {
            for event in crate::mobile_view::dialog_events(input) {
                session.dialog_event(event);
            }
        })
    })
    .await
}

#[tauri::command]
pub async fn post_text(window: WebviewWindow, text: String) -> Result<usize, String> {
    on_ui(window.app_handle(), move || {
        with_session(|_, session| session.post_text(&text))
    })
    .await
}

/// Pause or resume the running game while the overlay's pause menu or report is open.
#[tauri::command]
pub async fn pause_game(window: WebviewWindow, paused: bool) -> Result<(), String> {
    on_ui(window.app_handle(), move || {
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
    on_ui(window.app_handle(), move || {
        with_session(|_, session| session.set_upscaler(filter))
    })
    .await
}

#[tauri::command]
pub async fn set_game_playback(window: WebviewWindow, playback: Playback) -> Result<(), String> {
    on_ui(window.app_handle(), move || {
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
    on_ui(window.app_handle(), || {
        Ok(with_session(|_, session| session.fps()).unwrap_or(0.0))
    })
    .await
}

/// Whether the game has shown its first frame; the overlay waits for it.
#[tauri::command]
pub async fn game_presented(window: WebviewWindow) -> Result<bool, String> {
    on_ui(window.app_handle(), || {
        Ok(with_session(|_, session| session.presented()).unwrap_or(false))
    })
    .await
}

/// The performance overlay's frame graph: milliseconds between the presents
/// of the last second.
#[tauri::command]
pub async fn game_frames(window: WebviewWindow) -> Result<Vec<f32>, String> {
    on_ui(window.app_handle(), || {
        Ok(with_session(|_, session| session.frame_intervals()).unwrap_or_default())
    })
    .await
}

/// Direct touch or the trackpad cursor, kept for later games (AND-APP-14).
#[tauri::command]
pub async fn set_touch_mode(window: WebviewWindow, mode: String) -> Result<(), String> {
    let mode = crate::mobile_view::touch_mode(&mode)?;
    on_ui(window.app_handle(), move || {
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
    on_ui(window.app_handle(), move || {
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
    on_ui(window.app_handle(), move || {
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
    on_ui(window.app_handle(), move || {
        HOST.with_borrow(|host| {
            let view = host.view.as_ref().ok_or("no game is running")?;
            view.wheel(notches);
            Ok(())
        })
    })
    .await
}

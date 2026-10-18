//! macOS game ownership on Tauri's main AppKit thread.
use crate::library::Library;
use crate::overlay::OverlayState;
pub use crate::play::Playback;
use crate::play::upscaler;
use adv_shell::overlay::Target;
use adv_shell::{
    Session,
    window::{GameContainer, GameView},
};
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, NSObject, ProtocolObject},
    sel,
};
use objc2_app_kit::{
    NSApplication, NSApplicationDidBecomeActiveNotification,
    NSApplicationDidResignActiveNotification, NSView, NSWindow,
    NSWindowDidChangeOcclusionStateNotification, NSWindowDidDeminiaturizeNotification,
    NSWindowDidMiniaturizeNotification, NSWindowOcclusionState, NSWorkspace,
    NSWorkspaceDidWakeNotification, NSWorkspaceWillPowerOffNotification,
    NSWorkspaceWillSleepNotification,
};
use objc2_foundation::{
    NSAppleEventManager, NSNotificationCenter, NSObjectProtocol, NSOperationQueue,
};
use std::{cell::RefCell, sync::Mutex, time::Instant};
use tauri::{Emitter, Manager, WebviewWindow};

/// `kCoreEventClass` and `kAEQuitApplication`.
const CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt");
const QUIT_APPLICATION: u32 = u32::from_be_bytes(*b"quit");

define_class!(
    // tao's application delegate has no applicationShouldTerminate:, so the
    // default quit event (Dock Quit, logout, restart, shut down) would end the
    // process without asking the game. This handler routes it through
    // AppHandle::exit, which run_event can defer until the game has closed.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = tauri::AppHandle]
    struct QuitHandler;
    unsafe impl NSObjectProtocol for QuitHandler {}
    impl QuitHandler {
        #[unsafe(method(handleQuit:withReplyEvent:))]
        fn handle_quit(&self, _event: &AnyObject, _reply: &AnyObject) {
            self.ivars().exit(0);
        }
    }
);

thread_local! {
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
}
struct Host {
    container: Retained<GameContainer>,
    session: Option<Session>,
    playback: Option<Playback>,
    paused: bool,
    // Pending game close, with an exit code only when the application should quit.
    exit: Option<Option<i32>>,
    quit: Retained<QuitHandler>,
    /// The last overlay change applied to the webview's visibility.
    overlay_change: u64,
    observers: Vec<Observer>,
}
/// A block observer and the notification center it is registered with.
type Observer = (
    Retained<NSNotificationCenter>,
    Retained<ProtocolObject<dyn NSObjectProtocol>>,
);
impl Host {
    fn focus(&self, target: Target) {
        let views = self.container.subviews();
        let view = if target == Target::Game && self.session.is_some() {
            views.firstObject().unwrap()
        } else {
            views.lastObject().unwrap()
        };
        self.container
            .window()
            .unwrap()
            .makeFirstResponder(Some(&view));
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        for (center, observer) in &self.observers {
            unsafe { center.removeObserver((**observer).as_ref()) };
        }
    }
}

pub fn setup(app: &tauri::App) -> tauri::Result<()> {
    let window = app.get_webview_window("main").unwrap();
    let handle = app.handle().clone();
    window.with_webview(move |native| {
        // with_webview supplies the retained WKWebView on the main thread.
        let webview = unsafe { &*native.inner().cast::<NSView>() };
        let container =
            GameContainer::new(webview, handle.state::<OverlayState>().router.clone()).unwrap();
        let mtm = MainThreadMarker::new().unwrap();
        let native_window = container.window().unwrap();
        native_window.setAcceptsMouseMovedEvents(true);
        let center = NSNotificationCenter::defaultCenter();
        let mut observers = Vec::new();
        for name in unsafe {
            [
                NSWindowDidMiniaturizeNotification,
                NSWindowDidDeminiaturizeNotification,
                NSWindowDidChangeOcclusionStateNotification,
            ]
        } {
            let block = block2::RcBlock::new(
                move |note: std::ptr::NonNull<objc2_foundation::NSNotification>| {
                    MainThreadMarker::new().unwrap();
                    let object = unsafe { note.as_ref().object() }.unwrap();
                    let window = object.downcast_ref::<NSWindow>().unwrap();
                    HOST.with_borrow(|host| {
                        if let Some(session) = host.as_ref().and_then(|host| host.session.as_ref())
                        {
                            if name == unsafe { NSWindowDidChangeOcclusionStateNotification } {
                                session.set_occluded(
                                    !window
                                        .occlusionState()
                                        .contains(NSWindowOcclusionState::Visible),
                                );
                            } else {
                                session.set_minimized(window.isMiniaturized());
                            }
                        }
                    });
                },
            );
            observers.push((center.clone(), unsafe {
                center.addObserverForName_object_queue_usingBlock(
                    Some(name),
                    Some(&native_window),
                    Some(&NSOperationQueue::mainQueue()),
                    &block,
                )
            }));
        }
        for name in unsafe {
            [
                NSApplicationDidBecomeActiveNotification,
                NSApplicationDidResignActiveNotification,
            ]
        } {
            // Activation follows the application, as in the CLI shell: a guest
            // dialog taking key status from the game window keeps it active.
            let block = block2::RcBlock::new(
                move |_: std::ptr::NonNull<objc2_foundation::NSNotification>| {
                    HOST.with_borrow(|host| {
                        if let Some(session) = host.as_ref().and_then(|host| host.session.as_ref())
                        {
                            session.set_active(
                                name == unsafe { NSApplicationDidBecomeActiveNotification },
                            );
                            host.as_ref().unwrap().apply_pause();
                        }
                    });
                },
            );
            observers.push((center.clone(), unsafe {
                center.addObserverForName_object_queue_usingBlock(
                    Some(name),
                    None,
                    Some(&NSOperationQueue::mainQueue()),
                    &block,
                )
            }));
        }
        let workspace = NSWorkspace::sharedWorkspace().notificationCenter();
        for name in unsafe {
            [
                NSWorkspaceWillSleepNotification,
                NSWorkspaceDidWakeNotification,
                NSWorkspaceWillPowerOffNotification,
            ]
        } {
            let block = block2::RcBlock::new(
                move |_: std::ptr::NonNull<objc2_foundation::NSNotification>| {
                    HOST.with_borrow(|host| {
                        if let Some(session) = host.as_ref().and_then(|host| host.session.as_ref())
                        {
                            if name == unsafe { NSWorkspaceWillPowerOffNotification } {
                                // Workspace does not distinguish logout, restart and power-off.
                                session.power_off();
                            } else {
                                session.sleep(name == unsafe { NSWorkspaceWillSleepNotification });
                            }
                        }
                    });
                },
            );
            observers.push((workspace.clone(), unsafe {
                workspace.addObserverForName_object_queue_usingBlock(
                    Some(name),
                    None,
                    Some(&NSOperationQueue::mainQueue()),
                    &block,
                )
            }));
        }
        let quit = QuitHandler::alloc(mtm).set_ivars(handle.clone());
        let quit: Retained<QuitHandler> = unsafe { msg_send![super(quit), init] };
        HOST.with_borrow_mut(|host| {
            *host = Some(Host {
                container,
                session: None,
                playback: None,
                paused: false,
                exit: None,
                quit,
                observers,
                overlay_change: 0,
            })
        });
    })?;
    let handle = app.handle().clone();
    app.state::<OverlayState>().on_change(move |input, change| {
        let target = input.keyboard();
        let visible = input.visible;
        let _ = handle.run_on_main_thread(move || {
            HOST.with_borrow_mut(|host| {
                if let Some(host) = host {
                    host.focus(target);
                    // Changes from before the latest `pause` are stale.
                    if change > host.overlay_change {
                        host.overlay_change = change;
                        host.container.set_overlay_visible(visible);
                    }
                }
            })
        });
    });
    Ok(())
}

/// Emits `pause` after showing the overlay: a hidden WKWebView runs no
/// animation frames, so it could never publish the menu that shows it.
pub(crate) fn pause<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        HOST.with_borrow_mut(|host| {
            if let Some(host) = host {
                host.overlay_change = handle.state::<OverlayState>().changes();
                host.container.set_overlay_visible(true);
            }
        });
        let _ = handle.emit("pause", ());
    });
}

#[tauri::command]
pub async fn start_game(
    window: WebviewWindow,
    id: String,
    filter: String,
    playback: Playback,
) -> Result<(), String> {
    let filter = upscaler(&filter)?;
    let (exe, data_dir, codepage) = window
        .state::<Mutex<Library>>()
        .lock()
        .unwrap()
        .launch(&id)
        .map_err(|e| e.to_string())?;
    let (reply, result) = std::sync::mpsc::sync_channel(1);
    let host_window = window.clone();
    window
        .run_on_main_thread(move || {
            let started = HOST.with_borrow_mut(|host| {
                let host = host.as_mut().unwrap();
                if crate::saves::busy() {
                    return Err("存档操作正在进行，请稍后启动".into());
                }
                if host.session.is_some() {
                    return Err("a game is already running".into());
                }
                let backup_data = data_dir.clone();
                let options = playback.options(exe, data_dir, codepage)?;
                let view = GameView::new(640, 480)?;
                host.container.set_game(Some(&view));
                let finished_window = host_window.clone();
                let started_at = Instant::now();
                let session = Session::new(
                    view,
                    options,
                    Box::new(move |result| {
                        let exit = HOST.with_borrow_mut(|host| {
                            let host = host.as_mut().unwrap();
                            host.session.take();
                            host.container.set_game(None);
                            host.focus(Target::Overlay);
                            host.exit.take().flatten()
                        });
                        let elapsed = started_at.elapsed();
                        crate::saves::after_game(&finished_window, &backup_data);
                        crate::play::finished(&finished_window, id, result, elapsed);
                        if let Some(code) = exit {
                            finished_window.app_handle().exit(code);
                        }
                    }),
                );
                match session {
                    Ok(session) => {
                        let native = host.container.window().unwrap();
                        session.set_active(
                            NSApplication::sharedApplication(MainThreadMarker::new().unwrap())
                                .isActive(),
                        );
                        session.set_minimized(native.isMiniaturized());
                        session.set_occluded(
                            !native
                                .occlusionState()
                                .contains(NSWindowOcclusionState::Visible),
                        );
                        session.set_upscaler(filter);
                        playback.apply(&session)?;
                        host.session = Some(session);
                        host.playback = Some(playback);
                        host.paused = false;
                        host.apply_pause();
                        // AppKit installs its own quit handler while the app
                        // finishes launching; replace it once a game starts.
                        unsafe {
                            let _: () = msg_send![
                                &*NSAppleEventManager::sharedAppleEventManager(),
                                setEventHandler: &*host.quit,
                                andSelector: sel!(handleQuit:withReplyEvent:),
                                forEventClass: CORE_EVENT_CLASS,
                                andEventID: QUIT_APPLICATION
                            ];
                        }
                        host.focus(host_window.state::<OverlayState>().router().keyboard());
                        Ok(())
                    }
                    Err(error) => {
                        host.container.set_game(None);
                        host.focus(Target::Overlay);
                        Err(error)
                    }
                }
            });
            let _ = reply.send(started);
        })
        .map_err(|error| error.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        result.recv().map_err(|error| error.to_string())?
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Asks the running game to close while the launcher stays; asked again while
/// it is closing, the game is forced. Returns false when no game is running.
pub(crate) fn end_game() -> bool {
    HOST.with_borrow_mut(|host| host.as_mut().is_some_and(Host::end_game))
}

#[tauri::command]
pub async fn stop_game(window: WebviewWindow) -> Result<(), String> {
    let (reply, result) = std::sync::mpsc::sync_channel(1);
    window
        .run_on_main_thread(move || {
            let stopped = end_game()
                .then_some(())
                .ok_or("no game is running".to_string());
            let _ = reply.send(stopped);
        })
        .map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || result.recv().map_err(|e| e.to_string())?)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn post_text(window: WebviewWindow, text: String) -> Result<usize, String> {
    let (reply, result) = std::sync::mpsc::sync_channel(1);
    window
        .run_on_main_thread(move || {
            let posted = HOST.with_borrow(|host| {
                host.as_ref()
                    .and_then(|host| host.session.as_ref())
                    .map(|session| session.post_text(&text))
                    .ok_or_else(|| "no game is running".to_string())
            });
            let _ = reply.send(posted);
        })
        .map_err(|error| error.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        result.recv().map_err(|error| error.to_string())?
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Pause or resume the running game while the overlay's pause menu or report
/// page is open.
#[tauri::command]
pub async fn pause_game(window: WebviewWindow, paused: bool) -> Result<(), String> {
    let (reply, result) = std::sync::mpsc::sync_channel(1);
    window
        .run_on_main_thread(move || {
            let done = HOST.with_borrow_mut(|host| {
                let host = host.as_mut().unwrap();
                if host.session.is_none() {
                    return Err("no game is running".to_string());
                }
                host.paused = paused;
                host.apply_pause();
                Ok(())
            });
            let _ = reply.send(done);
        })
        .map_err(|error| error.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        result.recv().map_err(|error| error.to_string())?
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn set_game_filter(window: WebviewWindow, filter: String) -> Result<(), String> {
    let filter = upscaler(&filter)?;
    let (reply, result) = std::sync::mpsc::sync_channel(1);
    window
        .run_on_main_thread(move || {
            let done = HOST.with_borrow(|host| {
                host.as_ref()
                    .and_then(|host| host.session.as_ref())
                    .map(|session| session.set_upscaler(filter))
                    .ok_or_else(|| "no game is running".to_string())
            });
            let _ = reply.send(done);
        })
        .map_err(|error| error.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        result.recv().map_err(|error| error.to_string())?
    })
    .await
    .map_err(|error| error.to_string())?
}

pub fn window_event(window: &tauri::Window, event: &tauri::WindowEvent) {
    if window.label() != "main" {
        return;
    }
    HOST.with_borrow_mut(|host| {
        if let Some(host) = host
            && let Some(session) = &host.session
        {
            match event {
                tauri::WindowEvent::Focused(true) => {
                    host.focus(window.state::<OverlayState>().router().keyboard());
                }
                tauri::WindowEvent::Focused(false) => session.resign_key(),
                // Closing the window while a game runs ends only the game, as
                // Quit does; the library stays open.
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    host.end_game();
                }
                _ => {}
            }
        }
    });
}

pub fn run_event(_: &tauri::AppHandle, event: tauri::RunEvent) {
    if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
        HOST.with_borrow_mut(|host| {
            if let Some(host) = host
                && let Some(session) = &host.session
            {
                api.prevent_exit();
                // A second quit while the game is still closing forces it,
                // as the standalone shell's Quit item does.
                if host.exit.replace(Some(code.unwrap_or(0))).is_some() {
                    session.force_close();
                } else {
                    session.request_close();
                }
            }
        });
    }
}

impl Host {
    /// Asks the running game to close, or forces it when already asked.
    /// Returns false when no game is running.
    fn end_game(&mut self) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        if self.exit.is_some() {
            session.force_close();
        } else {
            self.exit = Some(None);
            session.request_close();
        }
        true
    }

    fn apply_pause(&self) {
        if let Some(session) = &self.session {
            let background =
                !NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).isActive();
            session.set_paused(
                self.paused
                    || (background
                        && self
                            .playback
                            .as_ref()
                            .is_some_and(|p| p.pause_in_background)),
            );
        }
    }
}
#[tauri::command]
pub fn set_game_playback(playback: Playback) -> Result<(), String> {
    HOST.with_borrow_mut(|host| {
        let host = host.as_mut().unwrap();
        let session = host.session.as_ref().ok_or("no game is running")?;
        playback.apply(session)?;
        host.playback = Some(playback);
        host.apply_pause();
        Ok(())
    })
}
#[tauri::command]
pub fn game_fps() -> f64 {
    HOST.with_borrow(|host| {
        host.as_ref()
            .and_then(|host| host.session.as_ref())
            .map_or(0.0, Session::fps)
    })
}

pub(crate) fn is_running() -> bool {
    HOST.with_borrow(|host| host.as_ref().is_some_and(|host| host.session.is_some()))
}

//! macOS game ownership on Tauri's main AppKit thread.
use crate::library::{self, Library};
use crate::overlay::OverlayState;
use adv_shell::overlay::Target;
use adv_shell::{
    Options, Session,
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
use serde::Serialize;
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
    close_window: bool,
    exit: Option<i32>,
    quit: Retained<QuitHandler>,
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
                close_window: false,
                exit: None,
                quit,
                observers,
            })
        });
    })?;
    let handle = app.handle().clone();
    app.state::<OverlayState>().on_change(move |input| {
        let target = input.keyboard();
        let _ = handle.run_on_main_thread(move || {
            HOST.with_borrow(|host| {
                if let Some(host) = host {
                    host.focus(target);
                }
            })
        });
    });
    Ok(())
}

#[derive(Clone, Serialize)]
#[serde(untagged)]
enum Finished {
    Exit {
        #[serde(rename = "gameId")]
        game_id: String,
        #[serde(rename = "exitCode")]
        exit_code: u8,
        /// Failure and frame timing for the anonymous report.
        summary: adv_report::RunSummary,
    },
    Error {
        #[serde(rename = "gameId")]
        game_id: String,
        error: String,
        /// The failure kind and time bucket; never the error text.
        summary: adv_report::RunSummary,
    },
}

#[tauri::command]
pub async fn start_game(window: WebviewWindow, id: String) -> Result<(), String> {
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
                if host.session.is_some() {
                    return Err("a game is already running".into());
                }
                let mut options = Options::new(exe, data_dir);
                options.codepage = Some(codepage);
                options.audio = true;
                let view = GameView::new(640, 480)?;
                host.container.set_game(Some(&view));
                let finished_window = host_window.clone();
                let started_at = Instant::now();
                let session = Session::new(
                    view,
                    options,
                    Box::new(move |result| {
                        let (close, exit) = HOST.with_borrow_mut(|host| {
                            let host = host.as_mut().unwrap();
                            host.session.take();
                            host.container.set_game(None);
                            host.focus(Target::Overlay);
                            (std::mem::take(&mut host.close_window), host.exit.take())
                        });
                        if let Err(error) = finished_window
                            .state::<Mutex<Library>>()
                            .lock()
                            .unwrap()
                            .finished(&id, library::now(), started_at.elapsed().as_secs() as u32)
                        {
                            eprintln!("could not save play session: {error}");
                        }
                        let payload = match result {
                            Ok(finished) => Finished::Exit {
                                game_id: id,
                                exit_code: finished.exit_code,
                                summary: finished.summary,
                            },
                            Err(error) => Finished::Error {
                                game_id: id,
                                summary: adv_shell::error_summary(&error, started_at.elapsed()),
                                error,
                            },
                        };
                        let _ = finished_window.emit("game_finished", payload);
                        if let Some(code) = exit {
                            finished_window.app_handle().exit(code);
                        } else if close {
                            let _ = finished_window.close();
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
                        host.session = Some(session);
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
            let done = HOST.with_borrow(|host| {
                host.as_ref()
                    .and_then(|host| host.session.as_ref())
                    .map(|session| session.set_paused(paused))
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
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    host.close_window = true;
                    session.request_close();
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
                if host.exit.replace(code.unwrap_or(0)).is_some() {
                    session.force_close();
                } else {
                    session.request_close();
                }
            }
        });
    }
}

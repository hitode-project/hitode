//! macOS game ownership on Tauri's main AppKit thread.
use crate::overlay::OverlayState;
use adv_shell::overlay::Target;
use adv_shell::{
    Options, Session,
    window::{GameContainer, GameView},
};
use objc2::{MainThreadMarker, rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{
    NSView, NSWindow, NSWindowDidChangeOcclusionStateNotification,
    NSWindowDidDeminiaturizeNotification, NSWindowDidMiniaturizeNotification,
    NSWindowOcclusionState,
};
use objc2_foundation::{NSNotificationCenter, NSObjectProtocol, NSOperationQueue};
use serde::Serialize;
use std::{cell::RefCell, path::PathBuf};
use tauri::{Emitter, Manager, WebviewWindow};

thread_local! {
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
}
struct Host {
    container: Retained<GameContainer>,
    session: Option<Session>,
    close_window: bool,
    exit: Option<i32>,
    observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
}
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
        for observer in &self.observers {
            unsafe { NSNotificationCenter::defaultCenter().removeObserver((&**observer).as_ref()) };
        }
    }
}

pub fn setup(app: &tauri::App) -> tauri::Result<()> {
    let window = app.get_webview_window("main").unwrap();
    let handle = app.handle().clone();
    window.with_webview(move |native| {
        MainThreadMarker::new().unwrap();
        // with_webview supplies the retained WKWebView on the main thread.
        let webview = unsafe { &*native.inner().cast::<NSView>() };
        let container =
            GameContainer::new(webview, handle.state::<OverlayState>().router.clone()).unwrap();
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
            observers.push(unsafe {
                center.addObserverForName_object_queue_usingBlock(
                    Some(name),
                    Some(&native_window),
                    Some(&NSOperationQueue::mainQueue()),
                    &block,
                )
            });
        }
        HOST.with_borrow_mut(|host| {
            *host = Some(Host {
                container,
                session: None,
                close_window: false,
                exit: None,
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
        #[serde(rename = "exitCode")]
        exit_code: u8,
    },
    Error {
        error: String,
    },
}

#[tauri::command]
pub async fn start_game(
    window: WebviewWindow,
    exe: PathBuf,
    data_dir: PathBuf,
    install_dir: Option<PathBuf>,
    cdroms: Option<Vec<(char, PathBuf)>>,
) -> Result<(), String> {
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
                options.install_dir = install_dir;
                options.cdroms = cdroms.unwrap_or_default();
                options.audio = true;
                let view = GameView::new(640, 480)?;
                host.container.set_game(Some(&view));
                let finished_window = host_window.clone();
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
                        let payload = match result {
                            Ok(exit_code) => Finished::Exit { exit_code },
                            Err(error) => Finished::Error { error },
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
                        session.set_active(native.isKeyWindow());
                        session.set_minimized(native.isMiniaturized());
                        session.set_occluded(
                            !native
                                .occlusionState()
                                .contains(NSWindowOcclusionState::Visible),
                        );
                        host.session = Some(session);
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

pub fn window_event(window: &tauri::Window, event: &tauri::WindowEvent) {
    if window.label() != "main" {
        return;
    }
    HOST.with_borrow_mut(|host| {
        if let Some(host) = host
            && let Some(session) = &host.session
        {
            match event {
                tauri::WindowEvent::Focused(active) => {
                    session.set_active(*active);
                    if *active {
                        host.focus(window.state::<OverlayState>().router().keyboard());
                    }
                }
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
                host.exit = Some(code.unwrap_or(0));
                session.request_close();
            }
        });
    }
}

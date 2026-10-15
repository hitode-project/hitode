//! JNI bridge for MainActivity and GameContainer (AND-APP-12).
//!
//! A1-d connects these synchronous, main-thread forwarding points to adv-shell:
//! - `forward_surface_created(window: NonNull<ANativeWindow>, width: u32, height: u32)`;
//! - `forward_surface_changed(window: NonNull<ANativeWindow>, width: u32, height: u32)`;
//! - `forward_surface_destroyed(window: NonNull<ANativeWindow>, width: u32, height: u32)`;
//! - `forward_touch(pointer: i32, action: i32, x: f32, y: f32)`;
//! - `forward_pause()`, `forward_resume()`, `forward_audio_focus(active: bool)`.
//!
//! Surface handles are borrowed only for the call, then released. A receiver
//! retaining a window must acquire its own reference. Touch actions are Android
//! DOWN (0), UP (1), MOVE (2), CANCEL (3), with coordinates in view pixels.
//! Kotlin captures one Router target for the whole gesture. Before Tauri setup,
//! new gestures belong to the webview; afterwards they use its OverlayState.
//! `set_game_mode(app, active)` calls MainActivity on the webview thread.

use crate::overlay::OverlayState;
use adv_shell::overlay::Target;
use jni::JNIEnv;
use jni::objects::{JObject, JValue};
use jni::sys::{jboolean, jfloat, jint};
use ndk_sys::ANativeWindow;
use std::ptr::NonNull;
use std::sync::OnceLock;
use tauri::{AppHandle, Manager};

static APP: OnceLock<AppHandle> = OnceLock::new();

pub(crate) fn setup(app: AppHandle) {
    let _ = APP.set(app);
}

/// Switches orientation, system bars, screen wakefulness and audio focus.
pub fn set_game_mode(app: &AppHandle, active: bool) -> tauri::Result<()> {
    app.get_webview_window("main")
        .ok_or(tauri::Error::WebviewNotFound)?
        .with_webview(move |webview| {
            webview.jni_handle().exec(move |env, activity, _| {
                if let Err(error) = env.call_method(
                    activity,
                    "setGameMode",
                    "(Z)V",
                    &[JValue::Bool(active.into())],
                ) {
                    log::error!("setGameMode: {error}");
                }
            });
        })
}

fn with_surface(
    env: &JNIEnv<'_>,
    surface: &JObject<'_>,
    width: jint,
    height: jint,
    forward: fn(NonNull<ANativeWindow>, u32, u32),
) {
    // SAFETY: JNI supplies a live Surface and environment for this callback.
    let window = unsafe {
        ndk_sys::ANativeWindow_fromSurface(
            env.get_native_interface().cast(),
            surface.as_raw().cast(),
        )
    };
    if let Some(window) = NonNull::new(window) {
        forward(window, width as u32, height as u32);
        // SAFETY: fromSurface acquired this reference; forward only borrows it.
        unsafe { ndk_sys::ANativeWindow_release(window.as_ptr()) };
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_hitode_launcher_MainActivity_nativeSurfaceCreated(
    env: JNIEnv<'_>,
    _activity: JObject<'_>,
    surface: JObject<'_>,
    width: jint,
    height: jint,
) {
    with_surface(&env, &surface, width, height, forward_surface_created);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_hitode_launcher_MainActivity_nativeSurfaceChanged(
    env: JNIEnv<'_>,
    _activity: JObject<'_>,
    surface: JObject<'_>,
    width: jint,
    height: jint,
) {
    with_surface(&env, &surface, width, height, forward_surface_changed);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_hitode_launcher_MainActivity_nativeSurfaceDestroyed(
    env: JNIEnv<'_>,
    _activity: JObject<'_>,
    surface: JObject<'_>,
    width: jint,
    height: jint,
) {
    with_surface(&env, &surface, width, height, forward_surface_destroyed);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_hitode_launcher_MainActivity_nativeRouteTouch(
    _env: JNIEnv<'_>,
    _activity: JObject<'_>,
    x: jfloat,
    y: jfloat,
) -> jboolean {
    APP.get()
        .is_some_and(|app| {
            app.state::<OverlayState>()
                .router()
                .input()
                .target_at(f64::from(x), f64::from(y))
                == Target::Game
        })
        .into()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_hitode_launcher_MainActivity_nativeTouch(
    _env: JNIEnv<'_>,
    _activity: JObject<'_>,
    pointer: jint,
    action: jint,
    x: jfloat,
    y: jfloat,
) {
    forward_touch(pointer, action, x, y);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_hitode_launcher_MainActivity_nativePause(
    _env: JNIEnv<'_>,
    _activity: JObject<'_>,
) {
    forward_pause();
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_hitode_launcher_MainActivity_nativeResume(
    _env: JNIEnv<'_>,
    _activity: JObject<'_>,
) {
    forward_resume();
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_hitode_launcher_MainActivity_nativeAudioFocus(
    _env: JNIEnv<'_>,
    _activity: JObject<'_>,
    active: jboolean,
) {
    forward_audio_focus(active != 0);
}

fn forward_surface_created(_window: NonNull<ANativeWindow>, width: u32, height: u32) {
    log::debug!("surface created: {width}x{height}");
}

fn forward_surface_changed(_window: NonNull<ANativeWindow>, width: u32, height: u32) {
    log::debug!("surface changed: {width}x{height}");
}

fn forward_surface_destroyed(_window: NonNull<ANativeWindow>, width: u32, height: u32) {
    log::debug!("surface destroyed: {width}x{height}");
}

fn forward_touch(pointer: i32, action: i32, x: f32, y: f32) {
    log::debug!("touch: pointer={pointer} action={action} x={x} y={y}");
}

fn forward_pause() {
    log::debug!("activity paused");
}

fn forward_resume() {
    log::debug!("activity resumed");
}

fn forward_audio_focus(active: bool) {
    log::debug!("audio focus: {active}");
}

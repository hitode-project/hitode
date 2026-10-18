//! JNI bridge for MainActivity and GameContainer (AND-APP-12).
//!
//! The callbacks run on the UI thread and forward to `game` (AND-APP-13).
//! Surface handles are borrowed only for the call, then released. A receiver
//! retaining a window must acquire its own reference. Touch actions are Android
//! DOWN (0), UP (1), MOVE (2), CANCEL (3), with coordinates in view pixels.
//! Kotlin captures one Router target for the whole gesture. Before Tauri setup,
//! new gestures belong to the webview; afterwards they use its OverlayState.
//! `set_game_mode(app, active)` and `reveal_folder` call MainActivity on the
//! webview thread.

use crate::overlay::OverlayState;
use adv_shell::mobile::{Insets, Occlusion, Rect};
use adv_shell::overlay::Target;
use jni::objects::{GlobalRef, JFloatArray, JObject, JValue};
use jni::sys::{jboolean, jfloat, jfloatArray, jint};
use jni::{JNIEnv, JavaVM};
use ndk_sys::ANativeWindow;
use std::ptr::NonNull;
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Manager};

static APP: OnceLock<AppHandle> = OnceLock::new();
/// MainActivity, for the trackpad cursor; set by `nativeScreen`.
static ACTIVITY: Mutex<Option<(JavaVM, GlobalRef)>> = Mutex::new(None);

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

/// Opens `folder`, below the primary storage root, in the system Files app.
pub fn reveal_folder(app: &AppHandle, folder: &std::path::Path) -> Result<(), String> {
    let relative = folder
        .strip_prefix("/storage/emulated/0")
        .map_err(|_| "only folders in shared storage can be shown")?
        .to_str()
        .ok_or("the folder name is not valid Unicode")?
        .to_owned();
    app.get_webview_window("main")
        .ok_or("no main window")?
        .with_webview(move |webview| {
            webview.jni_handle().exec(move |env, activity, _| {
                let shown = env.new_string(&relative).and_then(|name| {
                    env.call_method(
                        activity,
                        "revealFolder",
                        "(Ljava/lang/String;)Z",
                        &[JValue::Object(&name)],
                    )?
                    .z()
                });
                match shown {
                    Ok(true) => {}
                    Ok(false) => log::error!("revealFolder: no system Files app"),
                    Err(error) => log::error!("revealFolder: {error}"),
                }
            });
        })
        .map_err(|e| e.to_string())
}

/// Shows the trackpad cursor at a point of the game surface, in pixels, or
/// hides it. Runs on the UI thread, which the JVM already attached.
pub(crate) fn set_cursor(point: Option<(f32, f32)>) {
    let activity = ACTIVITY.lock().unwrap();
    let Some((vm, activity)) = activity.as_ref() else {
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let (x, y) = point.unwrap_or_default();
    if let Err(error) = env.call_method(
        activity,
        "setCursor",
        "(ZFF)V",
        &[
            JValue::Bool(point.is_some().into()),
            JValue::Float(x),
            JValue::Float(y),
        ],
    ) {
        log::error!("setCursor: {error}");
    }
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

/// The container's size, display cutouts as (x, y, width, height) runs,
/// largest corner radius and mandatory gesture insets (top, right, bottom,
/// left), all in dp. Returns where the game surface goes, in dp.
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_hitode_launcher_MainActivity_nativeScreen(
    mut env: JNIEnv<'_>,
    activity: JObject<'_>,
    width: jfloat,
    height: jfloat,
    cutouts: JFloatArray<'_>,
    corner_radius: jfloat,
    gestures: JFloatArray<'_>,
    density: jfloat,
) -> jfloatArray {
    let floats = |env: &mut JNIEnv<'_>, array: &JFloatArray<'_>| {
        let mut values = vec![0.0; env.get_array_length(array).unwrap_or(0) as usize];
        let _ = env.get_float_array_region(array, 0, &mut values);
        values.into_iter().map(f64::from).collect::<Vec<_>>()
    };
    let cutouts = floats(&mut env, &cutouts)
        .chunks_exact(4)
        .map(|r| Rect {
            x: r[0],
            y: r[1],
            width: r[2],
            height: r[3],
        })
        .collect();
    let gestures = match floats(&mut env, &gestures)[..] {
        [top, right, bottom, left] => Insets {
            top,
            right,
            bottom,
            left,
        },
        _ => Insets::default(),
    };
    if let (Ok(vm), Ok(activity)) = (env.get_java_vm(), env.new_global_ref(activity)) {
        *ACTIVITY.lock().unwrap() = Some((vm, activity));
    }
    let area = crate::game::screen(
        f64::from(width),
        f64::from(height),
        Occlusion {
            cutouts,
            corner_radius: f64::from(corner_radius),
            gestures,
        },
        f64::from(density),
    );
    let values = [area.x, area.y, area.width, area.height].map(|v| v as f32);
    let Ok(array) = env.new_float_array(4) else {
        return std::ptr::null_mut();
    };
    let _ = env.set_float_array_region(&array, 0, &values);
    array.into_raw()
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

fn forward_surface_created(window: NonNull<ANativeWindow>, width: u32, height: u32) {
    crate::game::surface(Some(window), width, height);
}

fn forward_surface_changed(window: NonNull<ANativeWindow>, width: u32, height: u32) {
    crate::game::surface(Some(window), width, height);
}

fn forward_surface_destroyed(_window: NonNull<ANativeWindow>, _width: u32, _height: u32) {
    crate::game::surface(None, 0, 0);
}

fn forward_touch(pointer: i32, action: i32, x: f32, y: f32) {
    crate::game::touch(pointer, action, x, y);
}

fn forward_pause() {
    crate::game::lifecycle(false);
}

fn forward_resume() {
    crate::game::lifecycle(true);
}

fn forward_audio_focus(active: bool) {
    crate::game::audio_focus(active);
}

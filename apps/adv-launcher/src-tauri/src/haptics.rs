//! A light tick for a discrete step under the finger, such as a pinch moving
//! the library to another column count: the slider segment tick on Android,
//! a selection change on iOS, alignment feedback on a Mac trackpad.

use tauri::AppHandle;

#[tauri::command]
pub fn haptic_tick(app: AppHandle) {
    tick(&app);
}

#[cfg(target_os = "android")]
fn tick(app: &AppHandle) {
    use jni::objects::JValue;
    use tauri::Manager;
    /// `HapticFeedbackConstants.SEGMENT_TICK`; follows the system touch feedback setting.
    const SEGMENT_TICK: i32 = 26;
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let _ = window.with_webview(|webview| {
        webview.jni_handle().exec(|env, _, view| {
            let tick = [JValue::Int(SEGMENT_TICK)];
            if let Err(error) = env.call_method(view, "performHapticFeedback", "(I)Z", &tick) {
                log::error!("performHapticFeedback: {error}");
            }
        });
    });
}

#[cfg(target_os = "ios")]
fn tick(app: &AppHandle) {
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2_ui_kit::UISelectionFeedbackGenerator;
    thread_local! {
        static GENERATOR: Retained<UISelectionFeedbackGenerator> =
            UISelectionFeedbackGenerator::new(MainThreadMarker::new().unwrap());
    }
    let _ = app.run_on_main_thread(|| {
        GENERATOR.with(|generator| {
            generator.selectionChanged();
            // Keeps the Taptic Engine awake for the next step of the gesture.
            generator.prepare();
        });
    });
}

#[cfg(target_os = "macos")]
fn tick(_: &AppHandle) {
    use objc2_app_kit::{
        NSHapticFeedbackManager, NSHapticFeedbackPattern, NSHapticFeedbackPerformanceTime,
        NSHapticFeedbackPerformer,
    };
    NSHapticFeedbackManager::defaultPerformer().performFeedbackPattern_performanceTime(
        NSHapticFeedbackPattern::Alignment,
        NSHapticFeedbackPerformanceTime::Now,
    );
}

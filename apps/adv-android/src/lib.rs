//! JNI owns only safe session handles. Each guest and its audio device have one worker.
#[cfg(feature = "gpu")]
mod gpu;
mod protocol;
mod worker;

use adv_sync::{WakeEvent, input::KeyState};
use jni::JNIEnv;
use jni::objects::{GlobalRef, JClass, JObject, JString};
use jni::sys::{jint, jlong, jstring};
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};

struct Frame {
    serial: u64,
    width: u32,
    height: u32,
    bgra: Vec<u8>,
}
struct Session {
    id: u64,
    commands: mpsc::SyncSender<Packet>,
    wake: WakeEvent,
    keys: Arc<KeyState>,
    alive: AtomicBool,
    stop: AtomicBool,
    lifecycle_paused: AtomicBool,
    audio_focus: AtomicBool,
    release_input: AtomicBool,
    input_epoch: AtomicU64,
    input_lock: Mutex<()>,
    poll: Mutex<Arc<String>>,
    frame: Mutex<Option<Arc<Frame>>>,
}
struct Packet {
    command: protocol::Command,
    epoch: u64,
    key_observation: Option<u64>,
}
fn held_key(key: u32) -> bool {
    matches!(key, 0x11 | 0xa2 | 0xa3)
}
fn packet_current(session: &Session, packet: &Packet) -> bool {
    if packet.epoch != session.input_epoch.load(Ordering::Acquire) {
        return false;
    }
    match &packet.command {
        protocol::Command::Key { key, .. } if held_key(*key) => {
            packet.key_observation == Some(session.keys.observation(*key))
        }
        _ => true,
    }
}
fn submit_command(session: &Session, command: protocol::Command) -> Result<(), String> {
    let _input = session
        .input_lock
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if !session.alive.load(Ordering::Acquire) || session.stop.load(Ordering::Acquire) {
        return Err("runtime is stopped or stopping".into());
    }
    match command {
        protocol::Command::Pause => {
            session.lifecycle_paused.store(true, Ordering::Release);
            session.input_epoch.fetch_add(1, Ordering::AcqRel);
            session.keys.release_all();
            session.release_input.store(true, Ordering::Release);
        }
        protocol::Command::Resume => session.lifecycle_paused.store(false, Ordering::Release),
        protocol::Command::AudioFocus(active) => {
            session.audio_focus.store(active, Ordering::Release);
            if !active {
                session.input_epoch.fetch_add(1, Ordering::AcqRel);
                session.keys.release_all();
                session.release_input.store(true, Ordering::Release);
            }
        }
        command => {
            if session.lifecycle_paused.load(Ordering::Acquire)
                || !session.audio_focus.load(Ordering::Acquire)
            {
                return Err("input rejected until resume and audio-focus handshake".into());
            }
            let key_observation = if let protocol::Command::Key { key, pressed } = &command {
                if held_key(*key) {
                    session.keys.set(*key, *pressed);
                    Some(session.keys.observation(*key))
                } else {
                    None
                }
            } else {
                None
            };
            let packet = Packet {
                command,
                epoch: session.input_epoch.load(Ordering::Acquire),
                key_observation,
            };
            if let Err(error) = session.commands.try_send(packet) {
                session.input_epoch.fetch_add(1, Ordering::AcqRel);
                session.keys.release_all();
                session.release_input.store(true, Ordering::Release);
                session.wake.signal();
                return Err(format!(
                    "bounded Android command queue rejected input: {error}"
                ));
            }
        }
    }
    session.wake.signal();
    Ok(())
}
fn validate_frame(frame: &Frame) -> Result<(), String> {
    let length = (frame.width as usize)
        .checked_mul(frame.height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or("frame dimensions overflow")?;
    if frame.width == 0
        || frame.height == 0
        || frame.width > 4096
        || frame.height > 4096
        || length > 64 * 1024 * 1024
        || frame.bgra.len() != length
    {
        return Err("frame pixels do not match bounded bitmap dimensions".into());
    }
    Ok(())
}
#[cfg(any(target_os = "android", test))]
fn rgba_row(source: &[u8], target: &mut [u8]) {
    for (source, target) in source.chunks_exact(4).zip(target.chunks_exact_mut(4)) {
        target.copy_from_slice(&[source[2], source[1], source[0], 255]);
    }
}
static APPLICATION: OnceLock<GlobalRef> = OnceLock::new();
static INITIALIZING: Mutex<()> = Mutex::new(());
static SESSION: Mutex<Option<Arc<Session>>> = Mutex::new(None);
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

fn session(handle: jlong) -> Result<Arc<Session>, String> {
    let current = SESSION.lock().unwrap_or_else(|error| error.into_inner());
    current
        .as_ref()
        .filter(|session| handle > 0 && session.id == handle as u64)
        .cloned()
        .ok_or_else(|| "invalid or stale Android session handle".into())
}
fn publish(session: &Session, value: serde_json::Value) {
    let text = Arc::new(value.to_string());
    *session
        .poll
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = text;
}
fn throw(env: &mut JNIEnv<'_>, error: impl AsRef<str>) {
    let _ = env.throw_new("java/lang/IllegalStateException", error.as_ref());
}
fn string(env: &mut JNIEnv<'_>, value: &JString<'_>) -> Result<String, String> {
    env.get_string(value)
        .map(Into::into)
        .map_err(|error| error.to_string())
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_advhost_player_GameActivity_nativeInit(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    application: JObject<'_>,
) {
    let result = (|| -> Result<(), String> {
        let _guard = INITIALIZING
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if APPLICATION.get().is_some() {
            return Ok(());
        }
        if application.is_null() {
            return Err("nativeInit requires application Context".into());
        }
        let reference = env
            .new_global_ref(application)
            .map_err(|error| error.to_string())?;
        #[cfg(target_os = "android")]
        {
            let vm = env.get_java_vm().map_err(|error| error.to_string())?;
            // The JVM and application GlobalRef remain valid for process lifetime.
            // This serialized initialization happens before any AAudio worker exists.
            unsafe {
                ndk_context::initialize_android_context(
                    vm.get_java_vm_pointer().cast(),
                    reference.as_obj().as_raw().cast(),
                );
            }
        }
        APPLICATION
            .set(reference)
            .map_err(|_| "Android Context was already initialized")?;
        Ok(())
    })();
    if let Err(error) = result {
        throw(&mut env, error);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_advhost_player_GameActivity_nativeStart(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    exe: JString<'_>,
    data: JString<'_>,
    codepage: jint,
) -> jlong {
    let result = (|| -> Result<u64, String> {
        if APPLICATION.get().is_none() {
            return Err("call nativeInit before nativeStart".into());
        }
        if !matches!(codepage, 932 | 936) {
            return Err("codepage must be 932 or 936".into());
        }
        let exe = PathBuf::from(string(&mut env, &exe)?);
        let data = PathBuf::from(string(&mut env, &data)?);
        if !exe.is_absolute() || !data.is_absolute() {
            return Err("runtime paths must be absolute".into());
        }
        let mut current = SESSION.lock().unwrap_or_else(|error| error.into_inner());
        if current
            .as_ref()
            .is_some_and(|session| session.alive.load(Ordering::Acquire))
        {
            return Err("the previous runtime is still running or stopping".into());
        }
        let id = NEXT_SESSION
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |id| {
                (id < i64::MAX as u64).then_some(id + 1)
            })
            .map_err(|_| "session IDs exhausted")?;
        let (commands, receiver) = mpsc::sync_channel(128);
        let session = Arc::new(Session {
            id,
            commands,
            wake: WakeEvent::new().map_err(|error| error.to_string())?,
            keys: Arc::new(KeyState::default()),
            alive: AtomicBool::new(true),
            stop: AtomicBool::new(false),
            lifecycle_paused: AtomicBool::new(true),
            audio_focus: AtomicBool::new(false),
            release_input: AtomicBool::new(false),
            input_epoch: AtomicU64::new(1),
            input_lock: Mutex::new(()),
            frame: Mutex::new(None),
            poll: Mutex::new(Arc::new(
                json!({"session":id,"status":"starting","startup_stage":"spawned","error":null,
                "frame":{"serial":0,"width":0,"height":0},"menus":[],"dialogs":[]})
                .to_string(),
            )),
        });
        let worker_session = session.clone();
        std::thread::Builder::new()
            .name("adv-android-runtime".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    worker::run(&worker_session, receiver, &exe, &data, codepage as u32)
                }));
                let error = match result {
                    Ok(Ok(())) => None,
                    Ok(Err(error)) => Some(error),
                    Err(payload) => Some(format!(
                        "Android execution worker panicked: {}",
                        payload
                            .downcast_ref::<String>()
                            .map(String::as_str)
                            .or_else(|| payload.downcast_ref::<&str>().copied())
                            .unwrap_or("unknown panic")
                    )),
                };
                worker_session.keys.release_all();
                if let Some(error) = error {
                    worker::startup_error(&worker_session, &data, &error);
                }
                // All Runtime/Output/background drops completed on this worker first.
                worker_session.alive.store(false, Ordering::Release);
            })
            .map_err(|error| error.to_string())?;
        *current = Some(session);
        Ok(id)
    })();
    match result {
        Ok(id) => id as jlong,
        Err(error) => {
            throw(&mut env, error);
            0
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_advhost_player_GameActivity_nativeCommand(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    text: JString<'_>,
) {
    let result = (|| -> Result<(), String> {
        let session = session(handle)?;
        let command = protocol::parse(&string(&mut env, &text)?)?;
        submit_command(&session, command)
    })();
    if let Err(error) = result {
        throw(&mut env, error);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_advhost_player_GameActivity_nativePoll(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jstring {
    let text = match session(handle) {
        Ok(session) => session
            .poll
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone(),
        Err(error) => Arc::new(json!({"status":"error","error":error}).to_string()),
    };
    match env.new_string(text.as_str()) {
        Ok(text) => text.into_raw(),
        Err(error) => {
            throw(&mut env, error.to_string());
            std::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_advhost_player_GameActivity_nativeStop(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) {
    if let Ok(session) = session(handle) {
        let _input = session
            .input_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        session.input_epoch.fetch_add(1, Ordering::AcqRel);
        session.keys.release_all();
        session.stop.store(true, Ordering::Release);
        session.wake.signal();
    }
}

#[repr(C)]
#[cfg(target_os = "android")]
struct BitmapInfo {
    width: u32,
    height: u32,
    stride: u32,
    format: i32,
    flags: u32,
}
#[cfg(target_os = "android")]
#[link(name = "jnigraphics")]
unsafe extern "C" {
    fn AndroidBitmap_getInfo(
        env: *mut jni::sys::JNIEnv,
        bitmap: jni::sys::jobject,
        info: *mut BitmapInfo,
    ) -> i32;
    fn AndroidBitmap_lockPixels(
        env: *mut jni::sys::JNIEnv,
        bitmap: jni::sys::jobject,
        pixels: *mut *mut std::ffi::c_void,
    ) -> i32;
    fn AndroidBitmap_unlockPixels(env: *mut jni::sys::JNIEnv, bitmap: jni::sys::jobject) -> i32;
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_advhost_player_GameActivity_nativeCopyFrame(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    bitmap: JObject<'_>,
) -> jlong {
    let result = (|| -> Result<u64, String> {
        let session = session(handle)?;
        let frame = session
            .frame
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let Some(frame) = frame else {
            return Ok(u64::MAX);
        };
        validate_frame(&frame)?;
        #[cfg(target_os = "android")]
        {
            if bitmap.is_null() {
                return Err("copyFrame requires a mutable Bitmap".into());
            }
            let mut info = BitmapInfo {
                width: 0,
                height: 0,
                stride: 0,
                format: 0,
                flags: 0,
            };
            let mut pixels = std::ptr::null_mut();
            // Java keeps Bitmap alive for this call; the frame Arc is immutable.
            let raw_env = env.get_raw();
            let raw_bitmap = bitmap.as_raw();
            if unsafe { AndroidBitmap_getInfo(raw_env, raw_bitmap, &mut info) } != 0
                || info.format != 1
                || info.width != frame.width
                || info.height != frame.height
                || info.stride < frame.width * 4
                || (info.stride as usize)
                    .checked_mul(info.height as usize)
                    .is_none()
            {
                return Ok(u64::MAX);
            }
            if unsafe { AndroidBitmap_lockPixels(raw_env, raw_bitmap, &mut pixels) } != 0
                || pixels.is_null()
            {
                return Err("cannot lock Android Bitmap pixels".into());
            }
            for y in 0..frame.height as usize {
                let source =
                    &frame.bgra[y * frame.width as usize * 4..(y + 1) * frame.width as usize * 4];
                // Validated dimensions/stride and the successful lock bound each row.
                let target = unsafe {
                    std::slice::from_raw_parts_mut(
                        pixels.cast::<u8>().add(y * info.stride as usize),
                        frame.width as usize * 4,
                    )
                };
                rgba_row(source, target);
            }
            if unsafe { AndroidBitmap_unlockPixels(raw_env, raw_bitmap) } != 0 {
                return Err("cannot unlock Android Bitmap pixels".into());
            }
            Ok(frame.serial)
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = (frame, bitmap);
            Err("Android Bitmap is unavailable on this host".into())
        }
    })();
    match result {
        Ok(serial) => serial as jlong,
        Err(error) => {
            throw(&mut env, error);
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(capacity: usize) -> (Session, mpsc::Receiver<Packet>) {
        let (commands, receiver) = mpsc::sync_channel(capacity);
        (
            Session {
                id: 42,
                commands,
                wake: WakeEvent::new().unwrap(),
                keys: Arc::new(KeyState::default()),
                alive: AtomicBool::new(true),
                stop: AtomicBool::new(false),
                lifecycle_paused: AtomicBool::new(true),
                audio_focus: AtomicBool::new(false),
                release_input: AtomicBool::new(false),
                input_epoch: AtomicU64::new(1),
                input_lock: Mutex::new(()),
                poll: Mutex::new(Arc::new("{}".into())),
                frame: Mutex::new(None),
            },
            receiver,
        )
    }
    fn activate(session: &Session) {
        submit_command(session, protocol::Command::Resume).unwrap();
        submit_command(session, protocol::Command::AudioFocus(true)).unwrap();
    }
    fn key(session: &Session, key: u32, pressed: bool) -> Result<(), String> {
        submit_command(session, protocol::Command::Key { key, pressed })
    }
    #[test]
    fn input_requires_both_lifecycle_and_audio_focus_handshakes() {
        let (session, _receiver) = fixture(4);
        assert!(key(&session, 17, true).is_err());
        submit_command(&session, protocol::Command::Resume).unwrap();
        assert!(key(&session, 17, true).is_err());
        activate(&session);
        assert!(key(&session, 17, true).is_ok());
    }
    #[test]
    fn ctrl_release_is_immediate_and_supersedes_a_queued_down() {
        let (session, receiver) = fixture(4);
        activate(&session);
        key(&session, 17, true).unwrap();
        let down = receiver.recv().unwrap();
        assert_eq!(session.keys.get(17) & 0x80, 0x80);
        key(&session, 17, false).unwrap();
        assert_eq!(session.keys.get(17) & 0x80, 0);
        assert!(!packet_current(&session, &down));
        assert!(packet_current(&session, &receiver.recv().unwrap()));
    }
    #[test]
    fn pause_resume_before_worker_observation_still_invalidates_old_inputs() {
        let (session, receiver) = fixture(4);
        activate(&session);
        key(&session, 17, true).unwrap();
        let old = receiver.recv().unwrap();
        submit_command(&session, protocol::Command::Pause).unwrap();
        submit_command(&session, protocol::Command::Resume).unwrap();
        assert!(!packet_current(&session, &old));
        assert_eq!(session.keys.get(17) & 0x80, 0);
        key(&session, 17, true).unwrap();
        assert!(packet_current(&session, &receiver.recv().unwrap()));
    }
    #[test]
    fn queue_saturation_cannot_leave_ctrl_physically_pressed() {
        let (session, receiver) = fixture(1);
        activate(&session);
        key(&session, 17, true).unwrap();
        assert!(key(&session, 17, false).is_err());
        assert_eq!(session.keys.get(17) & 0x80, 0);
        assert!(!packet_current(&session, &receiver.recv().unwrap()));
        assert!(session.release_input.load(Ordering::Acquire));
    }
    #[test]
    fn discrete_tap_packets_keep_down_and_up_order_without_superseding_down() {
        let (session, receiver) = fixture(4);
        activate(&session);
        key(&session, 13, true).unwrap();
        key(&session, 13, false).unwrap();
        let down = receiver.recv().unwrap();
        let up = receiver.recv().unwrap();
        assert!(packet_current(&session, &down));
        assert!(packet_current(&session, &up));
        assert!(matches!(
            down.command,
            protocol::Command::Key {
                key: 13,
                pressed: true
            }
        ));
        assert!(matches!(
            up.command,
            protocol::Command::Key {
                key: 13,
                pressed: false
            }
        ));
    }
    #[test]
    fn bitmap_copy_preserves_raw_frame_and_rejects_truncated_pixels() {
        let mut frame = Frame {
            serial: 1,
            width: 2,
            height: 1,
            bgra: vec![1, 2, 3, 0, 4, 5, 6, 0],
        };
        validate_frame(&frame).unwrap();
        let mut target = [0u8; 8];
        rgba_row(&frame.bgra, &mut target);
        assert_eq!(target, [3, 2, 1, 255, 6, 5, 4, 255]);
        assert_eq!(frame.bgra, [1, 2, 3, 0, 4, 5, 6, 0]);
        frame.bgra.pop();
        assert!(validate_frame(&frame).is_err());
        frame.width = u32::MAX;
        assert!(validate_frame(&frame).is_err());
    }
}

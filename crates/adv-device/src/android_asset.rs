//! Borrow APK asset buffers from Android's asset manager.
use jni::{JavaVM, objects::JObject};
use std::{ffi::CStr, io, ptr::NonNull};

struct Asset(NonNull<ndk_sys::AAsset>);

impl Drop for Asset {
    fn drop(&mut self) {
        // SAFETY: This wrapper owns the open asset.
        unsafe { ndk_sys::AAsset_close(self.0.as_ptr()) }
    }
}

/// Android maps stored assets and inflates compressed assets into its own buffer.
pub fn with_bytes<T>(name: &CStr, read: impl FnOnce(&[u8]) -> T) -> io::Result<T> {
    // Tao installs a process-wide application context before the launcher starts.
    let context = ndk_context::android_context();
    // SAFETY: The context contains the running JVM and a global application reference.
    let vm = unsafe { JavaVM::from_raw(context.vm().cast()) }.map_err(io::Error::other)?;
    let mut env = vm.attach_current_thread().map_err(io::Error::other)?;
    let raw = env.get_raw();
    // SAFETY: The attached environment creates a fresh local reference for JObject.
    let context = env.auto_local(unsafe {
        JObject::from_raw((**raw).NewLocalRef.unwrap()(raw, context.context().cast()))
    });
    let manager = env
        .call_method(
            &context,
            "getAssets",
            "()Landroid/content/res/AssetManager;",
            &[],
        )
        .and_then(|value| value.l())
        .map_err(io::Error::other)?;
    let manager = env.auto_local(manager);
    // SAFETY: The Java AssetManager reference stays live until the asset is closed.
    let asset = unsafe {
        let manager = ndk_sys::AAssetManager_fromJava(raw.cast(), manager.as_raw().cast());
        ndk_sys::AAssetManager_open(manager, name.as_ptr(), ndk_sys::AASSET_MODE_BUFFER as i32)
    };
    let asset =
        Asset(NonNull::new(asset).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("APK asset {name:?}"))
        })?);
    // SAFETY: The open asset owns the buffer for the duration of the read.
    unsafe {
        let buffer = ndk_sys::AAsset_getBuffer(asset.0.as_ptr());
        if buffer.is_null() {
            return Err(io::Error::other(format!("cannot read APK asset {name:?}")));
        }
        let len = ndk_sys::AAsset_getLength64(asset.0.as_ptr()) as usize;
        Ok(read(std::slice::from_raw_parts(buffer.cast(), len)))
    }
}

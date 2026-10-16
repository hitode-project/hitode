//! The separate debugger process embedded in the iOS player (IOS-JIT-2).
#[cfg(target_os = "ios")]
mod device;
#[cfg(target_os = "ios")]
mod extension;
#[cfg(any(target_os = "ios", test))]
mod gdb;

/// Called by the extension's native constructor before NSExtensionMain.
#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
pub extern "C" fn adv_jit_helper_init() {
    use objc2::ClassType;
    let _ = extension::HitodeJitRequestHandler::class();
}

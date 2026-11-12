//! The separate debugger process embedded in the iOS player (IOS-JIT-2).
#[cfg(target_os = "ios")]
mod device;
#[cfg(target_os = "ios")]
mod extension;
#[cfg(any(target_os = "ios", test))]
mod gdb;

/// The error followed by its causes. idevice's messages leave the cause out:
/// every transport failure reads only "device socket io failed".
#[cfg(any(target_os = "ios", test))]
fn describe(error: &(dyn std::error::Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let message = cause.to_string();
        if !text.contains(&message) {
            text = format!("{text}: {message}");
        }
        source = cause.source();
    }
    text
}

#[cfg(test)]
mod tests {
    // spec: IOS-JIT-2
    #[test]
    fn transport_errors_keep_their_cause() {
        let io = std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "early eof");
        let error = idevice::IdeviceError::from(io);
        assert_eq!(
            super::describe(&error),
            "device socket io failed: early eof"
        );
    }
}

/// Called by the extension's native constructor before NSExtensionMain.
#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
pub extern "C" fn adv_jit_helper_init() {
    use objc2::ClassType;
    let _ = extension::HitodeJitRequestHandler::class();
}

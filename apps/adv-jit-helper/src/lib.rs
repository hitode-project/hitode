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

/// lockdownd's port, open on every device but never RemotePairing.
#[cfg(any(target_os = "ios", test))]
const LOCKDOWN_PORT: u16 = 62078;

/// The ports Henge-style discovery probes when no candidate answers: iOS
/// picks the RemotePairing port from the dynamic range.
#[cfg(any(target_os = "ios", test))]
fn scan_ports(tried: &[u16]) -> Vec<u16> {
    (49152..=65535)
        .filter(|port| *port != LOCKDOWN_PORT && !tried.contains(port))
        .collect()
}

#[cfg(test)]
mod tests {
    // spec: IOS-JIT-2
    #[test]
    fn scan_skips_lockdown_and_tried_ports() {
        let ports = super::scan_ports(&[49152, 50000]);
        assert_eq!(ports.first(), Some(&49153));
        assert_eq!(ports.last(), Some(&65535));
        assert!(!ports.contains(&super::LOCKDOWN_PORT));
        assert!(!ports.contains(&50000));
        assert_eq!(ports.len(), 16384 - 3);
    }

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

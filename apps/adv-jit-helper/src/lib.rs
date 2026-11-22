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

/// Removes stale DDI revisions, inactive variants, and interrupted `.download`
/// files from the helper's Application Support directory without following links.
#[cfg(any(target_os = "ios", test))]
fn prune_stale_ddi(cache: &std::path::Path, keep_revision: &str, keep_variant: &str) {
    let Ok(entries) = std::fs::read_dir(cache) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        if entry.file_name().to_str() == Some(keep_revision) && kind.is_dir() {
            let Ok(variants) = std::fs::read_dir(entry.path()) else {
                continue;
            };
            for variant in variants.flatten() {
                let Ok(vkind) = variant.file_type() else {
                    continue;
                };
                if vkind.is_symlink() {
                    continue;
                }
                if variant.file_name().to_str() == Some(keep_variant) && vkind.is_dir() {
                    if let Ok(files) = std::fs::read_dir(variant.path()) {
                        for file in files.flatten() {
                            if file.file_type().is_ok_and(|t| t.is_file())
                                && file.path().extension().is_some_and(|ext| ext == "download")
                            {
                                let _ = std::fs::remove_file(file.path());
                            }
                        }
                    }
                } else if vkind.is_dir() {
                    let _ = std::fs::remove_dir_all(variant.path());
                } else {
                    let _ = std::fs::remove_file(variant.path());
                }
            }
        } else if kind.is_dir() {
            let _ = std::fs::remove_dir_all(entry.path());
        } else {
            let _ = std::fs::remove_file(entry.path());
        }
    }
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

    // spec: IOS-JIT-2
    #[test]
    fn prune_stale_ddi_keeps_active_set_and_removes_old_revisions_and_partials() {
        let root = std::env::temp_dir().join(format!("adv-ddi-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let active = root.join("rev-new/Xcode_iOS_DDI_Cryptex");
        let old_variant = root.join("rev-new/Xcode_iOS_DDI_Personalized");
        let old_rev = root.join("rev-old/Xcode_iOS_DDI_Cryptex");
        std::fs::create_dir_all(&active).unwrap();
        std::fs::create_dir_all(&old_variant).unwrap();
        std::fs::create_dir_all(&old_rev).unwrap();
        std::fs::write(active.join("Image.dmg"), b"ok").unwrap();
        std::fs::write(active.join("Image.dmg.download"), b"partial").unwrap();
        std::fs::write(old_variant.join("Image.dmg"), b"old-variant").unwrap();
        std::fs::write(old_rev.join("Image.dmg"), b"old-rev").unwrap();

        super::prune_stale_ddi(&root, "rev-new", "Xcode_iOS_DDI_Cryptex");

        assert!(active.join("Image.dmg").is_file());
        assert!(!active.join("Image.dmg.download").exists());
        assert!(!old_variant.exists());
        assert!(!old_rev.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}

/// Called by the extension's native constructor before NSExtensionMain.
#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
pub extern "C" fn adv_jit_helper_init() {
    use objc2::ClassType;
    let _ = extension::HitodeJitRequestHandler::class();
}

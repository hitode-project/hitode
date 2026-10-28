//! Installation of Hitode's host DLLs on drive C:, which a session may keep
//! outside its prefix. Wine recognizes the DOS-stub
//! signature and loads this file as a builtin through its ordinary PE loader.
//! Replacing an installation symlink unlinks it before writing, keeping the Wine
//! installation read-only. Call before starting the guest process.

use std::io;
use std::path::{Path, PathBuf};

use adv_abi::dll::ModuleImage;

/// Install `image` in `drive`'s system directory.
pub fn install(drive: &Path, image: &ModuleImage) -> io::Result<PathBuf> {
    install_bytes(drive, image.name(), image.bytes().to_vec())
}

/// Install a host image after its export metadata has been adjusted.
pub fn install_bytes(drive: &Path, name: &str, mut bytes: Vec<u8>) -> io::Result<PathBuf> {
    let directory = drive.join("windows/system32");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(name);
    let signature = b"Wine builtin DLL\0";
    bytes[0x40..0x40 + signature.len()].copy_from_slice(signature);
    if !path.is_symlink() && std::fs::read(&path).is_ok_and(|existing| existing == bytes) {
        return Ok(path);
    }
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::fs::write(&path, bytes)?;
    Ok(path)
}

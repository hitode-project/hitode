//! Prefix installation for Hitode's host DLLs. Wine recognizes the DOS-stub
//! signature and loads this file as a builtin through its ordinary PE loader.
//! Replacing a prefix symlink unlinks it before writing, keeping the Wine
//! installation read-only. Call before starting the guest process.

use std::io;
use std::path::{Path, PathBuf};

use adv_abi::dll::ModuleImage;

pub fn install(prefix: &Path, image: &ModuleImage) -> io::Result<PathBuf> {
    let directory = prefix.join("drive_c/windows/system32");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(image.name());
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut bytes = image.bytes().to_vec();
    let signature = b"Wine builtin DLL\0";
    bytes[0x40..0x40 + signature.len()].copy_from_slice(signature);
    std::fs::write(&path, bytes)?;
    Ok(path)
}

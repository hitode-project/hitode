//! FILE_FLAG_DELETE_ON_CLOSE: the file is deleted once no handle has it open,
//! including when the process exits with handles still open.
use adv_machine::{Env, Identity, Namespace};
use adv_os::vfs::FileIdentity;

use crate::{FileObj, Win32};

pub(super) const FLAG: u32 = 0x0400_0000;
/// CreateFile asks for DELETE access on the caller's behalf, so later opens
/// must share delete access.
pub(super) const DELETE: u32 = 0x1_0000;

/// A file to delete: the handle that asked, and the file it opened.
struct Entry {
    owner: Identity<FileObj>,
    file: Option<FileIdentity>,
    path: String,
}

#[derive(Default)]
struct Pending(Vec<Entry>);

/// Deletes `handle`'s file once no handle has it open.
pub(super) fn watch(env: &mut Env<'_>, handle: u32) {
    let Some(owner) = env
        .sys
        .objects
        .lookup_alias::<FileObj>(Namespace::DEFAULT, handle)
    else {
        return;
    };
    let opened = env.sys.objects.owned(owner).unwrap();
    let entry = Entry {
        owner,
        file: opened.file.cached_identity(),
        path: opened.guest_path.clone(),
    };
    if env.ext.try_get::<Pending>().is_none() {
        env.ext.insert(Pending::default());
    }
    env.ext.get_mut::<Pending>().0.push(entry);
}

/// Deletes the pending files no handle has open any more, or all of them
/// when the process exits.
pub(crate) fn release(env: &mut Env<'_>, exiting: bool) {
    let Some(pending) = env.ext.try_get_mut::<Pending>() else {
        return;
    };
    if pending.0.is_empty() {
        return;
    }
    let objects = &env.sys.objects;
    let open = |entry: &Entry| match entry.file {
        Some(file) => objects
            .all_hosts::<FileObj>()
            .any(|(_, other)| other.file.cached_identity() == Some(file)),
        None => objects.owned(entry.owner).is_some(),
    };
    let (closed, open) = std::mem::take(&mut pending.0)
        .into_iter()
        .partition::<Vec<_>, _>(|entry| exiting || !open(entry));
    pending.0 = open;
    let vfs = &env.ext.get::<Win32>().vfs;
    for Entry { path, .. } in closed {
        if let Err(error) = vfs.delete_file(&path) {
            log::debug!("delete on close {path}: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::misc::test_support::{DATA, Fixture, env};
    use adv_cpu::backend::Guest;

    const CREATE: &str = "CreateFileA";

    // spec: SYS-FILE-2
    #[test]
    fn the_file_goes_when_its_last_handle_closes_and_opens_must_share_delete() {
        let mut f = Fixture::new("delete-on-close");
        f.m.cpu.write(DATA, b"scratch.tmp\0").unwrap();
        let k32 = "kernel32.dll";
        let exists = |f: &mut Fixture| f.call(k32, "GetFileAttributesA", &[DATA]) != u32::MAX;
        // GENERIC_READ | GENERIC_WRITE, CREATE_ALWAYS, TEMPORARY | DELETE_ON_CLOSE.
        let file = f.call(k32, CREATE, &[DATA, 0xc000_0000, 7, 0, 2, 0x0400_0100, 0]);
        assert_ne!(file, u32::MAX);
        assert!(exists(&mut f));
        assert_eq!(
            f.call(k32, CREATE, &[DATA, 0x8000_0000, 3, 0, 3, 0, 0]),
            u32::MAX
        );
        assert_eq!(f.call(k32, "GetLastError", &[]), 32);
        let other = f.call(k32, CREATE, &[DATA, 0x8000_0000, 7, 0, 3, 0, 0]);
        assert_ne!(other, u32::MAX);
        assert_eq!(f.call(k32, "CloseHandle", &[file]), 1);
        assert!(exists(&mut f), "another handle keeps the file");
        assert_eq!(f.call(k32, "CloseHandle", &[other]), 1);
        assert!(!exists(&mut f));
        // Process exit closes handles still open.
        let file = f.call(k32, CREATE, &[DATA, 0xc000_0000, 7, 0, 2, 0x0400_0000, 0]);
        assert_ne!(file, u32::MAX);
        super::release(&mut env(&mut f.m), true);
        assert!(!exists(&mut f));
        f.cleanup();
    }
}

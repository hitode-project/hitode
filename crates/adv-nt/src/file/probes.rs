//! Repeated attribute queries by name are answered from memory. The server
//! counts its host calls that create, remove or rename names; every system call
//! or server request that can change a file's contents or metadata starts a new
//! revision on this execution owner, including another process's.

use std::cell::Cell;
use std::collections::HashMap;
use std::fs::Metadata;
use std::path::PathBuf;

use crate::status::Status;

thread_local! {
    static REVISION: Cell<u64> = const { Cell::new(0) };
}

fn bump() {
    REVISION.set(REVISION.get().wrapping_add(1));
}

pub(crate) fn invalidate_syscall(name: &str) {
    if matches!(
        name,
        "NtDeleteFile"
            | "NtWriteFile"
            | "NtWriteFileGather"
            | "NtSetInformationFile"
            | "NtSetEaFile"
            | "NtSetVolumeInformationFile"
            | "NtFsControlFile"
            | "NtDeviceIoControlFile"
            | "NtFlushBuffersFile"
            | "NtFlushBuffersFileEx"
            | "NtFlushVirtualMemory"
            | "NtUnmapViewOfSection"
            | "NtUnmapViewOfSectionEx"
            | "NtCreateSymbolicLinkObject"
            | "NtMakeTemporaryObject"
    ) {
        bump();
    }
}

pub(crate) fn invalidate_request(number: i32) {
    use crate::protocol::{
        CreateMappingRequest, CreateSymlinkRequest, FlushRequest, Request, SetFdDispInfoRequest,
        SetFdEofInfoRequest, SetFdNameInfoRequest, UnmapViewRequest,
    };
    if matches!(
        number,
        CreateMappingRequest::NUMBER
            | UnmapViewRequest::NUMBER
            | FlushRequest::NUMBER
            | SetFdDispInfoRequest::NUMBER
            | SetFdNameInfoRequest::NUMBER
            | SetFdEofInfoRequest::NUMBER
            | CreateSymlinkRequest::NUMBER
    ) {
        bump();
    }
}

/// Names one process keeps; a fuller cache starts over.
const NAMES: usize = 16384;

/// An NT name as a query gave it: root handle, object attributes and name.
pub(crate) type Key = (u32, u32, Vec<u16>);
pub(crate) type Found = Result<(PathBuf, Metadata, Metadata), Status>;

/// The revision a probe starts in; its result is kept only if nothing changed.
pub(crate) type Revision = (u64, u64);

pub(crate) fn revision() -> Revision {
    (REVISION.get(), adv_wineserver::names_revision())
}

#[derive(Default)]
pub(crate) struct Probes {
    revision: Revision,
    names: HashMap<Key, Found>,
}

impl Probes {
    fn current(&mut self) -> &mut HashMap<Key, Found> {
        let revision = revision();
        if self.revision != revision {
            self.names.clear();
            self.revision = revision;
        }
        &mut self.names
    }

    pub(crate) fn get(&mut self, key: &Key) -> Option<Found> {
        self.current().get(key).cloned()
    }

    pub(crate) fn insert(&mut self, since: Revision, key: Key, found: Found) {
        self.current();
        if self.revision != since {
            return;
        }
        let names = &mut self.names;
        if names.len() == NAMES {
            names.clear();
        }
        names.insert(key, found);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        CloseHandleRequest, CreateFileRequest, GetFileInfoRequest, Request, SetFdDispInfoRequest,
    };

    unsafe extern "C" {
        fn hitode_rmdir(path: *const std::ffi::c_char) -> std::ffi::c_int;
    }

    // spec: WINE-SYS-14
    #[test]
    fn probes_last_until_a_mutation() {
        let mut probes = Probes::default();
        let key = (0, 0x40, "\\??\\C:\\a".encode_utf16().collect::<Vec<_>>());
        probes.insert(revision(), key.clone(), Err(0xc000_0034));
        assert_eq!(
            probes.get(&key).map(|found| found.err()),
            Some(Some(0xc000_0034))
        );
        // Opening and closing names, as existence checks do, keeps them.
        for name in [
            "NtQueryAttributesFile",
            "NtCreateFile",
            "NtOpenFile",
            "NtClose",
        ] {
            invalidate_syscall(name);
        }
        for number in [
            GetFileInfoRequest::NUMBER,
            CreateFileRequest::NUMBER,
            CloseHandleRequest::NUMBER,
        ] {
            invalidate_request(number);
        }
        assert!(probes.get(&key).is_some());
        invalidate_syscall("NtWriteFile");
        assert!(probes.get(&key).is_none());
        probes.insert(revision(), key.clone(), Err(0xc000_0034));
        invalidate_request(SetFdDispInfoRequest::NUMBER);
        assert!(probes.get(&key).is_none());
        // The server's own removal of a name, such as a delete on close.
        probes.insert(revision(), key.clone(), Err(0xc000_0034));
        unsafe { hitode_rmdir(c"/nonexistent-adv-probe".as_ptr()) };
        assert!(probes.get(&key).is_none());
        // A probe that a mutation overtook is not kept.
        let since = revision();
        invalidate_syscall("NtWriteFile");
        probes.insert(since, key.clone(), Err(0xc000_0034));
        assert!(probes.get(&key).is_none());
        // Other tests may end the shared server revision meanwhile; either
        // way the cache never exceeds its capacity.
        for index in 0..=NAMES as u16 {
            probes.insert(revision(), (0, 0, vec![index]), Err(0));
        }
        assert!(probes.names.len() < NAMES);
    }
}

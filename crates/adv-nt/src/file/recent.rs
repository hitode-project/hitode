//! A bounded record of regular file reads and their guest destinations.

use std::{collections::VecDeque, fs::File, os::fd::BorrowedFd, sync::Arc};

use crate::server::HandleFd;

#[derive(Clone)]
pub struct ReadSource {
    pub file: Arc<File>,
    pub offset: u64,
}

struct ReadRange {
    source: ReadSource,
    length: u32,
    destination: u32,
}

#[derive(Default)]
pub struct RecentReads(VecDeque<ReadRange>);

impl RecentReads {
    pub(super) fn record(&mut self, fd: &HandleFd, destination: u32, offset: u64, length: u32) {
        let cached = fd.file.as_ref().map(|file| &file.read_source);
        let file = cached.and_then(|file| file.borrow().upgrade()).or_else(|| {
            // SAFETY: the server descriptor is live for this call.
            let fd = unsafe { BorrowedFd::borrow_raw(fd.fd) };
            let file = Arc::new(File::from(fd.try_clone_to_owned().ok()?));
            if let Some(cached) = cached {
                *cached.borrow_mut() = Arc::downgrade(&file);
            }
            Some(file)
        });
        if let Some(file) = file {
            self.push(ReadRange {
                source: ReadSource { file, offset },
                destination,
                length,
            });
        }
    }

    fn push(&mut self, read: ReadRange) {
        if self.0.len() == 16 {
            self.0.pop_front();
        }
        self.0.push_back(read);
    }

    pub fn direct(&self, address: u32, length: u32) -> Option<ReadSource> {
        self.0.iter().rev().find_map(|read| {
            let delta = address.checked_sub(read.destination)?;
            (delta.checked_add(length)? <= read.length).then(|| ReadSource {
                file: Arc::clone(&read.source.file),
                offset: read.source.offset + u64::from(delta),
            })
        })
    }

    pub fn ranges(&self) -> impl Iterator<Item = (ReadSource, u32)> {
        self.0
            .iter()
            .rev()
            .map(|read| (read.source.clone(), read.length))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: CODEC-CALL-3
    #[test]
    fn finds_the_file_range_for_a_direct_read() {
        let file =
            Arc::new(File::open(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap());
        let mut reads = RecentReads::default();
        reads.push(ReadRange {
            source: ReadSource {
                file: Arc::clone(&file),
                offset: 600,
            },
            destination: 1000,
            length: 80,
        });
        let direct = reads.direct(1020, 60).unwrap();
        assert_eq!(direct.offset, 620);
        assert!(Arc::ptr_eq(&direct.file, &file));
        assert!(reads.direct(1020, 61).is_none());
        assert!(reads.direct(1100, 60).is_none());
    }
}

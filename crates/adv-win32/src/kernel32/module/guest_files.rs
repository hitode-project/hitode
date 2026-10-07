//! One loader transaction's file cache over the common live NT reader.
use crate::kernel32::file::native;
use adv_machine::{Env, Outcome};
use adv_os::{LoadError, Vfs, loader::FileSource};
pub(super) use native::active;
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct Files(BTreeMap<String, Option<Vec<u8>>>);
impl Files {
    pub(super) fn insert(&mut self, path: String, bytes: Option<Vec<u8>>) {
        self.0.insert(path.to_lowercase(), bytes);
    }
}
pub(super) struct Source<'a> {
    pub files: &'a Files,
    pub vfs: &'a Vfs,
}
impl FileSource for Source<'_> {
    fn absolute_path(&self, path: &str) -> String {
        self.vfs.absolute_path(path)
    }
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        self.files
            .0
            .get(&self.vfs.normalize(path).to_lowercase())
            .cloned()
            .flatten()
    }
    fn read_for_loader(&self, path: &str) -> Result<Option<Vec<u8>>, LoadError> {
        let path = self.vfs.normalize(path);
        self.files
            .0
            .get(&path.to_lowercase())
            .cloned()
            .ok_or(LoadError::DeferredRead(path))
    }
}

type Completion = Box<dyn FnOnce(&mut Env<'_>, Result<Option<Vec<u8>>, u32>) -> Outcome>;
pub(super) fn read(env: &mut Env<'_>, path: String, then: Completion) -> Outcome {
    native::read(
        env,
        path,
        u32::MAX as usize,
        Box::new(move |env, result| {
            let result = match result {
                Ok(bytes) => Ok(Some(bytes)),
                Err(0xc0000034 | 0xc000003a | 0xc000000f) => Ok(None),
                Err(status) => Err(status),
            };
            then(env, result)
        }),
    )
}
#[cfg(test)]
mod tests;

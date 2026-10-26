//! Immutable index snapshots retain their maximum across queue retries.
use std::{
    hash::{Hash, Hasher},
    ops::{Deref, DerefMut},
    sync::{Arc, OnceLock},
};
#[derive(Clone, Debug)]
struct Storage {
    bytes: Vec<u32>,
    maximum: OnceLock<Option<u32>>,
}
#[derive(Clone, Debug)]
pub struct IndexBytes(Arc<Storage>);
impl IndexBytes {
    pub fn maximum(&self) -> Option<u32> {
        *self
            .0
            .maximum
            .get_or_init(|| self.0.bytes.iter().max().copied())
    }
    pub fn rebase(&mut self, first: u32) {
        let maximum = self.maximum().map(|n| n - first);
        let storage = Arc::make_mut(&mut self.0);
        for index in &mut storage.bytes {
            *index -= first;
        }
        storage.maximum = OnceLock::from(maximum);
    }
}
impl From<Vec<u32>> for IndexBytes {
    fn from(bytes: Vec<u32>) -> Self {
        let maximum = bytes.iter().max().copied();
        Self(Arc::new(Storage {
            bytes,
            maximum: OnceLock::from(maximum),
        }))
    }
}
impl FromIterator<u32> for IndexBytes {
    fn from_iter<T: IntoIterator<Item = u32>>(indices: T) -> Self {
        let mut maximum = None;
        let bytes = indices
            .into_iter()
            .inspect(|&n| maximum = Some(maximum.map_or(n, |old: u32| old.max(n))))
            .collect();
        Self(Arc::new(Storage {
            bytes,
            maximum: OnceLock::from(maximum),
        }))
    }
}
impl Deref for IndexBytes {
    type Target = Vec<u32>;
    fn deref(&self) -> &Vec<u32> {
        &self.0.bytes
    }
}
impl DerefMut for IndexBytes {
    fn deref_mut(&mut self) -> &mut Vec<u32> {
        let storage = Arc::make_mut(&mut self.0);
        storage.maximum.take();
        &mut storage.bytes
    }
}
impl PartialEq for IndexBytes {
    fn eq(&self, other: &Self) -> bool {
        self.0.bytes == other.0.bytes
    }
}
impl Eq for IndexBytes {}
impl Hash for IndexBytes {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.bytes.hash(state);
    }
}

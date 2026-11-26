//! Immutable operation sequences from deferred contexts.
use std::sync::Arc;

pub(crate) struct List<T>(Vec<Arc<T>>);

impl<T> Default for List<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T> List<T> {
    pub fn push(&mut self, command: T) {
        self.0.push(Arc::new(command));
    }

    pub fn splice(&mut self, other: &Self) {
        self.0.extend(other.0.iter().cloned());
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.0.iter().map(AsRef::as_ref)
    }
}

impl<T> Clone for List<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

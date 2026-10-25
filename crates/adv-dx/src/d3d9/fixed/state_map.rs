//! Shared fixed-state categories retain their content key across unrelated writes.
use std::{
    collections::{BTreeMap, BTreeSet},
    hash::{Hash, Hasher},
    ops::{Deref, DerefMut},
    sync::{Arc, OnceLock},
};

#[derive(Clone)]
pub(in crate::d3d9) struct StateMap(Arc<Contents>);
#[derive(Clone, Default)]
struct Contents {
    values: BTreeMap<i32, u32>,
    key: OnceLock<u128>,
}
impl std::fmt::Debug for StateMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.values.fmt(f)
    }
}
impl PartialEq for StateMap {
    fn eq(&self, other: &Self) -> bool {
        self.0.values == other.0.values
    }
}
impl StateMap {
    pub(in crate::d3d9) fn insert(&mut self, key: i32, value: u32) -> Option<u32> {
        if self.get(&key) == Some(&value) {
            return Some(value);
        }
        self.deref_mut().insert(key, value)
    }
    pub(in crate::d3d9) fn capture(&self, keys: &BTreeSet<i32>) -> Self {
        if keys.len() == self.len() {
            self.clone()
        } else {
            keys.iter().map(|&i| (i, self[&i])).collect()
        }
    }
}
impl Default for StateMap {
    fn default() -> Self {
        static EMPTY: OnceLock<StateMap> = OnceLock::new();
        EMPTY.get_or_init(|| Self(Arc::default())).clone()
    }
}
impl From<BTreeMap<i32, u32>> for StateMap {
    fn from(values: BTreeMap<i32, u32>) -> Self {
        if values.is_empty() {
            Self::default()
        } else {
            Self(Arc::new(Contents {
                values,
                key: OnceLock::new(),
            }))
        }
    }
}
impl FromIterator<(i32, u32)> for StateMap {
    fn from_iter<T: IntoIterator<Item = (i32, u32)>>(iter: T) -> Self {
        BTreeMap::from_iter(iter).into()
    }
}
impl Deref for StateMap {
    type Target = BTreeMap<i32, u32>;
    fn deref(&self) -> &Self::Target {
        &self.0.values
    }
}
impl DerefMut for StateMap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        let contents = Arc::make_mut(&mut self.0);
        contents.key.take();
        &mut contents.values
    }
}
impl Hash for StateMap {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0
            .key
            .get_or_init(|| {
                let mut h = xxhash_rust::xxh3::Xxh3::new();
                self.0.values.hash(&mut h);
                h.digest128()
            })
            .hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // spec: GFX-REC-2, GFX-SHADER-4
    #[test]
    fn captures_share_keys_and_mutations_detach_only_the_changed_category() {
        let mut state = StateMap::from(BTreeMap::from([(1, 0), (2, 0x8000_0000)]));
        let saved = state.capture(&BTreeSet::from([1, 2]));
        let before = crate::d3d9::recording::hash(&saved);
        assert!(Arc::ptr_eq(&state.0, &saved.0));
        let cached = state.0.key.get().copied();
        state.insert(2, 0x8000_0000);
        assert!(Arc::ptr_eq(&state.0, &saved.0));
        assert_eq!(state.0.key.get().copied(), cached);
        state.insert(2, 0);
        assert_ne!(crate::d3d9::recording::hash(&state), before);
        assert_eq!(crate::d3d9::recording::hash(&saved), before);
        assert_eq!(saved[&2], 0x8000_0000);
        let subset = saved.capture(&BTreeSet::from([1]));
        assert_eq!(&*subset, &BTreeMap::from([(1, 0)]));
    }
}

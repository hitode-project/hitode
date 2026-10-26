//! Shared dense SDK state tables retain keys across unrelated writes. Validated
//! state selectors fit 0..256; ordered keys describe captured subsets.
use std::{
    collections::BTreeMap,
    hash::{Hash, Hasher},
    ops::Index,
    sync::{Arc, OnceLock},
};

#[derive(Clone)]
pub(in crate::d3d9) struct StateMap(Arc<Contents>);
#[derive(Clone, Default)]
struct Contents {
    values: Vec<u32>,
    present: [u64; 4],
    keys: Vec<i32>,
    key: OnceLock<u128>,
}
impl std::fmt::Debug for StateMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}
impl PartialEq for StateMap {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}
impl StateMap {
    pub(in crate::d3d9) fn get(&self, key: &i32) -> Option<&u32> {
        self.0
            .values
            .get(*key as usize)
            .filter(|_| self.0.present[*key as usize / 64] & (1 << (*key as usize % 64)) != 0)
    }
    fn mutable(&mut self) -> &mut Contents {
        let contents = Arc::make_mut(&mut self.0);
        contents.key.take();
        contents
    }
    #[cfg(test)]
    pub(in crate::d3d9) fn get_mut(&mut self, key: &i32) -> Option<&mut u32> {
        self.get(key)?;
        self.mutable().values.get_mut(*key as usize)
    }
    pub(in crate::d3d9) fn keys(&self) -> std::slice::Iter<'_, i32> {
        self.0.keys.iter()
    }
    fn iter(&self) -> impl Iterator<Item = (&i32, &u32)> {
        self.keys().map(|key| (key, &self.0.values[*key as usize]))
    }
    #[cfg(test)]
    pub(in crate::d3d9) fn values_mut(&mut self) -> impl Iterator<Item = &mut u32> {
        let contents = self.mutable();
        let present = contents.present;
        contents
            .values
            .iter_mut()
            .enumerate()
            .filter_map(move |(key, value)| {
                (present[key / 64] & (1 << (key % 64)) != 0).then_some(value)
            })
    }
    pub(in crate::d3d9) fn len(&self) -> usize {
        self.0.keys.len()
    }
    pub(in crate::d3d9) fn insert(&mut self, key: i32, value: u32) -> Option<u32> {
        let old = self.get(&key).copied();
        if old == Some(value) {
            return Some(value);
        }
        let fingerprint = self.fingerprint()
            ^ old.map_or(0, |v| crate::d3d9::recording::hash((key, v)))
            ^ crate::d3d9::recording::hash((key, value));
        let contents = self.mutable();
        contents.key = OnceLock::from(fingerprint);
        let index = key as usize;
        if index >= contents.values.len() {
            contents.values.resize(index + 1, 0);
        }
        if old.is_none() {
            let at = contents.keys.binary_search(&key).unwrap_err();
            contents.keys.insert(at, key);
            contents.present[index / 64] |= 1 << (index % 64);
        }
        contents.values[index] = value;
        old
    }
    pub(in crate::d3d9) fn capture(&self, keys: impl ExactSizeIterator<Item = i32>) -> Self {
        if keys.len() == self.len() {
            self.clone()
        } else {
            keys.map(|i| (i, self[&i])).collect()
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
        values.into_iter().collect()
    }
}
impl FromIterator<(i32, u32)> for StateMap {
    fn from_iter<T: IntoIterator<Item = (i32, u32)>>(iter: T) -> Self {
        let mut result = Self::default();
        for (key, value) in iter {
            result.insert(key, value);
        }
        result
    }
}
impl Index<&i32> for StateMap {
    type Output = u32;
    fn index(&self, key: &i32) -> &u32 {
        self.get(key).expect("captured fixed state")
    }
}
impl StateMap {
    fn fingerprint(&self) -> u128 {
        let key = *self.0.key.get_or_init(|| self.recompute());
        #[cfg(any(test, feature = "d3d9-counters"))]
        debug_assert_eq!(key, self.recompute());
        key
    }
    fn recompute(&self) -> u128 {
        self.iter().fold(0, |key, (slot, value)| {
            key ^ crate::d3d9::recording::hash((slot, value))
        })
    }
}
impl Hash for StateMap {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.fingerprint().hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    // spec: GFX-REC-2, GFX-SHADER-4
    #[test]
    fn captures_share_keys_and_mutations_detach_only_the_changed_category() {
        let mut state = StateMap::from(BTreeMap::from([(1, 0), (2, 0x8000_0000)]));
        let saved = state.capture(BTreeSet::from([1, 2]).into_iter());
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
        let subset = saved.capture(BTreeSet::from([1]).into_iter());
        assert_eq!(
            subset
                .iter()
                .map(|(&key, &value)| (key, value))
                .collect::<BTreeMap<_, _>>(),
            BTreeMap::from([(1, 0)])
        );
    }
}

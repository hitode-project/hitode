use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ResourceKey(pub [u64; 2], pub u64, pub u8);
#[derive(Default)]
pub(crate) struct Contents {
    indices: HashMap<ResourceKey, usize>,
    pub(crate) slots: Vec<(ResourceKey, Option<u128>, Option<u128>)>,
    pub(crate) touched: Vec<usize>,
    vacant: Vec<usize>,
}
impl Contents {
    pub(crate) fn resolve(&mut self, key: ResourceKey) -> usize {
        #[cfg(feature = "d3d9-counters")]
        lookup(1);
        *self.indices.entry(key).or_insert_with(|| {
            if let Some(n) = self.vacant.pop() {
                self.slots[n] = (key, None, None);
                n
            } else {
                self.slots.push((key, None, None));
                self.slots.len() - 1
            }
        })
    }
    pub(crate) fn get(&self, key: &ResourceKey) -> Option<&u128> {
        #[cfg(feature = "d3d9-counters")]
        lookup(1);
        self.slots.get(*self.indices.get(key)?)?.1.as_ref()
    }
    pub(crate) fn had_base(&self, key: &ResourceKey) -> bool {
        #[cfg(feature = "d3d9-counters")]
        lookup(1);
        self.indices
            .get(key)
            .is_some_and(|&n| self.slots[n].2.is_some())
    }
    pub(crate) fn restore(&mut self, key: ResourceKey, value: Option<u128>, had_base: bool) {
        let n = self.resolve(key);
        self.slots[n].1 = value;
        if !had_base {
            self.slots[n].2 = None;
        }
    }
    pub(crate) fn unchanged(&self) -> bool {
        self.touched.iter().all(|&n| {
            self.slots[n]
                .2
                .is_none_or(|base| self.slots[n].1 == Some(base))
        })
    }
    pub(crate) fn reset(&mut self) {
        for n in self.touched.drain(..) {
            self.slots[n].2 = None;
        }
    }
    pub(crate) fn forget(&mut self, matches: impl Fn(&ResourceKey) -> bool) -> bool {
        let before = self.indices.len();
        self.indices.retain(|k, &mut n| {
            if !matches(k) {
                return true;
            }
            self.slots[n].1 = None;
            self.slots[n].2 = None;
            self.vacant.push(n);
            false
        });
        self.indices.len() != before
    }
    #[cfg(test)]
    pub(crate) fn keys(&self) -> impl Iterator<Item = &ResourceKey> {
        self.slots.iter().filter(|s| s.1.is_some()).map(|s| &s.0)
    }
}

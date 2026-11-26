//! Ordered preparation reuse and backoff for unused target histories.
use super::{Arc, HashMap, damage, passes};

pub(crate) struct Cache<K, E> {
    targets: HashMap<K, Sequence<E>>,
    /// Targets whose retained entries were dropped unused. Their draws record
    /// directly until `until`; then one frame retains entries for the next.
    resting: HashMap<K, Rest>,
    /// Frames finished, which rests count.
    frame: u64,
    pub(crate) disabled: bool,
}

/// The first frame after a rest and its span, which doubles while retained
/// entries keep going unused. The first waste rests for no frame, so one
/// unavailable history alone keeps reuse.
#[derive(Clone, Copy)]
struct Rest {
    until: u64,
    span: u64,
}

const MAX_REST: u64 = 64;

struct Sequence<E> {
    previous: Vec<Arc<E>>,
    next: Vec<Arc<E>>,
    index: HashMap<u128, Vec<usize>>,
    next_index: HashMap<u128, Vec<usize>>,
    cursor: usize,
    hits: u32,
}
#[derive(Clone)]
pub(crate) struct Saved {
    pub command: u128,
    pub attachment: u128,
    pub damage: Arc<damage::Saved>,
    pub pass: passes::Effects,
    pub destinations: Vec<Option<u128>>,
    pub outputs: Vec<u128>,
}
impl<K: Copy + Eq + std::hash::Hash, E> Cache<K, E> {
    /// Drops all entries; this frame's draws record directly.
    pub fn invalidate(&mut self) {
        for (target, s) in std::mem::take(&mut self.targets) {
            if s.hits == 0 {
                self.waste(target);
            }
        }
        self.disabled = true;
    }

    fn waste(&mut self, target: K) {
        let frame = self.frame;
        let rest = self.resting.entry(target).or_insert(Rest {
            until: 0,
            span: u64::MAX,
        });
        rest.span = match rest.span {
            u64::MAX => 0,
            0 => 1,
            span => (span * 2).min(MAX_REST),
        };
        rest.until = frame + 1 + rest.span;
    }
    #[cfg(test)]
    pub fn any_entry(&self, mut matches: impl FnMut(&E) -> bool) -> bool {
        self.targets
            .values()
            .any(|s| s.previous.iter().chain(&s.next).any(|entry| matches(entry)))
    }
    pub fn finish(&mut self) {
        let wasted: Vec<_> = self
            .targets
            .iter()
            .filter(|(_, s)| !s.previous.is_empty())
            .map(|(&target, s)| (target, s.hits == 0))
            .collect();
        for (target, unused) in wasted {
            if unused {
                self.waste(target);
            } else {
                self.resting.remove(&target);
            }
        }
        self.frame += 1;
        let (frame, resting) = (self.frame, &self.resting);
        self.targets.retain(|target, s| {
            s.previous = std::mem::take(&mut s.next);
            s.index = std::mem::take(&mut s.next_index);
            s.cursor = 0;
            s.hits = 0;
            !s.previous.is_empty() && resting.get(target).is_none_or(|r| r.until <= frame)
        });
        self.disabled = false;
    }

    pub fn rests(&self, target: K) -> bool {
        self.resting
            .get(&target)
            .is_some_and(|r| r.until > self.frame)
    }
    pub fn candidates<'a>(
        &'a self,
        target: K,
        same_state: impl Fn(&E) -> bool + 'a,
        key: impl FnOnce() -> u128,
        entry_key: impl Fn(&E) -> u128,
    ) -> (u128, bool, impl Iterator<Item = &'a Arc<E>>) {
        let sequence = self.targets.get(&target);
        let expected = sequence
            .and_then(|s| s.previous.get(s.cursor))
            .filter(|e| same_state(e));
        let index_key = expected.map_or_else(key, |e| entry_key(e));
        let others = sequence
            .into_iter()
            .flat_map(move |s| {
                s.index
                    .get(&index_key)
                    .into_iter()
                    .flatten()
                    .filter_map(|&n| s.previous.get(n))
                    .chain(
                        s.next_index
                            .get(&index_key)
                            .into_iter()
                            .flatten()
                            .filter_map(|&n| s.next.get(n)),
                    )
            })
            .filter(move |e| same_state(e));
        (
            index_key,
            expected.is_some(),
            expected.into_iter().chain(others),
        )
    }

    pub fn remember(
        &mut self,
        target: K,
        index_key: u128,
        expected: bool,
        origin: Option<&Arc<E>>,
        entry: Arc<E>,
    ) {
        let sequence = self.targets.entry(target).or_default();
        sequence.hits += u32::from(origin.is_some());
        if let Some(positions) = sequence.index.get(&index_key)
            && let Some(&n) = positions.iter().find(|&&n| {
                n >= sequence.cursor
                    && origin.is_some_and(|o| Arc::ptr_eq(&sequence.previous[n], o))
            })
        {
            sequence.cursor = n + 1;
        } else if expected {
            sequence.cursor += 1;
        }
        sequence
            .next_index
            .entry(index_key)
            .or_default()
            .push(sequence.next.len());
        sequence.next.push(entry);
    }
}
impl<K, E> Default for Cache<K, E> {
    fn default() -> Self {
        Self {
            targets: HashMap::default(),
            resting: HashMap::default(),
            frame: 0,
            disabled: false,
        }
    }
}
impl<E> Default for Sequence<E> {
    fn default() -> Self {
        Self {
            previous: Vec::new(),
            next: Vec::new(),
            index: HashMap::default(),
            next_index: HashMap::default(),
            cursor: 0,
            hits: 0,
        }
    }
}

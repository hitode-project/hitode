//! Allocation-free membership for validated SDK slots and state selectors.
use std::marker::PhantomData;

pub(super) trait Key: Copy {
    fn index(self) -> usize;
    fn from_index(index: usize) -> Self;
}
impl Key for usize {
    fn index(self) -> usize {
        self
    }
    fn from_index(index: usize) -> Self {
        index
    }
}
impl Key for i32 {
    fn index(self) -> usize {
        self as usize
    }
    fn from_index(index: usize) -> Self {
        index as i32
    }
}
impl Key for (usize, i32) {
    fn index(self) -> usize {
        self.0 * 32 + self.1 as usize
    }
    fn from_index(index: usize) -> Self {
        (index / 32, (index % 32) as i32)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Slots<T, const N: usize> {
    words: [u64; N],
    marker: PhantomData<T>,
}
impl<T, const N: usize> Default for Slots<T, N> {
    fn default() -> Self {
        Self {
            words: [0; N],
            marker: PhantomData,
        }
    }
}
impl<T: Key, const N: usize> Slots<T, N> {
    pub fn insert(&mut self, key: T) -> bool {
        let index = key.index();
        let mask = 1 << (index % 64);
        let word = &mut self.words[index / 64];
        let new = *word & mask == 0;
        *word |= mask;
        new
    }
    pub fn contains(&self, key: &T) -> bool {
        let index = key.index();
        self.words
            .get(index / 64)
            .is_some_and(|word| word & (1 << (index % 64)) != 0)
    }
    pub fn is_empty(&self) -> bool {
        self.words.iter().all(|&word| word == 0)
    }
    pub fn len(&self) -> usize {
        self.words
            .iter()
            .map(|word| word.count_ones() as usize)
            .sum()
    }
    pub fn iter(&self) -> Iter<T, N> {
        Iter {
            words: self.words,
            at: 0,
            remaining: self.len(),
            marker: PhantomData,
        }
    }
}
pub(super) struct Iter<T, const N: usize> {
    words: [u64; N],
    at: usize,
    remaining: usize,
    marker: PhantomData<T>,
}
impl<T: Key, const N: usize> Iterator for Iter<T, N> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        while self.at < N {
            let word = &mut self.words[self.at];
            if *word == 0 {
                self.at += 1;
                continue;
            }
            let bit = word.trailing_zeros() as usize;
            *word &= *word - 1;
            self.remaining -= 1;
            return Some(T::from_index(self.at * 64 + bit));
        }
        None
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}
impl<T: Key, const N: usize> ExactSizeIterator for Iter<T, N> {}
impl<T: Key, const N: usize> IntoIterator for &Slots<T, N> {
    type Item = T;
    type IntoIter = Iter<T, N>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
impl<T: Key, const N: usize> Extend<T> for Slots<T, N> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, values: I) {
        for key in values {
            self.insert(key);
        }
    }
}
impl<T: Key, const N: usize> FromIterator<T> for Slots<T, N> {
    fn from_iter<I: IntoIterator<Item = T>>(values: I) -> Self {
        let mut slots = Self::default();
        slots.extend(values);
        slots
    }
}

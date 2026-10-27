//! Per-page values indexed by page number. Large views insert and remove
//! thousands of consecutive pages, so storage is chunked by page number
//! instead of searched: each change is O(1) and a range skips empty chunks.

const CHUNK: usize = 512;

struct Chunk<T> {
    pages: [Option<T>; CHUNK],
    used: usize,
}

pub(super) struct PageMap<T> {
    chunks: Vec<Option<Box<Chunk<T>>>>,
}

impl<T> Default for PageMap<T> {
    fn default() -> Self {
        Self { chunks: Vec::new() }
    }
}

impl<T> PageMap<T> {
    pub fn get(&self, page: &usize) -> Option<&T> {
        self.chunks.get(page / CHUNK)?.as_ref()?.pages[page % CHUNK].as_ref()
    }

    pub fn get_mut(&mut self, page: &usize) -> Option<&mut T> {
        self.chunks.get_mut(page / CHUNK)?.as_mut()?.pages[page % CHUNK].as_mut()
    }

    pub fn contains_key(&self, page: &usize) -> bool {
        self.get(page).is_some()
    }

    pub fn insert(&mut self, page: usize, value: T) -> Option<T> {
        let index = page / CHUNK;
        if self.chunks.len() <= index {
            self.chunks.resize_with(index + 1, || None);
        }
        let chunk = self.chunks[index].get_or_insert_with(|| {
            Box::new(Chunk {
                pages: std::array::from_fn(|_| None),
                used: 0,
            })
        });
        let old = chunk.pages[page % CHUNK].replace(value);
        chunk.used += usize::from(old.is_none());
        old
    }

    pub fn remove(&mut self, page: &usize) -> Option<T> {
        let slot = self.chunks.get_mut(page / CHUNK)?;
        let chunk = slot.as_mut()?;
        let old = chunk.pages[page % CHUNK].take();
        if old.is_some() {
            chunk.used -= 1;
            if chunk.used == 0 {
                *slot = None;
            }
        }
        old
    }

    /// Present pages within `pages`, in page order.
    pub fn range(
        &self,
        pages: impl std::ops::RangeBounds<usize>,
    ) -> impl Iterator<Item = (usize, &T)> + '_ {
        use std::ops::Bound;
        let start = match pages.start_bound() {
            Bound::Included(&start) => start,
            Bound::Excluded(&start) => start + 1,
            Bound::Unbounded => 0,
        };
        let end = match pages.end_bound() {
            Bound::Included(&end) => end + 1,
            Bound::Excluded(&end) => end,
            Bound::Unbounded => usize::MAX,
        }
        .min(self.chunks.len() * CHUNK);
        let chunks = start / CHUNK..end.div_ceil(CHUNK);
        chunks.flat_map(move |index| {
            let chunk = self.chunks[index].as_deref();
            let base = index * CHUNK;
            let pages = start.max(base)..end.min(base + CHUNK);
            chunk.into_iter().flat_map(move |chunk| {
                pages
                    .clone()
                    .filter_map(move |page| Some((page, chunk.pages[page - base].as_ref()?)))
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_see_only_present_pages_across_chunks() {
        let mut map = PageMap::default();
        for page in [3, CHUNK - 1, CHUNK, 5 * CHUNK + 2] {
            assert!(map.insert(page, page * 10).is_none());
        }
        assert_eq!(map.insert(3, 31), Some(30));
        let all: Vec<_> = map.range(..).map(|(page, &value)| (page, value)).collect();
        assert_eq!(
            all,
            [
                (3, 31),
                (CHUNK - 1, (CHUNK - 1) * 10),
                (CHUNK, CHUNK * 10),
                (5 * CHUNK + 2, (5 * CHUNK + 2) * 10)
            ]
        );
        let middle: Vec<_> = map.range(4..=CHUNK).map(|(page, _)| page).collect();
        assert_eq!(middle, [CHUNK - 1, CHUNK]);
        assert_eq!(map.remove(&CHUNK), Some(CHUNK * 10));
        assert_eq!(map.remove(&(CHUNK - 1)), Some((CHUNK - 1) * 10));
        assert!(map.chunks[1].is_none(), "an emptied chunk is released");
        assert!(!map.contains_key(&CHUNK));
        *map.get_mut(&3).unwrap() += 1;
        assert_eq!(map.get(&3), Some(&32));
        assert_eq!(map.range(10_000_000..).count(), 0);
    }
}

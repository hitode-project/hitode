//! Ordinary-window denial bits. Each higher word marks nonempty child
//! words, so a range can find the first denied page without walking pages.
//! A change propagates only when a word becomes empty or nonempty.

use super::{EXEC, READ, SHARED, WRITE};
use std::cell::Cell;

pub const WINDOW_SPAN_WORDS: [usize; 4] = [1 << 14, 1 << 8, 1 << 2, 1];
const PAGES: usize = WINDOW_SPAN_WORDS[0] * 64;
const WORDS: usize =
    WINDOW_SPAN_WORDS[0] + WINDOW_SPAN_WORDS[1] + WINDOW_SPAN_WORDS[2] + WINDOW_SPAN_WORDS[3];

pub(super) struct WindowSpans {
    first: usize,
    tables: [Box<[Cell<u64>]>; 4],
}

impl WindowSpans {
    pub fn new(first: usize, classes: &[u8], plain: &[Cell<u64>]) -> Self {
        let spans = Self {
            first,
            tables: std::array::from_fn(|_| vec![Cell::new(u64::MAX); WORDS].into_boxed_slice()),
        };
        for (index, words) in plain.as_chunks::<3>().0.iter().enumerate() {
            let mut eligible = (words[0].get() | words[1].get()) & !words[2].get();
            while eligible != 0 {
                let page = index * 64 + eligible.trailing_zeros() as usize;
                spans.refresh(page, classes[page], plain);
                eligible &= eligible - 1;
            }
        }
        spans
    }

    pub fn pointers(&self) -> [*const u64; 4] {
        self.tables.each_ref().map(|table| table.as_ptr().cast())
    }

    pub fn refresh(&self, page: usize, class: u8, plain: &[Cell<u64>]) {
        let group = page / 64 * 3;
        let bit = 1 << (page % 64);
        let unwatched = plain[group + 2].get() & bit == 0;
        let read = unwatched && plain[group].get() & bit != 0 && class & (READ | SHARED) == READ;
        let write =
            unwatched && plain[group + 1].get() & bit != 0 && class & (WRITE | EXEC) == WRITE;
        let page = self.first + page;
        let reverse = PAGES - 1 - page;
        for (table, (page, denied)) in self.tables.iter().zip([
            (page, !read),
            (page, !write),
            (reverse, !read),
            (reverse, !write),
        ]) {
            Self::update(table, page, denied);
        }
    }

    fn update(table: &[Cell<u64>], mut index: usize, mut denied: bool) {
        let mut offset = 0;
        for size in WINDOW_SPAN_WORDS {
            let word = &table[offset + index / 64];
            let old = word.get();
            let bit = 1 << (index % 64);
            let new = old & !bit | if denied { bit } else { 0 };
            if old == new {
                break;
            }
            word.set(new);
            denied = new != 0;
            if denied == (old != 0) {
                break;
            }
            offset += size;
            index /= 64;
        }
    }
}

pub(super) fn denied_tables() -> [*const u64; 4] {
    static DENIED: [u64; WORDS] = [u64::MAX; WORDS];
    [DENIED.as_ptr(); 4]
}

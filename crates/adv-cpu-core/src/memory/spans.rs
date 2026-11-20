//! Ordinary-window denial bitmaps indexed by absolute guest page. A parent
//! bit is set when its child word contains a denial, so contiguous ranges
//! never need to walk their pages (A64-OPT-10).

use std::cell::Cell;

pub const WINDOW_SPAN_WORDS: [usize; 4] = [1 << 14, 1 << 8, 1 << 2, 1];
const WORDS: usize =
    WINDOW_SPAN_WORDS[0] + WINDOW_SPAN_WORDS[1] + WINDOW_SPAN_WORDS[2] + WINDOW_SPAN_WORDS[3];

pub(super) struct WindowSpans {
    read: Box<[Cell<u64>]>,
    write: Box<[Cell<u64>]>,
}

impl WindowSpans {
    pub fn new() -> Self {
        let denied = || vec![Cell::new(u64::MAX); WORDS].into_boxed_slice();
        Self {
            read: denied(),
            write: denied(),
        }
    }

    pub fn pointers(&self) -> [*const u64; 2] {
        [self.read.as_ptr().cast(), self.write.as_ptr().cast()]
    }

    pub fn update(&self, page: usize, read: bool, write: bool) {
        for (table, allowed) in [(&self.read, read), (&self.write, write)] {
            let mut index = page;
            let mut denied = !allowed;
            let mut level = 0;
            for words in WINDOW_SPAN_WORDS {
                let cell = &table[level + index / 64];
                let bit = 1 << (index % 64);
                let before = cell.get();
                let after = if denied { before | bit } else { before & !bit };
                if before == after {
                    break;
                }
                cell.set(after);
                if (before == 0) == (after == 0) {
                    break;
                }
                denied = after != 0;
                level += words;
                index /= 64;
            }
        }
    }
}

pub(super) fn denied() -> [*const u64; 2] {
    static TABLE: [u64; WORDS] = [u64::MAX; WORDS];
    [TABLE.as_ptr(); 2]
}

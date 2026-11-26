//! Reverse index for overlapping subresource bindings.
use super::{BoundView, ViewSlot};
use rustc_hash::FxHashMap;

#[derive(Clone, Copy)]
struct Link {
    previous: Option<ViewSlot>,
    next: Option<ViewSlot>,
}

pub(super) struct Index {
    first: FxHashMap<(u64, u64), ViewSlot>,
    links: Box<[Link]>,
}

impl Default for Index {
    fn default() -> Self {
        let empty = Link {
            previous: None,
            next: None,
        };
        Self {
            first: FxHashMap::with_capacity_and_hasher(ViewSlot::COUNT, Default::default()),
            links: vec![empty; ViewSlot::COUNT].into_boxed_slice(),
        }
    }
}

impl Index {
    pub(super) fn clear(&mut self) {
        self.first.clear();
    }

    pub(super) fn first(&self, resource: (u64, u64)) -> Option<ViewSlot> {
        self.first.get(&resource).copied()
    }

    pub(super) fn next(&self, slot: ViewSlot) -> Option<ViewSlot> {
        self.links[slot.index()].next
    }

    pub(super) fn insert(&mut self, resource: (u64, u64), slot: ViewSlot) {
        let next = self.first.insert(resource, slot);
        self.links[slot.index()] = Link {
            previous: None,
            next,
        };
        if let Some(next) = next {
            self.links[next.index()].previous = Some(slot);
        }
    }

    pub(super) fn remove(&mut self, resource: (u64, u64), slot: ViewSlot) {
        let Link { previous, next } = self.links[slot.index()];
        match (previous, next) {
            (Some(previous), _) => self.links[previous.index()].next = next,
            (None, Some(next)) => {
                self.first.insert(resource, next);
            }
            (None, None) => {
                self.first.remove(&resource);
            }
        }
        if let Some(next) = next {
            self.links[next.index()].previous = previous;
        }
    }
}

pub(super) fn conflict(new: &BoundView, old: &BoundView) -> bool {
    let (new, old) = (&new.desc, &old.desc);
    new.subresources().overlaps(old.subresources())
        && (new.writable_aspects().intersects(old.aspects())
            || old.writable_aspects().intersects(new.aspects()))
}

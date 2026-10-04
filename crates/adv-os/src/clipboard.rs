//! Process-local clipboard metadata. Memory payloads remain in common Objects.
use crate::global::Block;
use adv_machine::{CtxId, Identity};

#[derive(Clone, Copy, Debug)]
pub struct Open {
    pub thread: CtxId,
    pub window: u32,
}
#[derive(Clone, Copy, Debug)]
pub struct Data {
    pub format: u32,
    pub identity: Identity<Block>,
    /// Synthesized data is invalidated when explicit text/locale data changes.
    pub generated: bool,
}
#[derive(Clone, Copy, Debug)]
pub struct Viewer {
    pub window: u32,
}
#[derive(Default)]
pub struct Clipboard {
    pub open: Option<Open>,
    pub owner: u32,
    /// Each item corresponds to one explicit Objects strong reference.
    pub data: Vec<Data>,
    /// Format IDs identify names; they are not resource handles or owners.
    names: Vec<Vec<u16>>,
    pub viewers: Vec<Viewer>,
    pub sequence: u32,
    pub changed: bool,
}
impl Clipboard {
    pub fn name(&self, format: u32) -> Option<&[u16]> {
        self.names
            .get(format.checked_sub(0xc000)? as usize)
            .map(Vec::as_slice)
    }
    pub fn register(&mut self, name: Vec<u16>) -> Option<u32> {
        if name.is_empty() || name.len() > 255 {
            return None;
        }
        if let Some(index) = self.names.iter().position(|old| {
            old.len() == name.len()
                && old.iter().zip(&name).all(|(&a, &b)| {
                    crate::nls::unit_case(a, true) == crate::nls::unit_case(b, true)
                })
        }) {
            return Some(0xc000 + index as u32);
        }
        if self.names.len() >= 0x4000 {
            return None;
        }
        let id = 0xc000 + self.names.len() as u32;
        self.names.push(name);
        Some(id)
    }
    pub fn head(&self) -> u32 {
        self.viewers.first().map_or(0, |v| v.window)
    }
    pub fn touch(&mut self) {
        self.changed = true;
        self.sequence = self.sequence.wrapping_add(1);
        if self.sequence == 0 {
            self.sequence = 1;
        }
    }
    /// Remove borrowed window links and return one chain notification.
    pub fn remove_window(&mut self, window: u32) -> Option<(u32, u32)> {
        if self.open.is_some_and(|o| o.window == window) {
            self.open = None;
        }
        if self.owner == window {
            self.owner = 0;
        }
        let index = self.viewers.iter().position(|v| v.window == window)?;
        let next = self.viewers.get(index + 1).map_or(0, |v| v.window);
        self.viewers.remove(index);
        Some((self.head(), next))
    }
}

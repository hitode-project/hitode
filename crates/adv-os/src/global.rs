//! Global/Local memory handles: one typed namespace over the shared guest heaps.
//! This stores metadata only. Byte allocation/free is performed through Heaps.
use crate::Heaps;
use adv_cpu::backend::Guest;
use adv_machine::Vm;
use adv_machine::{Identity, Namespace, Object, Objects};
use std::collections::BTreeMap;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreeError {
    Invalid,
    Owned,
}
#[derive(Clone, Debug)]
pub struct Block {
    pub handle: u32,
    pub pointer: u32,
    pub size: u32,
    pub movable: bool,
    pub locks: u8,
    pub attributes: u32,
    /// Ownership transferred to the virtual clipboard; API callers cannot free it.
    pub clipboard: bool,
}
#[derive(Default)]
pub struct Globals {
    namespace: Option<Namespace>,
    /// Borrowed identities only; Objects remains the sole payload owner.
    pointers: BTreeMap<u32, Identity<Block>>,
}
impl Globals {
    /// Retire a unique application or clipboard owner through the same heap and
    /// object table. A borrower cannot release bytes held by another owner.
    pub fn free(
        &mut self,
        objects: &mut Objects,
        heaps: &mut Heaps,
        cpu: &mut dyn Guest,
        vm: &mut Vm,
        handle: u32,
        clipboard: bool,
    ) -> Result<(), FreeError> {
        let id = self.identity(objects, handle).ok_or(FreeError::Invalid)?;
        let block = objects.owned(id).ok_or(FreeError::Invalid)?.clone();
        if block.clipboard != clipboard || objects.strong_count(id) != Some(1) {
            return Err(FreeError::Owned);
        }
        if block.pointer != 0 && !heaps.free(cpu, vm, 0, block.pointer) {
            return Err(FreeError::Invalid);
        }
        self.remove(objects, handle)
            .expect("validated sole alias owner");
        Ok(())
    }
    fn namespace(&mut self, objects: &mut Objects) -> Option<Namespace> {
        if self.namespace.is_none() {
            self.namespace = objects.new_namespace_in(0x6800_0004, 0x6aff_fffc);
        }
        self.namespace
    }
    pub fn insert(&mut self, objects: &mut Objects, mut block: Block) -> Option<u32> {
        let pointer = block.pointer;
        let ns = self.namespace(objects)?;
        let handle = if block.movable {
            objects.insert_host_in(ns, block)?
        } else {
            let handle = block.pointer;
            block.handle = handle;
            if !objects.insert_host_at(ns, handle, block) {
                return None;
            }
            handle
        };
        self.get_mut(objects, handle).unwrap().handle = handle;
        if pointer != 0 {
            self.pointers
                .insert(pointer, self.identity(objects, handle).unwrap());
        }
        Some(handle)
    }
    pub fn identity(&self, objects: &Objects, handle: u32) -> Option<Identity<Block>> {
        objects.lookup_alias(self.namespace?, handle)
    }
    pub fn get<'a>(&self, objects: &'a Objects, handle: u32) -> Option<&'a Block> {
        objects.owned(self.identity(objects, handle)?)
    }
    pub fn get_mut<'a>(&self, objects: &'a mut Objects, handle: u32) -> Option<&'a mut Block> {
        objects.owned_mut(self.identity(objects, handle)?)
    }
    /// Commit a successful heap move/discard together with the borrowed index.
    /// Other metadata mutations must not change pointer or handle directly.
    pub fn set_storage(&mut self, objects: &mut Objects, handle: u32, pointer: u32, size: u32) {
        let id = self.identity(objects, handle).expect("live allocation");
        let block = objects.owned_mut(id).unwrap();
        let previous = block.pointer;
        block.pointer = pointer;
        block.size = size;
        if self.pointers.get(&previous) == Some(&id) {
            self.pointers.remove(&previous);
        }
        if pointer != 0 {
            self.pointers.insert(pointer, id);
        }
    }
    pub fn from_pointer(&self, objects: &Objects, pointer: u32) -> Option<u32> {
        if pointer == 0 {
            return None;
        }
        let id = *self.pointers.get(&pointer)?;
        let block = objects.owned(id)?;
        (block.pointer == pointer && self.identity(objects, block.handle) == Some(id))
            .then_some(block.handle)
    }
    pub fn remove(&mut self, objects: &mut Objects, handle: u32) -> Option<Block> {
        let id = self.identity(objects, handle)?;
        if objects.strong_count(id) != Some(1) {
            return None;
        }
        let pointer = objects.owned(id)?.pointer;
        // Fixed -> movable publishes a new identity at the same pointer first.
        // Retiring the old alias must preserve that replacement's index.
        if self.pointers.get(&pointer) == Some(&id) {
            self.pointers.remove(&pointer);
        }
        match objects.close_alias(self.namespace?, handle)?? {
            Object::Host(value) => value.downcast::<Block>().ok().map(|v| *v),
            _ => unreachable!("typed memory namespace"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_machine::ObjectLimits;
    fn block(pointer: u32, movable: bool) -> Block {
        Block {
            handle: 0,
            pointer,
            size: 16,
            movable,
            locks: 0,
            attributes: 0,
            clipboard: false,
        }
    }
    #[test]
    fn borrowed_pointer_index_survives_failed_and_committed_conversion_then_move_discard_remove() {
        let mut objects = Objects::with_limits(ObjectLimits {
            objects: 1,
            ..Default::default()
        });
        let mut globals = Globals::default();
        let fixed = globals.insert(&mut objects, block(0x10000, false)).unwrap();
        assert!(globals.insert(&mut objects, block(0x10000, true)).is_none());
        assert_eq!(globals.from_pointer(&objects, 0x10000), Some(fixed));
        assert_eq!(
            objects.strong_count(globals.identity(&objects, fixed).unwrap()),
            Some(1)
        );
        globals.remove(&mut objects, fixed).unwrap();
        assert_eq!(globals.from_pointer(&objects, 0x10000), None);

        let mut objects = Objects::default();
        let mut globals = Globals::default();
        let fixed = globals.insert(&mut objects, block(0x10000, false)).unwrap();
        let movable = globals.insert(&mut objects, block(0x10000, true)).unwrap();
        globals.remove(&mut objects, fixed).unwrap();
        assert_eq!(globals.from_pointer(&objects, 0x10000), Some(movable));
        globals.set_storage(&mut objects, movable, 0x20000, 64);
        assert_eq!(globals.from_pointer(&objects, 0x10000), None);
        assert_eq!(globals.from_pointer(&objects, 0x20000), Some(movable));
        globals.set_storage(&mut objects, movable, 0, 0);
        assert_eq!(globals.from_pointer(&objects, 0x20000), None);
        globals.set_storage(&mut objects, movable, 0x30000, 16);
        globals.remove(&mut objects, movable).unwrap();
        assert_eq!(globals.from_pointer(&objects, 0x30000), None);
        assert_eq!(objects.object_count(), 0);
        assert!(globals.pointers.is_empty());
    }
}

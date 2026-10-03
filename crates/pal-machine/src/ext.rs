//! Per-subsystem state (win32, pal, ...) keyed by type, so the core doesn't depend on them.

use std::any::{Any, TypeId};
use std::collections::HashMap;

#[derive(Default)]
pub struct Ext {
    map: HashMap<TypeId, Box<dyn Any>>,
}

impl Ext {
    pub fn insert<T: 'static>(&mut self, v: T) {
        self.map.insert(TypeId::of::<T>(), Box::new(v));
    }

    pub fn get<T: 'static>(&self) -> &T {
        self.map
            .get(&TypeId::of::<T>())
            .and_then(|b| b.downcast_ref())
            .unwrap_or_else(|| panic!("{} not installed", std::any::type_name::<T>()))
    }

    pub fn get_mut<T: 'static>(&mut self) -> &mut T {
        self.map
            .get_mut(&TypeId::of::<T>())
            .and_then(|b| b.downcast_mut())
            .unwrap_or_else(|| panic!("{} not installed", std::any::type_name::<T>()))
    }

    pub fn try_get_mut<T: 'static>(&mut self) -> Option<&mut T> {
        self.map.get_mut(&TypeId::of::<T>()).and_then(|b| b.downcast_mut())
    }

    pub fn try_get<T: 'static>(&self) -> Option<&T> {
        self.map.get(&TypeId::of::<T>()).and_then(|b| b.downcast_ref())
    }
}

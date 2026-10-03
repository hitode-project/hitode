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
        self.map
            .get_mut(&TypeId::of::<T>())
            .and_then(|b| b.downcast_mut())
    }

    pub fn try_get<T: 'static>(&self) -> Option<&T> {
        self.map
            .get(&TypeId::of::<T>())
            .and_then(|b| b.downcast_ref())
    }

    /// The `T` state, installing a default one first if there is none.
    pub fn get_or_default<T: Default + 'static>(&mut self) -> &mut T {
        self.map
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Box::new(T::default()))
            .downcast_mut()
            .expect("state stored under its own type")
    }

    /// Two different states at once (a subsystem and the module table, say). `B` is
    /// installed with its default if missing; `A` must be installed.
    pub fn get2_mut<A: 'static, B: Default + 'static>(&mut self) -> (&mut A, &mut B) {
        assert_ne!(TypeId::of::<A>(), TypeId::of::<B>(), "get2_mut of one type");
        self.get_or_default::<B>();
        let [a, b] = self
            .map
            .get_disjoint_mut([&TypeId::of::<A>(), &TypeId::of::<B>()]);
        let a = a
            .and_then(|a| a.downcast_mut())
            .unwrap_or_else(|| panic!("{} not installed", std::any::type_name::<A>()));
        (a, b.and_then(|b| b.downcast_mut()).expect("just installed"))
    }
}

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

    /// Borrow two already installed states without constructing either one.
    /// Distinct TypeIds are required even when a state is missing.
    pub fn try_get2_installed_mut<A: 'static, B: 'static>(&mut self) -> Option<(&mut A, &mut B)> {
        assert_ne!(
            TypeId::of::<A>(),
            TypeId::of::<B>(),
            "two mutable borrows of one type"
        );
        let [a, b] = self
            .map
            .get_disjoint_mut([&TypeId::of::<A>(), &TypeId::of::<B>()]);
        Some((a?.downcast_mut()?, b?.downcast_mut()?))
    }

    pub fn get2_installed_mut<A: 'static, B: 'static>(&mut self) -> (&mut A, &mut B) {
        self.try_get2_installed_mut().unwrap_or_else(|| {
            panic!(
                "{} and {} must be installed",
                std::any::type_name::<A>(),
                std::any::type_name::<B>()
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct A(u32);
    struct B(u32);
    #[test]
    fn installed_borrows_need_no_default_and_do_not_create_missing_states() {
        let mut ext = Ext::default();
        ext.insert(A(3));
        assert!(ext.try_get2_installed_mut::<A, B>().is_none());
        assert!(ext.try_get::<B>().is_none());
        ext.insert(B(7));
        let (a, b) = ext.get2_installed_mut::<A, B>();
        a.0 += b.0;
        b.0 += 1;
        assert_eq!(ext.get::<A>().0, 10);
        assert_eq!(ext.get::<B>().0, 8);
    }
    #[test]
    #[should_panic(expected = "two mutable borrows of one type")]
    fn duplicate_type_is_rejected_before_lookup() {
        let mut ext = Ext::default();
        let _ = ext.try_get2_installed_mut::<A, A>();
    }
}

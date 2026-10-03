//! ABI layer between the machine's thunk mechanism and the API implementations.
//!
//! Implementations are registered by `(module, name)` with their calling convention; the
//! machine binds a guest thunk to whatever [`Registry`] returns. Signatures, argument
//! decoding and stubs for unimplemented exports will be generated here.

use std::collections::HashMap;

pub use adv_machine::{Conv, Handler, HostFn};

/// Implementations by (lowercased module, name).
#[derive(Default)]
pub struct Registry {
    fns: HashMap<(String, String), HostFn>,
}

impl Registry {
    pub fn add(&mut self, module: &str, name: &str, conv: Conv, handler: Handler) {
        self.fns.insert(
            (module.to_ascii_lowercase(), name.to_owned()),
            HostFn {
                conv,
                handler,
                trace: false,
            },
        );
    }

    pub fn add_traced(&mut self, module: &str, name: &str, conv: Conv, handler: Handler) {
        self.add(module, name, conv, handler);
        self.set_trace(module, name, true);
    }

    pub fn set_trace(&mut self, module: &str, name: &str, on: bool) {
        if let Some(f) = self
            .fns
            .get_mut(&(module.to_ascii_lowercase(), name.to_owned()))
        {
            f.trace = on;
        }
    }

    pub fn get(&self, module: &str, name: &str) -> Option<HostFn> {
        self.fns
            .get(&(module.to_ascii_lowercase(), name.to_owned()))
            .copied()
    }

    pub fn len(&self) -> usize {
        self.fns.len()
    }

    /// Whether any function is registered for `module` (lowercased file name).
    pub fn has_module(&self, module: &str) -> bool {
        let m = module.to_ascii_lowercase();
        self.fns.keys().any(|(k, _)| *k == m)
    }

    pub fn is_empty(&self) -> bool {
        self.fns.is_empty()
    }
}

impl adv_machine::Resolver for Registry {
    fn host_fn(&self, module: &str, name: &str) -> Option<HostFn> {
        self.get(module, name)
    }

    fn provides(&self, module: &str) -> bool {
        self.has_module(module)
    }
}

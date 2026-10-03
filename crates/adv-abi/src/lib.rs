//! x86 calling conventions, generated signatures, typed guest values and API diagnostics.
//! API behavior belongs to adv-win32/adv-dx; this crate never owns their state.
//!
//! Pointer parameters cannot accidentally become raw integer arguments:
//! ```compile_fail
//! #[adv_abi::win32(kernel32, name = "QueryPerformanceCounter", level = implement)]
//! fn invalid(cx: &mut adv_abi::Env<'_>, pointer: u32) -> i32 { 1 }
//! ```
//! Return values are checked against the same metadata:
//! ```compile_fail
//! #[adv_abi::win32(kernel32, name = "GetTickCount", level = implement)]
//! fn invalid(cx: &mut adv_abi::Env<'_>) -> adv_abi::GuestPtr<u32> {
//!     adv_abi::GuestPtr::new(0)
//! }
//! ```
use std::collections::HashMap;

pub use adv_abi_macros::win32;
pub use adv_machine::{Conv, Env, Handler, HostFn, Outcome};

pub mod constants;
pub mod guest;
pub mod ordinals;
pub mod signature;
pub mod stub;
pub mod trace;
#[path = "../../../data/abi/structs.rs"]
pub mod types;

pub use guest::{AnsiStr, Arguments, GuestArg, GuestPtr, GuestReturn, Handle, WideStr};
pub use signature::{AbiKind, Parameter, Signature};
pub use stub::{ApiDiagnostics, RunMode, StubCall};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImplementationLevel {
    Implement,
    State,
    SafeFail,
}

/// Emitted by #[win32]; registration lists only reference this descriptor.
#[derive(Clone, Copy)]
pub struct Api {
    pub module: &'static str,
    pub name: &'static str,
    pub conv: Conv,
    pub handler: Handler,
    pub level: ImplementationLevel,
}

struct Entry {
    host: HostFn,
    level: Option<ImplementationLevel>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureConflict {
    pub module: String,
    pub name: String,
    pub registered: Conv,
    pub metadata: Option<Conv>,
}

/// Implementations by (lowercased DLL, case-sensitive export).
/// Legacy registrations coexist with generated adapters and are audited separately.
pub struct Registry {
    fns: HashMap<(String, String), Entry>,
    mode: RunMode,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            fns: HashMap::new(),
            mode: RunMode::Strict,
        }
    }
}

impl Registry {
    pub fn add(&mut self, module: &str, name: &str, conv: Conv, handler: Handler) {
        self.add_with_data(module, name, conv, handler, 0);
    }

    /// Bind immutable, machine-opaque data once rather than resolving per call.
    pub fn add_with_data(
        &mut self,
        module: &str,
        name: &str,
        conv: Conv,
        handler: Handler,
        data: u64,
    ) {
        self.fns.insert(
            (module.to_ascii_lowercase(), name.to_owned()),
            Entry {
                host: HostFn {
                    conv,
                    handler,
                    trace: false,
                    data,
                },
                level: None,
            },
        );
    }

    pub fn register(&mut self, api: Api) {
        self.add(api.module, api.name, api.conv, api.handler);
        self.fns
            .get_mut(&(api.module.to_ascii_lowercase(), api.name.to_owned()))
            .expect("just inserted")
            .level = Some(api.level);
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
            f.host.trace = on;
        }
    }
    pub fn mode(&self) -> RunMode {
        self.mode
    }
    pub fn set_mode(&mut self, mode: RunMode) {
        self.mode = mode;
    }

    pub fn get(&self, module: &str, name: &str) -> Option<HostFn> {
        let canonical = self.canonical_name(module, name);
        let name = canonical.unwrap_or(name);
        if let Some(entry) = self
            .fns
            .get(&(module.to_ascii_lowercase(), name.to_owned()))
        {
            return Some(entry.host);
        }
        let signature = signature::lookup(module, name)?;
        let index = signature::signatures()
            .iter()
            .position(|s| core::ptr::eq(s, signature))
            .expect("signature in table");
        Some(HostFn {
            conv: signature.conv.unwrap_or(Conv::Cdecl { argc: 0 }),
            handler: stub::dispatch,
            trace: true,
            data: index as u64,
        })
    }

    /// An explicit legacy ordinal implementation wins over an SDK alias.
    pub fn canonical_name(&self, module: &str, name: &str) -> Option<&'static str> {
        let module = module.to_ascii_lowercase();
        if self.fns.contains_key(&(module.clone(), name.to_owned())) {
            return None;
        }
        ordinals::lookup(&module, name)
    }

    pub fn level(&self, module: &str, name: &str) -> Option<ImplementationLevel> {
        let name = self.canonical_name(module, name).unwrap_or(name);
        self.fns
            .get(&(module.to_ascii_lowercase(), name.to_owned()))
            .and_then(|e| e.level)
    }
    pub fn is_implemented(&self, module: &str, name: &str) -> bool {
        let name = self.canonical_name(module, name).unwrap_or(name);
        self.fns
            .contains_key(&(module.to_ascii_lowercase(), name.to_owned()))
    }
    pub fn len(&self) -> usize {
        self.fns.len()
    }
    pub fn is_empty(&self) -> bool {
        self.fns.is_empty()
    }

    /// Include generated DLLs so static imports and GetProcAddress resolve stubs.
    pub fn has_module(&self, module: &str) -> bool {
        let module = module.to_ascii_lowercase();
        self.fns.keys().any(|(m, _)| *m == module)
            || signature::signatures().iter().any(|s| s.module == module)
    }

    /// Names for the synthetic PE export writer, with implementations and stubs together.
    pub fn exports(&self, module: &str) -> Vec<String> {
        let module = module.to_ascii_lowercase();
        let mut names: Vec<_> = signature::signatures()
            .iter()
            .filter(|s| s.module == module)
            .map(|s| s.name.to_owned())
            .collect();
        names.extend(
            self.fns
                .keys()
                .filter(|(m, _)| *m == module)
                .map(|(_, n)| n.clone()),
        );
        names.sort();
        names.dedup();
        names
    }

    pub fn signature_conflicts(&self) -> Vec<SignatureConflict> {
        let mut conflicts = Vec::new();
        for ((module, name), entry) in &self.fns {
            let metadata = if let Some((interface, method)) = name.split_once("::") {
                signature::lookup_com(interface, method)
            } else {
                signature::lookup(module, name)
            };
            if let Some(metadata) = metadata {
                if metadata.conv != Some(entry.host.conv) {
                    conflicts.push(SignatureConflict {
                        module: module.clone(),
                        name: name.clone(),
                        registered: entry.host.conv,
                        metadata: metadata.conv,
                    });
                }
            }
        }
        conflicts.sort_by(|a, b| (&a.module, &a.name).cmp(&(&b.module, &b.name)));
        conflicts
    }

    /// APIs whose legacy hand-written signature has no authoritative metadata entry.
    pub fn missing_signatures(&self) -> Vec<String> {
        let mut missing: Vec<_> = self
            .fns
            .keys()
            .filter(|(module, name)| {
                if let Some((interface, method)) = name.split_once("::") {
                    signature::lookup_com(interface, method).is_none()
                } else {
                    signature::lookup(module, name).is_none()
                }
            })
            .map(|(module, name)| format!("{module}!{name}"))
            .collect();
        missing.sort();
        missing
    }

    pub fn coverage(&self) -> Coverage {
        let mut coverage = Coverage {
            generated_exports: signature::signatures().len(),
            ..Coverage::default()
        };
        for entry in self.fns.values() {
            match entry.level {
                Some(ImplementationLevel::Implement) => coverage.implement += 1,
                Some(ImplementationLevel::State) => coverage.state += 1,
                Some(ImplementationLevel::SafeFail) => coverage.safe_fail += 1,
                None => coverage.legacy += 1,
            }
        }
        coverage
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Coverage {
    pub generated_exports: usize,
    pub implement: usize,
    pub state: usize,
    pub safe_fail: usize,
    pub legacy: usize,
}

impl adv_machine::Resolver for Registry {
    fn canonical_name(&self, module: &str, name: &str) -> Option<String> {
        Registry::canonical_name(self, module, name).map(str::to_owned)
    }
    fn host_fn(&self, module: &str, name: &str) -> Option<HostFn> {
        self.get(module, name)
    }
    fn provides(&self, module: &str) -> bool {
        self.has_module(module)
    }
    fn implemented(&self, module: &str, name: &str) -> bool {
        self.is_implemented(module, name)
    }
}

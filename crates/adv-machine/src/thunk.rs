//! Host function binding and the guest thunk page.
//!
//! Every imported function the loader resolves (and every GetProcAddress result) gets an
//! hookable thunk with a Win32 prologue and an explicit trap token. Unknown functions
//! still get a thunk, bound to a handler
//! that fails loudly with the function's name, so the run stops at the first unimplemented
//! call that is actually used.

use std::any::Any;
use std::collections::HashMap;

use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{Guest, MemFault};

use crate::layout::{THUNK_CAP, thunk_addr};
use crate::{Env, Outcome};

pub type Handler = fn(&mut Env<'_>) -> Outcome;

/// Calling convention of a host function, as seen by the guest caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conv {
    /// Caller cleans up; `argc` is used for tracing only.
    Cdecl { argc: u8 },
    /// Caller cleans up; only the mandatory prefix is captured in the trace.
    CdeclVariadic { fixed: u8 },
    /// Callee pops `argc * 4` bytes.
    Stdcall { argc: u8 },
}

impl Conv {
    pub fn argc(self) -> u8 {
        match self {
            Conv::Cdecl { argc } | Conv::Stdcall { argc } => argc,
            Conv::CdeclVariadic { fixed } => fixed,
        }
    }
}

#[derive(Clone, Copy)]
pub struct HostFn {
    pub conv: Conv,
    pub handler: Handler,
    /// Opaque binding data interpreted by the registered handler, never the CPU.
    /// Bound once when resolving an import, avoiding runtime name lookups.
    pub data: u64,
    /// Record calls in the trace.
    pub trace: bool,
}

/// Where host implementations come from. The machine only binds what it is given; naming,
/// signatures and stubs belong to the ABI layer (`adv_abi::Registry`).
pub trait Resolver: Any {
    /// Host implementation of `module!name` (`module` is a lowercase file name), if any.
    fn host_fn(&self, module: &str, name: &str) -> Option<HostFn>;
    /// Whether the host implements anything in `module` (lowercase file name).
    fn provides(&self, module: &str) -> bool;
    /// Whether this binding has an implementation rather than a generated stub.
    fn implemented(&self, module: &str, name: &str) -> bool {
        self.host_fn(module, name).is_some()
    }
}

pub struct Thunk {
    pub module: String,
    pub name: String,
    pub f: HostFn,
    pub implemented: bool,
}

/// Index 0 is reserved: the pad guest callees return to after an [`Outcome::Call`].
pub const RETURN_PAD: u32 = 0;

pub struct ThunkTable {
    resolver: Box<dyn Resolver>,
    /// Machine-internal functions, bound ahead of the resolver.
    internal: HashMap<(String, String), HostFn>,
    thunks: Vec<Thunk>,
    by_name: HashMap<(String, String), u32>,
}

fn unimplemented(env: &mut Env<'_>) -> Outcome {
    let t = env.sys.thunks.get(env.thunk);
    Outcome::Fail(format!("unimplemented: {}!{}", t.module, t.name))
}

fn return_pad(_: &mut Env<'_>) -> Outcome {
    unreachable!("the return pad is dispatched by the machine")
}

impl ThunkTable {
    pub fn new(resolver: Box<dyn Resolver>) -> Self {
        let pad = Thunk {
            module: String::new(),
            name: "<return pad>".into(),
            f: HostFn {
                conv: Conv::Cdecl { argc: 0 },
                handler: return_pad,
                data: 0,
                trace: false,
            },
            implemented: true,
        };
        Self {
            resolver,
            internal: HashMap::new(),
            thunks: vec![pad],
            by_name: HashMap::new(),
        }
    }

    pub fn resolver(&self) -> &dyn Resolver {
        self.resolver.as_ref()
    }

    /// The resolver as its concrete type, to add implementations after the machine exists.
    pub fn resolver_mut<T: Resolver>(&mut self) -> Option<&mut T> {
        (self.resolver.as_mut() as &mut dyn Any).downcast_mut()
    }

    /// Bind a machine-internal function; it takes precedence over the resolver.
    pub(crate) fn add_internal(&mut self, module: &str, name: &str, f: HostFn) {
        self.internal
            .insert((module.to_ascii_lowercase(), name.to_owned()), f);
    }

    /// A hook library can relocate the entry instructions or the entire thunk. The token
    /// identifies the handler independently of the address where `int 0x2e` executes.
    pub(crate) fn emit(index: u32, conv: Conv) -> Vec<u8> {
        let mut a = Asm::new(thunk_addr(index));
        // mov edi,edi; push ebp; mov ebp,esp; pop ebp. The first five bytes are
        // ordinary relocatable instructions; the frame is gone before argument decoding.
        a.bytes
            .extend_from_slice(&[0x8b, 0xff, 0x55, 0x8b, 0xec, 0x5d]);
        a.push_imm(index);
        a.trap();
        match conv {
            Conv::Cdecl { .. } | Conv::CdeclVariadic { .. } => a.ret(),
            Conv::Stdcall { argc } => a.ret_n(u16::from(argc) * 4),
        };
        a.bytes
    }

    /// Thunk address for `module!name`, creating (and writing) it on first use.
    pub fn resolve(
        &mut self,
        cpu: &mut dyn Guest,
        module: &str,
        name: &str,
    ) -> Result<u32, MemFault> {
        let key = (module.to_ascii_lowercase(), name.to_owned());
        if let Some(&i) = self.by_name.get(&key) {
            return Ok(thunk_addr(i));
        }
        let index = self.thunks.len() as u32;
        assert!(index < THUNK_CAP, "thunk table full");
        let found = self
            .internal
            .get(&key)
            .copied()
            .or_else(|| self.resolver.host_fn(&key.0, name));
        let (f, implemented) = match found {
            Some(f) => (
                f,
                self.internal.contains_key(&key) || self.resolver.implemented(&key.0, name),
            ),
            // Unknown convention: it fails before returning, so `ret` is never reached.
            None => (
                HostFn {
                    conv: Conv::Cdecl { argc: 0 },
                    handler: unimplemented,
                    data: 0,
                    trace: false,
                },
                false,
            ),
        };
        cpu.write(thunk_addr(index), &Self::emit(index, f.conv))?;
        self.thunks.push(Thunk {
            module: key.0.clone(),
            name: name.to_owned(),
            f,
            implemented,
        });
        self.by_name.insert(key, index);
        Ok(thunk_addr(index))
    }

    pub fn get(&self, index: u32) -> &Thunk {
        &self.thunks[index as usize]
    }

    pub fn len(&self) -> u32 {
        self.thunks.len() as u32
    }

    pub fn is_empty(&self) -> bool {
        self.thunks.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (u32, &Thunk)> {
        self.thunks.iter().enumerate().map(|(i, t)| (i as u32, t))
    }
}

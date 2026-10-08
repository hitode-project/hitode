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

use crate::layout::{GUEST_CODE_BASE, GUEST_CODE_SIZE, THUNK_CAP, THUNK_STRIDE, thunk_addr};
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
    /// Native-semantics guest code that runs instead of the trap when it can.
    pub guest: Option<&'static GuestCode>,
}

/// Position-independent x86 code a thunk jumps to, entered with the caller's
/// frame (return address at `[esp]`). It leaves through its own `ret`, or by
/// jumping to the host implementation with the frame unchanged.
#[derive(Debug)]
pub struct GuestCode {
    pub code: &'static [u8],
    /// Offsets of the rel32 fields of jumps to the host implementation.
    pub host: &'static [usize],
    /// rel32 fields of jumps to other bindings, as `(offset, module, name)`.
    pub links: &'static [(usize, &'static str, &'static str)],
}

/// Where host implementations come from. The machine only binds what it is given; naming,
/// signatures and stubs belong to the ABI layer (`adv_abi::Registry`).
pub trait Resolver: Any {
    /// Optional symbol alias. The machine keeps aliases on the same hookable thunk.
    fn canonical_name(&self, _module: &str, _name: &str) -> Option<String> {
        None
    }
    /// Host implementation of `module!name` (`module` is a lowercase file name), if any.
    fn host_fn(&self, module: &str, name: &str) -> Option<HostFn>;
    /// Declarative guest data storage, never a callable host function.
    fn host_data(&self, _module: &str, _name: &str) -> Option<&[u8]> {
        None
    }
    /// A known data declaration may lack an initialized host implementation.
    fn data_size(&self, module: &str, name: &str) -> Option<u32> {
        self.host_data(module, name).map(|bytes| bytes.len() as u32)
    }
    /// Whether the host implements anything in `module` (lowercase file name).
    fn provides(&self, module: &str) -> bool;
    /// Explicit whole-module provider selected by assembly after a capability
    /// check. Ordinary API registrations never override a shipped guest module.
    fn replaces_module(&self, _module: &str) -> bool {
        false
    }
    /// Whether this binding has an implementation rather than a generated stub.
    fn implemented(&self, module: &str, name: &str) -> bool {
        self.host_fn(module, name).is_some() || self.host_data(module, name).is_some()
    }
}

pub struct Thunk {
    pub module: String,
    pub name: String,
    pub f: HostFn,
    pub implemented: bool,
    /// An import or dynamic lookup requested this binding. Merely publishing a
    /// module's export table must not count every diagnostic export as imported.
    pub requested: bool,
    /// Address of the pointer the entry jumps through: to the guest code, or to
    /// the trap after it when guest code is off.
    pub guest_slot: Option<u32>,
    /// Registered host transport identity, distinct from a numeric system service.
    pub transport_token: Option<TransportToken>,
    pristine: Vec<u8>,
}

/// Opaque word transported on the guest stack. Its identity survives ordinary
/// instruction relocation and arithmetic reconstruction; it is not an API index.
/// Tokens belong to the current process's binding table. Copying code within
/// that process preserves its identity; cross-process stub transfer is not modeled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TransportToken(u32);

const TRANSPORT_TAG: u32 = 0xad80_0000;

impl TransportToken {
    fn for_binding(index: u32) -> Self {
        debug_assert!(index > EXCEPTION_DISPATCH && index < THUNK_CAP);
        debug_assert_eq!(TRANSPORT_TAG & (THUNK_CAP - 1), 0);
        Self(TRANSPORT_TAG | index)
    }

    pub fn word(self) -> u32 {
        self.0
    }
}

impl Thunk {
    /// Immutable guest body, including when the optional direct route is off.
    /// A handler may resume here when CPU execution is part of its semantics.
    pub fn guest_body(&self) -> Option<u32> {
        self.guest_slot.map(|slot| slot + 4)
    }
}

/// Index 0 is reserved: the pad guest callees return to after an [`Outcome::Call`].
pub const RETURN_PAD: u32 = 0;
/// Diagnostic binding for a CPU interruption; never emitted as a callable API.
pub(crate) const EXCEPTION_DISPATCH: u32 = 1;

pub struct ThunkTable {
    resolver: Box<dyn Resolver>,
    /// Machine-internal functions, bound ahead of the resolver.
    internal: HashMap<(String, String), HostFn>,
    thunks: Vec<Thunk>,
    by_name: HashMap<String, HashMap<String, u32>>,
    /// Only successfully published ordinary bindings authorize transport.
    by_transport: HashMap<TransportToken, u32>,
    /// Next free byte of the guest code arena.
    guest_next: u32,
    guest_code: bool,
}

fn unimplemented(env: &mut Env<'_>) -> Outcome {
    let t = env.sys.thunks.get(env.thunk);
    let api = format!("{}!{}", t.module, t.name);
    let caller = env.caller();
    let reason = "host implementation is unavailable (machine thunk fallback)";
    // This fallback terminates the run, so one stderr/trace record needs no
    // additional diagnostic owner or ABI-layer dependency.
    eprintln!("warning: {api}: {reason} (guest caller {caller:#010x})");
    env.sys
        .trace
        .warning(env.context, env.sys.frame, caller, &api, reason, None);
    Outcome::Fail(format!("unimplemented: {api}"))
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
                guest: None,
            },
            implemented: true,
            requested: false,
            guest_slot: None,
            transport_token: None,
            pristine: {
                let mut pad = Asm::new(thunk_addr(RETURN_PAD));
                pad.trap().hlt();
                pad.bytes
            },
        };
        Self {
            resolver,
            internal: HashMap::new(),
            thunks: vec![
                pad,
                Thunk {
                    module: String::new(),
                    name: "<exception dispatch>".into(),
                    f: HostFn {
                        conv: Conv::Cdecl { argc: 0 },
                        handler: return_pad,
                        data: 0,
                        trace: false,
                        guest: None,
                    },
                    implemented: true,
                    requested: false,
                    guest_slot: None,
                    transport_token: None,
                    pristine: Vec::new(),
                },
            ],
            by_name: HashMap::new(),
            by_transport: HashMap::new(),
            guest_next: GUEST_CODE_BASE,
            guest_code: true,
        }
    }

    pub fn resolver(&self) -> &dyn Resolver {
        self.resolver.as_ref()
    }

    /// Immutable encoding of an existing binding, independent of guest hook
    /// writes to either its published entry or the original thunk page.
    pub fn pristine_entry(&self, index: u32) -> Vec<u8> {
        self.pristine_entry_bytes(index).to_vec()
    }

    /// Borrow the encoding retained when this binding was first published.
    pub fn pristine_entry_bytes(&self, index: u32) -> &[u8] {
        &self.get(index).pristine
    }

    /// Inspect current bytes without allocating or trusting the mutable thunk page.
    pub fn pristine_entry_matches(
        &self,
        cpu: &dyn Guest,
        index: u32,
        address: u32,
    ) -> Result<bool, MemFault> {
        let expected = self.pristine_entry_bytes(index);
        let mut live = [0; THUNK_STRIDE as usize];
        cpu.read(address, &mut live[..expected.len()])?;
        Ok(&live[..expected.len()] == expected)
    }

    /// Exact published name in a canonical module; no alias or new binding lookup.
    pub fn published_index(&self, module: &str, name: &str) -> Option<u32> {
        self.by_name.get(module)?.get(name).copied()
    }

    /// Guest code replaces host traps for later bindings, and existing entries
    /// switch at once. Off, every call reaches the host (e.g. a full API trace).
    pub fn set_guest_code(&mut self, cpu: &mut dyn Guest, on: bool) -> Result<(), MemFault> {
        self.guest_code = on;
        for (index, thunk) in self.thunks.iter().enumerate() {
            if let Some(slot) = thunk.guest_slot {
                let target = if on {
                    slot + 4
                } else {
                    Self::trap_addr(index as u32)
                };
                cpu.write(slot, &target.to_le_bytes())?;
            }
        }
        Ok(())
    }

    /// The `push transport-token; int` pair of a thunk with guest code.
    fn trap_addr(index: u32) -> u32 {
        thunk_addr(index) + 12
    }

    /// Install `guest` behind a pointer slot in the arena. `links` are the
    /// bound addresses of `guest.links`.
    fn place(
        &mut self,
        cpu: &mut dyn Guest,
        index: u32,
        guest: &GuestCode,
        links: &[u32],
    ) -> Result<u32, MemFault> {
        let slot = self.guest_next;
        let body = slot + 4;
        let end = (body + guest.code.len() as u32 + 15) & !15;
        assert!(
            end <= GUEST_CODE_BASE + GUEST_CODE_SIZE,
            "guest code arena full"
        );
        let mut code = guest.code.to_vec();
        let host = guest.host.iter().map(|&at| (at, Self::trap_addr(index)));
        let links = guest
            .links
            .iter()
            .zip(links)
            .map(|(&(at, ..), &target)| (at, target));
        for (at, target) in host.chain(links) {
            let next = body + at as u32 + 4;
            code[at..at + 4].copy_from_slice(&target.wrapping_sub(next).to_le_bytes());
        }
        let target = if self.guest_code {
            body
        } else {
            Self::trap_addr(index)
        };
        cpu.write(slot, &target.to_le_bytes())?;
        cpu.write(body, &code)?;
        self.guest_next = end;
        Ok(slot)
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
    pub(crate) fn emit(
        index: u32,
        token: TransportToken,
        conv: Conv,
        guest_slot: Option<u32>,
    ) -> Vec<u8> {
        let mut a = Asm::new(thunk_addr(index));
        // mov edi,edi; push ebp; mov ebp,esp; pop ebp. The first five bytes are
        // ordinary relocatable instructions; the frame is gone before argument decoding.
        a.bytes
            .extend_from_slice(&[0x8b, 0xff, 0x55, 0x8b, 0xec, 0x5d]);
        if let Some(slot) = guest_slot {
            // jmp [slot]: absolute, so copies of the entry in synthetic system
            // images reach the same code.
            a.bytes.extend_from_slice(&[0xff, 0x25]);
            a.bytes.extend_from_slice(&slot.to_le_bytes());
        }
        a.push_imm(token.word());
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
        self.bind(cpu, module, name, true)
    }

    /// Publish a relocatable entry without recording an import or feature probe.
    pub fn publish(
        &mut self,
        cpu: &mut dyn Guest,
        module: &str,
        name: &str,
    ) -> Result<u32, MemFault> {
        self.bind(cpu, module, name, false)
    }

    fn bind(
        &mut self,
        cpu: &mut dyn Guest,
        module: &str,
        name: &str,
        requested: bool,
    ) -> Result<u32, MemFault> {
        let mut key = (module.to_ascii_lowercase(), name.to_owned());
        if !self.internal.contains_key(&key)
            && let Some(canonical) = self.resolver.canonical_name(&key.0, name)
        {
            key.1 = canonical;
        }
        let name = key.1.as_str();
        if let Some(size) = self.resolver.data_size(&key.0, name) {
            // Data imports are resolved by the image loader to RW/NX storage.
            return Err(MemFault { addr: 0, len: size });
        }
        if let Some(i) = self.published_index(&key.0, &key.1) {
            self.thunks[i as usize].requested |= requested;
            return Ok(thunk_addr(i));
        }
        let found = self
            .internal
            .get(&key)
            .copied()
            .or_else(|| self.resolver.host_fn(&key.0, name));
        // Bindings guest code jumps to come first: they take thunk indices too.
        let mut links = Vec::new();
        for &(_, module, name) in found.and_then(|f| f.guest).map_or(&[][..], |g| g.links) {
            links.push(self.bind(cpu, module, name, false)?);
        }
        let index = self.thunks.len() as u32;
        assert!(index < THUNK_CAP, "thunk table full");
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
                    guest: None,
                },
                false,
            ),
        };
        let guest_slot = match f.guest {
            Some(guest) => Some(self.place(cpu, index, guest, &links)?),
            None => None,
        };
        let token = TransportToken::for_binding(index);
        let pristine = Self::emit(index, token, f.conv, guest_slot);
        assert!(pristine.len() <= THUNK_STRIDE as usize);
        cpu.write(thunk_addr(index), &pristine)?;
        self.thunks.push(Thunk {
            module: key.0.clone(),
            name: name.to_owned(),
            f,
            implemented,
            requested,
            guest_slot,
            transport_token: Some(token),
            pristine,
        });
        self.by_name.entry(key.0).or_default().insert(key.1, index);
        self.by_transport.insert(token, index);
        Ok(thunk_addr(index))
    }

    /// A prefix or a coincidental small index never authorizes dispatch. The
    /// exact opaque word must name an ordinary binding owned by this table.
    pub fn transport_index(&self, word: u32) -> Option<u32> {
        if word & !(THUNK_CAP - 1) != TRANSPORT_TAG {
            return None;
        }
        let index = *self.by_transport.get(&TransportToken(word))?;
        (index > EXCEPTION_DISPATCH).then_some(index)
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

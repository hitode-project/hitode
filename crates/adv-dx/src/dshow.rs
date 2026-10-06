//! DirectShow's empty filter graph, over the common COM/object owner.
//!
//! Graph activation, identity, enumeration, stop/state and event configuration
//! are available without a media backend. Filter installation, decoding and
//! rendering deliberately fail: this front neither decodes media nor invents
//! playback/completion. A future device backend belongs in adv-device.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use adv_abi::{Api, Arguments, GuestPtr, ImplementationLevel, Registry, Signature, WideStr};
use adv_cpu::backend::{Backend, GuestExt, Prot};
use adv_machine::{Env, Machine, Object, Outcome, Wait};
use adv_os::{
    Heaps,
    com::{ClassFactories, Com, Iid, QueryError},
};

const E_POINTER: u32 = 0x80004003;
const E_INVALIDARG: u32 = 0x80070057;
const E_NOTIMPL: u32 = 0x80004001;
const E_NOINTERFACE: u32 = 0x80004002;
const E_OUTOFMEMORY: u32 = 0x8007000e;
const E_ABORT: u32 = 0x80004004;
const CLASS_E_NOAGGREGATION: u32 = 0x80040110;
const VFW_E_NOT_CONNECTED: u32 = 0x80040209;
const VFW_E_NOT_FOUND: u32 = 0x80040216;
const VFW_E_CANNOT_CONNECT: u32 = 0x80040217;
const VFW_E_CANNOT_RENDER: u32 = 0x80040218;

// SDK 10.0.26100.0: shared/uuids.h, um/strmif.h, um/control.h.
pub const CLSID_FILTERGRAPH: Iid = [
    0xb3, 0xeb, 0x36, 0xe4, 0x4f, 0x52, 0xce, 0x11, 0x9f, 0x53, 0, 0x20, 0xaf, 0x0b, 0xa7, 0x70,
];
const fn graph_iid(first: u8) -> Iid {
    [
        first, 0x68, 0xa8, 0x56, 0xd4, 0x0a, 0xce, 0x11, 0xb0, 0x3a, 0, 0x20, 0xaf, 0x0b, 0xa7,
        0x70,
    ]
}
const IID_GRAPH: Iid = graph_iid(0xa9);
const IID_FILTER_GRAPH: Iid = graph_iid(0x9f);
const IID_CONTROL: Iid = graph_iid(0xb1);
const IID_EVENT: Iid = graph_iid(0xb6);
const IID_EVENT_EX: Iid = graph_iid(0xc0);
const IID_AUDIO: Iid = graph_iid(0xb3);
const IID_WINDOW: Iid = graph_iid(0xb4);
const IID_ENUM: Iid = graph_iid(0x93);
const IID_SEEKING: Iid = [
    0x80, 0x38, 0xb7, 0x36, 0xc8, 0xc2, 0xcf, 0x11, 0x8b, 0x46, 0, 0x80, 0x5f, 0x6c, 0xef, 0x60,
];
const IID_DISPATCH: Iid = [0, 4, 2, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];
const GRAPH_INTERFACES: [(&str, Iid); 6] = [
    ("IGraphBuilder", IID_GRAPH),
    ("IMediaControl", IID_CONTROL),
    ("IMediaEventEx", IID_EVENT_EX),
    ("IMediaSeeking", IID_SEEKING),
    ("IBasicAudio", IID_AUDIO),
    ("IVideoWindow", IID_WINDOW),
];

#[derive(Default)]
struct Graph {
    event: u32,
    notify: [u32; 3],
    notify_flags: u32,
    canceled_events: BTreeSet<u32>,
    aborted: bool,
}
enum Payload {
    Graph(Graph),
    EmptyEnumerator,
}
struct State {
    com: Com,
    tables: BTreeMap<&'static str, u32>,
}
struct Binding {
    signature: &'static Signature,
    name: &'static str,
}

/// All stack widths and slots come from the pinned Win32Metadata projection.
/// One data-driven adapter also covers inherited methods and explicit failures.
fn bindings() -> &'static [Binding] {
    static BINDINGS: OnceLock<Vec<Binding>> = OnceLock::new();
    BINDINGS.get_or_init(|| {
        adv_abi::signature::com_signatures()
            .iter()
            .filter(|s| {
                GRAPH_INTERFACES.iter().any(|(name, _)| *name == s.module)
                    || s.module == "IEnumFilters"
            })
            .map(|signature| Binding {
                name: Box::leak(
                    format!("{}::{}", signature.module, signature.name).into_boxed_str(),
                ),
                signature,
            })
            .collect()
    })
}
fn level(signature: &Signature) -> ImplementationLevel {
    use ImplementationLevel::{Implement, SafeFail, State};
    match signature.name {
        "QueryInterface" | "AddRef" | "Release" | "EnumFilters" | "FindFilterByName"
        | "RemoveFilter" | "Next" | "Skip" | "Reset" | "Clone" | "GetEventHandle" | "GetEvent"
        | "WaitForCompletion" | "FreeEventParams" => Implement,
        "GetTypeInfoCount"
        | "Stop"
        | "StopWhenReady"
        | "GetState"
        | "SetDefaultSyncSource"
        | "Abort"
        | "ShouldOperationContinue"
        | "SetNotifyWindow"
        | "SetNotifyFlags"
        | "GetNotifyFlags"
        | "CancelDefaultHandling"
        | "RestoreDefaultHandling"
        | "GetCapabilities"
        | "CheckCapabilities" => State,
        _ => SafeFail,
    }
}
pub fn register(registry: &mut Registry) {
    for (index, binding) in bindings().iter().enumerate() {
        registry.register_with_data(
            Api {
                module: "quartz.dll",
                name: binding.name,
                conv: binding
                    .signature
                    .conv
                    .expect("projected DirectShow stdcall"),
                handler: dispatch,
                level: level(binding.signature),
            },
            index as u64,
        );
    }
}
/// Assembly only registers the in-process class. Guest storage is created lazily.
pub fn install<B: Backend>(machine: &mut Machine<B>) -> Result<(), String> {
    machine
        .ext
        .get_or_default::<ClassFactories>()
        .register(CLSID_FILTERGRAPH, create_graph);
    Ok(())
}
fn alloc(env: &mut Env<'_>, size: u32) -> Option<u32> {
    let heaps = env.ext.get_mut::<Heaps>();
    heaps.alloc(env.cpu, &mut env.sys.vm, heaps.process_heap, size, true)
}
fn free(env: &mut Env<'_>, pointer: u32) {
    let heaps = env.ext.get_mut::<Heaps>();
    heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, pointer);
}
fn initialize(env: &mut Env<'_>) -> bool {
    if env.ext.try_get::<State>().is_some() {
        return true;
    }
    let mut tables = BTreeMap::new();
    let result = (|| {
        for (interface, count) in [
            ("IGraphBuilder", 18),
            ("IMediaControl", 16),
            ("IMediaEventEx", 16),
            ("IMediaSeeking", 20),
            ("IBasicAudio", 11),
            ("IVideoWindow", 46),
            ("IEnumFilters", 7),
        ] {
            let methods: Vec<_> = bindings()
                .iter()
                .filter(|b| b.signature.module == interface)
                .collect();
            if methods.len() != count {
                return None;
            }
            let table = alloc(env, count as u32 * 4)?;
            tables.insert(interface, table);
            for (slot, method) in methods.iter().enumerate() {
                if method.signature.com_slot != Some(slot as u16) {
                    return None;
                }
                let thunk = env
                    .sys
                    .thunks
                    .publish(env.cpu, "quartz.dll", method.name)
                    .ok()?;
                env.cpu.write_u32(table + slot as u32 * 4, thunk).ok()?;
            }
        }
        let com = Com::new(&mut env.sys.objects)?;
        env.ext.insert(State {
            com,
            tables: tables.clone(),
        });
        Some(())
    })();
    if result.is_none() {
        for &table in tables.values() {
            free(env, table);
        }
    }
    result.is_some()
}
fn release(env: &mut Env<'_>, pointer: u32) -> u32 {
    let Some(com) = env.ext.try_get::<State>().map(|s| s.com) else {
        return 0;
    };
    let Some(released) = com.release::<Payload>(&mut env.sys.objects, pointer) else {
        return 0;
    };
    if let Some(destroyed) = released.destroyed {
        if let Payload::Graph(graph) = destroyed.value
            && graph.event != 0
        {
            env.sys.objects.close(graph.event);
        }
        for pointer in destroyed.interfaces {
            free(env, pointer);
        }
    }
    released.references
}
fn interface(env: &mut Env<'_>, name: &str) -> Option<u32> {
    let table = *env.ext.get::<State>().tables.get(name)?;
    let pointer = alloc(env, 4)?;
    if env.cpu.write_u32(pointer, table).is_err() {
        free(env, pointer);
        return None;
    }
    Some(pointer)
}
fn new_graph(env: &mut Env<'_>) -> Option<u32> {
    if !initialize(env) {
        return None;
    }
    let com = env.ext.get::<State>().com;
    let pointer = interface(env, "IGraphBuilder")?;
    let Some(id) = com.create(
        &mut env.sys.objects,
        pointer,
        IID_GRAPH,
        Payload::Graph(Graph::default()),
    ) else {
        free(env, pointer);
        return None;
    };
    for (name, iid) in GRAPH_INTERFACES.into_iter().skip(1) {
        let Some(other) = interface(env, name) else {
            release(env, pointer);
            return None;
        };
        if !com.register_interface(&mut env.sys.objects, id, iid, other) {
            free(env, other);
            release(env, pointer);
            return None;
        }
        let base = match name {
            "IMediaControl" => Some(IID_DISPATCH),
            "IMediaEventEx" => Some(IID_EVENT),
            _ => None,
        };
        if let Some(base) = base
            && !com.register_interface(&mut env.sys.objects, id, base, other)
        {
            release(env, pointer);
            return None;
        }
    }
    if !com.register_interface(&mut env.sys.objects, id, IID_FILTER_GRAPH, pointer) {
        release(env, pointer);
        return None;
    }
    Some(pointer)
}
fn empty_enumerator(env: &mut Env<'_>, output: u32) -> u32 {
    if !clear(env, output, 4) {
        return E_POINTER;
    }
    let Some(pointer) = interface(env, "IEnumFilters") else {
        return E_OUTOFMEMORY;
    };
    let com = env.ext.get::<State>().com;
    if com
        .create(
            &mut env.sys.objects,
            pointer,
            IID_ENUM,
            Payload::EmptyEnumerator,
        )
        .is_none()
    {
        free(env, pointer);
        return E_OUTOFMEMORY;
    }
    if GuestPtr::<u32>::new(output)
        .write(env.cpu, pointer)
        .is_err()
    {
        release(env, pointer);
        return E_POINTER;
    }
    0
}
fn create_graph(env: &mut Env<'_>, iid: Iid, outer: u32, output: u32) -> Outcome {
    if !clear(env, output, 4) {
        return Outcome::Return(E_POINTER);
    }
    if outer != 0 {
        return Outcome::Return(CLASS_E_NOAGGREGATION);
    }
    if !GRAPH_INTERFACES.iter().any(|(_, known)| *known == iid)
        && ![
            adv_os::com::IUNKNOWN,
            IID_FILTER_GRAPH,
            IID_DISPATCH,
            IID_EVENT,
        ]
        .contains(&iid)
    {
        adv_abi::warn_unavailable(
            env,
            "ole32.dll!CoCreateInstance",
            &format!("FilterGraph does not support IID bytes {iid:02x?}"),
        );
        return Outcome::Return(E_NOINTERFACE);
    }
    let Some(pointer) = new_graph(env) else {
        return Outcome::Return(E_OUTOFMEMORY);
    };
    let com = env.ext.get::<State>().com;
    let target = com
        .resolve_interface::<Payload>(&mut env.sys.objects, pointer, iid)
        .unwrap();
    if GuestPtr::<u32>::new(output).write(env.cpu, target).is_err() {
        release(env, pointer);
        return Outcome::Return(E_POINTER);
    }
    Outcome::Return(0)
}
fn clear(env: &mut Env<'_>, pointer: u32, size: u32) -> bool {
    if adv_abi::guest::check_range(env.cpu, pointer, size, Prot::WRITE).is_err() {
        return false;
    }
    // All uses are fixed SDK scalar/GUID outputs, at most sixteen bytes.
    size <= 16 && env.cpu.write(pointer, &[0; 16][..size as usize]).is_ok()
}
fn readable(env: &Env<'_>, pointer: u32, size: u32) -> bool {
    adv_abi::guest::check_range(env.cpu, pointer, size, Prot::READ).is_ok()
}
fn wide(env: &Env<'_>, pointer: u32) -> bool {
    WideStr(GuestPtr::new(pointer)).read(env.cpu, 32768).is_ok()
}
fn query(env: &mut Env<'_>, this: u32, requested: u32, output: u32) -> u32 {
    if !clear(env, output, 4) || !readable(env, requested, 16) {
        return E_POINTER;
    }
    let Ok(iid) = GuestPtr::<Iid>::new(requested).read(env.cpu) else {
        return E_POINTER;
    };
    let com = env.ext.get::<State>().com;
    match com.query::<Payload>(&mut env.sys.objects, this, iid) {
        Ok(target) => {
            if GuestPtr::<u32>::new(output).write(env.cpu, target).is_ok() {
                0
            } else {
                release(env, target);
                E_POINTER
            }
        }
        Err(QueryError::NoInterface) => {
            adv_abi::warn_unavailable(
                env,
                "quartz.dll!IUnknown::QueryInterface",
                &format!("DirectShow object does not support IID bytes {iid:02x?}"),
            );
            E_NOINTERFACE
        }
        Err(QueryError::ReferenceLimit) => E_OUTOFMEMORY,
        Err(QueryError::InvalidObject) => E_INVALIDARG,
    }
}
fn unavailable(env: &mut Env<'_>, binding: &Binding, reason: &str, error: u32) -> u32 {
    adv_abi::warn_unavailable(env, &format!("quartz.dll!{}", binding.name), reason);
    error
}
fn graph_mut<'a>(env: &'a mut Env<'_>, this: u32) -> Option<&'a mut Graph> {
    let com = env.ext.get::<State>().com;
    match com.get_mut::<Payload>(&mut env.sys.objects, this)? {
        Payload::Graph(graph) => Some(graph),
        _ => None,
    }
}
fn event_handle(env: &mut Env<'_>, this: u32) -> Option<u32> {
    let existing = graph_mut(env, this)?.event;
    if existing != 0 {
        return Some(existing);
    }
    let handle = env.sys.objects.insert(Object::Event {
        manual_reset: true,
        signaled: false,
    });
    if handle == 0 {
        return None;
    }
    graph_mut(env, this)?.event = handle;
    Some(handle)
}
fn event_timeout(env: &mut Env<'_>, this: u32, timeout: i32) -> Outcome {
    if timeout == 0 {
        return Outcome::Return(E_ABORT);
    }
    if timeout < -1 {
        return Outcome::Return(E_INVALIDARG);
    }
    let Some(handle) = event_handle(env, this) else {
        return Outcome::Return(E_OUTOFMEMORY);
    };
    let deadline =
        (timeout >= 0).then(|| env.sys.clock.now_us().saturating_add(timeout as u64 * 1000));
    // No producer exists for this empty graph. Real scheduler waits still honor
    // the requested timeout; the event is never signaled as fake EC_COMPLETE.
    Outcome::block(Wait::Object { handle, deadline }, |_, _| {
        Outcome::Return(E_ABORT)
    })
}
fn dispatch(env: &mut Env<'_>) -> Outcome {
    let index = env.sys.thunks.get(env.thunk).f.data as usize;
    let binding = &bindings()[index];
    let mut cursor = Arguments::new(env.cpu);
    let mut args = Vec::with_capacity(binding.signature.stack_words() as usize);
    for _ in 0..binding.signature.stack_words() {
        match cursor.next::<u32>() {
            Ok(word) => args.push(word),
            Err(fault) => return adv_abi::guest::memory_failure(fault),
        }
    }
    call(env, binding, &args)
}
fn call(env: &mut Env<'_>, binding: &Binding, a: &[u32]) -> Outcome {
    let method = binding.signature.name;
    let Some(com) = env.ext.try_get::<State>().map(|state| state.com) else {
        return Outcome::Return(E_INVALIDARG);
    };
    let Some(payload) = com.get::<Payload>(&env.sys.objects, a[0]) else {
        return Outcome::Return(E_INVALIDARG);
    };
    let enumerator = matches!(payload, Payload::EmptyEnumerator);
    if (binding.signature.module == "IEnumFilters") != enumerator {
        return Outcome::Return(E_NOINTERFACE);
    }
    let value = match method {
        "QueryInterface" => query(env, a[0], a[1], a[2]),
        "AddRef" => com
            .add_ref::<Payload>(&mut env.sys.objects, a[0])
            .unwrap_or(0),
        "Release" => release(env, a[0]),
        _ if enumerator => match method {
            "Next" => {
                if (a[1] != 1 && a[3] == 0)
                    || (a[1] != 0 && !clear(env, a[2], 4))
                    || (a[3] != 0 && !clear(env, a[3], 4))
                {
                    E_POINTER
                } else {
                    u32::from(a[1] != 0)
                }
            }
            "Skip" => {
                if a[1] == 0 {
                    0
                } else {
                    E_INVALIDARG
                }
            }
            "Reset" => 0,
            "Clone" => empty_enumerator(env, a[1]),
            _ => unreachable!("projected IEnumFilters method"),
        },
        "GetTypeInfoCount" => {
            if clear(env, a[1], 4) {
                0
            } else {
                E_POINTER
            }
        }
        "GetTypeInfo" => {
            if !clear(env, a[3], 4) {
                E_POINTER
            } else {
                unavailable(
                    env,
                    binding,
                    "DirectShow automation type information is unavailable",
                    E_NOTIMPL,
                )
            }
        }
        "GetIDsOfNames" | "Invoke" => unavailable(
            env,
            binding,
            "DirectShow IDispatch invocation is unavailable; direct vtable methods are supported",
            E_NOTIMPL,
        ),
        _ => match binding.signature.module {
            "IGraphBuilder" => graph_call(env, binding, a),
            "IMediaControl" => match method {
                "Stop" | "StopWhenReady" => 0,
                "GetState" => {
                    if clear(env, a[2], 4) {
                        0
                    } else {
                        E_POINTER
                    }
                } // State_Stopped
                "Run" | "Pause" => unavailable(
                    env,
                    binding,
                    "the graph has no connected media; playback is unavailable",
                    VFW_E_NOT_CONNECTED,
                ),
                "RenderFile" => render_file(env, binding, a[1], 0),
                "AddSourceFilter" | "get_FilterCollection" | "get_RegFilterCollection" => {
                    let output = if method == "AddSourceFilter" {
                        a[2]
                    } else {
                        a[1]
                    };
                    if !clear(env, output, 4) {
                        E_POINTER
                    } else {
                        unavailable(
                            env,
                            binding,
                            "DirectShow source filters and automation collections are unavailable",
                            E_NOTIMPL,
                        )
                    }
                }
                _ => unreachable!("projected IMediaControl method"),
            },
            "IMediaEventEx" => return event_call(env, binding, a),
            "IMediaSeeking" => seeking_call(env, binding, a),
            "IBasicAudio" | "IVideoWindow" => unavailable(
                env,
                binding,
                "the empty graph has no renderer to supply this interface's media properties",
                VFW_E_NOT_CONNECTED,
            ),
            _ => unreachable!("selected DirectShow interface"),
        },
    };
    Outcome::Return(value)
}
fn render_file(env: &mut Env<'_>, binding: &Binding, file: u32, playlist: u32) -> u32 {
    if !wide(env, file) || (playlist != 0 && !wide(env, playlist)) {
        return E_POINTER;
    }
    unavailable(
        env,
        binding,
        "no DirectShow media decoder or renderer backend is installed",
        VFW_E_CANNOT_RENDER,
    )
}
fn graph_call(env: &mut Env<'_>, binding: &Binding, a: &[u32]) -> u32 {
    match binding.signature.name {
        "EnumFilters" => empty_enumerator(env, a[1]),
        "FindFilterByName" => {
            if !clear(env, a[2], 4) || !wide(env, a[1]) {
                E_POINTER
            } else {
                VFW_E_NOT_FOUND
            }
        }
        "RemoveFilter" => {
            if readable(env, a[1], 4) {
                VFW_E_NOT_FOUND
            } else {
                E_POINTER
            }
        }
        "AddFilter" => {
            if !readable(env, a[1], 4) || (a[2] != 0 && !wide(env, a[2])) {
                E_POINTER
            } else {
                unavailable(
                    env,
                    binding,
                    "guest DirectShow filters cannot be attached without a media graph backend",
                    E_NOTIMPL,
                )
            }
        }
        "ConnectDirect" | "Connect" | "Reconnect" | "Disconnect" | "Render" => {
            if !readable(env, a[1], 4)
                || (["ConnectDirect", "Connect"].contains(&binding.signature.name)
                    && !readable(env, a[2], 4))
            {
                E_POINTER
            } else {
                unavailable(
                    env,
                    binding,
                    "DirectShow pin connection and media rendering are unavailable",
                    VFW_E_CANNOT_CONNECT,
                )
            }
        }
        "RenderFile" => render_file(env, binding, a[1], a[2]),
        "AddSourceFilter" => {
            if !clear(env, a[3], 4) || !wide(env, a[1]) {
                E_POINTER
            } else {
                unavailable(
                    env,
                    binding,
                    "no DirectShow source-filter backend is installed",
                    VFW_E_CANNOT_RENDER,
                )
            }
        }
        "SetDefaultSyncSource" => 0, // There are no filters requiring a clock.
        "SetLogFile" => unavailable(
            env,
            binding,
            "DirectShow graph logging to a guest handle is unavailable",
            E_NOTIMPL,
        ),
        "Abort" => {
            graph_mut(env, a[0]).unwrap().aborted = true;
            0
        }
        "ShouldOperationContinue" => {
            if graph_mut(env, a[0]).unwrap().aborted {
                E_ABORT
            } else {
                0
            }
        }
        _ => unreachable!("projected IGraphBuilder method"),
    }
}
fn event_call(env: &mut Env<'_>, binding: &Binding, a: &[u32]) -> Outcome {
    let value = match binding.signature.name {
        "GetEventHandle" => {
            if !clear(env, a[1], 4) {
                E_POINTER
            } else if let Some(event) = event_handle(env, a[0]) {
                GuestPtr::<u32>::new(a[1])
                    .write(env.cpu, event)
                    .map_or(E_POINTER, |_| 0)
            } else {
                E_OUTOFMEMORY
            }
        }
        "GetEvent" => {
            if ![a[1], a[2], a[3]]
                .iter()
                .all(|&output| clear(env, output, 4))
            {
                E_POINTER
            } else {
                return event_timeout(env, a[0], a[4] as i32);
            }
        }
        "WaitForCompletion" => {
            if !clear(env, a[2], 4) {
                E_POINTER
            } else {
                return event_timeout(env, a[0], a[1] as i32);
            }
        }
        "SetNotifyWindow" => {
            graph_mut(env, a[0]).unwrap().notify = [a[1], a[2], a[3]];
            0
        }
        "SetNotifyFlags" => {
            if a[1] & !1 != 0 {
                E_INVALIDARG
            } else {
                graph_mut(env, a[0]).unwrap().notify_flags = a[1];
                0
            }
        }
        "GetNotifyFlags" => {
            let flags = graph_mut(env, a[0]).unwrap().notify_flags;
            GuestPtr::<u32>::new(a[1])
                .write(env.cpu, flags)
                .map_or(E_POINTER, |_| 0)
        }
        "CancelDefaultHandling" | "RestoreDefaultHandling" => {
            if binding.signature.name == "CancelDefaultHandling" {
                graph_mut(env, a[0]).unwrap().canceled_events.insert(a[1]);
            } else {
                graph_mut(env, a[0]).unwrap().canceled_events.remove(&a[1]);
            }
            0
        }
        "FreeEventParams" => 0, // No event parameters were ever allocated.
        _ => unreachable!("projected IMediaEventEx method"),
    };
    Outcome::Return(value)
}
fn seeking_call(env: &mut Env<'_>, binding: &Binding, a: &[u32]) -> u32 {
    match binding.signature.name {
        "GetCapabilities" => {
            if clear(env, a[1], 4) {
                0
            } else {
                E_POINTER
            }
        }
        "CheckCapabilities" => {
            let Ok(requested) = GuestPtr::<u32>::new(a[1]).read(env.cpu) else {
                return E_POINTER;
            };
            if !clear(env, a[1], 4) {
                E_POINTER
            } else if requested == 0 {
                0
            } else {
                0x80004005
            }
        }
        _ => unavailable(
            env,
            binding,
            "the empty graph has no media duration, time format or seek provider",
            E_NOTIMPL,
        ),
    }
}

#[cfg(test)]
mod tests;

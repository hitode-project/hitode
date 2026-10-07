//! Virtual-thread TSF manager and document identities. Text-store editing and
//! services are unavailable; every exposed object uses the common COM owner.
use adv_abi::{GuestPtr, Handle, Registry, guest::check_range, win32};
use adv_cpu::backend::{Backend, GuestExt, Prot};
use adv_machine::{Env, Machine, Outcome};
use adv_os::{
    Heaps,
    com::{Com, ComIdentity, Destroyed, Iid, QueryError},
};
use std::collections::BTreeMap;

pub const CLSID_THREAD: Iid = [
    0x6b, 0x9e, 0x9a, 0x52, 0x87, 0x65, 0x23, 0x4f, 0xab, 0x9e, 0x9c, 0x7d, 0x68, 0x3e, 0x3c, 0x50,
];
pub const IID_THREAD: Iid = [
    0x01, 0xe8, 0x80, 0xaa, 0x21, 0x20, 0xd2, 0x11, 0x93, 0xe0, 0, 0x60, 0xb0, 0x67, 0xb8, 0x6e,
];
const IID_DOCUMENT: Iid = [
    0xf4, 0xe7, 0x80, 0xaa, 0x21, 0x20, 0xd2, 0x11, 0x93, 0xe0, 0, 0x60, 0xb0, 0x67, 0xb8, 0x6e,
];
const IID_CONTEXT: Iid = [
    0xfd, 0xe7, 0x80, 0xaa, 0x21, 0x20, 0xd2, 0x11, 0x93, 0xe0, 0, 0x60, 0xb0, 0x67, 0xb8, 0x6e,
];
const INVALID: i32 = 0x80070057u32 as i32;
const POINTER: i32 = 0x80004003u32 as i32;
const UNEXPECTED: i32 = 0x8000ffffu32 as i32;
const NO_INTERFACE: i32 = 0x80004002u32 as i32;
const OUT_OF_MEMORY: i32 = 0x8007000eu32 as i32;
const NOT_IMPLEMENTED: i32 = 0x80004001u32 as i32;
type Id = ComIdentity<Payload>;

enum Payload {
    Manager {
        thread: u32,
        activations: u32,
        client: u32,
    },
    Document {
        thread: u32,
        manager: Id,
        default: Option<Id>,
    },
    Context {
        thread: u32,
        document: Id,
    },
}
struct State {
    com: Com,
    // Class vtables survive individual documents, including guest-installed
    // hooks. Entries are never rebuilt when a temporary object is released.
    tables: [u32; 3],
    managers: BTreeMap<u32, Id>,
    next_client: u32,
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
    let mut tables = [0; 3];
    let result = (|| {
        for (index, (interface, slots)) in [
            ("ITfThreadMgr", 14u32),
            ("ITfDocumentMgr", 9),
            ("ITfContext", 18),
        ]
        .into_iter()
        .enumerate()
        {
            let signatures: Vec<_> = adv_abi::signature::com_signatures()
                .iter()
                .filter(|signature| signature.module == interface)
                .collect();
            if signatures.len() != slots as usize {
                return None;
            }
            let table = alloc(env, slots * 4)?;
            tables[index] = table;
            let mut occupied = 0u32;
            for signature in signatures {
                let slot = u32::from(signature.com_slot?);
                if slot >= slots || occupied & (1 << slot) != 0 {
                    return None;
                }
                occupied |= 1 << slot;
                let method = env
                    .sys
                    .thunks
                    .publish(
                        env.cpu,
                        "msctf.dll",
                        &format!("{interface}::{}", signature.name),
                    )
                    .ok()?;
                env.cpu.write_u32(table + slot * 4, method).ok()?;
            }
        }
        let com = Com::new(&mut env.sys.objects)?;
        env.ext.insert(State {
            com,
            tables,
            managers: BTreeMap::new(),
            next_client: 0,
        });
        Some(())
    })();
    if result.is_none() {
        for pointer in tables.into_iter().filter(|&pointer| pointer != 0) {
            free(env, pointer);
        }
    }
    result.is_some()
}
fn object(env: &mut Env<'_>, payload: Payload) -> Option<(u32, Id)> {
    if !initialize(env) {
        return None;
    }
    let (index, iid) = match &payload {
        Payload::Manager { .. } => (0, IID_THREAD),
        Payload::Document { .. } => (1, IID_DOCUMENT),
        Payload::Context { .. } => (2, IID_CONTEXT),
    };
    let pointer = alloc(env, 4)?;
    let state = env.ext.get::<State>();
    let com = state.com;
    if env.cpu.write_u32(pointer, state.tables[index]).is_err() {
        free(env, pointer);
        return None;
    }
    let Some(id) = com.create(&mut env.sys.objects, pointer, iid, payload) else {
        free(env, pointer);
        return None;
    };
    Some((pointer, id))
}
fn payload<'a>(env: &'a Env<'_>, this: u32) -> Option<&'a Payload> {
    env.ext.try_get::<State>()?.com.get(&env.sys.objects, this)
}
fn unavailable(env: &mut Env<'_>, reason: &str) -> i32 {
    let thunk = env.sys.thunks.get(env.thunk);
    let api = format!("{}!{}", thunk.module, thunk.name);
    adv_abi::warn_unavailable(env, &api, reason);
    NOT_IMPLEMENTED
}
fn on_thread(env: &mut Env<'_>, thread: u32) -> bool {
    if thread == env.context {
        true
    } else {
        unavailable(env, "TSF interfaces cannot be used across virtual threads");
        false
    }
}
fn writable(env: &Env<'_>, output: GuestPtr<u32>) -> bool {
    !output.is_null() && check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_ok()
}
fn destroy(env: &mut Env<'_>, destroyed: Destroyed<Payload>) {
    match destroyed.value {
        Payload::Manager { thread, .. } => {
            env.ext.get_mut::<State>().managers.remove(&thread);
        }
        Payload::Document {
            default: Some(default),
            ..
        } => {
            let com = env.ext.get::<State>().com;
            if let Some(Some(child)) = com.release_internal(&mut env.sys.objects, default) {
                destroy(env, child);
            }
        }
        _ => {}
    }
    for pointer in destroyed.interfaces {
        free(env, pointer);
    }
}
fn release(env: &mut Env<'_>, this: u32) -> u32 {
    let Some(state) = env.ext.try_get::<State>() else {
        return 0;
    };
    let Some(released) = state.com.release::<Payload>(&mut env.sys.objects, this) else {
        return 0;
    };
    if let Some(destroyed) = released.destroyed {
        destroy(env, destroyed);
    }
    released.references
}
fn add_ref(env: &mut Env<'_>, this: u32) -> u32 {
    env.ext
        .try_get::<State>()
        .and_then(|state| state.com.add_ref::<Payload>(&mut env.sys.objects, this))
        .unwrap_or(0)
}
fn query(env: &mut Env<'_>, this: u32, requested: GuestPtr<Iid>, output: GuestPtr<u32>) -> i32 {
    if !writable(env, output) {
        return POINTER;
    }
    let _ = output.write(env.cpu, 0);
    let Ok(iid) = requested.read(env.cpu) else {
        return POINTER;
    };
    let Some(state) = env.ext.try_get::<State>() else {
        return INVALID;
    };
    match state.com.query::<Payload>(&mut env.sys.objects, this, iid) {
        Ok(pointer) => {
            if output.write(env.cpu, pointer).is_ok() {
                0
            } else {
                release(env, pointer);
                POINTER
            }
        }
        Err(QueryError::InvalidObject) => INVALID,
        Err(QueryError::NoInterface) => NO_INTERFACE,
        Err(QueryError::ReferenceLimit) => OUT_OF_MEMORY,
    }
}
fn create(env: &mut Env<'_>, iid: Iid, outer: u32, output: u32) -> Outcome {
    let output = GuestPtr::<u32>::new(output);
    if !writable(env, output) {
        return Outcome::Return(POINTER as u32);
    }
    let _ = output.write(env.cpu, 0);
    if outer != 0 {
        return Outcome::Return(0x80040110);
    }
    if iid != adv_os::com::IUNKNOWN && iid != IID_THREAD {
        return Outcome::Return(NO_INTERFACE as u32);
    }
    if !initialize(env) {
        return Outcome::Return(OUT_OF_MEMORY as u32);
    }
    let state = env.ext.get::<State>();
    let existing = state
        .managers
        .get(&env.context)
        .and_then(|&id| state.com.canonical(&env.sys.objects, id));
    let pointer = if let Some(pointer) = existing {
        if add_ref(env, pointer) == 0 {
            return Outcome::Return(OUT_OF_MEMORY as u32);
        }
        pointer
    } else {
        let Some((pointer, id)) = object(
            env,
            Payload::Manager {
                thread: env.context,
                activations: 0,
                client: 0,
            },
        ) else {
            return Outcome::Return(OUT_OF_MEMORY as u32);
        };
        env.ext.get_mut::<State>().managers.insert(env.context, id);
        pointer
    };
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return Outcome::Return(POINTER as u32);
    }
    Outcome::Return(0)
}
pub(super) fn install<B: Backend>(machine: &mut Machine<B>) {
    machine
        .ext
        .get_or_default::<adv_os::com::ClassFactories>()
        .register(CLSID_THREAD, create);
}

macro_rules! unknown_methods {
    ($query:ident,$add:ident,$release:ident,$q:tt,$a:tt,$r:tt) => {
        #[win32(msctf, name=$q, level=implement)]
        fn $query(
            env: &mut Env<'_>,
            this: GuestPtr<()>,
            iid: GuestPtr<Iid>,
            output: GuestPtr<u32>,
        ) -> i32 {
            query(env, this.addr(), iid, output)
        }
        #[win32(msctf, name=$a, level=implement)]
        fn $add(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
            add_ref(env, this.addr())
        }
        #[win32(msctf, name=$r, level=implement)]
        fn $release(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
            release(env, this.addr())
        }
    };
}
unknown_methods!(
    thread_query,
    thread_add,
    thread_release,
    "ITfThreadMgr::QueryInterface",
    "ITfThreadMgr::AddRef",
    "ITfThreadMgr::Release"
);
unknown_methods!(
    document_query,
    document_add,
    document_release,
    "ITfDocumentMgr::QueryInterface",
    "ITfDocumentMgr::AddRef",
    "ITfDocumentMgr::Release"
);
unknown_methods!(
    context_query,
    context_add,
    context_release,
    "ITfContext::QueryInterface",
    "ITfContext::AddRef",
    "ITfContext::Release"
);

#[win32(msctf, name="ITfThreadMgr::Activate", level=implement)]
fn activate(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> i32 {
    if !writable(env, output) {
        return INVALID;
    }
    let Some(Payload::Manager {
        thread,
        activations,
        client,
    }) = payload(env, this.addr())
    else {
        return INVALID;
    };
    let (thread, count, mut client) = (*thread, *activations, *client);
    if !on_thread(env, thread) {
        return NOT_IMPLEMENTED;
    }
    let Some(count) = count.checked_add(1) else {
        return OUT_OF_MEMORY;
    };
    let state = env.ext.get_mut::<State>();
    if client == 0 {
        let Some(next) = state.next_client.checked_add(1) else {
            return OUT_OF_MEMORY;
        };
        state.next_client = next;
        client = next;
    }
    let com = state.com;
    let id = com
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    if count == 1 && !com.retain_internal(&mut env.sys.objects, id) {
        return OUT_OF_MEMORY;
    }
    let Some(Payload::Manager {
        activations,
        client: stored,
        ..
    }) = com.get_mut(&mut env.sys.objects, this.addr())
    else {
        unreachable!()
    };
    *activations = count;
    *stored = client;
    output.write(env.cpu, client).map_or(POINTER, |()| 0)
}
#[win32(msctf, name="ITfThreadMgr::Deactivate", level=implement)]
fn deactivate(env: &mut Env<'_>, this: GuestPtr<()>) -> i32 {
    let Some(Payload::Manager {
        thread,
        activations,
        ..
    }) = payload(env, this.addr())
    else {
        return INVALID;
    };
    let (thread, count) = (*thread, *activations);
    if !on_thread(env, thread) {
        return NOT_IMPLEMENTED;
    }
    let Some(count) = count.checked_sub(1) else {
        return UNEXPECTED;
    };
    let com = env.ext.get::<State>().com;
    let id = com
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    let Some(Payload::Manager { activations, .. }) = com.get_mut(&mut env.sys.objects, this.addr())
    else {
        unreachable!()
    };
    *activations = count;
    if count == 0
        && let Some(Some(destroyed)) = com.release_internal(&mut env.sys.objects, id)
    {
        destroy(env, destroyed);
    }
    0
}
#[win32(msctf, name="ITfThreadMgr::CreateDocumentMgr", level=implement)]
fn create_document(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> i32 {
    if !writable(env, output) {
        return POINTER;
    }
    let _ = output.write(env.cpu, 0);
    let Some(Payload::Manager { thread, .. }) = payload(env, this.addr()) else {
        return INVALID;
    };
    let thread = *thread;
    if !on_thread(env, thread) {
        return NOT_IMPLEMENTED;
    }
    let com = env.ext.get::<State>().com;
    let manager = com
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    let Some((pointer, document)) = object(
        env,
        Payload::Document {
            thread,
            manager,
            default: None,
        },
    ) else {
        return OUT_OF_MEMORY;
    };
    let Some((context, child)) = object(env, Payload::Context { thread, document }) else {
        release(env, pointer);
        return OUT_OF_MEMORY;
    };
    if !com.retain_internal(&mut env.sys.objects, child) {
        release(env, context);
        release(env, pointer);
        return OUT_OF_MEMORY;
    }
    let Some(Payload::Document { default, .. }) = com.get_mut(&mut env.sys.objects, pointer) else {
        unreachable!()
    };
    *default = Some(child);
    release(env, context); // The default context is owned only by the document.
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return POINTER;
    }
    0
}
#[win32(msctf, name="ITfDocumentMgr::CreateContext", level=implement)]
fn create_context(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    owner: u32,
    flags: u32,
    store: GuestPtr<()>,
    output: GuestPtr<u32>,
    cookie: GuestPtr<u32>,
) -> i32 {
    // Preflight both SDK outputs before allocating or publishing either one.
    if !writable(env, output) || !writable(env, cookie) {
        if writable(env, cookie) {
            let _ = cookie.write(env.cpu, 0);
        }
        if writable(env, output) {
            let _ = output.write(env.cpu, 0);
        }
        return INVALID;
    }
    let _ = output.write(env.cpu, 0);
    let _ = cookie.write(env.cpu, 0);
    let Some(Payload::Document {
        thread, manager, ..
    }) = payload(env, this.addr())
    else {
        return INVALID;
    };
    let (thread, manager) = (*thread, *manager);
    if !on_thread(env, thread) {
        return NOT_IMPLEMENTED;
    }
    let com = env.ext.get::<State>().com;
    let Some(Payload::Manager { client, .. }) = com.get_identity(&env.sys.objects, manager) else {
        return unavailable(env, "TSF document manager's thread manager has retired");
    };
    if owner == 0 || owner != *client {
        return INVALID;
    }
    if flags != 0 || !store.is_null() {
        return unavailable(
            env,
            "TSF CreateContext supports only flags zero and a NULL text store",
        );
    }
    let document = com
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    let Some((pointer, _)) = object(env, Payload::Context { thread, document }) else {
        return OUT_OF_MEMORY;
    };
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return POINTER;
    }
    // Native NULL-store contexts supply TF_INVALID_EDIT_COOKIE, not an editing
    // capability. Context identity and references remain real COM objects.
    0
}
fn default_context(env: &mut Env<'_>, this: u32, output: GuestPtr<u32>) -> i32 {
    if !writable(env, output) {
        return POINTER;
    }
    let _ = output.write(env.cpu, 0);
    let Some(Payload::Document {
        thread,
        default: Some(default),
        ..
    }) = payload(env, this)
    else {
        return INVALID;
    };
    let (thread, default) = (*thread, *default);
    if !on_thread(env, thread) {
        return NOT_IMPLEMENTED;
    }
    let Some(pointer) = env
        .ext
        .get::<State>()
        .com
        .canonical(&env.sys.objects, default)
    else {
        return INVALID;
    };
    if add_ref(env, pointer) == 0 {
        return OUT_OF_MEMORY;
    }
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return POINTER;
    }
    0
}
#[win32(msctf, name="ITfDocumentMgr::GetTop", level=implement)]
fn get_top(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> i32 {
    default_context(env, this.addr(), output)
}
#[win32(msctf, name="ITfDocumentMgr::GetBase", level=implement)]
fn get_base(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> i32 {
    default_context(env, this.addr(), output)
}
#[win32(msctf, name="ITfContext::GetDocumentMgr", level=implement)]
fn get_document(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> i32 {
    if !writable(env, output) {
        return POINTER;
    }
    let _ = output.write(env.cpu, 0);
    let Some(Payload::Context { thread, document }) = payload(env, this.addr()) else {
        return INVALID;
    };
    let (thread, document) = (*thread, *document);
    if !on_thread(env, thread) {
        return NOT_IMPLEMENTED;
    }
    let Some(pointer) = env
        .ext
        .get::<State>()
        .com
        .canonical(&env.sys.objects, document)
    else {
        return unavailable(env, "TSF context's document manager has retired");
    };
    if add_ref(env, pointer) == 0 {
        return OUT_OF_MEMORY;
    }
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return POINTER;
    }
    0
}

macro_rules! unsupported_method {
    ($function:ident, $name:tt, $( $argument:ident : $kind:ty ),*) => {
        #[win32(msctf, name=$name, level=safe_fail)]
        fn $function(env: &mut Env<'_>, $( $argument: $kind ),*) -> i32 {
            unavailable(env, "TSF text-store editing, document stacks, focus and service providers are unavailable")
        }
    };
}

unsupported_method!(itf_thread_mgr_enum_document_mgrs, "ITfThreadMgr::EnumDocumentMgrs", _this: GuestPtr<()>, _pp_enum: GuestPtr<()>);
unsupported_method!(itf_thread_mgr_get_focus, "ITfThreadMgr::GetFocus", _this: GuestPtr<()>, _ppdim_focus: GuestPtr<()>);
unsupported_method!(itf_thread_mgr_set_focus, "ITfThreadMgr::SetFocus", _this: GuestPtr<()>, _pdim_focus: GuestPtr<()>);
unsupported_method!(itf_thread_mgr_associate_focus, "ITfThreadMgr::AssociateFocus", _this: GuestPtr<()>, _hwnd: Handle, _pdim_new: GuestPtr<()>, _ppdim_prev: GuestPtr<()>);
unsupported_method!(itf_thread_mgr_is_thread_focus, "ITfThreadMgr::IsThreadFocus", _this: GuestPtr<()>, _pf_thread_focus: GuestPtr<()>);
unsupported_method!(itf_thread_mgr_get_function_provider, "ITfThreadMgr::GetFunctionProvider", _this: GuestPtr<()>, _clsid: GuestPtr<()>, _pp_func_prov: GuestPtr<()>);
unsupported_method!(itf_thread_mgr_enum_function_providers, "ITfThreadMgr::EnumFunctionProviders", _this: GuestPtr<()>, _pp_enum: GuestPtr<()>);
unsupported_method!(itf_thread_mgr_get_global_compartment, "ITfThreadMgr::GetGlobalCompartment", _this: GuestPtr<()>, _pp_comp_mgr: GuestPtr<()>);
unsupported_method!(itf_document_mgr_push, "ITfDocumentMgr::Push", _this: GuestPtr<()>, _pic: GuestPtr<()>);
unsupported_method!(itf_document_mgr_pop, "ITfDocumentMgr::Pop", _this: GuestPtr<()>, _dw_flags: u32);
unsupported_method!(itf_document_mgr_enum_contexts, "ITfDocumentMgr::EnumContexts", _this: GuestPtr<()>, _pp_enum: GuestPtr<()>);
unsupported_method!(itf_context_request_edit_session, "ITfContext::RequestEditSession", _this: GuestPtr<()>, _tid: u32, _pes: GuestPtr<()>, _dw_flags: u32, _phr_session: GuestPtr<()>);
unsupported_method!(itf_context_in_write_session, "ITfContext::InWriteSession", _this: GuestPtr<()>, _tid: u32, _pf_write_session: GuestPtr<()>);
unsupported_method!(itf_context_get_selection, "ITfContext::GetSelection", _this: GuestPtr<()>, _ec: u32, _ul_index: u32, _ul_count: u32, _p_selection: GuestPtr<()>, _pc_fetched: GuestPtr<()>);
unsupported_method!(itf_context_set_selection, "ITfContext::SetSelection", _this: GuestPtr<()>, _ec: u32, _ul_count: u32, _p_selection: GuestPtr<()>);
unsupported_method!(itf_context_get_start, "ITfContext::GetStart", _this: GuestPtr<()>, _ec: u32, _pp_start: GuestPtr<()>);
unsupported_method!(itf_context_get_end, "ITfContext::GetEnd", _this: GuestPtr<()>, _ec: u32, _pp_end: GuestPtr<()>);
unsupported_method!(itf_context_get_active_view, "ITfContext::GetActiveView", _this: GuestPtr<()>, _pp_view: GuestPtr<()>);
unsupported_method!(itf_context_enum_views, "ITfContext::EnumViews", _this: GuestPtr<()>, _pp_enum: GuestPtr<()>);
unsupported_method!(itf_context_get_status, "ITfContext::GetStatus", _this: GuestPtr<()>, _pdcs: GuestPtr<()>);
unsupported_method!(itf_context_get_property, "ITfContext::GetProperty", _this: GuestPtr<()>, _guid_prop: GuestPtr<()>, _pp_prop: GuestPtr<()>);
unsupported_method!(itf_context_get_app_property, "ITfContext::GetAppProperty", _this: GuestPtr<()>, _guid_prop: GuestPtr<()>, _pp_prop: GuestPtr<()>);
unsupported_method!(itf_context_track_properties, "ITfContext::TrackProperties", _this: GuestPtr<()>, _prg_prop: GuestPtr<()>, _c_prop: u32, _prg_app_prop: GuestPtr<()>, _c_app_prop: u32, _pp_property: GuestPtr<()>);
unsupported_method!(itf_context_enum_properties, "ITfContext::EnumProperties", _this: GuestPtr<()>, _pp_enum: GuestPtr<()>);
unsupported_method!(itf_context_create_range_backup, "ITfContext::CreateRangeBackup", _this: GuestPtr<()>, _ec: u32, _p_range: GuestPtr<()>, _pp_backup: GuestPtr<()>);

pub(super) fn register(registry: &mut Registry) {
    for api in [
        thread_query_ABI,
        thread_add_ABI,
        thread_release_ABI,
        document_query_ABI,
        document_add_ABI,
        document_release_ABI,
        context_query_ABI,
        context_add_ABI,
        context_release_ABI,
        activate_ABI,
        deactivate_ABI,
        create_document_ABI,
        create_context_ABI,
        get_top_ABI,
        get_base_ABI,
        get_document_ABI,
        itf_thread_mgr_enum_document_mgrs_ABI,
        itf_thread_mgr_get_focus_ABI,
        itf_thread_mgr_set_focus_ABI,
        itf_thread_mgr_associate_focus_ABI,
        itf_thread_mgr_is_thread_focus_ABI,
        itf_thread_mgr_get_function_provider_ABI,
        itf_thread_mgr_enum_function_providers_ABI,
        itf_thread_mgr_get_global_compartment_ABI,
        itf_document_mgr_push_ABI,
        itf_document_mgr_pop_ABI,
        itf_document_mgr_enum_contexts_ABI,
        itf_context_request_edit_session_ABI,
        itf_context_in_write_session_ABI,
        itf_context_get_selection_ABI,
        itf_context_set_selection_ABI,
        itf_context_get_start_ABI,
        itf_context_get_end_ABI,
        itf_context_get_active_view_ABI,
        itf_context_enum_views_ABI,
        itf_context_get_status_ABI,
        itf_context_get_property_ABI,
        itf_context_get_app_property_ABI,
        itf_context_track_properties_ABI,
        itf_context_enum_properties_ABI,
        itf_context_create_range_backup_ABI,
    ] {
        registry.register(api);
    }
}

//! The port of this device's RemotePairing service (IOS-JIT-2). iOS 27 picks
//! it anew at each boot and advertises it as `_remotepairing._tcp`; the
//! service on the loopback interface is this device's own.
use std::{
    ffi::{CStr, CString, c_char, c_void},
    time::{Duration, Instant},
};

/// The port iOS used before it randomized it.
pub const FALLBACK: u16 = 49152;
const SERVICE: &CStr = c"_remotepairing._tcp";
const FLAGS_ADD: u32 = 0x2;
/// Resolving a found instance answers from the cache or one query.
const RESOLVE: Duration = Duration::from_secs(1);

type ServiceRef = *mut c_void;
type BrowseReply = extern "C" fn(
    ServiceRef,
    u32,
    u32,
    i32,
    *const c_char,
    *const c_char,
    *const c_char,
    *mut c_void,
);
type ResolveReply = extern "C" fn(
    ServiceRef,
    u32,
    u32,
    i32,
    *const c_char,
    *const c_char,
    u16,
    u16,
    *const u8,
    *mut c_void,
);
unsafe extern "C" {
    fn DNSServiceBrowse(
        sd: *mut ServiceRef,
        flags: u32,
        interface: u32,
        kind: *const c_char,
        domain: *const c_char,
        reply: BrowseReply,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceResolve(
        sd: *mut ServiceRef,
        flags: u32,
        interface: u32,
        name: *const c_char,
        kind: *const c_char,
        domain: *const c_char,
        reply: ResolveReply,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceRefSockFD(sd: ServiceRef) -> i32;
    fn DNSServiceProcessResult(sd: ServiceRef) -> i32;
    fn DNSServiceRefDeallocate(sd: ServiceRef);
}

/// A found service instance: what resolving it needs.
struct Instance {
    interface: u32,
    name: CString,
    kind: CString,
    domain: CString,
}

/// The port this device advertises, if its loopback instance appears within
/// `timeout`. Other interfaces carry other devices' services. The helper
/// still finds the port when Bonjour misses it.
pub fn port(timeout: Duration) -> Option<u16> {
    browse(Instant::now() + timeout)
        .and_then(|instance| resolve(&instance, Instant::now() + RESOLVE))
}

/// This device's own instance, the one on the loopback interface.
fn browse(deadline: Instant) -> Option<Instance> {
    extern "C" fn add(
        _: ServiceRef,
        flags: u32,
        interface: u32,
        error: i32,
        name: *const c_char,
        kind: *const c_char,
        domain: *const c_char,
        context: *mut c_void,
    ) {
        if error != 0 {
            return;
        }
        if flags & FLAGS_ADD == 0 || !loopback(interface) {
            return;
        }
        // SAFETY: the context is `browse`'s slot; the strings live for this call.
        unsafe {
            *context.cast::<Option<Instance>>() = Some(Instance {
                interface,
                name: CStr::from_ptr(name).to_owned(),
                kind: CStr::from_ptr(kind).to_owned(),
                domain: CStr::from_ptr(domain).to_owned(),
            });
        }
    }
    let mut found: Option<Instance> = None;
    let context = &raw mut found;
    let mut sd = std::ptr::null_mut();
    // SAFETY: `found` outlives the reference, which `serve` deallocates.
    let error = unsafe {
        DNSServiceBrowse(
            &mut sd,
            0,
            0,
            SERVICE.as_ptr(),
            std::ptr::null(),
            add,
            context.cast(),
        )
    };
    if error != 0 {
        return None;
    }
    serve(sd, deadline, context, Option::is_some);
    found
}

fn resolve(instance: &Instance, deadline: Instant) -> Option<u16> {
    extern "C" fn resolved(
        _: ServiceRef,
        _: u32,
        _: u32,
        error: i32,
        _: *const c_char,
        _: *const c_char,
        port: u16,
        _: u16,
        _: *const u8,
        context: *mut c_void,
    ) {
        if error == 0 {
            // SAFETY: the context is `resolve`'s slot.
            unsafe { *context.cast::<Option<u16>>() = Some(u16::from_be(port)) };
        }
    }
    let mut port: Option<u16> = None;
    let context = &raw mut port;
    let mut sd = std::ptr::null_mut();
    // SAFETY: `port` outlives the reference, which `serve` deallocates.
    let error = unsafe {
        DNSServiceResolve(
            &mut sd,
            0,
            instance.interface,
            instance.name.as_ptr(),
            instance.kind.as_ptr(),
            instance.domain.as_ptr(),
            resolved,
            context.cast(),
        )
    };
    if error != 0 {
        return None;
    }
    serve(sd, deadline, context, Option::is_some);
    port
}

/// Delivers `sd`'s replies into `state` until `done` or the deadline, then
/// releases `sd`. Replies write through `state`, so it is only read between them.
fn serve<T>(sd: ServiceRef, deadline: Instant, state: *mut T, done: impl Fn(&T) -> bool) {
    // SAFETY: `sd` is a live reference until the deallocation below.
    let fd = unsafe { DNSServiceRefSockFD(sd) };
    // SAFETY: no reply runs while the borrow lives.
    while !done(unsafe { &*state }) {
        let left = deadline.saturating_duration_since(Instant::now());
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let ready = unsafe { libc::poll(&mut poll, 1, left.as_millis() as i32) };
        // SAFETY: the socket is readable, so processing does not block.
        if ready <= 0 || unsafe { DNSServiceProcessResult(sd) } != 0 {
            break;
        }
    }
    // SAFETY: no reply can arrive after deallocation.
    unsafe { DNSServiceRefDeallocate(sd) };
}

fn loopback(interface: u32) -> bool {
    let mut name = [0 as c_char; libc::IF_NAMESIZE];
    // SAFETY: the buffer holds IF_NAMESIZE bytes.
    !unsafe { libc::if_indextoname(interface, name.as_mut_ptr()) }.is_null()
        // SAFETY: if_indextoname wrote a terminated name.
        && unsafe { CStr::from_ptr(name.as_ptr()) }.to_bytes().starts_with(b"lo")
}

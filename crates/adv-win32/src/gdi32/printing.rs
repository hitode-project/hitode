//! No printer DC can exist in the configured empty printer catalog.
use super::*;
fn no_printer_dc(env: &mut Env<'_>) -> i32 {
    // The print pipeline requires a printer DC. Display, information, memory
    // and stale GDI handles cannot start or complete a spool job.
    crate::util::set_last_error(env, 6);
    -1
}
#[win32(gdi32, name="StartDocA", level=implement)]
fn start_doc(env: &mut Env<'_>, _dc: Handle, _document: GuestPtr<()>) -> i32 {
    no_printer_dc(env)
}
#[win32(gdi32, name="StartPage", level=implement)]
fn start_page(env: &mut Env<'_>, _dc: Handle) -> i32 {
    no_printer_dc(env)
}
#[win32(gdi32, name="EndPage", level=implement)]
fn end_page(env: &mut Env<'_>, _dc: Handle) -> i32 {
    no_printer_dc(env)
}
#[win32(gdi32, name="EndDoc", level=implement)]
fn end_doc(env: &mut Env<'_>, _dc: Handle) -> i32 {
    no_printer_dc(env)
}
#[win32(gdi32, name="SetAbortProc", level=implement)]
fn abort_proc(env: &mut Env<'_>, _dc: Handle, _callback: GuestPtr<()>) -> i32 {
    no_printer_dc(env)
}
pub(super) fn register(registry: &mut Registry) {
    for api in [
        start_doc_ABI,
        start_page_ABI,
        end_page_ABI,
        end_doc_ABI,
        abort_proc_ABI,
    ] {
        registry.register(api);
    }
}

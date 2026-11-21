//! Shared driver metadata and USER bodies over the in-process Wine server.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Wine 11 i386 `struct window_surface` field offsets (DWARF).
pub const WINDOW_SURFACE_RECT: u32 = 0x14;
pub const WINDOW_SURFACE_MUTEX: u32 = 0x24;
pub const WINDOW_SURFACE_BOUNDS: u32 = 0x3c;
pub const WINDOW_SURFACE_COLOR_KEY: u32 = 0x54;
pub const WINDOW_SURFACE_ALPHA_MASK: u32 = 0x5c;
pub const WINDOW_SURFACE_SHAPE_REGION: u32 = 0x60;
pub const WINDOW_SURFACE_SHAPE_BITMAP: u32 = 0x64;

/// Clamp `window_surface.bounds` against `window_surface.rect` with Wine 11's
/// 8-pixel horizontal alignment (`window_surface_flush`).
pub fn window_surface_dirty(rect: [i32; 4], bounds: [i32; 4]) -> Option<[i32; 4]> {
    let left = (bounds[0] & !7).max(0);
    let top = bounds[1].max(0);
    let right = (bounds[2].wrapping_add(7) & !7).min(rect[2].wrapping_sub(rect[0]));
    let bottom = bounds[3].min(rect[3].wrapping_sub(rect[1]));
    (left < right && top < bottom).then_some([left, top, right, bottom])
}

#[derive(Clone, Copy, Debug)]
struct DirtySurface {
    writer: u32,
    since: Instant,
}

/// Driver-owned data that bodies reach only through shared allocations.
#[derive(Default)]
pub struct UserServices(pub Rc<UserState>);
#[derive(Default)]
pub struct UserState {
    pending: RefCell<Vec<Rc<Cell<u32>>>>,
    surfaces: RefCell<Vec<u32>>,
    dirty: RefCell<Vec<(u32, DirtySurface)>>,
    has_dirty: Cell<bool>,
}
impl UserServices {
    pub fn pending(&self, context: u32) -> Rc<Cell<u32>> {
        let mut pending = self.0.pending.borrow_mut();
        let length = pending.len().max(context as usize + 1);
        pending.resize_with(length, || Rc::new(Cell::new(0)));
        pending[context as usize].clone()
    }
    pub fn surface(&self, address: u32, present: bool) {
        let mut surfaces = self.0.surfaces.borrow_mut();
        match (surfaces.binary_search(&address), present) {
            (Err(index), true) => surfaces.insert(index, address),
            (Ok(index), false) => {
                surfaces.remove(index);
                drop(surfaces);
                self.mark_surface_dirty(address, None);
            }
            _ => {}
        }
    }
    /// Return the tracked `window_surface` owning `mutex`, if any.
    pub fn surface_for_mutex(&self, mutex: u32) -> Option<u32> {
        let address = mutex.wrapping_sub(WINDOW_SURFACE_MUTEX);
        self.0
            .surfaces
            .borrow()
            .binary_search(&address)
            .ok()
            .map(|_| address)
    }
    /// Record or clear unflushed GDI dirt on a tracked `window_surface`.
    pub fn mark_surface_dirty(&self, address: u32, writer: Option<u32>) {
        let mut dirty = self.0.dirty.borrow_mut();
        match writer {
            Some(writer) => {
                if let Some((_, entry)) = dirty.iter_mut().find(|(at, _)| *at == address) {
                    entry.writer = writer;
                } else {
                    dirty.push((
                        address,
                        DirtySurface {
                            writer,
                            since: Instant::now(),
                        },
                    ));
                }
            }
            None => dirty.retain(|(at, _)| *at != address),
        }
        self.0.has_dirty.set(!dirty.is_empty());
    }
    /// Whether any tracked `window_surface` has unflushed GDI dirt.
    pub fn has_dirty_surfaces(&self) -> bool {
        self.0.has_dirty.get()
    }
    /// Dirty surfaces written by `context` or overdue past `overdue`.
    pub fn due_surfaces(&self, context: u32, overdue: Duration) -> Vec<u32> {
        if !self.0.has_dirty.get() {
            return Vec::new();
        }
        self.0
            .dirty
            .borrow()
            .iter()
            .filter_map(|&(address, entry)| {
                (entry.writer == context || entry.since.elapsed() >= overdue).then_some(address)
            })
            .collect()
    }
}

#[cfg(any(target_vendor = "apple", target_os = "android"))]
mod bodies {
    use super::*;
    use adv_cpu::body::{EAX, Input, Mem, Ret, Stop};

    fn state<M: Mem>(mem: &M) -> Result<&UserState, Stop> {
        let pointer = mem.services().user_state.cast::<UserState>();
        if pointer.is_null() {
            return Err(Stop::Guest);
        }
        // SAFETY: the installed driver retains this shared allocation for the run.
        Ok(unsafe { &*pointer })
    }

    fn events<M: Mem>(mem: &mut M, mask: u32) -> Result<(), Stop> {
        let services = mem.services();
        let state = state(mem)? as *const UserState;
        // SAFETY: state belongs to shared storage, not to mem or its owner borrow.
        let state = unsafe { &*state };
        if state
            .pending
            .borrow()
            .get(services.context as usize)
            .is_some_and(|bits| bits.get() == u32::MAX || bits.get() & mask != 0)
        {
            return Err(Stop::Guest);
        }
        if mask & 0x20 != 0 {
            return Ok(());
        } // QS_PAINT: check_for_events does not flush.
        let surfaces = state.surfaces.borrow();
        let (mut page, mut bytes) = (u32::MAX, std::ptr::null());
        let mut read = |at: u32| -> Result<i32, Stop> {
            let next = at & !4095;
            if page != next {
                bytes = mem.input(next, 4096)?;
                page = next;
            }
            // SAFETY: driver surface fields are aligned i32s, in the checked page.
            Ok(i32::from_le(unsafe {
                bytes
                    .add((at - page) as usize)
                    .cast::<i32>()
                    .read_unaligned()
            }))
        };
        for &surface in surfaces.iter() {
            let rect = [
                read(surface + WINDOW_SURFACE_RECT)?,
                read(surface + WINDOW_SURFACE_RECT + 4)?,
                read(surface + WINDOW_SURFACE_RECT + 8)?,
                read(surface + WINDOW_SURFACE_RECT + 12)?,
            ];
            let bounds = [
                read(surface + WINDOW_SURFACE_BOUNDS)?,
                read(surface + WINDOW_SURFACE_BOUNDS + 4)?,
                read(surface + WINDOW_SURFACE_BOUNDS + 8)?,
                read(surface + WINDOW_SURFACE_BOUNDS + 12)?,
            ];
            if window_surface_dirty(rect, bounds).is_some() {
                return Err(Stop::Guest); // A surface's guest flush callback must run.
            }
        }
        Ok(())
    }

    fn error<M: Mem>(mem: &mut M, code: u32) -> Result<Ret, Stop> {
        let output = mem.output(mem.services().teb + 0x34, 4)?;
        unsafe { output.cast::<u32>().write_unaligned(code.to_le()) };
        mem.commit()?;
        unsafe { adv_machine::service::work(mem.services()) };
        Ok(Ret::from(0u32))
    }

    adv_cpu::body!(pub(crate) ASYNC_STATE = async_state, inputs: &[Input::Stack(0)], outputs: EAX, pop: 4);
    adv_cpu::body!(pub(crate) CURSOR_POS = cursor_pos, inputs: &[Input::Stack(0)], outputs: EAX, pop: 4);
    adv_cpu::body!(pub(crate) QUEUE_STATUS = queue_status, inputs: &[Input::Stack(0)], outputs: EAX, pop: 4);
    adv_cpu::body!(pub(crate) INPUT_STATE = input_state, inputs: &[], outputs: EAX, pop: 0);
    adv_cpu::body!(pub(crate) PEEK = peek, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2), Input::Stack(3), Input::Stack(4)], outputs: EAX, pop: 20);

    fn async_state<M: Mem>(mem: &mut M, [key, ..]: [u32; 7]) -> Result<Ret, Stop> {
        if key < 256 {
            events(mem, 0x1cff)?;
        }
        let out = adv_wineserver::user_service(mem.services().thread, 0, [key, 0, 0, 0])
            .ok_or(Stop::Guest)?;
        if out[3] != 0 {
            return error(mem, out[3]);
        }
        unsafe { adv_machine::service::work(mem.services()) };
        Ok(Ret::from(out[0] as u16 as i16 as i32 as u32))
    }
    fn cursor_pos<M: Mem>(mem: &mut M, [at, ..]: [u32; 7]) -> Result<Ret, Stop> {
        state(mem)?; // The installed driver has one 96-DPI logical/physical desktop.
        let out =
            adv_wineserver::user_service(mem.services().thread, 1, [0; 4]).ok_or(Stop::Guest)?;
        if out[3] != 0 {
            return error(mem, out[3]);
        }
        let output = match mem.output(at, 8) {
            Ok(output) => output,
            Err(Stop::Fault { .. }) => return error(mem, 998), // Windows catches the output fault.
            Err(stop) => return Err(stop),
        };
        unsafe {
            output.cast::<u32>().write_unaligned(out[1].to_le());
            output.add(4).cast::<u32>().write_unaligned(out[2].to_le());
        }
        mem.commit()?;
        unsafe { adv_machine::service::work(mem.services()) };
        Ok(Ret::from(1u32))
    }
    fn queue_status<M: Mem>(mem: &mut M, [flags, ..]: [u32; 7]) -> Result<Ret, Stop> {
        // Windows rejects invalid flags before creating a thread queue; QS_SMRESULT
        // is a Wine extension, not an accepted Windows GetQueueStatus flag.
        if flags & !0x1dff != 0 {
            return error(mem, 1004);
        }
        events(mem, flags)?;
        let out = adv_wineserver::user_service(mem.services().thread, 2, [flags, 0, 0, 0])
            .ok_or(Stop::Guest)?;
        unsafe { adv_machine::service::work(mem.services()) };
        Ok(Ret::from(out[0]))
    }
    fn input_state<M: Mem>(mem: &mut M, _: [u32; 7]) -> Result<Ret, Stop> {
        events(mem, 7)?;
        let out =
            adv_wineserver::user_service(mem.services().thread, 3, [0; 4]).ok_or(Stop::Guest)?;
        unsafe { adv_machine::service::work(mem.services()) };
        Ok(Ret::from(u32::from(out[0] != 0)))
    }
    fn peek<M: Mem>(
        mem: &mut M,
        [_, window, first, last, flags, ..]: [u32; 7],
    ) -> Result<Ret, Stop> {
        // A/W empty peeks never touch MSG or last error, including null MSG.
        // Windows dispatches sent messages despite filters, and does not call
        // GETMESSAGE/FOREGROUNDIDLE hooks for an empty result.
        events(mem, 0x1cff & !0x20)?;
        let out =
            adv_wineserver::user_service(mem.services().thread, 4, [window, first, last, flags])
                .ok_or(Stop::Guest)?;
        if out[3] != 0 {
            return error(mem, out[3]);
        }
        let mut result = Ret::from(0u32);
        result.yield_now = unsafe { adv_machine::service::empty_peek(mem.services()) };
        Ok(result)
    }

    adv_cpu::body!(pub(crate) KEY_STATE = key_state, inputs: &[Input::Stack(0)], outputs: EAX, pop: 4);

    fn key_state<M: Mem>(mem: &mut M, [key, ..]: [u32; 7]) -> Result<Ret, Stop> {
        let state = adv_wineserver::key_state(mem.services().thread, key).ok_or(Stop::Guest)?;
        unsafe { adv_machine::service::work(mem.services()) };
        Ok(Ret::from((state & 0x81) as i8 as i32 as u32))
    }

    adv_cpu::body!(pub(crate) KEYBOARD_STATE = keyboard_state, inputs: &[Input::Stack(0)], outputs: EAX, pop: 4);

    fn keyboard_state<M: Mem>(mem: &mut M, [at, ..]: [u32; 7]) -> Result<Ret, Stop> {
        // Test queue existence before checking output: without it guest code creates
        // the queue, so no guest memory may have been changed.
        if !adv_wineserver::has_queue(mem.services().thread) {
            return Err(Stop::Guest);
        }
        let output = mem.output(at, 256)?;
        // SAFETY: the full output is checked; the server copies this thread's bytes.
        if !unsafe { adv_wineserver::keyboard_state(mem.services().thread, output) } {
            return Err(Stop::Guest);
        }
        mem.commit()?;
        unsafe { adv_machine::service::work(mem.services()) };
        Ok(Ret::from(1u32))
    }
}
#[cfg(any(target_vendor = "apple", target_os = "android"))]
pub(super) use bodies::*;

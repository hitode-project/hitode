//! Queue keyboard state from the in-process Wine server.

use adv_cpu::body::{EAX, Input, Mem, Ret, Stop};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Driver-owned data that bodies reach only through shared allocations.
#[derive(Default)]
pub struct UserServices(pub Rc<UserState>);
#[derive(Default)]
pub struct UserState {
    pending: RefCell<Vec<Rc<Cell<u32>>>>,
    surfaces: RefCell<Vec<u32>>,
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
            }
            _ => {}
        }
    }
}

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
        // Wine 11 i386 window_surface: rect at 20, bounds at 60 (DWARF).
        let rect = [
            read(surface + 20)?,
            read(surface + 24)?,
            read(surface + 28)?,
            read(surface + 32)?,
        ];
        let bounds = [
            read(surface + 60)?,
            read(surface + 64)?,
            read(surface + 68)?,
            read(surface + 72)?,
        ];
        let left = bounds[0] & !7;
        let right = bounds[2].wrapping_add(7) & !7;
        if left.max(0) < right.min(rect[2].wrapping_sub(rect[0]))
            && bounds[1].max(0) < bounds[3].min(rect[3].wrapping_sub(rect[1]))
        {
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

adv_cpu::body!(pub(super) ASYNC_STATE = async_state, inputs: &[Input::Stack(0)], outputs: EAX, pop: 4);
adv_cpu::body!(pub(super) CURSOR_POS = cursor_pos, inputs: &[Input::Stack(0)], outputs: EAX, pop: 4);
adv_cpu::body!(pub(super) QUEUE_STATUS = queue_status, inputs: &[Input::Stack(0)], outputs: EAX, pop: 4);
adv_cpu::body!(pub(super) INPUT_STATE = input_state, inputs: &[], outputs: EAX, pop: 0);
adv_cpu::body!(pub(super) PEEK = peek, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2), Input::Stack(3), Input::Stack(4)], outputs: EAX, pop: 20);

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
    let out = adv_wineserver::user_service(mem.services().thread, 1, [0; 4]).ok_or(Stop::Guest)?;
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
    let out = adv_wineserver::user_service(mem.services().thread, 3, [0; 4]).ok_or(Stop::Guest)?;
    unsafe { adv_machine::service::work(mem.services()) };
    Ok(Ret::from(u32::from(out[0] != 0)))
}
fn peek<M: Mem>(mem: &mut M, [_, window, first, last, flags, ..]: [u32; 7]) -> Result<Ret, Stop> {
    // A/W empty peeks never touch MSG or last error, including null MSG.
    // Windows dispatches sent messages despite filters, and does not call
    // GETMESSAGE/FOREGROUNDIDLE hooks for an empty result.
    events(mem, 0x1cff & !0x20)?;
    let out = adv_wineserver::user_service(mem.services().thread, 4, [window, first, last, flags])
        .ok_or(Stop::Guest)?;
    if out[3] != 0 {
        return error(mem, out[3]);
    }
    let mut result = Ret::from(0u32);
    result.yield_now = unsafe { adv_machine::service::empty_peek(mem.services()) };
    Ok(result)
}

adv_cpu::body!(pub(super) KEY_STATE = key_state, inputs: &[Input::Stack(0)], outputs: EAX, pop: 4);

fn key_state<M: Mem>(mem: &mut M, [key, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let state = adv_wineserver::key_state(mem.services().thread, key).ok_or(Stop::Guest)?;
    unsafe { adv_machine::service::work(mem.services()) };
    Ok(Ret::from((state & 0x81) as i8 as i32 as u32))
}

adv_cpu::body!(pub(super) KEYBOARD_STATE = keyboard_state, inputs: &[Input::Stack(0)], outputs: EAX, pop: 4);

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

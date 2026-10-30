//! Queue keyboard state from the in-process Wine server.

use adv_cpu::body::{EAX, Input, Mem, Ret, Stop};

adv_cpu::body!(pub(super) KEY_STATE = key_state, inputs: &[Input::Stack(0)], outputs: EAX, pop: 4);

fn key_state<M: Mem>(mem: &mut M, [key, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let state = adv_wineserver::key_state(mem.services().thread, key).ok_or(Stop::Guest)?;
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
    Ok(Ret::from(1u32))
}

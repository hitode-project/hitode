//! Prediction metadata for one borrowed native chain, never architectural CPU
//! state. No valid hint survives a native return to its execution owner.
use std::mem::MaybeUninit;

pub(crate) const CAPACITY: u32 = 64;

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Hint {
    pub sp: u32,
    pub pc: u32,
    pub entry: usize,
}

#[repr(C)]
pub(crate) struct Stack {
    pub top: u32,
    pub matches: u64,
    pub hits: u64,
    pub misses: u64,
    pub overflows: u64,
    // Generated code initializes all fields before publishing top. Only indices
    // below top can be read; clear invalidates them without reading raw targets.
    pub entries: [MaybeUninit<Hint>; CAPACITY as usize],
}

impl Stack {
    pub fn new() -> Self {
        Self {
            top: 0,
            matches: 0,
            hits: 0,
            misses: 0,
            overflows: 0,
            entries: [MaybeUninit::uninit(); CAPACITY as usize],
        }
    }

    pub fn clear(&mut self) {
        self.top = 0;
    }
}

#[cfg(all(test, windows, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use crate::{EXEC, Executor, READ, Region, RunOutcome, WRITE};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::{Duration, Instant};

    #[test]
    fn interrupt_after_a_committed_call_discards_the_whole_borrowed_prediction_chain() {
        const CODE: u32 = 0x10000;
        const BODY: u32 = 0x11000;
        const STOP: u32 = 0x18000;
        const STACK: u32 = 0x2f800;
        let mut m = Executor::address_space().unwrap();
        m.set_shadow_returns(true).unwrap();
        m.map(CODE, 8192, READ | EXEC).unwrap();
        m.map(STOP, 4096, READ | EXEC).unwrap();
        m.map(STACK & !4095, 4096, READ | WRITE).unwrap();
        let ready = m.memory.map_shared_audio(0x20000, 4096).unwrap();
        // call BODY; inc eax; ret. BODY publishes a byte-atomic witness before
        // its infinite local loop, so the interrupt cannot arrive before CALL.
        m.write(CODE, &[0xe8, 0xfb, 0x0f, 0, 0, 0x40, 0xc3])
            .unwrap();
        m.write(BODY, &[0xc6, 5, 0, 0, 2, 0, 1, 0xeb, 0xfe])
            .unwrap();
        m.write(STACK, &STOP.to_le_bytes()).unwrap();
        m.state.eip = CODE;
        m.state.regs[4] = STACK;
        m.add_region(Region::new(CODE, 7)).unwrap();
        m.add_region(Region::new(BODY, 9)).unwrap();
        let interrupt = Arc::new(AtomicBool::new(false));
        m.set_host_interrupt_flag(interrupt.clone());
        let sender = interrupt.clone();
        let notifier = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while ready.load_u8(0) != 1 {
                if Instant::now() >= deadline {
                    sender.store(true, Ordering::Release);
                    return false;
                }
                std::thread::yield_now();
            }
            sender.store(true, Ordering::Release);
            true
        });
        let outcome = m.run(STOP, u64::MAX).unwrap();
        assert!(
            notifier.join().unwrap(),
            "guest did not publish the committed-call witness"
        );
        assert_eq!(outcome, RunOutcome::Interrupted);
        assert_eq!(m.state.eip, BODY + 7);
        assert_eq!(m.state.regs[4], STACK - 4);
        interrupt.store(false, Ordering::Release);
        m.write(BODY + 7, &[0xc3]).unwrap();
        m.add_region(Region::new(BODY + 7, 1)).unwrap();
        assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 1);
        assert_eq!(m.stats.shadow_hits, 0);
        assert!(m.stats.shadow_misses >= 1);
        assert_eq!(std::mem::size_of::<Stack>(), 1064);
    }
}

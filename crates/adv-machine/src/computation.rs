//! The scheduler collects a bounded round of prepared contexts on the owner.
//! During a computation scope only these snapshots and host bookkeeping may
//! run; normal dispatch, waits and guest memory access resume after the drain.
//! Schedule trace: compute_offer reports the prepared EIP and compute_submit
//! starts a worker assignment. compute_done reports retired instructions,
//! execution ns, and flags (local=1, owner=2).
//! compute_round reports offered contexts, scope wall ns, and admitted contexts.

use std::time::{Duration, Instant};

use adv_cpu::PROCESSORS;
use adv_cpu::backend::{Backend, Completion, Computation};
use adv_sync::WakeEvent;

use crate::{CtxId, Machine, Sys};

const TEAM: usize = PROCESSORS as usize;

#[derive(Default)]
pub(super) struct Round {
    offered: Vec<CtxId>,
    remaining: Vec<CtxId>,
    decline: Vec<CtxId>,
    delivery: Option<u64>,
}

impl Round {
    pub(super) fn holds(&self, id: CtxId) -> bool {
        self.offered.contains(&id)
    }

    pub(super) fn take_decline(&mut self, id: CtxId) -> bool {
        self.decline
            .iter()
            .position(|&declined| declined == id)
            .is_some_and(|index| {
                self.decline.swap_remove(index);
                true
            })
    }

    pub(super) fn select(&mut self, id: CtxId, sys: &Sys) {
        if self.offered.is_empty() {
            return;
        }
        if let Some(index) = self.remaining.iter().position(|&peer| peer == id) {
            self.remaining.swap_remove(index);
        } else {
            // An urgent new arrival or a second ordinary turn takes precedence.
            self.release(sys);
        }
    }

    pub(super) fn offer(&mut self, id: CtxId, sys: &Sys) {
        if self.offered.is_empty() {
            self.remaining.extend(
                sys.active_contexts()
                    .filter(|peer| peer.id != id && peer.is_ready())
                    .map(|peer| peer.id),
            );
            self.delivery = sys.poll_wake.as_ref().map(WakeEvent::generation);
        }
        self.offered.push(id);
    }

    pub(super) fn collected(&self, sys: &Sys) -> bool {
        !self.offered.is_empty()
            && (self.offered.len() == TEAM
                || !self.remaining.iter().any(|&id| sys.context(id).is_ready()))
    }

    pub(super) fn delivery_changed(&self, sys: &Sys) -> bool {
        !self.offered.is_empty()
            && self.delivery != sys.poll_wake.as_ref().map(WakeEvent::generation)
    }

    pub(super) fn real_deadline(&self, sys: &Sys) -> Option<u64> {
        if self.offered.is_empty() || sys.clock.is_virtual() {
            None
        } else {
            sys.next_deadline()
        }
    }

    pub(super) fn release(&mut self, sys: &Sys) {
        self.remaining.clear();
        self.decline.extend(
            self.offered
                .drain(..)
                .filter(|&id| !sys.context(id).is_done()),
        );
        self.decline.retain(|&id| !sys.context(id).is_done());
    }

    fn finish(&mut self, sys: &Sys, admitted: u32) {
        // Only offers that ran no job need a turn with offering disabled.
        let mut index = 0;
        self.offered.retain(|_| {
            let unsubmitted = admitted & (1 << index) == 0;
            index += 1;
            unsubmitted
        });
        self.release(sys);
    }
}

impl<B: Backend> Machine<B> {
    pub(super) fn restore_computation_current(&mut self) {
        let id = self.sys.current;
        if self.loaded.is_none()
            && let Some(Some(context)) = self.regs.get(id as usize)
        {
            self.cpu.restore(context);
            self.loaded = Some(id);
        }
    }

    pub(super) fn run_computation_round(
        &mut self,
        slice: Option<Duration>,
        started: Instant,
        hard_budget: Option<Duration>,
    ) {
        let mut admitted = 0;
        self.computation
            .offered
            .retain(|&id| self.sys.context(id).is_ready());
        if !self.computation.offered.is_empty() {
            let timer = (!self.sys.clock.is_virtual())
                .then(|| self.sys.next_deadline())
                .flatten()
                .and_then(|at| {
                    let remaining =
                        Duration::from_micros(at.saturating_sub(self.sys.clock.now_us()));
                    Instant::now().checked_add(remaining)
                });
            let deadline = [
                timer,
                slice.and_then(|slice| started.checked_add(slice)),
                hard_budget
                    .and_then(|hard| started.checked_add(hard.saturating_sub(self.slice_elapsed))),
            ]
            .into_iter()
            .flatten()
            .min();
            let wake = self.sys.poll_wake.clone();
            let trace = self.sys.schedule_trace.enabled().then(Instant::now);
            let (sys, regs, round) = (&mut self.sys, &mut self.regs, &self.computation);
            // SAFETY: only exclusively held snapshots reach the backend. This
            // callback never accesses guest memory, a CPU, or a context hook,
            // and drains every submitted job before releasing the snapshots.
            unsafe {
                self.cpu
                    .with_computation(wake.clone(), deadline, &mut |jobs| {
                        admitted = run_batch(
                            jobs,
                            sys,
                            regs,
                            &round.offered,
                            wake.as_ref(),
                            round.delivery,
                            deadline,
                        );
                    });
            }
            if let Some(started) = trace {
                sys.schedule_trace.event(
                    "compute_round",
                    sys.current,
                    round.offered.len() as u64,
                    started.elapsed().as_nanos() as u64,
                    u64::from(admitted.count_ones()),
                );
            }
        }
        self.computation.finish(&self.sys, admitted);
        self.restore_computation_current();
    }
}

fn run_batch<C>(
    jobs: &mut dyn Computation<C>,
    sys: &mut Sys,
    regs: &mut [Option<C>],
    offered: &[CtxId],
    wake: Option<&WakeEvent>,
    delivery: Option<u64>,
    deadline: Option<Instant>,
) -> u32 {
    if let [id] = *offered {
        // A single context may split its loop (A64-TR-9).
        let context = regs[id as usize].as_ref().expect("held snapshot");
        return match jobs.run_local(id, context) {
            Some(done) => {
                complete(sys, regs, done, true);
                1
            }
            None => 0,
        };
    }
    let mut pending = [true; TEAM];
    let pending = &mut pending[..offered.len()];
    let mut active = 0;
    let mut admitted = 0;
    let mut owner_needed = false;
    loop {
        while let Some(done) = jobs.poll() {
            active -= 1;
            pending[offered.iter().position(|&id| id == done.id).unwrap()] = !done.requires_owner;
            owner_needed |= complete(sys, regs, done, false);
        }
        if owner_needed
            || deadline.is_some_and(|at| Instant::now() >= at)
            || delivery != wake.map(WakeEvent::generation)
        {
            break;
        }
        let local = pending.iter().rposition(|&ready| ready);
        for (index, ready) in pending.iter_mut().enumerate() {
            if !*ready || Some(index) == local {
                continue;
            }
            let id = offered[index];
            if jobs.submit(id, regs[id as usize].as_ref().expect("held snapshot")) {
                sys.schedule_trace.event("compute_submit", id, 0, 0, 0);
                *ready = false;
                admitted |= 1 << index;
                active += 1;
            }
        }
        if active == 0 {
            // There is no overlapping work to justify this scope.
            break;
        }
        if let Some((index, done)) =
            pending
                .iter()
                .zip(offered)
                .enumerate()
                .find_map(|(index, (ready, &id))| {
                    if !*ready {
                        return None;
                    }
                    let context = regs[id as usize].as_ref().expect("held snapshot");
                    jobs.run_local(id, context).map(|done| (index, done))
                })
        {
            pending[index] = !done.requires_owner;
            admitted |= 1 << index;
            owner_needed = complete(sys, regs, done, true);
            continue;
        }
        // Completion notifications retain the pipe wake but do not change its
        // generation. A host delivery interrupts the wait and closes admission.
        let remaining = deadline.map(|at| at.saturating_duration_since(Instant::now()));
        if let Some(wake) = wake {
            if wake.wait(remaining).is_err() {
                break;
            }
        } else if let Some(done) = jobs.wait(remaining) {
            active -= 1;
            pending[offered.iter().position(|&id| id == done.id).unwrap()] = !done.requires_owner;
            owner_needed = complete(sys, regs, done, false);
        }
    }
    jobs.recall();
    while active != 0 {
        let done = jobs
            .wait(None)
            .expect("submitted computation returns a completion");
        active -= 1;
        complete(sys, regs, done, false);
    }
    admitted
}

fn complete<C>(sys: &mut Sys, regs: &mut [Option<C>], done: Completion<C>, local: bool) -> bool {
    let id = done.id;
    regs[id as usize] = Some(done.context);
    let context = &mut sys.contexts[id as usize];
    context.spin.ran(done.elapsed);
    context.progress.new_work();
    context.progress.ran(done.retired);
    context.yielded = false;
    sys.wakes.mark_turn(id, false);
    let ns = done.elapsed.as_nanos() as u64;
    if local {
        sys.load.computed(done.elapsed, done.helpers);
        sys.scheduling
            .context_run(id as usize, done.elapsed, 0, true);
        sys.stats.total.cpu_runs += 1;
        if sys.stats.enabled || sys.stats.services_enabled || sys.stats.frame_budget.is_some() {
            sys.stats.total.run_ns += ns;
        }
    }
    sys.schedule_trace.event(
        "compute_done",
        id,
        done.retired,
        ns,
        u64::from(local) | u64::from(done.requires_owner) << 1,
    );
    done.requires_owner
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::io;
    use std::rc::Rc;

    use adv_cpu::Cpu;
    use adv_cpu::backend::{Guest, GuestExt, Prot, Reg, asm::Asm};

    use super::*;
    use crate::{
        Clock, Conv, HostFn, Object, Outcome, Resolver, RunResult, Wait, layout::PARK_ADDR,
        schedule_trace::ScheduleTrace,
    };

    const CODE: u32 = 0x0040_0000;

    struct TestHost;

    impl Resolver for TestHost {
        fn host_fn(&self, module: &str, name: &str) -> Option<HostFn> {
            if !self.provides(module) {
                return None;
            }
            let handler: crate::thunk::Handler = match name {
                "wait" => |env: &mut crate::Env<'_>| {
                    Outcome::block_then_return(
                        Wait::Object {
                            handle: env.arg(0),
                            deadline: None,
                        },
                        0,
                    )
                },
                "wake" => |env: &mut crate::Env<'_>| {
                    assert!(env.sys.set_event(env.arg(0)));
                    env.sys.resume_context(2).unwrap();
                    Outcome::Return(0)
                },
                _ => return None,
            };
            Some(HostFn {
                conv: Conv::Stdcall { argc: 1 },
                handler,
                data: 0,
                trace: false,
                guest: None,
                leaf: None,
            })
        }

        fn provides(&self, module: &str) -> bool {
            module == "test"
        }
    }

    fn machine(jit: bool) -> Machine<Cpu> {
        Machine::new(
            Cpu::new(PARK_ADDR, jit).unwrap(),
            TestHost,
            Clock::virtual_time(),
        )
        .unwrap()
    }

    // spec: PROC-MACH-1 PROC-MACH-5
    #[test]
    fn collection_uses_actual_turns_through_the_initial_ready_set() {
        let mut m = machine(false);
        for _ in 0..12 {
            m.spawn("test", CODE, &[], 0x1_0000).unwrap();
        }
        m.sys.contexts[1].yielded = true;
        let mut round = Round::default();
        round.offer(7, &m.sys);
        round.select(1, &m.sys);
        assert!(round.holds(7));
        for peer in [11, 8, 9, 10, 6, 5, 4] {
            assert!(!round.collected(&m.sys));
            round.select(peer, &m.sys);
            round.offer(peer, &m.sys);
        }
        assert_eq!(round.offered, [7, 11, 8, 9, 10, 6, 5, 4]);
        assert!(round.collected(&m.sys));
        assert!(round.remaining.contains(&0));
    }

    struct FakeJobs {
        submitted: VecDeque<(CtxId, u32)>,
        wake: WakeEvent,
        local: Option<CtxId>,
        recalled: bool,
    }

    impl Computation<u32> for FakeJobs {
        fn submit(&mut self, id: u32, context: &u32) -> bool {
            assert!(!self.recalled);
            if id < 2 {
                self.submitted.push_back((id, *context));
                true
            } else {
                false
            }
        }

        fn run_local(&mut self, id: u32, context: &u32) -> Option<Completion<u32>> {
            assert!(!self.recalled);
            assert_eq!(self.submitted.len(), 2);
            self.local = Some(id);
            self.wake.signal();
            Some(done(id, *context, false))
        }

        fn poll(&mut self) -> Option<Completion<u32>> {
            None
        }

        fn wait(&mut self, _: Option<Duration>) -> Option<Completion<u32>> {
            assert!(self.recalled);
            self.submitted
                .pop_front()
                .map(|(id, context)| done(id, context, true))
        }

        fn recall(&mut self) {
            self.recalled = true;
        }
    }

    fn done(id: u32, context: u32, requires_owner: bool) -> Completion<u32> {
        Completion {
            id,
            context: context + 1,
            retired: 1,
            elapsed: Duration::from_micros(1),
            helpers: Duration::ZERO,
            requires_owner,
        }
    }

    // spec: PROC-MACH-1 PROC-MACH-2 PROC-MACH-5 PROC-CLOCK-1 PROC-CLOCK-5
    #[test]
    fn delivery_recalls_workers_before_the_owner_resumes_normal_work() {
        let mut machine = machine(false);
        for _ in 0..4 {
            machine.spawn("test", CODE, &[], 0x1_0000).unwrap();
        }
        let wake = WakeEvent::new().unwrap();
        let mut jobs = FakeJobs {
            submitted: VecDeque::new(),
            wake: wake.clone(),
            local: None,
            recalled: false,
        };
        let mut regs = [Some(0), Some(1), Some(2), Some(3)];
        let time = machine.sys.clock.now_us();
        let mut round = Round {
            offered: vec![0, 1, 2, 3],
            ..Round::default()
        };
        let admitted = run_batch(
            &mut jobs,
            &mut machine.sys,
            &mut regs,
            &round.offered,
            Some(&wake),
            Some(wake.generation()),
            None,
        );
        round.finish(&machine.sys, admitted);
        assert_eq!(round.decline, [3]);
        assert!(jobs.recalled && jobs.submitted.is_empty());
        assert_eq!(jobs.local, Some(2));
        assert_eq!(regs, [Some(1), Some(2), Some(3), Some(3)]);
        assert_eq!(machine.sys.clock.now_us(), time);
        assert_eq!(machine.sys.stats.total.cpu_runs, 1);
    }

    // spec: PROC-MACH-1 PROC-MACH-2 PROC-MACH-5 PROC-CLOCK-5
    #[test]
    fn completed_slices_receive_fresh_admission_until_delivery() {
        struct Slices {
            queued: Option<(CtxId, u32)>,
            remote: Vec<u32>,
            local: Vec<u32>,
            polled: usize,
            wake: WakeEvent,
            recalled: bool,
        }

        impl Computation<u32> for Slices {
            fn submit(&mut self, id: u32, context: &u32) -> bool {
                assert!(!self.recalled);
                assert!(self.queued.is_none());
                self.remote.push(*context);
                self.queued = Some((id, *context));
                true
            }

            fn run_local(&mut self, id: u32, context: &u32) -> Option<Completion<u32>> {
                assert!(!self.recalled);
                assert!(self.queued.is_some());
                self.local.push(*context);
                if *context == 2 {
                    self.wake.signal();
                }
                Some(done(id, *context, false))
            }

            fn poll(&mut self) -> Option<Completion<u32>> {
                if self.polled == self.local.len() || self.local.len() == 3 {
                    return None;
                }
                self.polled = self.local.len();
                self.queued
                    .take()
                    .map(|(id, context)| done(id, context, false))
            }

            fn wait(&mut self, _: Option<Duration>) -> Option<Completion<u32>> {
                assert!(self.recalled);
                self.queued
                    .take()
                    .map(|(id, context)| done(id, context, true))
            }

            fn recall(&mut self) {
                self.recalled = true;
            }
        }

        let mut machine = machine(false);
        for _ in 0..2 {
            machine.spawn("test", CODE, &[], 0x1_0000).unwrap();
        }
        let wake = WakeEvent::new().unwrap();
        let mut jobs = Slices {
            queued: None,
            remote: Vec::new(),
            local: Vec::new(),
            polled: 0,
            wake: wake.clone(),
            recalled: false,
        };
        let mut regs = [Some(0), Some(0)];
        let admitted = run_batch(
            &mut jobs,
            &mut machine.sys,
            &mut regs,
            &[0, 1],
            Some(&wake),
            Some(wake.generation()),
            None,
        );
        assert_eq!(jobs.remote, [0, 1, 2]);
        assert_eq!(jobs.local, [0, 1, 2]);
        assert_eq!(regs, [Some(3), Some(3)]);
        assert_eq!(admitted, 3);
        assert!(jobs.recalled && jobs.queued.is_none());
        assert_eq!(machine.sys.stats.total.cpu_runs, 3);
    }

    #[derive(Clone, Default)]
    struct Trace(Rc<RefCell<Vec<u8>>>);

    impl io::Write for Trace {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    // spec: PROC-MACH-1 PROC-MACH-2 PROC-MACH-5
    #[test]
    #[cfg(target_arch = "aarch64")]
    fn prepared_contexts_compute_disjoint_outputs_and_resume_their_own_tails() {
        if !std::thread::available_parallelism().is_ok_and(|n| n.get() > 1) {
            return;
        }
        const INPUT: u32 = 0x0050_0000;
        const OUTPUT: u32 = 0x0060_0000;
        const SPACE: u32 = 0x10_0000;
        // Enough budget slices to promote the loop before nominating it.
        const COUNT: u32 = 160_000;
        let mut m = machine(true);
        let trace = Trace::default();
        m.sys.schedule_trace = ScheduleTrace::to_writer(Box::new(trace.clone())).unwrap();
        m.sys.stats.services_enabled = true;
        m.cpu.map(CODE, 4096, Prot::RX).unwrap();
        m.cpu.map(INPUT, 0x4000, Prot::RW).unwrap();
        m.cpu.map(OUTPUT, SPACE * 2, Prot::RW).unwrap();
        m.cpu.write_u32(INPUT, 23).unwrap();
        m.cpu.write_u32(INPUT + 4, 42).unwrap();
        let mut code = Asm::new(CODE);
        // nop; head: mov eax,[esi]; mov [edi],eax; add edi,4
        code.bytes
            .extend_from_slice(&[0x90, 0x8b, 0x06, 0x89, 0x07, 0x83, 0xc7, 4]);
        code.dec_ecx().jnz(CODE + 1).ret();
        m.cpu.write(CODE, &code.bytes).unwrap();
        let event = m.sys.objects.insert(Object::Event {
            signaled: false,
            manual_reset: false,
        });
        for (id, function) in [(0, "wait"), (1, "wake")] {
            let entry = CODE + 0x200 + id * 0x100;
            let thunk = m.resolve_import("test", function).unwrap();
            let mut prefix = Asm::new(entry);
            prefix.push_imm(event).call(thunk).jmp(CODE);
            m.cpu.write(entry, &prefix.bytes).unwrap();
            let context = m.spawn("test", entry, &[], 0x1_0000).unwrap();
            assert!(m.sys.set_initial_registers(
                context,
                &[
                    (Reg::Ecx, COUNT),
                    (Reg::Esi, INPUT + id * 4),
                    (Reg::Edi, OUTPUT + id * SPACE)
                ]
            ));
        }
        // A ready peer need not offer a computation for this round to finish.
        const ORDINARY: u32 = CODE + 0x100;
        m.cpu.write(ORDINARY, &[0xc3]).unwrap();
        let ordinary = m.spawn("test", ORDINARY, &[], 0x1_0000).unwrap();
        assert!(m.sys.set_initial_registers(ordinary, &[(Reg::Eax, 17)]));
        m.sys.suspend_context(ordinary).unwrap();
        assert!(matches!(m.run(), RunResult::Exited(_)));
        assert_eq!(m.sys.context(0).exit_code(), Some(23));
        assert_eq!(m.sys.context(1).exit_code(), Some(42));
        assert_eq!(m.sys.context(ordinary).exit_code(), Some(17));
        for (id, value) in [(0, 23), (1, 42)] {
            let mut data = vec![0; COUNT as usize * 4];
            m.cpu.read(OUTPUT + id * SPACE, &mut data).unwrap();
            assert!(
                data.as_chunks::<4>()
                    .0
                    .iter()
                    .all(|&word| u32::from_le_bytes(word) == value)
            );
        }
        let trace = trace.0.borrow();
        let trace = std::str::from_utf8(&trace).unwrap();
        assert!(trace.contains("\tcompute_offer\t1\t"), "{trace}");
        assert!(trace.contains("\tcompute_submit\t"), "{trace}");
        let done: Vec<_> = trace
            .lines()
            .filter(|line| line.contains("\tcompute_done\t"))
            .collect();
        assert!(done.iter().any(|line| line.ends_with("\t0")), "{trace}");
        assert!(done.iter().any(|line| line.ends_with("\t1")), "{trace}");
    }
}

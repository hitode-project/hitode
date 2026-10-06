//! Timer deadlines belong to payload identity, never to one duplicate alias.
use super::*;

impl Objects {
    pub(crate) fn arm_timer(&mut self, handle: Handle, deadline: u64, period: u64) -> bool {
        let Some(&id) = self.targets.get(&(0, handle)) else {
            return false;
        };
        let Object::Timer {
            deadline: old,
            signaled,
            period_us,
            ..
        } = &mut self.entries.get_mut(&id).unwrap().value
        else {
            return false;
        };
        if let Some(old) = old {
            self.timer_deadlines.remove(&(*old, id));
        }
        *old = Some(deadline);
        *period_us = period;
        *signaled = false;
        self.timer_deadlines.insert((deadline, id));
        true
    }
    pub(crate) fn cancel_timer(&mut self, handle: Handle) -> bool {
        let Some(&id) = self.targets.get(&(0, handle)) else {
            return false;
        };
        let Object::Timer { deadline, .. } = &mut self.entries.get_mut(&id).unwrap().value else {
            return false;
        };
        if let Some(deadline) = deadline.take() {
            self.timer_deadlines.remove(&(deadline, id));
        }
        true
    }
    pub(crate) fn next_timer_deadline(&self) -> Option<u64> {
        // An unconsumed signal cannot become more signaled. Do not wake an idle
        // process on every unused periodic tick; the next acquisition/wake
        // coalesces missed periods before consuming the existing signal.
        self.timer_deadlines.iter().find_map(|&(deadline, id)| {
            matches!(
                self.entries.get(&id)?.value,
                Object::Timer {
                    signaled: false,
                    ..
                }
            )
            .then_some(deadline)
        })
    }
    pub(crate) fn expire_timers(&mut self, now: u64) {
        while let Some(&(at, id)) = self.timer_deadlines.first() {
            if at > now {
                break;
            }
            self.timer_deadlines.remove(&(at, id));
            let Object::Timer {
                signaled,
                deadline,
                period_us,
                ..
            } = &mut self
                .entries
                .get_mut(&id)
                .expect("indexed timer is live")
                .value
            else {
                unreachable!()
            };
            *signaled = true;
            // A timer holds one signal, rather than a backlog of missed ticks.
            // Keep its original phase and jump directly to the next future tick.
            *deadline = if *period_us == 0 {
                None
            } else {
                (now - at)
                    .checked_div(*period_us)
                    .and_then(|n| n.checked_add(1))
                    .and_then(|n| n.checked_mul(*period_us))
                    .and_then(|delta| at.checked_add(delta))
            };
            if let Some(next) = *deadline {
                self.timer_deadlines.insert((next, id));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn timer(manual_reset: bool) -> Object {
        Object::Timer {
            manual_reset,
            signaled: false,
            deadline: None,
            period_us: 0,
        }
    }
    #[test]
    fn timer_deadlines_follow_duplicate_identity_rearm_and_last_alias_close() {
        let mut objects = Objects::default();
        let original = objects.insert(timer(false));
        let duplicate = objects.duplicate(original).unwrap();
        assert!(objects.arm_timer(original, 100, 10));
        assert!(matches!(objects.close(original), Some(None)));
        assert_eq!(objects.next_timer_deadline(), Some(100));
        objects.expire_timers(100);
        assert_eq!(
            objects.next_timer_deadline(),
            None,
            "unconsumed periodic signal needs no extra idle wake"
        );
        assert!(crate::sched::poll_object(
            &mut objects,
            &[],
            duplicate,
            0,
            true
        ));
        assert_eq!(objects.next_timer_deadline(), Some(110));
        assert!(objects.arm_timer(duplicate, 150, 0));
        assert_eq!(objects.timer_deadlines.len(), 1);
        assert_eq!(objects.next_timer_deadline(), Some(150));
        assert!(objects.cancel_timer(duplicate));
        assert!(objects.timer_deadlines.is_empty());
        assert!(objects.arm_timer(duplicate, 200, 0));
        assert!(matches!(
            objects.close(duplicate),
            Some(Some(Object::Timer { .. }))
        ));
        assert!(objects.timer_deadlines.is_empty());
        assert_eq!(objects.object_count(), 0);
    }
    #[test]
    fn timer_periods_coalesce_and_waitall_does_not_consume_a_partial_signal() {
        let mut objects = Objects::default();
        let timer = objects.insert(timer(false));
        let event = objects.insert(Object::Event {
            manual_reset: false,
            signaled: false,
        });
        assert!(objects.arm_timer(timer, 10, 10));
        objects.expire_timers(95);
        assert!(matches!(
            objects.get(timer),
            Some(Object::Timer {
                deadline: Some(100),
                signaled: true,
                ..
            })
        ));
        assert!(crate::sched::poll_objects(&mut objects, &[], &[timer, event], 0, true).is_none());
        assert!(matches!(
            objects.get(timer),
            Some(Object::Timer { signaled: true, .. })
        ));
        if let Some(Object::Event { signaled, .. }) = objects.get_mut(event) {
            *signaled = true;
        }
        assert!(crate::sched::poll_objects(&mut objects, &[], &[timer, event], 0, true).is_some());
        assert!(!crate::sched::poll_object(
            &mut objects,
            &[],
            timer,
            0,
            true
        ));
        assert_eq!(objects.next_timer_deadline(), Some(100));
        objects.expire_timers(100);
        assert!(crate::sched::poll_object(&mut objects, &[], timer, 0, true));
        assert!(!crate::sched::poll_object(
            &mut objects,
            &[],
            timer,
            0,
            true
        ));
    }
}

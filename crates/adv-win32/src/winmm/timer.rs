//! Objects owns each timer until cancellation or callback completion. The queue
//! contains only borrowed identities; neither deadlines nor continuations own
//! a second payload or manufacture their own generation counter.
use adv_machine::{Identity, Namespace, Objects};
use std::collections::BTreeMap;

use super::{MAX_TIMERS, PERIODIC};

#[derive(Clone, Copy)]
pub(super) struct Timer {
    pub due: Option<u64>,
    pub interval: u64,
    pub target: u32,
    pub user: u32,
    pub flags: u32,
    pub running: bool,
    pub canceled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    pub id: u32,
    identity: Identity<Timer>,
}

#[derive(Default)]
pub(super) struct Queue {
    namespace: Option<Namespace>,
    deadlines: BTreeMap<(u64, u32), Identity<Timer>>,
    pub active: Option<Entry>,
}

impl Queue {
    pub fn insert(&mut self, objects: &mut Objects, timer: Timer) -> Option<Entry> {
        if self.namespace.is_none() {
            self.namespace = objects.new_namespace();
        }
        let ns = self.namespace?;
        if objects.alias_count(ns) >= MAX_TIMERS {
            return None;
        }
        let id = objects.insert_host_in(ns, timer)?;
        Some(Entry {
            id,
            identity: objects.lookup_alias(ns, id).unwrap(),
        })
    }

    pub fn publish(&mut self, objects: &Objects, entry: Entry) {
        let timer = objects.owned(entry.identity).unwrap();
        self.deadlines
            .insert((timer.due.unwrap(), entry.id), entry.identity);
    }

    pub fn remove(&mut self, objects: &mut Objects, entry: Entry) {
        let Some(ns) = self.namespace else { return };
        if objects.lookup_alias(ns, entry.id) != Some(entry.identity) {
            return;
        }
        if let Some(due) = objects.owned(entry.identity).unwrap().due {
            self.deadlines.remove(&(due, entry.id));
        }
        objects.close_alias(ns, entry.id);
    }

    pub fn cancel(&mut self, objects: &mut Objects, id: u32) -> Option<Timer> {
        let identity = objects.lookup_alias::<Timer>(self.namespace?, id)?;
        let timer = objects.owned_mut(identity)?;
        if timer.canceled {
            return None;
        }
        timer.canceled = true;
        if let Some(due) = timer.due.take() {
            self.deadlines.remove(&(due, id));
        }
        let result = *timer;
        // A callback still holds this alias and its charged Objects slot. It
        // is retired by finished, including when the worker exits abnormally.
        if !result.running {
            self.remove(objects, Entry { id, identity });
        }
        Some(result)
    }

    pub fn next_due(&self) -> Option<u64> {
        self.deadlines.first_key_value().map(|(&(due, _), _)| due)
    }

    pub fn dispatch(&mut self, objects: &mut Objects, now: u64) -> Option<(Entry, Timer)> {
        if self.active.is_some() || self.next_due()? > now {
            return None;
        }
        let ((due, id), identity) = self.deadlines.pop_first().unwrap();
        let timer = objects
            .owned_mut(identity)
            .expect("deadline keeps its timer alias live");
        debug_assert!(!timer.canceled && !timer.running);
        timer.running = true;
        timer.due = if timer.flags & PERIODIC != 0 {
            Some(due.saturating_add(
                (now.saturating_sub(due) / timer.interval + 1).saturating_mul(timer.interval),
            ))
        } else {
            None
        };
        if let Some(next) = timer.due {
            self.deadlines.insert((next, id), identity);
        }
        let entry = Entry { id, identity };
        self.active = Some(entry);
        Some((entry, *timer))
    }

    pub fn finished(&mut self, objects: &mut Objects, entry: Entry) {
        if self.active != Some(entry) {
            return;
        }
        self.active = None;
        let timer = objects
            .owned_mut(entry.identity)
            .expect("active callback retains timer alias");
        timer.running = false;
        if timer.canceled || timer.due.is_none() {
            self.remove(objects, entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_machine::{Object, ObjectLimits};

    fn periodic() -> Timer {
        Timer {
            due: Some(10_000),
            interval: 10_000,
            target: 1,
            user: 0,
            flags: PERIODIC,
            running: false,
            canceled: false,
        }
    }

    #[test]
    fn late_periodic_timer_coalesces_missed_ticks_and_keeps_original_phase() {
        let (mut queue, mut objects) = (Queue::default(), Objects::default());
        let entry = queue.insert(&mut objects, periodic()).unwrap();
        queue.publish(&objects, entry);
        let now = 3_600_005_000;
        assert_eq!(queue.dispatch(&mut objects, now).unwrap().0, entry);
        queue.finished(&mut objects, entry);
        assert!(queue.dispatch(&mut objects, now).is_none());
        assert_eq!(queue.next_due(), Some(3_600_010_000));
        assert_eq!(
            queue.dispatch(&mut objects, 3_600_010_000).unwrap().0,
            entry
        );
    }

    #[test]
    fn cancellation_keeps_active_payload_charged_until_completion() {
        let (mut queue, mut objects) = (
            Queue::default(),
            Objects::with_limits(ObjectLimits {
                objects: 1,
                aliases: 1,
                ..ObjectLimits::default()
            }),
        );
        let entry = queue.insert(&mut objects, periodic()).unwrap();
        queue.publish(&objects, entry);
        queue.dispatch(&mut objects, 10_000).unwrap();
        assert!(queue.cancel(&mut objects, entry.id).unwrap().running);
        assert!(queue.cancel(&mut objects, entry.id).is_none());
        assert_eq!(queue.next_due(), None);
        assert_eq!(objects.object_count(), 1);
        assert_eq!(objects.strong_count(entry.identity), Some(1));
        assert!(queue.insert(&mut objects, periodic()).is_none());
        queue.finished(&mut objects, entry);
        assert_eq!(objects.object_count(), 0);
        assert!(objects.owned(entry.identity).is_none());
        let next = queue.insert(&mut objects, periodic()).unwrap();
        assert_ne!(entry.id, next.id);
        assert_ne!(entry.identity, next.identity);
        assert!(queue.cancel(&mut objects, entry.id).is_none());
        queue.publish(&objects, next);
        queue.dispatch(&mut objects, 10_000).unwrap();
        queue.finished(&mut objects, entry);
        assert_eq!(queue.active, Some(next));
    }

    #[test]
    fn one_shot_remains_owned_through_callback_and_retires_once() {
        let (mut queue, mut objects) = (Queue::default(), Objects::default());
        let mut timer = periodic();
        timer.flags = 0;
        let entry = queue.insert(&mut objects, timer).unwrap();
        queue.publish(&objects, entry);
        queue.dispatch(&mut objects, 10_000).unwrap();
        assert_eq!(queue.next_due(), None);
        assert_eq!(objects.object_count(), 1);
        assert!(objects.owned(entry.identity).unwrap().running);
        queue.finished(&mut objects, entry);
        queue.finished(&mut objects, entry);
        assert_eq!(objects.object_count(), 0);
    }

    #[test]
    fn timer_aliases_are_isolated_and_never_wrap_or_reuse_after_exhaustion() {
        let mut objects = Objects::default();
        let ns = objects.new_namespace_in(0x104, 0x108).unwrap();
        let kernel = objects.insert(Object::Event {
            manual_reset: false,
            signaled: false,
        });
        let mut queue = Queue {
            namespace: Some(ns),
            ..Queue::default()
        };
        let first = queue.insert(&mut objects, periodic()).unwrap();
        assert_eq!(first.id, kernel);
        objects.close(kernel).unwrap();
        assert!(objects.owned(first.identity).is_some());
        queue.remove(&mut objects, first);
        let second = queue.insert(&mut objects, periodic()).unwrap();
        queue.remove(&mut objects, second);
        assert!(queue.insert(&mut objects, periodic()).is_none());
        assert_eq!(objects.object_count(), 0);
    }

    #[test]
    fn timer_limit_counts_active_canceled_callbacks_and_can_be_reused_after_finish() {
        let (mut queue, mut objects) = (Queue::default(), Objects::default());
        let entries: Vec<_> = (0..MAX_TIMERS)
            .map(|_| {
                let entry = queue.insert(&mut objects, periodic()).unwrap();
                queue.publish(&objects, entry);
                entry
            })
            .collect();
        assert!(queue.insert(&mut objects, periodic()).is_none());
        let active = queue.dispatch(&mut objects, 10_000).unwrap().0;
        queue.cancel(&mut objects, active.id).unwrap();
        assert!(queue.insert(&mut objects, periodic()).is_none());
        queue.finished(&mut objects, active);
        assert!(queue.insert(&mut objects, periodic()).is_some());
        for entry in entries.into_iter().filter(|e| *e != active) {
            queue.cancel(&mut objects, entry.id).unwrap();
        }
        assert_eq!(queue.next_due(), None);
        assert_eq!(objects.object_count(), 1);
    }
}

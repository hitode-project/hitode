//! One-shot wake preference with a protected round-robin lane.
//! A promoted dispatch never moves the normal cursor. At most one promotion may
//! precede each normal dispatch, so a stream of short waits cannot starve workers.

/// Urgency belongs to the event that made a context runnable, not its identity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum WakePriority {
    Audio,
    Input,
    #[default]
    Normal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WakeStamp {
    pub observed_us: u64,
    pub deadline_us: Option<u64>,
    pub priority: WakePriority,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Selection {
    pub id: u32,
    pub promoted: bool,
    pub wake: Option<WakeStamp>,
}

#[derive(Default)]
pub(crate) struct WakeQueue {
    pending: Vec<Option<WakeStamp>>,
    last_promoted: Option<u32>,
    urgent_run: u8,
}

impl WakeQueue {
    pub fn retire(&mut self, id: u32) {
        if let Some(stamp) = self.pending.get_mut(id as usize) {
            *stamp = None;
        }
        if self.last_promoted == Some(id) {
            self.last_promoted = None;
            self.urgent_run = 0;
        }
    }

    pub fn wake(&mut self, id: u32, observed_us: u64, deadline_us: Option<u64>) {
        self.wake_as(id, observed_us, deadline_us, WakePriority::Normal);
    }
    pub fn wake_as(
        &mut self,
        id: u32,
        observed_us: u64,
        deadline_us: Option<u64>,
        priority: WakePriority,
    ) {
        self.pending
            .resize(self.pending.len().max(id as usize + 1), None);
        // Re-observation must not renew the priority or lose the original wait.
        let stamp = self.pending[id as usize].get_or_insert(WakeStamp {
            observed_us,
            deadline_us,
            priority,
        });
        stamp.priority = stamp.priority.min(priority);
    }

    pub fn pick(
        &mut self,
        after: &mut Option<u32>,
        count: usize,
        ready: impl Fn(u32) -> bool,
        prefer_wakes: bool,
    ) -> Option<Selection> {
        if count == 0 {
            return None;
        }
        if !prefer_wakes {
            self.last_promoted = None;
        }
        if prefer_wakes {
            let first = self
                .pending
                .iter()
                .enumerate()
                .filter_map(|(i, stamp)| stamp.map(|stamp| (i as u32, stamp)))
                .filter(|&(id, _)| (id as usize) < count && ready(id))
                .min_by_key(|&(id, stamp)| (stamp.priority, stamp.observed_us, id));
            if let Some((id, stamp)) = first
                && (self.last_promoted.is_none()
                    || (stamp.priority == WakePriority::Audio && self.urgent_run < 8))
            {
                self.urgent_run = if stamp.priority == WakePriority::Audio {
                    self.urgent_run + 1
                } else {
                    0
                };
                self.last_promoted = Some(id);
                return Some(Selection {
                    id,
                    promoted: true,
                    wake: self.pending[id as usize].take(),
                });
            }
        }
        let n = count as u32;
        let first = after.map_or(0, |id| id.wrapping_add(1)) % n;
        let find = |skip: Option<u32>| {
            (0..n)
                .map(|k| (first + k) % n)
                .find(|&id| Some(id) != skip && ready(id))
        };
        // Do not immediately give the promoted context a second turn when there
        // is another runnable context. Blocking or repeated waking does not
        // rewind the normal cursor or erase the normal lane's turn.
        let id = find(self.last_promoted).or_else(|| find(None))?;
        self.last_promoted = None;
        self.urgent_run = 0;
        *after = Some(id);
        let wake = self.pending.get_mut(id as usize).and_then(Option::take);
        Some(Selection {
            id,
            promoted: false,
            wake,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // spec: PROC-MACH-5
    #[test]
    fn audio_can_follow_input_without_waiting_for_a_normal_turn() {
        let mut q = WakeQueue::default();
        let mut cursor = None;
        q.wake(2, 0, None);
        q.wake_as(1, 1, None, WakePriority::Input);
        assert_eq!(q.pick(&mut cursor, 3, |_| true, true).unwrap().id, 1);
        q.wake_as(0, 2, None, WakePriority::Audio);
        let next = q.pick(&mut cursor, 3, |_| true, true).unwrap();
        assert_eq!(next.id, 0);
        assert_eq!(next.wake.unwrap().priority, WakePriority::Audio);
        assert_eq!(cursor, None);
        assert!(!q.pick(&mut cursor, 3, |_| true, true).unwrap().promoted);
    }
    #[test]
    fn audio_event_storm_has_a_bounded_burst_and_preserves_normal_progress() {
        let mut q = WakeQueue::default();
        let mut cursor = None;
        let mut counts = [0; 3];
        let mut streak = 0;
        for time in 0..900 {
            q.wake_as(0, time, None, WakePriority::Audio);
            let selected = q.pick(&mut cursor, 3, |_| true, true).unwrap();
            counts[selected.id as usize] += 1;
            streak = if selected.promoted { streak + 1 } else { 0 };
            assert!(streak <= 8);
        }
        assert!(counts[1] >= 50 && counts[2] >= 50, "{counts:?}");
    }
    #[test]
    fn promoted_wake_does_not_rewind_round_robin() {
        let mut q = WakeQueue::default();
        let mut cursor = Some(1);
        q.wake(0, 100, Some(90));
        let s = q.pick(&mut cursor, 3, |_| true, true).unwrap();
        assert_eq!(s.id, 0);
        assert!(s.promoted);
        assert_eq!(s.wake.unwrap().deadline_us, Some(90));
        assert_eq!(cursor, Some(1));
        let s = q.pick(&mut cursor, 3, |_| true, true).unwrap();
        assert_eq!(s.id, 2);
        assert!(!s.promoted);
        assert_eq!(cursor, Some(2));
    }
    // spec: PROC-MACH-5
    #[test]
    fn recurring_wakes_cannot_starve_normal_workers() {
        let mut q = WakeQueue::default();
        let mut cursor = None;
        let mut counts = [0; 4];
        for now in 0..600 {
            q.wake(0, now, Some(now));
            let s = q.pick(&mut cursor, 4, |_| true, true).unwrap();
            counts[s.id as usize] += 1;
        }
        assert_eq!(counts, [300, 100, 100, 100]);
    }
    #[test]
    fn oldest_observed_wake_first_and_each_stamp_consumed_once() {
        let mut q = WakeQueue::default();
        let mut cursor = None;
        q.wake(2, 5, None);
        q.wake(1, 7, Some(6));
        q.wake(2, 20, None);
        assert_eq!(q.pick(&mut cursor, 3, |_| true, true).unwrap().id, 2);
        assert_eq!(q.pick(&mut cursor, 3, |_| true, true).unwrap().id, 0);
        assert_eq!(q.pick(&mut cursor, 3, |_| true, true).unwrap().id, 1);
        for _ in 0..10 {
            let s = q.pick(&mut cursor, 3, |_| true, true).unwrap();
            assert!(!s.promoted && s.wake.is_none());
        }
    }
    #[test]
    fn disabled_preference_keeps_normal_order_and_drains_telemetry() {
        let mut q = WakeQueue::default();
        let mut cursor = Some(0);
        q.wake(2, 0, None);
        assert_eq!(q.pick(&mut cursor, 3, |_| true, false).unwrap().id, 1);
        let s = q.pick(&mut cursor, 3, |_| true, false).unwrap();
        assert_eq!(s.id, 2);
        assert!(s.wake.is_some() && !s.promoted);
    }
    #[test]
    fn empty_ready_set_and_growing_context_table_are_safe() {
        let mut q = WakeQueue::default();
        let mut cursor = None;
        assert!(q.pick(&mut cursor, 0, |_| true, true).is_none());
        q.wake(7, 1, None);
        assert!(q.pick(&mut cursor, 8, |_| false, true).is_none());
        assert_eq!(q.pick(&mut cursor, 8, |id| id == 7, true).unwrap().id, 7);
        assert_eq!(q.pick(&mut cursor, 8, |id| id == 7, true).unwrap().id, 7);
    }
}

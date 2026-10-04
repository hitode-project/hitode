//! Event and relative-priority preferences with a protected round-robin lane.
//! Preferences never move the normal cursor. Mixed relative priorities receive
//! at most eight preferred dispatches before a normal turn. Equal priorities
//! preserve the original wake-promotion policy and round-robin order.

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
    pub relative_preferred: bool,
    pub wake: Option<WakeStamp>,
}

#[derive(Default)]
pub(crate) struct WakeQueue {
    pending: Vec<Option<WakeStamp>>,
    last_promoted: Option<u32>,
    urgent_run: u8,
    preferred_run: u8,
    priority_after: Option<u32>,
    yield_debt: Option<u32>,
}

impl WakeQueue {
    /// An explicit guest yield gives an available peer the next ordinary turn.
    /// The debt is consumed by one selection, never a permanent ready filter.
    pub fn note_yield(&mut self, id: u32) {
        self.yield_debt = Some(id);
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
        ready: impl Fn(u32) -> Option<i8>,
        prefer_wakes: bool,
    ) -> Option<Selection> {
        if count == 0 {
            return None;
        }
        if !prefer_wakes {
            self.last_promoted = None;
        }
        let n = count as u32;
        let yielding = self.yield_debt.take();
        let has_peer =
            yielding.is_some_and(|yielding| (0..n).any(|id| id != yielding && ready(id).is_some()));
        let ordinary_ready = |id| {
            if has_peer && yielding == Some(id) {
                None
            } else {
                ready(id)
            }
        };
        let mut lowest = i8::MAX;
        let mut highest = i8::MIN;
        for id in 0..n {
            if let Some(priority) = ordinary_ready(id) {
                lowest = lowest.min(priority);
                highest = highest.max(priority);
            }
        }
        let mixed_priorities = lowest < highest;
        if !mixed_priorities {
            self.preferred_run = 0;
        }
        let allow_preference = !mixed_priorities || self.preferred_run < 8;
        if prefer_wakes && allow_preference {
            let first = self
                .pending
                .iter()
                .enumerate()
                .filter_map(|(i, stamp)| stamp.map(|stamp| (i as u32, stamp)))
                .filter(|&(id, _)| (id as usize) < count && ready(id).is_some())
                .filter(|&(id, stamp)| {
                    stamp.priority != WakePriority::Normal
                        || (ordinary_ready(id).is_some()
                            && (!mixed_priorities || ordinary_ready(id) == Some(highest)))
                })
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
                if mixed_priorities {
                    self.preferred_run += 1;
                }
                return Some(Selection {
                    id,
                    promoted: true,
                    relative_preferred: false,
                    wake: self.pending[id as usize].take(),
                });
            }
        }
        if mixed_priorities && allow_preference {
            let first = self.priority_after.map_or(0, |id| id.wrapping_add(1)) % n;
            let id = (0..n)
                .map(|k| (first + k) % n)
                .find(|&id| ordinary_ready(id) == Some(highest))?;
            self.priority_after = Some(id);
            self.preferred_run += 1;
            return Some(Selection {
                id,
                promoted: false,
                relative_preferred: true,
                wake: self.pending.get_mut(id as usize).and_then(Option::take),
            });
        }
        let first = after.map_or(0, |id| id.wrapping_add(1)) % n;
        let find = |skip: Option<u32>| {
            (0..n)
                .map(|k| (first + k) % n)
                .find(|&id| Some(id) != skip && ordinary_ready(id).is_some())
        };
        // Do not immediately give the promoted context a second turn when there
        // is another runnable context. Blocking or repeated waking does not
        // rewind the normal cursor or erase the normal lane's turn.
        let id = find(self.last_promoted).or_else(|| find(None))?;
        self.last_promoted = None;
        self.urgent_run = 0;
        self.preferred_run = 0;
        *after = Some(id);
        let wake = self.pending.get_mut(id as usize).and_then(Option::take);
        Some(Selection {
            id,
            promoted: false,
            relative_preferred: false,
            wake,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn audio_can_follow_input_without_waiting_for_a_normal_turn() {
        let mut q = WakeQueue::default();
        let mut cursor = None;
        q.wake(2, 0, None);
        q.wake_as(1, 1, None, WakePriority::Input);
        assert_eq!(q.pick(&mut cursor, 3, |_| Some(0), true).unwrap().id, 1);
        q.wake_as(0, 2, None, WakePriority::Audio);
        let next = q.pick(&mut cursor, 3, |_| Some(0), true).unwrap();
        assert_eq!(next.id, 0);
        assert_eq!(next.wake.unwrap().priority, WakePriority::Audio);
        assert_eq!(cursor, None);
        assert!(!q.pick(&mut cursor, 3, |_| Some(0), true).unwrap().promoted);
    }
    #[test]
    fn audio_event_storm_has_a_bounded_burst_and_preserves_normal_progress() {
        let mut q = WakeQueue::default();
        let mut cursor = None;
        let mut counts = [0; 3];
        let mut streak = 0;
        for time in 0..900 {
            q.wake_as(0, time, None, WakePriority::Audio);
            let selected = q.pick(&mut cursor, 3, |_| Some(0), true).unwrap();
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
        let s = q.pick(&mut cursor, 3, |_| Some(0), true).unwrap();
        assert_eq!(s.id, 0);
        assert!(s.promoted);
        assert_eq!(s.wake.unwrap().deadline_us, Some(90));
        assert_eq!(cursor, Some(1));
        let s = q.pick(&mut cursor, 3, |_| Some(0), true).unwrap();
        assert_eq!(s.id, 2);
        assert!(!s.promoted);
        assert_eq!(cursor, Some(2));
    }
    #[test]
    fn recurring_wakes_cannot_starve_normal_workers() {
        let mut q = WakeQueue::default();
        let mut cursor = None;
        let mut counts = [0; 4];
        for now in 0..600 {
            q.wake(0, now, Some(now));
            let s = q.pick(&mut cursor, 4, |_| Some(0), true).unwrap();
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
        assert_eq!(q.pick(&mut cursor, 3, |_| Some(0), true).unwrap().id, 2);
        assert_eq!(q.pick(&mut cursor, 3, |_| Some(0), true).unwrap().id, 0);
        assert_eq!(q.pick(&mut cursor, 3, |_| Some(0), true).unwrap().id, 1);
        for _ in 0..10 {
            let s = q.pick(&mut cursor, 3, |_| Some(0), true).unwrap();
            assert!(!s.promoted && s.wake.is_none());
        }
    }
    #[test]
    fn disabled_preference_keeps_normal_order_and_drains_telemetry() {
        let mut q = WakeQueue::default();
        let mut cursor = Some(0);
        q.wake(2, 0, None);
        assert_eq!(q.pick(&mut cursor, 3, |_| Some(0), false).unwrap().id, 1);
        let s = q.pick(&mut cursor, 3, |_| Some(0), false).unwrap();
        assert_eq!(s.id, 2);
        assert!(s.wake.is_some() && !s.promoted);
    }
    #[test]
    fn empty_ready_set_and_growing_context_table_are_safe() {
        let mut q = WakeQueue::default();
        let mut cursor = None;
        assert!(q.pick(&mut cursor, 0, |_| Some(0), true).is_none());
        q.wake(7, 1, None);
        assert!(q.pick(&mut cursor, 8, |_| None, true).is_none());
        assert_eq!(
            q.pick(&mut cursor, 8, |id| (id == 7).then_some(0), true)
                .unwrap()
                .id,
            7
        );
        assert_eq!(
            q.pick(&mut cursor, 8, |id| (id == 7).then_some(0), true)
                .unwrap()
                .id,
            7
        );
    }

    #[test]
    fn relative_priority_prefers_higher_ready_peers_with_equal_level_round_robin() {
        let mut q = WakeQueue::default();
        let mut cursor = None;
        let priorities = [0, 1, 1];
        for expected in [1, 2, 1, 2, 1, 2, 1, 2] {
            let selected = q
                .pick(&mut cursor, 3, |id| Some(priorities[id as usize]), true)
                .unwrap();
            assert_eq!(selected.id, expected);
            assert!(selected.relative_preferred && !selected.promoted);
            assert_eq!(
                cursor, None,
                "priority lane must not move the ordinary cursor"
            );
        }
        let selected = q
            .pick(&mut cursor, 3, |id| Some(priorities[id as usize]), true)
            .unwrap();
        assert_eq!(selected.id, 0);
        assert!(!selected.relative_preferred && !selected.promoted);
        assert_eq!(cursor, Some(0));
    }

    #[test]
    fn relative_priority_storm_keeps_every_ordinary_peer_progressing() {
        let mut q = WakeQueue::default();
        let mut cursor = None;
        let priorities = [0, 2, -1, 1];
        let mut counts = [0; 4];
        let mut streak = 0;
        for _ in 0..900 {
            let selected = q
                .pick(&mut cursor, 4, |id| Some(priorities[id as usize]), true)
                .unwrap();
            counts[selected.id as usize] += 1;
            streak = if selected.relative_preferred {
                streak + 1
            } else {
                0
            };
            assert!(streak <= 8);
        }
        assert_eq!(counts, [25, 825, 25, 25]);
    }

    #[test]
    fn lower_normal_wakes_wait_for_fairness_but_audio_and_input_keep_urgency() {
        let mut q = WakeQueue::default();
        let mut cursor = None;
        let priorities = [0, 1];
        for time in 0..8 {
            q.wake(0, time, Some(time));
            let selected = q
                .pick(&mut cursor, 2, |id| Some(priorities[id as usize]), true)
                .unwrap();
            assert_eq!(
                selected.id, 1,
                "a normal timed wake cannot erase relative priority"
            );
            assert!(selected.relative_preferred && !selected.promoted);
        }
        let selected = q
            .pick(&mut cursor, 2, |id| Some(priorities[id as usize]), true)
            .unwrap();
        assert_eq!(selected.id, 0);
        assert!(!selected.promoted && !selected.relative_preferred);
        assert_eq!(selected.wake.unwrap().observed_us, 0);
        for urgency in [WakePriority::Input, WakePriority::Audio] {
            q.wake_as(0, 100, None, urgency);
            let selected = q
                .pick(&mut cursor, 2, |id| Some(priorities[id as usize]), true)
                .unwrap();
            assert_eq!(selected.id, 0);
            assert!(selected.promoted && !selected.relative_preferred);
            assert_eq!(selected.wake.unwrap().priority, urgency);
        }
    }

    #[test]
    fn event_and_relative_preferences_share_the_bounded_fairness_budget() {
        let mut q = WakeQueue::default();
        let mut cursor = None;
        let priorities = [0, 1, -2];
        let mut counts = [0; 3];
        let mut streak = 0;
        for now in 0..900 {
            q.wake_as(0, now, None, WakePriority::Audio);
            let selected = q
                .pick(&mut cursor, 3, |id| Some(priorities[id as usize]), true)
                .unwrap();
            counts[selected.id as usize] += 1;
            streak = if selected.relative_preferred || selected.promoted {
                streak + 1
            } else {
                0
            };
            assert!(streak <= 8);
        }
        assert!(counts[1] >= 40 && counts[2] >= 40, "{counts:?}");
    }

    #[test]
    fn blocked_priority_never_enters_selection_and_changes_apply_on_next_pick() {
        let mut q = WakeQueue::default();
        let mut cursor = None;
        q.wake_as(1, 0, None, WakePriority::Audio);
        let selected = q
            .pick(&mut cursor, 2, |id| (id == 0).then_some(-15), true)
            .unwrap();
        assert_eq!(
            selected.id, 0,
            "a pending wake cannot make a blocked context ready"
        );
        let selected = q
            .pick(
                &mut cursor,
                2,
                |id| Some(if id == 0 { 15 } else { -15 }),
                true,
            )
            .unwrap();
        assert_eq!(
            selected.id, 1,
            "an actual Audio wake overrides relative priority"
        );
        let selected = q
            .pick(
                &mut cursor,
                2,
                |id| Some(if id == 0 { 15 } else { -15 }),
                true,
            )
            .unwrap();
        assert_eq!(selected.id, 0);
        let selected = q
            .pick(
                &mut cursor,
                2,
                |id| Some(if id == 0 { -15 } else { 15 }),
                true,
            )
            .unwrap();
        assert_eq!(selected.id, 1);
    }

    #[test]
    fn explicit_yield_owes_one_peer_turn_but_does_not_permanently_exclude_the_yielder() {
        let mut q = WakeQueue::default();
        let mut cursor = None;
        let ready = |id| Some(if id == 1 { 15 } else { 0 });
        assert_eq!(q.pick(&mut cursor, 2, ready, true).unwrap().id, 1);
        q.note_yield(1);
        assert_eq!(q.pick(&mut cursor, 2, ready, true).unwrap().id, 0);
        assert_eq!(q.pick(&mut cursor, 2, ready, true).unwrap().id, 1);
        q.note_yield(1);
        q.wake_as(1, 0, None, WakePriority::Audio);
        assert_eq!(
            q.pick(&mut cursor, 2, ready, true).unwrap().id,
            1,
            "a new urgent event may override the ordinary yield debt"
        );
        q.note_yield(1);
        assert_eq!(
            q.pick(&mut cursor, 2, |id| (id == 1).then_some(15), true)
                .unwrap()
                .id,
            1,
            "yielding alone remains runnable"
        );
    }
}

use super::*;
fn sequence(slot: u128, key: u128, r: Rect) -> Sequence {
    Sequence {
        entries: vec![
            (
                0,
                Rect {
                    x: 0,
                    y: 0,
                    w: 100,
                    h: 100,
                },
            ),
            (key, r),
        ],
        full: Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 100,
        },
        view: Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 100,
        },
        shape: 1,
        slot,
        independent: true,
        ..Default::default()
    }
}
fn feed(h: &mut History, mut next: Sequence) {
    let entries = std::mem::take(&mut next.entries);
    h.next = next;
    for (key, bounds) in entries {
        h.record(key, bounds, Arc::default());
    }
}
fn step(h: &mut History, slot: u128, key: u128, r: Rect) -> (Rect, Rect) {
    feed(h, sequence(slot, key, r));
    h.advance(false, &HashMap::default())
}
// spec: GFX-REC-4 GFX-REC-5
#[test]
fn ordered_differences_include_old_new_and_removed_bounds_and_buffer_ages() {
    let a = Rect {
        x: 2,
        y: 3,
        w: 4,
        h: 5,
    };
    let b = Rect { x: 12, ..a };
    let c = Rect { y: 13, ..a };
    let mut h = History::default();
    assert_eq!(step(&mut h, 1, 1, a).1.w, 100);
    assert_eq!(step(&mut h, 2, 2, b).1.w, 100);
    let (changed, redraw) = step(&mut h, 1, 3, c);
    assert_eq!(changed, b.union(c));
    assert_eq!(
        redraw,
        a.union(b).union(c),
        "age two includes both intervening damages"
    );
    step(&mut h, 3, 4, a);
    assert_eq!(
        step(&mut h, 2, 5, b).1,
        a.union(b).union(c),
        "age three includes three damages"
    );
    let mut removed = sequence(2, 5, b);
    removed.entries.pop();
    feed(&mut h, removed);
    assert_eq!(
        h.advance(false, &HashMap::default()).0,
        b,
        "removed commands retain their old bounds"
    );
}
// spec: GFX-REC-4 GFX-REC-5
#[test]
fn missing_slots_and_shape_changes_need_full_but_large_damage_keeps_its_bounds() {
    let r = Rect {
        x: 2,
        y: 3,
        w: 4,
        h: 5,
    };
    let mut h = History::default();
    step(&mut h, 1, 1, r);
    for n in 2..=10 {
        step(&mut h, 2, n, r);
    }
    assert_eq!(h.changes.len(), 8);
    assert_eq!(step(&mut h, 1, 11, r).1.w, 100);
    feed(&mut h, sequence(1, 11, r));
    h.next.shape = 2;
    assert_eq!(h.advance(false, &HashMap::default()).1.w, 100);
    step(&mut h, 1, 11, r);
    let large = Rect { w: 80, h: 80, ..r };
    assert_eq!(step(&mut h, 1, 12, large).1, large);
    let mut d = Damage::default();
    for x in 0..5 {
        d.add(Rect {
            x: x * 5,
            y: 0,
            w: 1,
            h: 1,
        });
    }
    assert_eq!(d.len, 1);
    assert_eq!(d.bound().w, 21);
}

// spec: GFX-REC-4, GFX-REC-11
#[test]
fn d3d9_differential_insertions_removals_and_reordering_damage_only_unmatched_bounds() {
    let a = Rect {
        x: 2,
        y: 2,
        w: 3,
        h: 3,
    };
    let b = Rect { x: 12, ..a };
    let c = Rect { x: 22, ..a };
    let inserted = Rect { x: 32, ..a };
    let mut h = History::default();
    let mut run = |entries: &[(u128, Rect)]| {
        let mut next = sequence(1, 1, a);
        next.entries = entries.to_vec();
        feed(&mut h, next);
        h.advance(false, &HashMap::default()).0
    };
    assert_eq!(run(&[(1, a), (2, b), (3, c)]).w, 100);
    assert_eq!(run(&[(1, a), (4, inserted), (2, b), (3, c)]), inserted);
    assert_eq!(run(&[(1, a), (2, b), (3, c)]), inserted);
    assert_eq!(run(&[(1, a), (2, b), (3, c)]), Rect::default());
    assert_eq!(run(&[(2, b), (1, a), (3, c)]), a);
}

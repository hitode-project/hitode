use super::*;
fn sequence(slot: u64, key: u64, r: Rect) -> Sequence {
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
        shape: 1,
        slot,
        independent: true,
    }
}
fn step(h: &mut History, slot: u64, key: u64, r: Rect) -> (Rect, Rect) {
    h.next = sequence(slot, key, r);
    h.advance(false)
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
    h.next = sequence(2, 5, b);
    h.next.entries.pop();
    assert_eq!(
        h.advance(false).0,
        b,
        "removed commands retain their old bounds"
    );
}
// spec: GFX-REC-4 GFX-REC-5
#[test]
fn missing_slots_overflow_shape_changes_and_area_threshold_force_full() {
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
    h.next = sequence(1, 11, r);
    h.next.shape = 2;
    assert_eq!(h.advance(false).1.w, 100);
    let large = Rect { w: 80, h: 80, ..r };
    assert_eq!(step(&mut h, 1, 12, large).1.w, 100);
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

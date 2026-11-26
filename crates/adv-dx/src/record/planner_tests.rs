use super::*;

const VIEW: Rect = Rect {
    x: 0,
    y: 0,
    w: 128,
    h: 32,
};

fn key(allocation: u64) -> ResourceKey {
    ResourceKey((0, allocation), 0, 0)
}

fn draw<'a>(
    payload: u128,
    slot: u128,
    bounds: Rect,
    reads: &'a [ResourceKey],
    writes: &'a [(ResourceKey, bool)],
) -> Op<'a, u128> {
    Op {
        payload,
        effects: damage::Input {
            kind: Kind::Draw,
            view: VIEW,
            attachment: None,
            color_shape: 0,
            slot,
            bounds,
            key: payload,
            reads,
            samples: &[],
            writes,
            ignores_sampled_color: false,
            clear_keys: [None; 3],
        },
        bytes: 0,
        fresh: false,
        reuse: None,
        trace: None,
    }
}

#[test]
// spec: GFX-REC-5
fn readback_retires_only_the_observed_version_and_its_inputs() {
    let mut recording = Recording::default();
    let [source, unrelated, output] = [key(1), key(2), key(3)];
    recording.admit(draw(10, 1, VIEW, &[], &[(source, true)]));
    recording.admit(draw(11, 2, VIEW, &[], &[(unrelated, true)]));
    recording.admit(draw(12, 3, VIEW, &[source], &[(output, true)]));
    recording.admit(draw(13, 1, VIEW, &[], &[(source, true)]));

    let plan = recording.plan(Reason::Readback(output));
    let payloads = |plan: &Plan<_>, recording: &Recording<_>| {
        (recording.cursor.ops..recording.commands.len())
            .filter_map(|n| plan.at(recording, n).map(|step| *step.payload))
            .collect::<Vec<_>>()
    };
    assert_eq!(payloads(&plan, &recording), [10, 12]);
    recording.retire(
        &plan,
        Cursor {
            ops: 4,
            scissors: 0,
        },
    );
    recording.end(Some(plan));

    let plan = recording.plan(Reason::Wait);
    assert_eq!(payloads(&plan, &recording), [11, 13]);
    assert!(!recording.observes(output, false));

    recording.forget(unrelated);
    assert!(recording.contents.get(&unrelated).is_none());
    assert!(recording.contents.get(&output).is_some());
}

#[test]
// spec: GFX-REC-1, GFX-REC-5
fn retirement_keeps_the_scissor_cursor_of_a_busy_write() {
    let mut recording = Recording::default();
    let output = key(1);
    let first = Rect { w: 8, h: 8, ..VIEW };
    let last = Rect { x: 112, ..first };
    let reason = Reason::Present {
        slot: 1,
        verify: false,
    };
    let record_frame = |recording: &mut Recording<_>, value| {
        let mut clear = draw(1, 1, VIEW, &[], &[]);
        let writes = [(output, true)];
        clear.effects.writes = &writes;
        clear.effects.kind = Kind::Clear;
        clear.effects.clear_keys[0] = Some(1);
        recording.admit(clear);
        recording.admit(draw(value, 1, first, &[], &[(output, false)]));
        recording.admit(draw(value, 1, last, &[], &[(output, false)]));
    };
    record_frame(&mut recording, 10);
    let plan = recording.plan(reason);
    recording.retire(
        &plan,
        Cursor {
            ops: 3,
            scissors: 0,
        },
    );
    recording.end(Some(plan));
    recording.finish(0, Some(1));

    record_frame(&mut recording, 20);
    let plan = recording.plan(reason);
    let clipped = plan.at(&recording, 0).unwrap().scissors.unwrap();
    assert_eq!(clipped.len(), 2);
    recording.retire(
        &plan,
        Cursor {
            ops: 0,
            scissors: 1,
        },
    );

    let cursor = recording.cursor;
    let busy = plan.at(&recording, cursor.ops).unwrap();
    assert_eq!(*busy.payload, 1);
    assert_eq!(&busy.scissors.unwrap()[cursor.scissors..], &clipped[1..]);
    recording.retire(
        &plan,
        Cursor {
            ops: 1,
            scissors: 0,
        },
    );
    assert!(plan.at(&recording, 0).is_none());
    assert_eq!(*plan.at(&recording, 1).unwrap().payload, 20);
    assert_eq!(recording.cursor.scissors, 0);
}

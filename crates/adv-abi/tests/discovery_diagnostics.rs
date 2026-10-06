use adv_abi::{ApiDiagnostics, DynamicLookupFailure, DynamicLookupKind, MessageBoxCall, RunMode};

fn failure(context: u32, name: &str) -> DynamicLookupFailure {
    DynamicLookupFailure {
        sequence: 0,
        module: "user32.dll".into(),
        name: name.into(),
        failure_module: Some("user32.dll".into()),
        failure_name: Some(name.into()),
        caller: 0x401010,
        context,
        kind: DynamicLookupKind::Unimplemented,
        reason: "known SDK export has no host implementation".into(),
    }
}

fn message(context: u32) -> MessageBoxCall {
    MessageBoxCall {
        sequence: 0,
        frame: 0,
        api: "user32.dll!MessageBoxA".into(),
        caller: 0x401020,
        context,
        caption: "Test".into(),
        text: "Cannot find function LoadAcceleratorsA".into(),
        style: 0x10,
        result: 1,
        preceding_lookup_failure: None,
    }
}

#[test]
fn optional_probe_observation_is_not_a_stub_or_an_acceptance_failure() {
    let mut diagnostics = ApiDiagnostics::default();
    diagnostics.record_dynamic_lookup_failure(failure(3, "LoadAcceleratorsA"));
    assert_eq!(diagnostics.dynamic_lookup_failure_count, 1);
    assert!(diagnostics.gaps.is_empty());
    assert_eq!(diagnostics.stub_calls, 0);
    assert!(diagnostics.acceptance_eligible(RunMode::Strict));
    assert!(!diagnostics.acceptance_eligible(RunMode::Discovery));
}

#[test]
fn message_keeps_same_context_history_after_the_query_ring_is_evicted() {
    let mut diagnostics = ApiDiagnostics::default();
    diagnostics.record_dynamic_lookup_failure(failure(3, "LoadAcceleratorsA"));
    diagnostics.record_dynamic_lookup_failure(failure(4, "OtherOptionalExport"));
    diagnostics.record_message_box(message(3));
    let captured = diagnostics.message_boxes[0].clone();
    assert_eq!(captured.sequence, 3);
    let preceding = captured.preceding_lookup_failure.unwrap();
    assert_eq!((preceding.context, preceding.sequence), (3, 1));
    assert_eq!(preceding.name, "LoadAcceleratorsA");
    for index in 0..70 {
        diagnostics.record_dynamic_lookup_failure(failure(4, &format!("Other{index}")));
    }
    assert_eq!(diagnostics.dynamic_lookup_failures.len(), 64);
    assert_eq!(diagnostics.dynamic_lookup_failure_count, 72);
    assert_eq!(diagnostics.dropped_dynamic_lookup_failures, 8);
    assert_eq!(
        diagnostics.message_boxes[0]
            .preceding_lookup_failure
            .as_ref()
            .unwrap()
            .name,
        "LoadAcceleratorsA"
    );
    diagnostics.record_message_box(message(9));
    assert!(
        diagnostics.message_boxes[1]
            .preceding_lookup_failure
            .is_none()
    );
}

#[test]
fn message_history_and_unicode_fields_are_bounded_with_explicit_loss_counts() {
    let mut diagnostics = ApiDiagnostics::default();
    let mut long = message(0);
    long.caption = "漢".repeat(300);
    long.text = "😀".repeat(5000);
    diagnostics.record_message_box(long);
    assert_eq!(diagnostics.message_boxes[0].caption.chars().count(), 256);
    assert_eq!(diagnostics.message_boxes[0].text.chars().count(), 4096);
    assert_eq!(diagnostics.truncated_diagnostic_fields, 2);
    for _ in 0..35 {
        diagnostics.record_message_box(message(0));
    }
    assert_eq!(diagnostics.message_boxes.len(), 32);
    assert_eq!(diagnostics.message_box_count, 36);
    assert_eq!(diagnostics.dropped_message_boxes, 4);
    assert_eq!(diagnostics.message_boxes[0].sequence, 5);
    assert_eq!(diagnostics.message_boxes[31].sequence, 36);
}

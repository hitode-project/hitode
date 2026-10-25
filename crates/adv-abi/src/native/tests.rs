use super::*;

fn snapshot(bytes: Vec<u8>) -> ImageSnapshot {
    ImageSnapshot::new(vec![Dependency {
        offset: 0x1000,
        bytes: bytes.into(),
        kind: DependencyKind::Code,
    }])
}

// Authored x86 loop, with independent register allocation and field offsets.
fn byte_loop(
    source: u8,
    destination: u8,
    remaining: u8,
    temporary: u8,
    offset: i8,
    add: bool,
    wide: bool,
) -> Vec<u8> {
    let mut bytes = if wide { vec![0x0f, 0xb6] } else { vec![0x8a] };
    let memory = |bytes: &mut Vec<u8>, r: u8, base: u8| {
        bytes.push(0x40 | (r << 3) | base);
        if base == 4 {
            bytes.push(0x24);
        }
        bytes.push(offset as u8);
    };
    memory(&mut bytes, temporary, source);
    bytes.push(if add { 0x00 } else { 0x88 });
    memory(&mut bytes, temporary, destination);
    bytes.extend([0x40 + source, 0x40 + destination, 0x48 + remaining]);
    let size = bytes.len() + 2;
    bytes.extend([0x75, (-(size as i8)) as u8]);
    bytes
}

fn budget() -> Budget {
    Budget {
        instructions: 10000,
    }
}

fn copy(image: &ImageSnapshot) -> CandidateBinding {
    analyze::discover(image, &mut budget())
        .into_iter()
        .find(|c| c.contract.computation == Computation::Lz77)
        .unwrap()
}

// spec: EXEC-NATIVE-1, EXEC-NATIVE-2
#[test]
fn register_allocation_and_offsets_do_not_require_signatures() {
    for temporary in 0..4 {
        for source in 0..8 {
            for destination in 0..8 {
                for remaining in 0..8 {
                    let roles = [temporary, source, destination, remaining];
                    if roles
                        .iter()
                        .enumerate()
                        .any(|(i, r)| roles[..i].contains(r))
                    {
                        continue;
                    }
                    for offset in [-37, 0, 51] {
                        for wide in [false, true] {
                            let image = snapshot(byte_loop(
                                source,
                                destination,
                                remaining,
                                temporary,
                                offset,
                                false,
                                wide,
                            ));
                            let verified =
                                verify::verify(&image, copy(&image), &mut budget()).unwrap();
                            assert_eq!(verified.candidate().mapping.source.index(), source);
                            assert_eq!(
                                verified.candidate().mapping.destination.index(),
                                destination
                            );
                            assert_eq!(
                                verified.candidate().mapping.source_offset,
                                offset as i32 as u32
                            );
                            assert_eq!(verified.conditions().aliasing, Aliasing::ForwardHistory);
                            assert_eq!(
                                verified.dependencies()[0].bytes,
                                image.dependencies[0].bytes
                            );
                            assert_eq!(verified.transition().instructions, 6);
                        }
                    }
                }
            }
        }
    }
}

// spec: EXEC-NATIVE-1, EXEC-NATIVE-2
#[test]
fn png_sum_uses_the_same_code_proof_with_distinct_alias_conditions() {
    let image = snapshot(byte_loop(6, 7, 1, 0, 0, true, true));
    let candidates = analyze::discover(&image, &mut budget());
    assert_eq!(candidates.len(), 2);
    for candidate in candidates {
        let computation = candidate.contract.computation;
        let verified = verify::verify(&image, candidate, &mut budget()).unwrap();
        assert_eq!(
            verified.conditions().aliasing,
            if computation == Computation::PngUp {
                Aliasing::Disjoint
            } else {
                Aliasing::ForwardHistory
            }
        );
        assert_eq!(verified.transition().flags.len(), 4);
    }
}

// spec: EXEC-NATIVE-1
#[test]
fn proposals_cannot_change_contract_identity_or_dependencies() {
    let image = snapshot(byte_loop(6, 7, 1, 0, 0, false, true));
    let original = copy(&image);
    let mut changed = original.clone();
    changed.contract.version += 1;
    assert!(matches!(
        verify::verify(&image, changed, &mut budget()),
        Err(Rejection::Counterexample {
            reason: "reference contract identity",
            ..
        })
    ));
    let mut changed = original.clone();
    changed.image[0] ^= 1;
    assert!(matches!(
        verify::verify(&image, changed, &mut budget()),
        Err(Rejection::Counterexample {
            reason: "image identity",
            ..
        })
    ));
    let truncated = snapshot(image.dependencies[0].bytes[..3].to_vec());
    let mut original = original;
    original.image = truncated.identity();
    assert!(matches!(
        verify::verify(&truncated, original, &mut budget()),
        Err(Rejection::Counterexample {
            reason: "incomplete code dependency",
            ..
        })
    ));
}

// spec: EXEC-NATIVE-2
#[test]
fn wrong_memory_recurrences_and_loop_tests_are_counterexamples() {
    let image = snapshot(byte_loop(6, 7, 1, 0, 0, false, true));
    let original = copy(&image);
    let mut bytes = image.dependencies[0].bytes.to_vec();
    bytes[2] = 0x43; // load from EBX, outside the declared source
    let wrong_source = snapshot(bytes);
    let mut candidate = original.clone();
    candidate.image = wrong_source.identity();
    assert!(matches!(
        verify::verify(&wrong_source, candidate, &mut budget()),
        Err(Rejection::Counterexample {
            reason: "byte recurrence",
            ..
        })
    ));
    let mut bytes = image.dependencies[0].bytes.to_vec();
    bytes[9] = 0x4a; // decrement EDX instead of the counter
    let wrong_counter = snapshot(bytes);
    let mut candidate = original;
    candidate.image = wrong_counter.identity();
    assert!(matches!(
        verify::verify(&wrong_counter, candidate, &mut budget()),
        Err(Rejection::Counterexample {
            reason: "loop test is not the decreasing counter",
            ..
        })
    ));
}

// spec: EXEC-NATIVE-2
#[test]
fn hidden_effects_and_unaccounted_edges_cannot_be_admitted() {
    let mut bytes = byte_loop(6, 7, 1, 0, 0, false, true);
    let original_image = snapshot(bytes.clone());
    let original = copy(&original_image);
    // Insert another store before the two pointer increments.
    bytes.splice(7..7, [0x88, 0x07]);
    let len = bytes.len();
    bytes[len - 1] = (-(len as i8)) as u8;
    let image = snapshot(bytes);
    let mut candidate = original.clone();
    candidate.image = image.identity();
    candidate.end += 2;
    assert!(matches!(
        verify::verify(&image, candidate, &mut budget()),
        Err(Rejection::Unsupported {
            reason: "more than one memory write per step",
            ..
        })
    ));
    let mut bytes = original_image.dependencies[0].bytes.to_vec();
    bytes[0] = 0x74; // a second conditional edge, not an opaque operation
    let wrong_edge = snapshot(bytes);
    let mut candidate = original;
    candidate.image = wrong_edge.identity();
    assert!(matches!(
        verify::verify(&wrong_edge, candidate, &mut budget()),
        Err(Rejection::Unsupported { .. })
    ));
}

// spec: EXEC-NATIVE-2
#[test]
fn budget_exhaustion_and_unsupported_rules_are_distinct() {
    let image = snapshot(byte_loop(6, 7, 1, 0, 0, false, true));
    let mut candidate = copy(&image);
    assert!(matches!(
        verify::verify(&image, candidate.clone(), &mut Budget { instructions: 2 }),
        Err(Rejection::Timeout)
    ));
    candidate.contract = ReferenceContract::get(Computation::PngPaeth);
    assert!(matches!(
        verify::verify(&image, candidate, &mut budget()),
        Err(Rejection::Unsupported {
            reason: "reference recurrence",
            ..
        })
    ));
}

// spec: EXEC-NATIVE-1, EXEC-NATIVE-2
#[test]
fn operand_size_branch_cannot_be_relocated_as_a_32_bit_loop() {
    let original_image = snapshot(byte_loop(6, 7, 1, 0, 0, false, true));
    let mut candidate = copy(&original_image);
    let mut bytes = original_image.dependencies[0].bytes.to_vec();
    bytes.truncate(bytes.len() - 2);
    let displacement = -((bytes.len() + 5) as i16);
    bytes.extend([0x66, 0x0f, 0x85]);
    bytes.extend(displacement.to_le_bytes());
    let image = snapshot(bytes);
    candidate.end += 3;
    candidate.image = image.identity();
    assert!(matches!(
        verify::verify(&image, candidate, &mut budget()),
        Err(Rejection::Unsupported {
            reason: "nonrelocatable branch width",
            ..
        })
    ));
}

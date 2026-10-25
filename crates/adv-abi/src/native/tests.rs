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

// EXEC-NATIVE-3: a correct one-step recurrence alone cannot justify a scalar
// tail if its flags read a temporary left by an earlier iteration.
#[test]
fn scalar_tail_cannot_read_an_unrestored_loop_carried_temporary() {
    let mut bytes = vec![0x39, 0xd0]; // CMP EAX, EDX carries CF into the final DEC.
    bytes.extend(byte_loop(6, 7, 1, 0, 0, false, true));
    *bytes.last_mut().unwrap() = (-(bytes.len() as i8)) as u8;
    let image = snapshot(bytes);
    assert!(matches!(
        verify::verify(&image, copy(&image), &mut budget()),
        Err(Rejection::Unsupported {
            reason: "scalar tail reads a loop-carried temporary before defining it",
            ..
        })
    ));
}

// spec: EXEC-NATIVE-4
#[test]
fn scratch_stores_cannot_depend_on_loop_carried_condition_flags() {
    let body = vec![
        0x0f, 0x42, 0xc3, 0x89, 0x44, 0x24, 0x04, 0x8b, 0x04, 0x24, 0x0f, 0xb6, 0x16, 0x00, 0x17,
        0x46, 0x47, 0x49, 0x75, 0xec,
    ];
    for prefix in [None, Some(0x45), Some(0x4d)] {
        let mut bytes: Vec<_> = prefix.into_iter().collect();
        bytes.extend_from_slice(&body);
        *bytes.last_mut().unwrap() = (-(bytes.len() as i8)) as u8;
        let image = snapshot(bytes);
        let candidates = analyze::discover(&image, &mut budget());
        assert!(!candidates.is_empty());
        for candidate in candidates {
            assert!(matches!(
                verify::verify(&image, candidate, &mut budget()),
                Err(Rejection::Unsupported {
                    reason: "condition reads flags before a local definition",
                    ..
                })
            ));
        }
    }
}

// spec: EXEC-NATIVE-2, EXEC-NATIVE-3
#[test]
fn average_admission_checks_the_wide_sum_and_prefix_mapping() {
    let bytes = vec![
        0x0f, 0xb6, 0x1c, 0x30, 0x0f, 0xb6, 0x14, 0x37, 0x01, 0xda, 0xd1, 0xea, 0x00, 0x14, 0x31,
        0x46, 0x39, 0xf5, 0x75, 0xec,
    ];
    let image = snapshot(bytes.clone());
    let candidate = analyze::discover(&image, &mut budget())
        .into_iter()
        .find(|c| c.contract.computation == Computation::PngAverage)
        .unwrap();
    let binding = verify::verify(&image, candidate.clone(), &mut budget()).unwrap();
    assert!(
        binding
            .conditions()
            .iter()
            .any(|p| matches!(p, Predicate::BackwardDistance { .. }))
    );
    let mut incorrect = candidate;
    incorrect
        .view
        .inputs
        .iter_mut()
        .find(|(role, _)| *role == Input::BytesPerPixel)
        .unwrap()
        .1 = Expression::constant(Width::Dword, 4);
    assert!(matches!(
        verify::verify(&image, incorrect, &mut budget()),
        Err(Rejection::Counterexample {
            reason: "input mapping",
            ..
        })
    ));
    let mut wrong = bytes;
    wrong[9] = 0xd2; // ADD EDX, EDX loses the other predictor input.
    let image = snapshot(wrong);
    for candidate in analyze::discover(&image, &mut budget()) {
        assert!(matches!(
            verify::verify(&image, candidate, &mut budget()),
            Err(Rejection::Counterexample {
                reason: "byte recurrence",
                ..
            })
        ));
    }
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
                            assert_eq!(
                                verified
                                    .candidate()
                                    .view
                                    .input(Input::History)
                                    .unwrap()
                                    .register_offset(),
                                Some((Register(source), offset as i32 as u32))
                            );
                            assert_eq!(
                                verified.candidate().view.output.address.register_offset(),
                                Some((Register(destination), offset as i32 as u32))
                            );
                            assert!(
                                verified
                                    .conditions()
                                    .iter()
                                    .any(|p| matches!(p, Predicate::BackwardDistance { .. }))
                            );
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
            verified
                .conditions()
                .iter()
                .any(|p| matches!(p, Predicate::Disjoint(..))),
            computation == Computation::PngUp
        );
        assert_eq!(
            verified
                .conditions()
                .iter()
                .any(|p| matches!(p, Predicate::BackwardDistance { .. })),
            computation == Computation::PngSub
        );
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
    candidate.exits = vec![candidate.end];
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
    let candidate = copy(&image);
    assert!(matches!(
        verify::verify(&image, candidate.clone(), &mut Budget { instructions: 2 }),
        Err(Rejection::Timeout)
    ));
    let mut bytes = vec![0x0f, 0xaf, 0xc0]; // IMUL lies outside the checked integer subset.
    bytes.extend(byte_loop(6, 7, 1, 0, 0, false, true));
    *bytes.last_mut().unwrap() = (-(bytes.len() as i8)) as u8;
    let unsupported = snapshot(bytes);
    assert!(matches!(
        verify::verify(&unsupported, copy(&unsupported), &mut budget()),
        Err(Rejection::Unsupported {
            reason: "instruction form",
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
    candidate.exits = vec![candidate.end];
    candidate.image = image.identity();
    assert!(matches!(
        verify::verify(&image, candidate, &mut budget()),
        Err(Rejection::Unsupported {
            reason: "nonrelocatable branch width",
            ..
        })
    ));
}

// spec: EXEC-NATIVE-2, EXEC-NATIVE-3
#[test]
fn exit_models_match_guest_flags_partial_registers_and_updated_history() {
    use adv_cpu::{
        CpuState, EXEC, Memory, READ, WRITE,
        interpreter::{Interpreter, Step},
    };
    for computation in [Computation::Lz77, Computation::PngSub, Computation::PngUp] {
        for wide in [false, true] {
            for count in [1, 3] {
                for flags in [0x202, 0x203, 0x2d7] {
                    let image = snapshot(byte_loop(
                        6,
                        7,
                        1,
                        0,
                        0,
                        computation != Computation::Lz77,
                        wide,
                    ));
                    let candidate = analyze::discover(&image, &mut budget())
                        .into_iter()
                        .find(|c| c.contract.computation == computation)
                        .unwrap();
                    let binding = verify::verify(&image, candidate, &mut budget()).unwrap();
                    let mut memory = Memory::new(0x1000, 0x4000).unwrap();
                    memory.protect(0x1000, 0x4000, READ | WRITE).unwrap();
                    memory.write(0x1000, &image.dependencies[0].bytes).unwrap();
                    memory.protect(0x1000, 4096, READ | EXEC).unwrap();
                    let source = if computation == Computation::PngUp {
                        0x2000
                    } else {
                        0x20ff
                    };
                    memory.write(source, &[10, 20, 30]).unwrap();
                    memory.write(0x2100, &[250, 241, 230]).unwrap();
                    let mut state = CpuState {
                        regs: [
                            0x1234abff, count, 0x87654321, 0x11111111, 0x4000, 0x3000, source,
                            0x2100,
                        ],
                        eip: 0x1000,
                        eflags: flags,
                        ..Default::default()
                    };
                    let mut interpreter = Interpreter::new();
                    for iteration in 0..count {
                        let read = |version, address, width| {
                            assert_eq!(version, MemoryVersion(0));
                            assert_eq!(width, Width::Byte);
                            u64::from(memory.read(address, 1).unwrap()[0])
                        };
                        let eval = |e: &Expression| e.evaluate(&state.regs, state.eflags, &read);
                        let exits: Vec<_> = binding
                            .transition()
                            .exits
                            .iter()
                            .filter(|exit| eval(&exit.condition) != 0)
                            .collect();
                        assert_eq!(exits.len(), 1);
                        let exit = exits[0];
                        assert_eq!(
                            exit.kind,
                            if iteration + 1 == count {
                                verify::ExitKind::Complete
                            } else {
                                verify::ExitKind::Continue
                            }
                        );
                        assert_eq!(exit.state.memory, MemoryVersion(1));
                        let registers = exit.state.registers.each_ref().map(|e| eval(e) as u32);
                        let expected_flags = eval(&exit.state.flags) as u32;
                        let target = exit.target;
                        let store = &binding.transition().store;
                        assert_eq!(
                            (store.before, store.after),
                            (MemoryVersion(0), MemoryVersion(1))
                        );
                        let address = eval(&store.address) as u32;
                        let value = eval(&store.value) as u8;
                        for _ in 0..binding.transition().instructions {
                            assert_eq!(interpreter.step(&mut state, &mut memory), Step::Executed);
                        }
                        assert_eq!(state.regs, registers);
                        assert_eq!(state.eflags, expected_flags);
                        assert_eq!(state.eip, target);
                        assert_eq!(memory.read(address, 1).unwrap(), [value]);
                    }
                }
            }
        }
    }
}

// spec: EXEC-NATIVE-2, EXEC-NATIVE-3
#[test]
fn typed_values_keep_wide_sums_and_memory_versions_distinct() {
    let byte = Expression::constant(Width::Byte, 255);
    let one = Expression::constant(Width::Byte, 1);
    assert_eq!(
        Expression::binary(Binary::Add, byte.clone(), one.clone())
            .unwrap()
            .as_constant(),
        Some(0)
    );
    let wide = Expression::extend(byte, Width::Word, false).unwrap();
    let one = Expression::extend(one, Width::Word, false).unwrap();
    assert_eq!(
        Expression::binary(Binary::Add, wide, one)
            .unwrap()
            .as_constant(),
        Some(256)
    );
    assert!(
        Expression::binary(
            Binary::Add,
            Expression::constant(Width::Byte, 1),
            Expression::constant(Width::Word, 1)
        )
        .is_none()
    );
    let address = Expression::register(Register(6));
    let before = Expression::load(MemoryVersion(0), address.clone(), Width::Byte).unwrap();
    let after = Expression::load(MemoryVersion(1), address, Width::Byte).unwrap();
    assert_ne!(before, after);
}

// spec: EXEC-NATIVE-1, EXEC-NATIVE-2
#[test]
fn candidate_views_and_declared_exits_cannot_change_checked_relations() {
    let image = snapshot(byte_loop(6, 7, 1, 0, 0, false, true));
    let original = copy(&image);
    let mut candidate = original.clone();
    candidate
        .view
        .inputs
        .push((Input::Length, Expression::constant(Width::Dword, 123)));
    assert!(matches!(
        verify::verify(&image, candidate, &mut budget()),
        Err(Rejection::Counterexample {
            reason: "input mapping",
            ..
        })
    ));
    let mut candidate = original;
    candidate.exits.push(0x2000);
    assert!(matches!(
        verify::verify(&image, candidate, &mut budget()),
        Err(Rejection::Counterexample {
            reason: "cutpoints or exits",
            ..
        })
    ));
}

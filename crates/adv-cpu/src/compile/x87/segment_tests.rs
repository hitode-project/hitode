use super::*;

fn decoded(code: &[u8]) -> Vec<Instruction> {
    Decoder::with_ip(32, code, u64::from(CODE), DecoderOptions::NONE)
        .into_iter()
        .collect()
}

#[test]
fn resident_stack_wrap_and_dead_raw_slots_match_every_budget_prefix() {
    let code = bytes(|a| {
        a.fld(dword_ptr(esi)).unwrap();
        a.fadd(dword_ptr(esi + 4)).unwrap();
        a.fld1().unwrap();
        a.fxch(st0, st1).unwrap();
        a.fmulp(st1, st0).unwrap();
        a.fchs().unwrap();
        a.fst(st2).unwrap();
        a.fstp(dword_ptr(esi + 16)).unwrap();
        a.fldz().unwrap();
        a.fstp(st0).unwrap();
        a.fxch(st0, st1).unwrap();
        a.fsubr_2(st0, st1).unwrap();
        a.fcompp().unwrap();
    });
    let instructions = decoded(&code);
    for top in 0..8 {
        let mut initial = state();
        let rotation = (top + 8 - initial.x87.top()) & 7;
        initial.x87.regs.rotate_right(rotation);
        initial.x87.tag = initial.x87.tag.rotate_left((rotation * 2) as u32);
        initial.x87.status = (initial.x87.status & !0x3800) | (top as u16) << 11;
        initial.refresh_mmx();
        let mut actual_memory = memory(&code);
        actual_memory.write(DATA, &1.3f32.to_le_bytes()).unwrap();
        actual_memory
            .write(DATA + 4, &0.7f32.to_le_bytes())
            .unwrap();
        let compiled = compile(&Region::new(CODE, code.len()), &actual_memory, false).unwrap();
        for budget in 0..=instructions.len() {
            let mut actual_memory = memory(&code);
            let mut expected_memory = memory(&code);
            for m in [&mut actual_memory, &mut expected_memory] {
                m.write(DATA, &1.3f32.to_le_bytes()).unwrap();
                m.write(DATA + 4, &0.7f32.to_le_bytes()).unwrap();
            }
            let mut actual = initial.clone();
            actual.fuel = budget as u64;
            let mut expected = actual.clone();
            let mut interpreter = Interpreter::new();
            for _ in 0..budget {
                assert!(matches!(
                    interpreter.step(&mut expected, &mut expected_memory),
                    Step::Executed
                ));
            }
            expected.fuel = 0;
            let before = testing::calls();
            assert_eq!(
                testing::run(&compiled, &mut actual, &mut actual_memory),
                if budget == instructions.len() {
                    Exit::Dispatch
                } else {
                    Exit::Budget
                } as u32
            );
            assert_eq!(testing::calls() - before, 0, "hot budget prefix {budget}");
            assert_eq!(actual, expected, "TOP {top}, budget prefix {budget}");
            assert_eq!(
                actual_memory.read_data(DATA, 64).unwrap(),
                expected_memory.read_data(DATA, 64).unwrap()
            );
            // Popped/free slots still expose the last written raw80 and MMX
            // alias, even though no later instruction reads them as a value.
            for slot in 0..8 {
                assert_eq!(actual.mmx[slot], actual.x87.regs[slot].significand());
            }
        }
        // Restart at each internal canonical entry with exactly one unit of
        // fuel. This exercises the alternate helper lane and then the next
        // segment's success lane, while checking every committed prefix again.
        let mut actual = initial.clone();
        let mut expected = initial;
        let mut expected_memory = memory(&code);
        for m in [&mut actual_memory, &mut expected_memory] {
            m.write(DATA, &1.3f32.to_le_bytes()).unwrap();
            m.write(DATA + 4, &0.7f32.to_le_bytes()).unwrap();
        }
        let mut interpreter = Interpreter::new();
        for i in &instructions {
            actual.fuel = 1;
            assert_eq!(actual.eip, i.ip32());
            let exit = testing::run(&compiled, &mut actual, &mut actual_memory);
            assert!(exit == Exit::Budget as u32 || exit == Exit::Dispatch as u32);
            assert!(matches!(
                interpreter.step(&mut expected, &mut expected_memory),
                Step::Executed
            ));
            expected.fuel = 0;
            assert_eq!(actual, expected, "TOP {top}, restart at {:#x}", i.ip32());
        }
    }
}

#[test]
fn every_resident_memory_fault_preserves_only_the_completed_prefix() {
    for failure in 0..6 {
        let code = bytes(|a| {
            let operand = |index| qword_ptr(esi + if index == failure { 4096 } else { index * 8 });
            a.fld(operand(0)).unwrap();
            a.fmul(operand(1)).unwrap();
            a.fsub(operand(2)).unwrap();
            a.fdiv(operand(3)).unwrap();
            a.fst(operand(4)).unwrap();
            a.fstp(operand(5)).unwrap();
        });
        compare(
            &code,
            state(),
            |m| {
                for (index, value) in [1.3f64, 0.7, 0.125, 0.25].into_iter().enumerate() {
                    m.write(DATA + index as u32 * 8, &value.to_le_bytes())
                        .unwrap();
                }
                m.protect(DATA + 4096, 4096, 0).unwrap();
            },
            None,
        );
    }
}

#[test]
fn resident_store_aliases_and_range_rejection_keep_original_access_order() {
    let code = bytes(|a| {
        a.fld(dword_ptr(esi)).unwrap();
        a.fadd(dword_ptr(esi + 4)).unwrap();
        a.fst(dword_ptr(esi)).unwrap();
        a.fmul(dword_ptr(esi)).unwrap();
        a.fstp(dword_ptr(esi + 8)).unwrap();
    });
    compare(
        &code,
        state(),
        |m| {
            m.write(DATA, &1.3f32.to_le_bytes()).unwrap();
            m.write(DATA + 4, &0.7f32.to_le_bytes()).unwrap();
        },
        Some(0),
    );
    let code = bytes(|a| {
        a.fld(qword_ptr(esi)).unwrap();
        a.fmul(qword_ptr(esi)).unwrap();
        a.fadd(qword_ptr(esi + 8)).unwrap();
        a.fstp(qword_ptr(esi + 16)).unwrap();
    });
    compare(
        &code,
        state(),
        |m| {
            m.write(DATA, &2f64.powi(300).to_le_bytes()).unwrap();
            m.write(DATA + 8, &1.0f64.to_le_bytes()).unwrap();
        },
        Some(2),
    );
    let code = bytes(|a| {
        a.fstp(st0).unwrap();
        a.fstp(st0).unwrap();
        a.fchs().unwrap();
    });
    compare(&code, state(), |_| {}, Some(1));
}

#[test]
fn segment_planning_respects_real_entries_and_bounds_native_body_size() {
    let code = bytes(|a| {
        a.fld(dword_ptr(esi)).unwrap();
        for _ in 0..5 {
            a.fsub(dword_ptr(esi + 4)).unwrap();
        }
        a.fstp(dword_ptr(esi + 8)).unwrap();
    });
    let instructions: BTreeMap<_, _> = decoded(&code).into_iter().map(|i| (i.ip32(), i)).collect();
    let split = instructions.values().nth(3).unwrap().ip32();
    let plan = segments(&instructions, &[CODE, split]);
    assert_eq!(plan.starts.len(), 2);
    assert_eq!(plan.starts[&CODE].len(), 3);
    assert_eq!(plan.starts[&split].len(), 4);
    assert!(!plan.interiors.contains(&split));
    let overlap = segments(&instructions, &[CODE + 1]);
    assert!(!overlap.starts.contains_key(&CODE));
    let wrapping = Decoder::with_ip(
        32,
        &[0xd9, 0xe0],
        u64::from(u32::MAX - 1),
        DecoderOptions::NONE,
    )
    .decode();
    assert_eq!(wrapping.next_ip32(), 0);
    assert!(
        segments(&BTreeMap::from([(wrapping.ip32(), wrapping)]), &[])
            .starts
            .is_empty()
    );
    let mut region = Region::new(CODE, code.len());
    region.entries.push(split);
    let mut m = memory(&code);
    m.write(DATA, &1.3f32.to_le_bytes()).unwrap();
    m.write(DATA + 4, &0.7f32.to_le_bytes()).unwrap();
    let compiled = compile(&region, &m, false).unwrap();
    let mut actual = state();
    actual.eip = split;
    let mut expected = actual.clone();
    let mut oracle = memory(&code);
    oracle.write(DATA, &1.3f32.to_le_bytes()).unwrap();
    oracle.write(DATA + 4, &0.7f32.to_le_bytes()).unwrap();
    let mut interpreter = Interpreter::new();
    for _ in 0..4 {
        assert!(matches!(
            interpreter.step(&mut expected, &mut oracle),
            Step::Executed
        ));
    }
    assert_eq!(
        testing::run(&compiled, &mut actual, &mut m),
        Exit::Dispatch as u32
    );
    expected.fuel = actual.fuel;
    assert_eq!(actual, expected);
    let m = memory(&code);
    let resident = compile(&Region::new(CODE, code.len()), &m, false).unwrap();
    testing::SINGLE_ONLY.set(true);
    let single = compile(&Region::new(CODE, code.len()), &m, false);
    testing::SINGLE_ONLY.set(false);
    let single = single.unwrap();
    assert!(
        // Bound the recovery lane's code size, not an execution-speed claim.
        // Throughput is measured separately by the native x87 A/B example.
        // PC=24 re-rounding keeps per-op temporaries live until the lane's
        // status is consumed, which costs about a fifth over single-op code.
        resident.code_size <= single.code_size * 5 / 4,
        "resident {}, single {}",
        resident.code_size,
        single.code_size
    );
}

/// Compiler-scheduled integer updates between x87 operations. `moved` sends one
/// integer access (0 load, 1 store, 2 read-modify-write) to another page:
/// +0x1000 is inaccessible, +0x2000 is writable code, +0x3000 is shared audio.
fn interleaved(moved: Option<(usize, i32)>) -> Vec<u8> {
    bytes(|a| {
        let away = |index| match moved {
            Some((at, offset)) if at == index => offset,
            _ => 0,
        };
        a.fld(dword_ptr(esi)).unwrap();
        a.mov(eax, dword_ptr(esi + 24 + away(0))).unwrap();
        a.add(ecx, 4).unwrap();
        a.fmul(dword_ptr(esi + 4)).unwrap();
        a.fld(dword_ptr(esi + 4)).unwrap();
        a.mov(dword_ptr(esi + 28 + away(1)), ecx).unwrap();
        a.lea(edx, ptr(eax + ecx * 4)).unwrap();
        a.fmul(dword_ptr(esi)).unwrap();
        a.add(dword_ptr(esi + 32 + away(2)), edx).unwrap();
        a.dec(ecx).unwrap();
        a.faddp(st1, st0).unwrap();
        a.fstp(dword_ptr(esi + 8)).unwrap();
    })
}

fn interleaved_data(m: &mut Memory) {
    m.write(DATA, &1.3f32.to_le_bytes()).unwrap();
    m.write(DATA + 4, &0.7f32.to_le_bytes()).unwrap();
    m.write(DATA + 24, &0x1234u32.to_le_bytes()).unwrap();
    m.protect(DATA + 0x1000, 4096, 0).unwrap();
    m.protect(DATA + 0x2000, 4096, READ | WRITE | EXEC).unwrap();
}

/// memory() with the last page as shared audio where the host supports it.
fn lane_memory(code: &[u8]) -> Memory {
    if !cfg!(windows) {
        return memory(code);
    }
    let mut m = Memory::reserve(CODE, 0x6000).unwrap();
    m.map(CODE, 0x5000, READ | WRITE).unwrap();
    m.write(CODE, code).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.write(DATA, &1.25f64.to_le_bytes()).unwrap();
    let shared = m.map_shared_audio(DATA + 0x3000, 4096).unwrap();
    shared.store_bytes(24, &0x5678u32.to_le_bytes());
    m
}

const MOVED: [Option<(usize, i32)>; 9] = [
    None,
    Some((0, 0x1000)),
    Some((1, 0x1000)),
    Some((2, 0x1000)),
    Some((1, 0x2000)),
    Some((2, 0x2000)),
    Some((0, 0x3000)),
    Some((1, 0x3000)),
    Some((2, 0x3000)),
];

#[test]
fn integer_lane_members_fault_after_the_completed_x87_prefix() {
    for moved in &MOVED[..4] {
        let code = interleaved(*moved);
        let instructions: BTreeMap<_, _> =
            decoded(&code).into_iter().map(|i| (i.ip32(), i)).collect();
        let plan = segments(&instructions, &[CODE]);
        assert_eq!(plan.starts.len(), 1);
        assert_eq!(plan.starts[&CODE].len(), instructions.len());
        assert_eq!(plan.integers.len(), 6);
        // Integer rejections re-execute canonically; only a fault, never an
        // x87 operation, leaves the native lane.
        compare(&code, state(), interleaved_data, Some(0));
    }
}

#[test]
fn integer_lanes_match_single_instruction_code_for_every_budget_and_entry() {
    for moved in MOVED {
        if cfg!(windows) || moved.is_none_or(|(_, offset)| offset != 0x3000) {
            lane_matches_single(&interleaved(moved));
        }
    }
}

fn lane_matches_single(code: &[u8]) {
    let instructions = decoded(code);
    let m = lane_memory(code);
    let lanes = compile(&Region::new(CODE, code.len()), &m, false).unwrap();
    testing::SINGLE_ONLY.set(true);
    let single = compile(&Region::new(CODE, code.len()), &m, false);
    testing::SINGLE_ONLY.set(false);
    let single = single.unwrap();
    for i in &instructions {
        for budget in 0..=instructions.len() as u64 {
            let mut runs = [(&lanes, state()), (&single, state())].map(|(compiled, mut s)| {
                s.eip = i.ip32();
                s.fuel = budget;
                let mut m = lane_memory(code);
                interleaved_data(&mut m);
                let exit = testing::run(compiled, &mut s, &mut m);
                let data = m.read_data(DATA, 64).unwrap().to_vec();
                let code = m.read_data(DATA + 0x2000, 64).unwrap().to_vec();
                let shared = m.read_data(DATA + 0x3000, 64).unwrap().to_vec();
                (exit, s, data, code, shared, m.epoch)
            });
            let (single_run, lane_run) = (runs[1].clone(), &mut runs[0]);
            assert_eq!(
                *lane_run,
                single_run,
                "entry {:#x}, budget {budget}",
                i.ip32()
            );
        }
    }
}

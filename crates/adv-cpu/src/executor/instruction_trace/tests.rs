use super::*;
use crate::{Executor, WRITE};
use serde_json::Value;

const CODE: u32 = 0x10000;
const DATA: u32 = 0x18000;
const STACK: u32 = 0x1ff00;
const STOP: u32 = 0x17000;

fn directory() -> PathBuf {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    let path = target
        .join("scratch/instruction-trace-tests")
        .join(format!("{}-{id}-{time}", std::process::id()));
    std::fs::create_dir_all(&path).unwrap();
    path.canonicalize().unwrap()
}

fn cpu(bytes: &[u8]) -> Executor {
    let mut cpu = Executor::new(CODE, 0x10000).unwrap();
    cpu.instruction_trace = None;
    cpu.protect(CODE, 0x10000, READ | WRITE).unwrap();
    cpu.write(CODE, bytes).unwrap();
    cpu.protect(CODE, PAGE_SIZE, READ | EXEC).unwrap();
    cpu.write(STACK, &STOP.to_le_bytes()).unwrap();
    cpu.state.eip = CODE;
    cpu.state.regs[4] = STACK;
    cpu
}

fn attach(cpu: &mut Executor, ranges: Vec<Range<u64>>, max: usize, words: usize) -> PathBuf {
    attach_operands(cpu, ranges, max, words, false)
}

fn attach_operands(
    cpu: &mut Executor,
    ranges: Vec<Range<u64>>,
    max: usize,
    words: usize,
    operands: bool,
) -> PathBuf {
    let trace = Trace::new(Config {
        directory: directory(),
        ranges,
        max,
        stack_words: words,
        operands,
    });
    let path = trace.path.clone();
    cpu.instruction_trace = Some(Box::new(trace));
    path
}

fn events(path: &PathBuf) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn run(cpu: &mut Executor, budget: u64) -> RunOutcome {
    cpu.run_execution(STOP, budget, None, true).unwrap()
}

#[test]
fn sequential_entry_inside_an_instruction_does_not_skip_the_first_selected_pc() {
    // Authored mov eax,9; inc eax; inc ebx; ret. The range starts inside MOV.
    let bytes = [0xb8, 9, 0, 0, 0, 0x40, 0x43, 0xc3];
    let mut control = cpu(&bytes);
    let mut traced = cpu(&bytes);
    let path = attach(
        &mut traced,
        vec![u64::from(CODE + 2)..u64::from(CODE + 7)],
        20,
        2,
    );
    assert_eq!(run(&mut control, 100), RunOutcome::ReachedStop);
    assert_eq!(run(&mut traced, 100), RunOutcome::ReachedStop);
    assert_eq!(traced.state, control.state);
    let rows = events(&path);
    let instructions: Vec<_> = rows.iter().filter(|r| r["kind"] == "instruction").collect();
    assert_eq!(instructions.len(), 2);
    assert_eq!(instructions[0]["before"]["eip"], CODE + 5);
    assert_eq!(instructions[1]["before"]["eip"], CODE + 6);
    assert_eq!(instructions[0]["before"]["eax"], 9);
    assert_eq!(instructions[0]["after"]["eax"], 10);
    assert_eq!(instructions[0]["before"]["stack"][0], STOP);
    assert_eq!(rows.last().unwrap()["outcome"], "ReachedStop");
    assert!(rows.iter().all(|r| r.get("bytes").is_none()));
}

#[test]
fn a_branch_into_a_range_records_the_target_and_preserves_stop_and_fuel() {
    // Authored short jump over padding; inc eax; ret.
    let bytes = [0xeb, 4, 0x90, 0x90, 0x90, 0x90, 0x40, 0xc3];
    let mut control = cpu(&bytes);
    let mut traced = cpu(&bytes);
    let path = attach(
        &mut traced,
        vec![u64::from(CODE + 6)..u64::from(CODE + 8)],
        10,
        1,
    );
    assert_eq!(run(&mut control, 100), RunOutcome::ReachedStop);
    assert_eq!(run(&mut traced, 100), RunOutcome::ReachedStop);
    assert_eq!(traced.state, control.state);
    let rows = events(&path);
    assert_eq!(rows[0]["before"]["eip"], CODE + 6);
    assert_eq!(rows[1]["opcode"], "Retnd");
    assert_eq!(rows[1]["after"]["esp"], STACK + 4);
    assert_eq!(rows[1]["after"]["eip"], STOP);
}

#[test]
fn event_limit_stops_capture_without_stopping_the_guest() {
    // Authored inc eax; jmp back to inc.
    let mut control = cpu(&[0x40, 0xeb, 0xfd]);
    let mut traced = cpu(&[0x40, 0xeb, 0xfd]);
    let path = attach(
        &mut traced,
        vec![u64::from(CODE)..u64::from(CODE + 3)],
        3,
        0,
    );
    assert_eq!(run(&mut control, 20), RunOutcome::Budget);
    assert_eq!(run(&mut traced, 20), RunOutcome::Budget);
    assert_eq!(traced.state, control.state);
    let rows = events(&path);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[2]["capture_stop"], "limit");
    assert_eq!(rows[0]["before"]["stack"], serde_json::json!([]));
}

#[test]
fn configuration_rejects_empty_or_unbounded_capture_and_non_target_output() {
    let dir = directory();
    for ranges in ["", "0x1000..0x1000", "0..4294967297"] {
        assert!(Config::parse(dir.clone(), ranges, "10", "4", "0").is_err());
    }
    for (max, words) in [("0", "4"), ("100001", "4"), ("10", "9")] {
        assert!(Config::parse(dir.clone(), "0x1000..0x2000", max, words, "0").is_err());
    }
    assert!(Config::parse(dir.clone(), "0..1", "1", "0", "true").is_err());
    let config = Config::parse(dir, "0X1000..0x2000,8192..12288", "1", "8", "1").unwrap();
    assert_eq!(config.ranges, [0x1000..0x2000, 0x2000..0x3000]);
    assert!(config.operands);
    assert!(
        Config::parse(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            "0..1",
            "1",
            "0",
            "0"
        )
        .is_err()
    );
}

#[test]
fn unreadable_stack_words_across_a_page_are_null_without_faulting_the_guest() {
    let mut traced = cpu(&[0x90]);
    let esp = DATA + PAGE_SIZE as u32 - 2;
    traced.state.regs[4] = esp;
    traced
        .protect(DATA + PAGE_SIZE as u32, PAGE_SIZE, 0)
        .unwrap();
    let path = attach(
        &mut traced,
        vec![u64::from(CODE)..u64::from(CODE + 1)],
        10,
        2,
    );
    assert_eq!(
        traced.run_execution(CODE + 1, 100, None, true).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(traced.state.regs[4], esp);
    assert_eq!(
        events(&path)[0]["before"]["stack"],
        serde_json::json!([null, null])
    );
}

#[test]
fn io_failure_disables_diagnostics_and_preserves_execution() {
    let mut control = cpu(&[0x40, 0xc3]);
    let mut traced = cpu(&[0x40, 0xc3]);
    let file = directory().join("a-file");
    std::fs::write(&file, b"not a directory").unwrap();
    traced.instruction_trace = Some(Box::new(Trace::new(Config {
        directory: file,
        ranges: vec![u64::from(CODE)..u64::from(CODE + 2)],
        max: 10,
        stack_words: 1,
        operands: false,
    })));
    assert_eq!(run(&mut control, 100), RunOutcome::ReachedStop);
    assert_eq!(run(&mut traced, 100), RunOutcome::ReachedStop);
    assert_eq!(traced.state, control.state);
    let trace = traced.instruction_trace.as_ref().unwrap();
    assert!(trace.disabled);
    assert!(trace.failure.is_some());
    assert_eq!(trace.written, 0);
}

#[test]
fn a_fetch_fault_outside_the_range_remains_visible_as_a_boundary() {
    let mut traced = cpu(&[0xc3]);
    traced.write(STACK, &0u32.to_le_bytes()).unwrap();
    let path = attach(
        &mut traced,
        vec![u64::from(CODE)..u64::from(CODE + 1)],
        10,
        1,
    );
    assert_eq!(
        run(&mut traced, 100),
        RunOutcome::FetchFault { pc: 0, address: 0 }
    );
    let rows = events(&path);
    assert_eq!(rows[0]["after"]["eip"], 0);
    assert_eq!(rows.last().unwrap()["kind"], "boundary");
    assert_eq!(
        rows.last().unwrap()["outcome"],
        "FetchFault { pc: 0, address: 0 }"
    );
    assert_eq!(rows.last().unwrap()["state"]["esp"], STACK + 4);
}

struct Trap;
impl crate::ExecutionTrap for Trap {
    fn call(&mut self, state: &mut CpuState, _: &mut Memory, vector: u32) -> Result<bool, String> {
        assert_eq!(vector, 0x2e);
        state.regs[0] = 0xaabbccdd;
        Ok(false)
    }
}

#[test]
fn trap_retirement_and_later_resume_are_distinct_observations() {
    // Authored mov eax,0x11223344; int 2e; ret. The trap owner changes EAX.
    let bytes = [0xb8, 0x44, 0x33, 0x22, 0x11, 0xcd, 0x2e, 0xc3];
    let mut control = cpu(&bytes);
    let mut traced = cpu(&bytes);
    let path = attach(
        &mut traced,
        vec![u64::from(CODE)..u64::from(CODE + 8)],
        20,
        1,
    );
    assert_eq!(
        control.run_with_traps(STOP, 100, &mut Trap, true).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(
        traced.run_with_traps(STOP, 100, &mut Trap, true).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(traced.state, control.state);
    let rows = events(&path);
    assert_eq!(rows[1]["opcode"], "Int_imm8");
    assert_eq!(rows[1]["after"]["eax"], 0x11223344u32);
    assert_eq!(rows[1]["after"]["eip"], CODE + 7);
    assert_eq!(rows[2]["kind"], "resume");
    assert_eq!(rows[2]["state"]["eax"], 0xaabbccddu32);
    assert_eq!(rows[3]["before"], rows[2]["state"]);
}

#[test]
fn jit_backend_selection_does_not_activate_instruction_capture() {
    let mut traced = cpu(&[0x40, 0xc3]);
    let path = attach(
        &mut traced,
        vec![u64::from(CODE)..u64::from(CODE + 2)],
        10,
        1,
    );
    assert_eq!(traced.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert!(!path.exists());
    assert_eq!(traced.instruction_trace.as_ref().unwrap().written, 0);
}

#[test]
fn explicit_cmp_register_and_sign_extended_immediates_identify_the_condition() {
    // Authored cmp ecx,78 and cmp eax,-1, both using sign-extended imm8.
    for (bytes, register, index, value, signed) in [
        ([0x83, 0xf9, 78, 0xc3], "ECX", 1, 78u32, 78i64),
        ([0x83, 0xf8, 0xff, 0xc3], "EAX", 0, u32::MAX, -1),
    ] {
        let mut traced = cpu(&bytes);
        traced.state.regs[index] = value;
        let path = attach_operands(
            &mut traced,
            vec![u64::from(CODE)..u64::from(CODE + 3)],
            10,
            0,
            true,
        );
        assert_eq!(run(&mut traced, 100), RunOutcome::ReachedStop);
        let rows = events(&path);
        let operands = &rows[0]["operands_before"];
        assert_eq!(operands[0]["register"], register);
        assert_eq!(operands[0]["width"], 4);
        assert_eq!(operands[0]["value"], value);
        assert_eq!(operands[1]["kind"], "Immediate8to32");
        assert_eq!(operands[1]["width"], 4);
        assert_eq!(operands[1]["value"], value);
        assert_eq!(operands[1]["signed_value"], signed);
        assert_ne!(rows[0]["after"]["eflags"].as_u64().unwrap() & 64, 0);
    }
}

#[test]
fn explicit_cmp_memory_addresses_include_index_scale_displacement_and_fs_base() {
    // Authored cmp dword [eax+ecx*4+4],78, optionally with FS override.
    for fs in [false, true] {
        let mut bytes = vec![0x83, 0x7c, 0x88, 4, 78, 0xc3];
        if fs {
            bytes.insert(0, 0x64);
        }
        let mut traced = cpu(&bytes);
        traced.state.regs[0] = if fs { 0 } else { DATA };
        traced.state.regs[1] = 3;
        traced.state.fs_base = if fs { DATA } else { 0 };
        traced.write(DATA + 16, &78u32.to_le_bytes()).unwrap();
        let path = attach_operands(
            &mut traced,
            vec![u64::from(CODE)..u64::from(CODE + bytes.len() as u32 - 1)],
            10,
            0,
            true,
        );
        assert_eq!(run(&mut traced, 100), RunOutcome::ReachedStop);
        let rows = events(&path);
        let memory = &rows[0]["operands_before"][0];
        assert_eq!(memory["kind"], "Memory");
        assert_eq!(memory["segment"], if fs { "FS" } else { "DS" });
        assert_eq!(memory["ea"], DATA + 16);
        assert_eq!(memory["width"], 4);
        assert_eq!(memory["scalar"], 78);
        assert_eq!(rows[0]["operands_after"][0], *memory);
    }
}

#[test]
fn memory_after_uses_the_actual_before_address_and_observes_written_scalars() {
    // Authored mov eax,[eax] changes its own address base.
    let mut traced = cpu(&[0x8b, 0x00, 0xc3]);
    traced.state.regs[0] = DATA;
    traced.write(DATA, &(DATA + 128).to_le_bytes()).unwrap();
    traced.write(DATA + 128, &42u32.to_le_bytes()).unwrap();
    let path = attach_operands(
        &mut traced,
        vec![u64::from(CODE)..u64::from(CODE + 2)],
        10,
        0,
        true,
    );
    assert_eq!(run(&mut traced, 100), RunOutcome::ReachedStop);
    let rows = events(&path);
    assert_eq!(rows[0]["operands_after"][0]["value"], DATA + 128);
    assert_eq!(rows[0]["operands_before"][1]["ea"], DATA);
    assert_eq!(rows[0]["operands_after"][1]["ea"], DATA);
    assert_eq!(rows[0]["operands_after"][1]["scalar"], DATA + 128);

    // Authored mov dword [eax],7 records the write at that same address.
    let mut traced = cpu(&[0xc7, 0x00, 7, 0, 0, 0, 0xc3]);
    traced.state.regs[0] = DATA;
    traced.write(DATA, &10u32.to_le_bytes()).unwrap();
    let path = attach_operands(
        &mut traced,
        vec![u64::from(CODE)..u64::from(CODE + 6)],
        10,
        0,
        true,
    );
    assert_eq!(run(&mut traced, 100), RunOutcome::ReachedStop);
    let rows = events(&path);
    assert_eq!(rows[0]["operands_before"][0]["scalar"], 10);
    assert_eq!(rows[0]["operands_after"][0]["scalar"], 7);
}

#[test]
fn explicit_pop_memory_uses_the_post_pop_destination_without_observing_implicit_stack_reads() {
    // Authored pop dword [esp]; ret. The destination is ESP+4, not ESP.
    let mut traced = cpu(&[0x8f, 0x04, 0x24, 0xc3]);
    traced.write(STACK + 4, &10u32.to_le_bytes()).unwrap();
    let path = attach_operands(
        &mut traced,
        vec![u64::from(CODE)..u64::from(CODE + 3)],
        10,
        0,
        true,
    );
    assert_eq!(run(&mut traced, 100), RunOutcome::ReachedStop);
    let rows = events(&path);
    assert_eq!(rows[0]["operands_before"].as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["operands_before"][0]["ea"], STACK + 4);
    assert_eq!(rows[0]["operands_before"][0]["scalar"], 10);
    assert_eq!(rows[0]["operands_after"][0]["ea"], STACK + 4);
    assert_eq!(rows[0]["operands_after"][0]["scalar"], STOP);
}

#[test]
fn unreadable_overwide_and_unknown_operands_are_null_without_mutating_execution() {
    let mut traced = cpu(&[0x83, 0x38, 1]); // Authored cmp dword [eax],1.
    traced
        .protect(DATA + PAGE_SIZE as u32, PAGE_SIZE, 0)
        .unwrap();
    for address in [DATA + PAGE_SIZE as u32 - 2, u32::MAX - 1] {
        traced.state.regs[0] = address;
        let state = traced.state.clone();
        let i = instruction(&traced.memory, CODE).unwrap();
        let entries = operands(&i, &traced.state);
        let row: Value =
            serde_json::from_str(&operand_snapshot(&entries, &traced.state, &traced.memory))
                .unwrap();
        assert_eq!(row[0]["ea"], address);
        assert!(row[0]["scalar"].is_null());
        assert_eq!(traced.state, state);
    }
    let mut traced = cpu(&[0x0f, 0x10, 0x00]); // Authored movups xmm0,[eax].
    traced.state.regs[0] = DATA;
    let i = instruction(&traced.memory, CODE).unwrap();
    let row: Value = serde_json::from_str(&operand_snapshot(
        &operands(&i, &traced.state),
        &traced.state,
        &traced.memory,
    ))
    .unwrap();
    assert!(row[0]["value"].is_null());
    assert_eq!(row[1]["width"], 16);
    assert!(row[1]["scalar"].is_null());
}

#[test]
fn disabled_operands_preserve_the_existing_schema_and_execution() {
    let mut plain = cpu(&[0x83, 0xf9, 78, 0xc3]);
    let mut off = cpu(&[0x83, 0xf9, 78, 0xc3]);
    let mut on = cpu(&[0x83, 0xf9, 78, 0xc3]);
    for cpu in [&mut plain, &mut off, &mut on] {
        cpu.state.regs[1] = 78;
    }
    let ranges = vec![u64::from(CODE)..u64::from(CODE + 4)];
    let off_path = attach_operands(&mut off, ranges.clone(), 10, 0, false);
    let on_path = attach_operands(&mut on, ranges, 10, 0, true);
    for cpu in [&mut plain, &mut off, &mut on] {
        assert_eq!(run(cpu, 100), RunOutcome::ReachedStop);
    }
    assert_eq!(off.state, plain.state);
    assert_eq!(on.state, plain.state);
    let off_rows = events(&off_path);
    let mut on_rows = events(&on_path);
    assert!(
        off_rows
            .iter()
            .all(|row| row.get("operands_before").is_none() && row.get("operands_after").is_none())
    );
    for row in &mut on_rows {
        row.as_object_mut().unwrap().remove("operands_before");
        row.as_object_mut().unwrap().remove("operands_after");
    }
    assert_eq!(off_rows, on_rows);
}

#[test]
fn rep_operands_do_not_expand_implicit_memory_observations() {
    let mut traced = cpu(&[0xf3, 0xa4, 0xc3]); // Authored rep movsb; ret.
    traced.state.regs[1] = 2;
    traced.state.regs[6] = DATA;
    traced.state.regs[7] = DATA + 16;
    traced.write(DATA, &[1, 2]).unwrap();
    let path = attach_operands(
        &mut traced,
        vec![u64::from(CODE)..u64::from(CODE + 2)],
        10,
        0,
        true,
    );
    assert_eq!(run(&mut traced, 100), RunOutcome::ReachedStop);
    for row in events(&path)
        .iter()
        .filter(|row| row["kind"] == "instruction")
    {
        for operand in row["operands_before"].as_array().unwrap() {
            assert!(operand.get("ea").is_none());
            assert!(operand["value"].is_null());
        }
    }
}

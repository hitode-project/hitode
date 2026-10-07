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
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/sidt-agent/instruction-trace-tests")
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
    let trace = Trace::new(Config {
        directory: directory(),
        ranges,
        max,
        stack_words: words,
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
        assert!(Config::parse(dir.clone(), ranges, "10", "4").is_err());
    }
    for (max, words) in [("0", "4"), ("100001", "4"), ("10", "9")] {
        assert!(Config::parse(dir.clone(), "0x1000..0x2000", max, words).is_err());
    }
    let config = Config::parse(dir, "0X1000..0x2000,8192..12288", "1", "8").unwrap();
    assert_eq!(config.ranges, [0x1000..0x2000, 0x2000..0x3000]);
    assert!(Config::parse(PathBuf::from(env!("CARGO_MANIFEST_DIR")), "0..1", "1", "0").is_err());
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

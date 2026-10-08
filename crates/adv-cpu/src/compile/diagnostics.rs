//! Opt-in native address map for an external sampler. This records finalized
//! bodies, not execution counts or publication/retirement guarantees. It never
//! changes compilation policy, links, or generated instructions. Trampolines
//! are not included. Reused address ranges require timestamp-aware attribution.
//!
//! The same switch tags emitted IR with guest-instruction source locations and
//! writes their native ranges. Tags are metadata only; regions that build an
//! opt-in hardware fault recipe stay untagged because that recipe reads its own
//! source locations.
use crate::Region;
use cranelift_codegen::ir::SourceLoc;
use iced_x86::{FlowControl, Instruction, Mnemonic as M, OpKind};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

// 1 and 2 belong to compile::fault. 3 and 5..=7 are Overhead.
const X87_RECOVERY: u32 = 4;
const GUEST: u32 = 16;

/// Region code outside any guest instruction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Overhead {
    /// Context/state loads and the entry switch.
    Entry = 3,
    /// Per-block fuel guard and MMX block setup.
    Budget = 5,
    /// Direct block transfers and the indirect dispatch switch.
    Transfer = 6,
    /// State writeback, exit footers and region chaining.
    Exit = 7,
    /// Materialization of lazy arithmetic flags.
    Flags = 8,
}

/// Code emitted for one guest instruction. Memory validation and its cold
/// completion are split out so their cost is visible beside the operation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Part {
    Body = 0,
    Check = 1,
    Slow = 2,
}

pub(super) fn guest_source(offset: u32, part: Part) -> SourceLoc {
    SourceLoc::new(GUEST + (offset << 2) + part as u32)
}

pub(super) fn overhead_source(kind: Overhead) -> SourceLoc {
    SourceLoc::new(kind as u32)
}

pub(super) fn x87_recovery_source() -> SourceLoc {
    SourceLoc::new(X87_RECOVERY)
}

pub(super) fn instruction_map_enabled() -> bool {
    #[cfg(test)]
    if testing::FORCE.get() {
        return true;
    }
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("ADV_JIT_MAP_DIR").is_some())
}

type Sink = OnceLock<Option<Mutex<Option<File>>>>;

fn write(sink: &'static Sink, name: &str, text: &str) {
    let Some(map) = sink.get_or_init(|| {
        let directory = std::env::var_os("ADV_JIT_MAP_DIR")?;
        let directory = std::path::PathBuf::from(directory);
        match std::fs::create_dir_all(&directory).and_then(|_| {
            File::create(directory.join(format!("{name}-{}.tsv", std::process::id())))
        }) {
            Ok(file) => Some(Mutex::new(Some(file))),
            Err(error) => {
                eprintln!("warning: JIT address map disabled: {error}");
                None
            }
        }
    }) else {
        return;
    };
    let mut sink = map.lock().unwrap_or_else(|poison| poison.into_inner());
    let Some(file) = sink.as_mut() else {
        return;
    };
    if let Err(error) = file.write_all(text.as_bytes()) {
        eprintln!("warning: JIT address map disabled after write failure: {error}");
        *sink = None;
    }
}

pub(super) fn record_body(
    region: &Region,
    optimization: &str,
    base: usize,
    size: usize,
    instructions: usize,
) -> u128 {
    static MAP: Sink = OnceLock::new();
    if !instruction_map_enabled() {
        return 0;
    }
    let ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // Decimal TSV: Unix ns, native base, body bytes, guest start, optimization,
    // guest address span bytes, decoded instructions. One row per compilation.
    write(
        &MAP,
        "jit",
        &format!(
            "{ns}\t{base}\t{size}\t{}\t{optimization}\t{}\t{instructions}\n",
            region.start, region.len,
        ),
    );
    ns
}

/// Writes `insn-{pid}.tsv` (outside the `jit-*` region-row pattern): the
/// region row's Unix ns and native base, start and end body offsets, guest PC (0 outside guest instructions),
/// part, class. Adjacent equal ranges are merged; untagged prologue/epilogue
/// ranges are omitted.
pub(super) fn record_instructions(
    region: &Region,
    ns: u128,
    base: usize,
    ranges: impl IntoIterator<Item = (u32, u32, SourceLoc)>,
    instructions: &BTreeMap<u32, Instruction>,
) {
    static MAP: Sink = OnceLock::new();
    let text = rows(region.start, &format!("{ns}\t{base}"), ranges, instructions);
    #[cfg(test)]
    testing::LAST.with_borrow_mut(|last| last.clone_from(&text));
    write(&MAP, "insn", &text);
}

#[cfg(test)]
pub(super) mod testing {
    use std::cell::{Cell, RefCell};
    thread_local! {
        pub(in crate::compile) static FORCE: Cell<bool> = const { Cell::new(false) };
        pub(in crate::compile) static LAST: RefCell<String> = const { RefCell::new(String::new()) };
    }
}

fn rows(
    origin: u32,
    key: &str,
    ranges: impl IntoIterator<Item = (u32, u32, SourceLoc)>,
    instructions: &BTreeMap<u32, Instruction>,
) -> String {
    let mut merged: Vec<(u32, u32, u32)> = Vec::new();
    for (start, end, loc) in ranges {
        if loc.is_default() {
            continue;
        }
        match merged.last_mut() {
            Some((_, last_end, bits)) if *last_end == start && *bits == loc.bits() => {
                *last_end = end
            }
            _ => merged.push((start, end, loc.bits())),
        }
    }
    let mut text = String::new();
    for (start, end, bits) in merged {
        let (pc, part, class) = match bits {
            3 => (0, "entry", "overhead"),
            5 => (0, "budget", "overhead"),
            6 => (0, "transfer", "overhead"),
            7 => (0, "exit", "overhead"),
            8 => (0, "flags", "overhead"),
            X87_RECOVERY => (0, "slow", "x87-recovery"),
            bits if bits >= GUEST => {
                let pc = origin.wrapping_add((bits - GUEST) >> 2);
                let part = match (bits - GUEST) & 3 {
                    0 => "body",
                    1 => "check",
                    _ => "slow",
                };
                (pc, part, instructions.get(&pc).map_or("unknown", class))
            }
            _ => (0, "fault", "fault"),
        };
        text.push_str(&format!("{key}\t{start}\t{end}\t{pc}\t{part}\t{class}\n"));
    }
    text
}

/// Coarse execution-unit class, with `.mem` when an operand addresses memory.
pub(super) fn class(i: &Instruction) -> &'static str {
    let memory = i.mnemonic() != M::Lea
        && (0..i.op_count()).any(|operand| i.op_kind(operand) == OpKind::Memory);
    let unit = if crate::strings::operation(i).is_some() || i.is_string_instruction() {
        "string"
    } else if i.flow_control() != FlowControl::Next {
        "control"
    } else {
        i.cpuid_features()
            .iter()
            .find_map(|feature| {
                let name = format!("{feature:?}");
                if name.starts_with("FPU") {
                    Some("x87")
                } else if name.starts_with("MMX") || name.starts_with("D3NOW") {
                    Some("mmx")
                } else if name.starts_with("SSE") || name.starts_with("AVX") {
                    Some("sse")
                } else {
                    None
                }
            })
            .unwrap_or("int")
    };
    match (unit, memory) {
        ("x87", true) => "x87.mem",
        ("mmx", true) => "mmx.mem",
        ("sse", true) => "sse.mem",
        ("int", true) => "int.mem",
        ("control", true) => "control.mem",
        (unit, _) => unit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_x86::{Decoder, DecoderOptions};

    fn decode(bytes: &[u8]) -> Instruction {
        Decoder::with_ip(32, bytes, 0x1000, DecoderOptions::NONE).decode()
    }

    #[test]
    fn classes_split_units_and_memory_operands() {
        assert_eq!(class(&decode(&[0x01, 0xd8])), "int"); // add eax,ebx
        assert_eq!(class(&decode(&[0x8b, 0x03])), "int.mem"); // mov eax,[ebx]
        assert_eq!(class(&decode(&[0x8d, 0x43, 0x04])), "int"); // lea eax,[ebx+4]
        assert_eq!(class(&decode(&[0xd8, 0x23])), "x87.mem"); // fsub dword [ebx]
        assert_eq!(class(&decode(&[0xde, 0xc9])), "x87"); // fmulp st1
        assert_eq!(class(&decode(&[0x0f, 0x58, 0xc1])), "sse"); // addps xmm0,xmm1
        assert_eq!(class(&decode(&[0x0f, 0xfe, 0x03])), "mmx.mem"); // paddd mm0,[ebx]
        assert_eq!(class(&decode(&[0xf3, 0xa5])), "string"); // rep movsd
        assert_eq!(class(&decode(&[0x74, 0x00])), "control"); // je
        assert_eq!(class(&decode(&[0xff, 0x13])), "control.mem"); // call [ebx]
    }

    /// Tags are metadata: the same region must lower to identical code.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn tagging_keeps_code_and_attributes_memory_and_x87_parts() {
        use crate::{EXEC, READ};
        use iced_x86::code_asm::*;
        let mut a = CodeAssembler::new(32).unwrap();
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.add(dword_ptr(esi + 4), eax).unwrap();
        a.fld(qword_ptr(esi + 8)).unwrap();
        a.fmul(qword_ptr(esi + 16)).unwrap();
        a.fstp(qword_ptr(esi + 24)).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
        a.ret().unwrap();
        let bytes = a.assemble(0x10000).unwrap();
        let mut machine = crate::Executor::new(0x10000, 4096).unwrap();
        machine.write(0x10000, &bytes).unwrap();
        machine.protect(0x10000, 4096, READ | EXEC).unwrap();
        let region = Region::new(0x10000, bytes.len());
        let plain = super::super::compile(&region, &mut machine.memory, false).unwrap();
        testing::FORCE.set(true);
        let tagged = super::super::compile(&region, &mut machine.memory, false);
        testing::FORCE.set(false);
        let tagged = tagged.unwrap();
        // Only per-compilation heap pointers (x87 operation descriptors) may
        // differ, as they do between any two compilations.
        let pointer = |line: &str| {
            line.strip_prefix("  movabsq $0x")
                .and_then(|rest| rest.split_once(", "))
                .map(|(_, register)| register.to_owned())
        };
        let (plain, tagged): (Vec<_>, Vec<_>) = (
            plain.disasm.lines().collect(),
            tagged.disasm.lines().collect(),
        );
        assert_eq!(plain.len(), tagged.len());
        for (a, b) in plain.iter().zip(&tagged) {
            assert!(
                a == b || pointer(a).is_some() && pointer(a) == pointer(b),
                "{a}\n{b}"
            );
        }
        let rows = testing::LAST.take();
        let parts: Vec<(&str, &str, &str)> = rows
            .lines()
            .map(|row| {
                let fields: Vec<_> = row.split('\t').collect();
                (fields[4], fields[5], fields[6])
            })
            .collect();
        let start = 0x10000.to_string();
        for (pc, part, class) in [
            (start.as_str(), "check", "int.mem"),
            (start.as_str(), "body", "int.mem"),
            ("0", "entry", "overhead"),
            ("0", "budget", "overhead"),
            ("0", "exit", "overhead"),
        ] {
            assert!(
                parts.contains(&(pc, part, class)),
                "{pc} {part} {class}\n{rows}"
            );
        }
        assert!(
            parts.iter().any(|&(_, _, class)| class == "x87.mem"),
            "{rows}"
        );
        assert!(
            parts.iter().any(|&(_, _, class)| class == "control"),
            "{rows}"
        );
    }

    #[test]
    fn tags_stay_clear_of_fault_markers() {
        let tag = guest_source(0x123, Part::Check).bits();
        assert_eq!(((tag - GUEST) >> 2, (tag - GUEST) & 3), (0x123, 1));
        // compile::fault owns 1 and 2.
        let overhead = [
            Overhead::Entry,
            Overhead::Budget,
            Overhead::Transfer,
            Overhead::Exit,
        ];
        assert!(
            overhead
                .iter()
                .all(|&kind| (3..GUEST).contains(&(kind as u32)))
        );
        assert!(X87_RECOVERY > 2 && guest_source(0, Part::Body).bits() > 2);
    }

    #[test]
    fn rows_merge_adjacent_tags_and_drop_untagged_ranges() {
        let instructions = BTreeMap::from([(0x1004, decode(&[0x8b, 0x03]))]);
        let check = guest_source(4, Part::Check);
        let text = rows(
            0x1000,
            "7",
            [
                (0, 4, SourceLoc::default()),
                (4, 8, overhead_source(Overhead::Exit)),
                (8, 12, check),
                (12, 20, check),
                (20, 24, guest_source(4, Part::Body)),
                (30, 31, x87_recovery_source()),
            ],
            &instructions,
        );
        assert_eq!(
            text,
            "7\t4\t8\t0\texit\toverhead\n\
             7\t8\t20\t4100\tcheck\tint.mem\n\
             7\t20\t24\t4100\tbody\tint.mem\n\
             7\t30\t31\t0\tslow\tx87-recovery\n"
        );
    }
}

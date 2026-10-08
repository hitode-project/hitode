//! Bounded local instruction observations for the explicit interpreter backend.
//!
//! Set ADV_INTERP_TRACE_DIR to an existing directory beneath target and
//! ADV_INTERP_TRACE_RANGES to comma-separated START..END half-open PC ranges
//! (decimal or 0x hexadecimal). ADV_INTERP_TRACE_MAX bounds all JSONL events
//! (default 4096, maximum 100000); ADV_INTERP_TRACE_STACK_WORDS selects 0..8
//! DWORDs at ESP (default 4). Each executor creates a separate file on its first
//! observation. ADV_INTERP_TRACE_OPERANDS=1 optionally adds explicit register,
//! immediate and memory operand scalars (at most eight bytes), never code bytes
//! or disassembly text. Memory after-values use the before effective address;
//! unknown or unreadable scalars are null. Implicit memory is not observed.
//! Unreadable stack words are null. REP observations represent one element,
//! not a completed repeated instruction.
//!
//! instruction.after precedes trap dispatch. A separate resume observation is
//! state at the next interpreter dispatch, possibly in a different CPU context;
//! it is not an instruction's after state. boundary records public run outcomes.
//! Configuration or IO errors warn and disable capture without failing execution.
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write as _};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use iced_x86::{
    Code, Decoder, DecoderOptions, Instruction, InstructionInfoFactory, OpKind, Register,
};

use super::RunOutcome;
use crate::interpreter::Step;
use crate::{CpuState, EXEC, Memory, PAGE_SIZE, READ};

const ADDRESS_END: u64 = 1 << 32;
const MAX_EVENTS: usize = 100_000;
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct Config {
    directory: PathBuf,
    ranges: Vec<Range<u64>>,
    max: usize,
    stack_words: usize,
    operands: bool,
}

impl Config {
    fn parse(
        directory: PathBuf,
        ranges: &str,
        max: &str,
        stack_words: &str,
        operands: &str,
    ) -> Result<Self, String> {
        let directory = directory.canonicalize().map_err(|e| e.to_string())?;
        if !directory.is_dir()
            || !directory.ancestors().any(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.eq_ignore_ascii_case("target"))
            })
        {
            return Err("output must be an existing directory beneath target".into());
        }
        let max = max.parse::<usize>().map_err(|e| e.to_string())?;
        let stack_words = stack_words.parse::<usize>().map_err(|e| e.to_string())?;
        let operands = match operands {
            "0" => false,
            "1" => true,
            _ => return Err("operands must be 0 or 1".into()),
        };
        if !(1..=MAX_EVENTS).contains(&max) || stack_words > 8 {
            return Err("max must be 1..100000 and stack words must be 0..8".into());
        }
        let mut parsed = Vec::new();
        for range in ranges.split(',') {
            let (start, end) = range.trim().split_once("..").ok_or("expected START..END")?;
            let number = |s: &str| -> Result<u64, String> {
                let s = s.trim();
                if let Some(s) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                    u64::from_str_radix(s, 16).map_err(|e| e.to_string())
                } else {
                    s.parse()
                        .map_err(|e: std::num::ParseIntError| e.to_string())
                }
            };
            let (start, end) = (number(start)?, number(end)?);
            if start >= end || end > ADDRESS_END || parsed.len() == 32 {
                return Err("expected 1..32 nonempty x86 PC ranges".into());
            }
            parsed.push(start..end);
        }
        parsed.sort_unstable_by_key(|r| r.start);
        Ok(Self {
            directory,
            ranges: parsed,
            max,
            stack_words,
            operands,
        })
    }
}

pub(super) struct Before {
    opcode: Option<Code>,
    state: String,
    operands: Option<OperandSnapshot>,
}

struct OperandSnapshot {
    entries: Vec<Operand>,
    before: String,
}

struct Operand {
    index: u32,
    kind: OpKind,
    detail: OperandDetail,
}

enum OperandDetail {
    Register(Register),
    Immediate {
        width: usize,
        value: u64,
        signed: Option<i64>,
    },
    Memory {
        segment: Register,
        address: Option<u32>,
        width: usize,
    },
    Unknown,
}

pub(crate) struct Trace {
    config: Config,
    path: PathBuf,
    writer: Option<BufWriter<File>>,
    written: usize,
    disabled: bool,
    pending_resume: bool,
    failure: Option<String>,
}

impl Trace {
    pub(super) fn from_env() -> Option<Box<Self>> {
        let directory = std::env::var_os("ADV_INTERP_TRACE_DIR")?;
        let read = |name, default: &str| std::env::var(name).unwrap_or_else(|_| default.into());
        match Config::parse(
            directory.into(),
            &read("ADV_INTERP_TRACE_RANGES", ""),
            &read("ADV_INTERP_TRACE_MAX", "4096"),
            &read("ADV_INTERP_TRACE_STACK_WORDS", "4"),
            &read("ADV_INTERP_TRACE_OPERANDS", "0"),
        ) {
            Ok(config) => Some(Box::new(Self::new(config))),
            Err(error) => {
                eprintln!("warning: interpreter instruction trace disabled: {error}");
                None
            }
        }
    }

    fn new(config: Config) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = config.directory.join(format!(
            "interpreter-{}-{id}-{time}.jsonl",
            std::process::id()
        ));
        Self {
            config,
            path,
            writer: None,
            written: 0,
            disabled: false,
            pending_resume: false,
            failure: None,
        }
    }

    pub(super) fn prepare(
        &mut self,
        units: u32,
        state: &CpuState,
        memory: &Memory,
    ) -> (u32, Option<Before>) {
        if self.disabled {
            return (units, None);
        }
        if self.pending_resume {
            self.pending_resume = false;
            self.record(
                "resume",
                &format!(
                    "\"state\":{}",
                    snapshot(state, memory, self.config.stack_words)
                ),
            );
        }
        if self.disabled {
            return (units, None);
        }
        let pc = u64::from(state.eip);
        if self.config.ranges.iter().any(|r| r.contains(&pc)) {
            let instruction = instruction(memory, state.eip);
            let operands = if self.config.operands {
                instruction.map(|i| {
                    let entries = operands(&i, state);
                    let before = operand_snapshot(&entries, state, memory);
                    OperandSnapshot { entries, before }
                })
            } else {
                None
            };
            return (
                1,
                Some(Before {
                    opcode: instruction.map(|i| i.code()),
                    state: snapshot(state, memory, self.config.stack_words),
                    operands,
                }),
            );
        }
        // Transfers already end decoded blocks. On fallthrough an x86 instruction
        // advances at most 15 bytes; this bound cannot skip an instruction whose
        // start is inside a range, even when the range begins mid-instruction.
        // This only shortens a batch: normal execution still charges actual units.
        let distance = self
            .config
            .ranges
            .iter()
            .map(|r| {
                if r.start > pc {
                    r.start - pc
                } else {
                    ADDRESS_END - pc + r.start
                }
            })
            .min()
            .unwrap();
        (
            units.min(distance.div_ceil(15).min(u64::from(u32::MAX)) as u32),
            None,
        )
    }

    pub(super) fn complete(
        &mut self,
        before: Before,
        step: Step,
        units: u32,
        state: &CpuState,
        memory: &Memory,
    ) {
        let opcode = before
            .opcode
            .map_or_else(|| "unavailable".into(), |c| format!("{c:?}"));
        let mut fields = format!(
            "\"opcode\":\"{opcode}\",\"step\":\"{step:?}\",\"units\":{units},\"before\":{},\"after\":{}",
            before.state,
            snapshot(state, memory, self.config.stack_words)
        );
        if self.config.operands {
            if let Some(operands) = before.operands {
                let _ = write!(
                    fields,
                    ",\"operands_before\":{},\"operands_after\":{}",
                    operands.before,
                    operand_snapshot(&operands.entries, state, memory)
                );
            } else {
                fields.push_str(",\"operands_before\":null,\"operands_after\":null");
            }
        }
        self.record("instruction", &fields);
        self.pending_resume = matches!(step, Step::Trap { .. });
    }

    pub(super) fn boundary(
        &mut self,
        result: &Result<RunOutcome, String>,
        state: &CpuState,
        memory: &Memory,
    ) {
        if self.written == 0 || self.disabled {
            return;
        }
        let outcome = result
            .as_ref()
            .map_or_else(|_| "executor_error".into(), |o| format!("{o:?}"));
        self.record(
            "boundary",
            &format!(
                "\"outcome\":\"{outcome}\",\"state\":{}",
                snapshot(state, memory, self.config.stack_words)
            ),
        );
    }

    fn record(&mut self, kind: &str, fields: &str) {
        if self.disabled {
            return;
        }
        let limit = self.written + 1 == self.config.max;
        let stop = if limit { "\"limit\"" } else { "null" };
        let text = format!(
            "{{\"seq\":{},\"kind\":\"{kind}\",{fields},\"capture_stop\":{stop}}}\n",
            self.written + 1
        );
        let result = (|| -> std::io::Result<()> {
            if self.writer.is_none() {
                let file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&self.path)?;
                self.writer = Some(BufWriter::new(file));
            }
            let writer = self.writer.as_mut().unwrap();
            writer.write_all(text.as_bytes())?;
            writer.flush()
        })();
        if let Err(error) = result {
            eprintln!(
                "warning: interpreter instruction trace disabled after IO failure ({}): {error}",
                self.path.display()
            );
            self.failure = Some(error.to_string());
            self.disabled = true;
            self.writer = None;
        } else {
            self.written += 1;
            self.disabled = limit;
        }
    }
}

fn permitted(memory: &Memory, address: u32, len: usize, permission: u8) -> bool {
    memory.range(address, len).is_ok_and(|r| {
        (r.start / PAGE_SIZE..=(r.end - 1) / PAGE_SIZE)
            .all(|p| memory.mapped[p] && memory.perms[p] & permission != 0)
    })
}

fn instruction(memory: &Memory, pc: u32) -> Option<Instruction> {
    let mut bytes = [0; 15];
    let mut len = 0;
    for offset in 0..15 {
        let Some(address) = pc.checked_add(offset) else {
            break;
        };
        if !permitted(memory, address, 1, EXEC) {
            break;
        }
        if memory.read_into(address, &mut bytes[len..len + 1]).is_err() {
            break;
        }
        len += 1;
    }
    let i = Decoder::with_ip(32, &bytes[..len], u64::from(pc), DecoderOptions::NONE).decode();
    (!i.is_invalid()).then_some(i)
}

fn register_scalar(state: &CpuState, register: Register) -> Option<u64> {
    if register.is_gpr32() {
        return state
            .regs
            .get(register.number())
            .map(|value| u64::from(*value));
    }
    if register.is_gpr16() {
        return state
            .regs
            .get(register.number())
            .map(|value| u64::from(*value & 0xffff));
    }
    if register.is_gpr8() {
        let shift = if matches!(
            register,
            Register::AH | Register::CH | Register::DH | Register::BH
        ) {
            8
        } else {
            0
        };
        return state
            .regs
            .get(register.full_register32().number())
            .map(|value| u64::from((value >> shift) & 0xff));
    }
    if register.is_segment_register() {
        return state
            .segments
            .selectors
            .get(register.number())
            .map(|value| u64::from(*value));
    }
    None
}

fn operands(instruction: &Instruction, state: &CpuState) -> Vec<Operand> {
    let mut factory = InstructionInfoFactory::new();
    let info = factory.info(instruction);
    (0..instruction.op_count())
        .map(|index| {
            let kind = instruction.op_kind(index);
            let detail = match kind {
                OpKind::Register => OperandDetail::Register(instruction.op_register(index)),
                OpKind::Memory => {
                    // Match verification's flat segments and guest-owned FS base.
                    let value = |register: Register, _: usize, _: usize| {
                        if register == Register::FS {
                            Some(u64::from(state.fs_base))
                        } else if register.is_segment_register() {
                            Some(0)
                        } else if register.is_gpr32() || register.is_gpr16() || register.is_gpr8() {
                            register_scalar(state, register)
                        } else {
                            None
                        }
                    };
                    // Associate only this explicit operand. InstructionInfo also
                    // applies architectural adjustments such as POP [ESP]'s
                    // post-pop destination, unlike the encoded address alone.
                    let mut used = info.used_memory().iter().filter(|used| {
                        used.access() == info.op_access(index)
                            && used.memory_size() == instruction.memory_size()
                    });
                    let address = if let Some(used_memory) = used.next() {
                        if used.next().is_some() {
                            None
                        } else {
                            used_memory.virtual_address(0, value)
                        }
                    } else {
                        None
                    };
                    OperandDetail::Memory {
                        segment: instruction.memory_segment(),
                        address: address.map(|value| value as u32),
                        width: instruction.memory_size().size(),
                    }
                }
                OpKind::Immediate8 | OpKind::Immediate8_2nd => OperandDetail::Immediate {
                    width: 1,
                    value: instruction.immediate(index),
                    signed: None,
                },
                OpKind::Immediate16 | OpKind::Immediate32 | OpKind::Immediate64 => {
                    OperandDetail::Immediate {
                        width: match kind {
                            OpKind::Immediate16 => 2,
                            OpKind::Immediate32 => 4,
                            _ => 8,
                        },
                        value: instruction.immediate(index),
                        signed: None,
                    }
                }
                OpKind::Immediate8to16 => OperandDetail::Immediate {
                    width: 2,
                    value: u64::from(instruction.immediate8to16() as u16),
                    signed: Some(i64::from(instruction.immediate8to16())),
                },
                OpKind::Immediate8to32 => OperandDetail::Immediate {
                    width: 4,
                    value: u64::from(instruction.immediate8to32() as u32),
                    signed: Some(i64::from(instruction.immediate8to32())),
                },
                OpKind::Immediate8to64 => OperandDetail::Immediate {
                    width: 8,
                    value: instruction.immediate8to64() as u64,
                    signed: Some(instruction.immediate8to64()),
                },
                OpKind::Immediate32to64 => OperandDetail::Immediate {
                    width: 8,
                    value: instruction.immediate32to64() as u64,
                    signed: Some(instruction.immediate32to64()),
                },
                _ => OperandDetail::Unknown,
            };
            Operand {
                index,
                kind,
                detail,
            }
        })
        .collect()
}

fn operand_snapshot(operands: &[Operand], state: &CpuState, memory: &Memory) -> String {
    let mut text = String::from("[");
    for (index, operand) in operands.iter().enumerate() {
        if index != 0 {
            text.push(',');
        }
        let _ = write!(
            text,
            "{{\"index\":{},\"kind\":\"{:?}\"",
            operand.index, operand.kind
        );
        match operand.detail {
            OperandDetail::Register(register) => {
                let _ = write!(
                    text,
                    ",\"register\":\"{register:?}\",\"width\":{},\"value\":",
                    register.size()
                );
                scalar_json(&mut text, register_scalar(state, register));
            }
            OperandDetail::Immediate {
                width,
                value,
                signed,
            } => {
                let _ = write!(text, ",\"width\":{width},\"value\":{value}");
                if let Some(signed) = signed {
                    let _ = write!(text, ",\"signed_value\":{signed}");
                }
            }
            OperandDetail::Memory {
                segment,
                address,
                width,
            } => {
                let _ = write!(
                    text,
                    ",\"segment\":\"{segment:?}\",\"width\":{width},\"ea\":"
                );
                scalar_json(&mut text, address.map(u64::from));
                text.push_str(",\"scalar\":");
                let value = address.and_then(|address| {
                    if !(1..=8).contains(&width) || !permitted(memory, address, width, READ) {
                        return None;
                    }
                    let mut bytes = [0; 8];
                    memory.read_into(address, &mut bytes[..width]).ok()?;
                    Some(u64::from_le_bytes(bytes))
                });
                scalar_json(&mut text, value);
            }
            OperandDetail::Unknown => text.push_str(",\"value\":null"),
        }
        text.push('}');
    }
    text.push(']');
    text
}

fn scalar_json(text: &mut String, value: Option<u64>) {
    if let Some(value) = value {
        let _ = write!(text, "{value}");
    } else {
        text.push_str("null");
    }
}

fn snapshot(state: &CpuState, memory: &Memory, stack_words: usize) -> String {
    let mut text = format!("{{\"eip\":{},\"eflags\":{}", state.eip, state.eflags);
    for (name, value) in ["eax", "ecx", "edx", "ebx", "esp", "ebp", "esi", "edi"]
        .iter()
        .zip(state.regs)
    {
        let _ = write!(text, ",\"{name}\":{value}");
    }
    text.push_str(",\"stack\":[");
    for index in 0..stack_words {
        if index != 0 {
            text.push(',');
        }
        let value = state.regs[4]
            .checked_add(index as u32 * 4)
            .and_then(|address| {
                if !permitted(memory, address, 4, READ) {
                    return None;
                }
                let mut bytes = [0; 4];
                memory.read_into(address, &mut bytes).ok()?;
                Some(u32::from_le_bytes(bytes))
            });
        match value {
            Some(value) => {
                let _ = write!(text, "{value}");
            }
            None => text.push_str("null"),
        }
    }
    text.push_str("]}");
    text
}

#[cfg(test)]
mod tests;

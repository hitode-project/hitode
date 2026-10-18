//! Opt-in, anonymous exact-path form/reason census. Each report emits a complete
//! cumulative snapshot to stderr; consumers use the last snapshot, not its sum.
//! No operand, instruction byte, address or control-flow identity is retained.
use super::*;
use iced_x86::Code;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Reason {
    NoFastForm,
    Pending,
    Control,
    PC,
    Operand,
    Memory,
    Range,
    Inexact,
    Other,
}

#[derive(Clone, Debug)]
pub struct Census {
    enabled: bool,
    counts: BTreeMap<(Code, Reason), u64>,
}

/// Read once: every executor run defaults a fresh census.
fn enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("ADV_TIER1_CENSUS").is_some_and(|v| v == "1"))
}

impl Default for Census {
    fn default() -> Self {
        Self {
            enabled: enabled(),
            counts: BTreeMap::new(),
        }
    }
}

impl Census {
    pub fn record(&mut self, op: &NativeX87, state: &CpuState, memory: &Memory) {
        if self.enabled {
            *self
                .counts
                .entry((op.instruction.code(), reason(op, state, memory)))
                .or_default() += 1;
        }
    }

    pub fn report(&self) {
        if !self.enabled {
            return;
        }
        let rows = self
            .counts
            .iter()
            .map(|(&(code, reason), count)| {
                format!("{:?}/{code:?}/{reason:?}={count}", code.mnemonic())
            })
            .collect::<Vec<_>>()
            .join(",");
        eprintln!("x87 forms: {rows}");
    }
}

fn reason(op: &NativeX87, state: &CpuState, memory: &Memory) -> Reason {
    let Some(fast) = op.fast else {
        return Reason::NoFastForm;
    };
    let x87 = &state.x87;
    if x87.status & 0x80 != 0 || state.mmx_mode != 0 {
        return Reason::Pending;
    }
    let i = &op.instruction;
    if i.op0_kind() == OpKind::Memory {
        let Some(at) = address(i, state) else {
            return Reason::Memory;
        };
        let width = i.memory_size().size();
        let write = matches!(
            fast,
            Fast::Store { .. } | Fast::StoreControl | Fast::StoreStatus
        );
        // Never repeat an observable file/device/foreign access just for a census.
        if memory.shared_range(at, width) || !accessible(memory, at, width, write) {
            return Reason::Memory;
        }
    }
    let operand = |index: u8| {
        let slot = x87.physical_index(usize::from(index));
        (x87.physical_tag(slot) != 3)
            .then(|| double(x87.regs[slot], i32::MAX))
            .flatten()
    };
    match fast {
        Fast::Arith {
            op,
            dest,
            source,
            reverse,
            ..
        } => {
            let control = x87.control;
            if control & 0xc3f != 0x3f {
                return Reason::Control;
            }
            if control & 0x300 == 0x100 {
                return Reason::PC;
            }
            let Some(mut a) = operand(dest) else {
                return Reason::Operand;
            };
            let Some((mut b, _)) = load(source, i, state, memory, i32::MAX) else {
                return Reason::Operand;
            };
            if !bounded(a, RANGE) || !bounded(b, RANGE) {
                return Reason::Range;
            }
            if reverse {
                std::mem::swap(&mut a, &mut b);
            }
            if op == Op::Div && b == 0.0 {
                return Reason::Operand;
            }
            if control & 0x300 == 0 && (a.to_bits() | b.to_bits()) & SINGLE_TAIL != 0 {
                return Reason::PC;
            }
            if control & 0x300 == 0x300 {
                if arithmetic(op, a, b).1 != 0.0 {
                    return Reason::Inexact;
                }
                return Reason::PC;
            }
        }
        Fast::Store { .. } | Fast::StoreSt { .. } | Fast::Negate | Fast::Absolute => {
            if operand(0).is_none() {
                return Reason::Operand;
            }
        }
        Fast::Compare { source, .. } => {
            if operand(0).is_none() || load(source, i, state, memory, i32::MAX).is_none() {
                return Reason::Operand;
            }
        }
        Fast::Exchange(index) => {
            if operand(0).is_none() || operand(index).is_none() {
                return Reason::Operand;
            }
        }
        Fast::Load(source)
            if (load(source, i, state, memory, i32::MAX).is_none()
                || x87.physical_tag(x87.top().wrapping_sub(1) & 7) != 3) =>
        {
            return Reason::Operand;
        }
        _ => {}
    }
    Reason::Other
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_x86::code_asm::*;

    #[test]
    fn census_is_opt_in_merges_and_never_changes_state() {
        let mut asm = CodeAssembler::new(32).unwrap();
        asm.fdiv_2(st0, st1).unwrap();
        let bytes = asm.assemble(0).unwrap();
        let instruction = iced_x86::Decoder::new(32, &bytes, 0).decode();
        let op = NativeX87 {
            instruction,
            opcode: None,
            fast: classify(&instruction),
        };
        let memory = Memory::new(0x10000, 4096).unwrap();
        let mut state = CpuState::default();
        state.x87.control = 0x037f;
        state.x87.push(extended(3.0));
        state.x87.push(extended(1.0));
        let before = state.clone();
        let mut census = Census {
            enabled: false,
            counts: BTreeMap::new(),
        };
        census.record(&op, &state, &memory);
        assert!(census.counts.is_empty());
        census.enabled = true;
        census.record(&op, &state, &memory);
        assert_eq!(census.counts[&(instruction.code(), Reason::Inexact)], 1);
        assert_eq!(state, before);
        state.x87.control = 0x077f;
        assert_eq!(reason(&op, &state, &memory), Reason::Control);
        state.x87.control = 0x017f;
        assert_eq!(reason(&op, &state, &memory), Reason::PC);
        state.x87.control = 0x037f;
        let slot = state.x87.physical_index(0);
        state.x87.regs[slot] = extended(2f64.powi(401));
        assert_eq!(reason(&op, &state, &memory), Reason::Range);
        state.x87.regs[slot] = Ext::from_parts((1 << 63) | 1, 0x3fff);
        assert_eq!(reason(&op, &state, &memory), Reason::Operand);
        state.x87.status |= 0x80;
        assert_eq!(reason(&op, &state, &memory), Reason::Pending);
    }
}

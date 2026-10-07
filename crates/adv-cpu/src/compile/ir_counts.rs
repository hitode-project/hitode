//! Opt-in, address-free counts of emitted IR before Cranelift optimization.
//! One successful request produces one JSON line on stderr. The service timer
//! belongs to the caller and excludes formatting/output. No source tags escape.
use super::{diagnostics, ir};
use iced_x86::Instruction;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::OnceLock;
use std::time::Duration;

type Label = (String, &'static str, &'static str);

#[derive(Default)]
pub(super) struct Counts {
    rows: BTreeMap<Label, (u64, u64)>,
    functions: u64,
}

pub(super) fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("ADV_JIT_IR_COUNTS").is_some_and(|v| v == "1"))
}

impl Counts {
    pub(super) fn function(
        counts: Option<&RefCell<Self>>,
        function: &ir::Function,
        origin: u32,
        instructions: &BTreeMap<u32, Instruction>,
    ) {
        let Some(counts) = counts else { return };
        let mut counts = counts.borrow_mut();
        counts.functions += 1;
        let mut sources = BTreeMap::<u32, (u64, u64)>::new();
        for block in function.layout.blocks() {
            // Attribute a block once, to its first instruction. Guest operations
            // sharing a block still receive their own individual IR counts.
            for (index, inst) in function.layout.block_insts(block).enumerate() {
                let row = sources.entry(function.srcloc(inst).bits()).or_default();
                row.0 += 1;
                row.1 += u64::from(index == 0);
            }
        }
        for (bits, (ir, blocks)) in sources {
            let label = label(ir::SourceLoc::new(bits), origin, instructions);
            let row = counts.rows.entry(label).or_default();
            row.0 += ir;
            row.1 += blocks;
        }
    }

    pub(super) fn finish(
        self,
        strategy: &'static str,
        instructions: &BTreeMap<u32, Instruction>,
    ) -> Summary {
        let mut decoded = BTreeMap::<_, u64>::new();
        for instruction in instructions.values() {
            *decoded
                .entry((
                    format!("{:?}", instruction.mnemonic()),
                    diagnostics::class(instruction),
                ))
                .or_default() += 1;
        }
        Summary {
            counts: self,
            strategy,
            decoded,
        }
    }
}

fn label(loc: ir::SourceLoc, origin: u32, instructions: &BTreeMap<u32, Instruction>) -> Label {
    let bits = loc.bits();
    let overhead = match bits {
        _ if loc.is_default() => "untagged",
        1 | 2 => "fault",
        3 => "entry",
        4 => "x87-recovery",
        5 => "budget",
        6 => "transfer",
        7 => "exit",
        8 => "flags",
        _ => "unknown",
    };
    if !loc.is_default() && bits >= 16 {
        let pc = origin.wrapping_add((bits - 16) >> 2);
        if let Some(instruction) = instructions.get(&pc) {
            return (
                format!("{:?}", instruction.mnemonic()),
                diagnostics::class(instruction),
                ["body", "check", "slow", "flags"][((bits - 16) & 3) as usize],
            );
        }
    }
    ("Overhead".into(), "overhead", overhead)
}

pub(super) struct Summary {
    counts: Counts,
    strategy: &'static str,
    decoded: BTreeMap<(String, &'static str), u64>,
}

impl Summary {
    pub(super) fn json(&self, native_bytes: usize, service: Duration) -> String {
        let ir: u64 = self.counts.rows.values().map(|r| r.0).sum();
        let blocks: u64 = self.counts.rows.values().map(|r| r.1).sum();
        let guest: u64 = self.decoded.values().sum();
        let mut text = format!(
            "{{\"version\":1,\"strategy\":\"{}\",\"guest\":{guest},\"ir\":{ir},\"blocks\":{blocks},\"functions\":{},\"native_bytes\":{native_bytes},\"service_us\":{},\"rows\":[",
            self.strategy,
            self.counts.functions,
            service.as_micros()
        );
        for (index, ((mnemonic, class, part), (ir, blocks))) in self.counts.rows.iter().enumerate()
        {
            if index != 0 {
                text.push(',');
            }
            // All labels are producer-owned enum names or fixed ASCII strings.
            write!(
                text,
                "[\"{mnemonic}\",\"{class}\",\"{part}\",{ir},{blocks}]"
            )
            .unwrap();
        }
        text.push_str("],\"decoded\":[");
        for (index, ((mnemonic, class), count)) in self.decoded.iter().enumerate() {
            if index != 0 {
                text.push(',');
            }
            write!(text, "[\"{mnemonic}\",\"{class}\",{count}]").unwrap();
        }
        text.push_str("]}");
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranelift_codegen::ir::{InstBuilder, types};
    use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
    use iced_x86::{Decoder, DecoderOptions};

    #[test]
    fn counts_reconcile_without_exporting_instruction_locations() {
        let instruction = Decoder::new(32, &[0x90], DecoderOptions::NONE).decode();
        let instructions = BTreeMap::from([(0x1234, instruction)]);
        let mut function = ir::Function::new();
        let mut context = FunctionBuilderContext::new();
        let mut b = FunctionBuilder::new(&mut function, &mut context);
        let block = b.create_block();
        b.switch_to_block(block);
        b.set_srcloc(diagnostics::guest_source(4, diagnostics::Part::Body));
        b.ins().iconst(types::I32, 123);
        b.set_srcloc(diagnostics::overhead_source(diagnostics::Overhead::Exit));
        b.ins().return_(&[]);
        b.seal_all_blocks();
        b.finalize();
        let counts = RefCell::new(Counts::default());
        Counts::function(Some(&counts), &function, 0x1230, &instructions);
        let json = counts
            .into_inner()
            .finish("fast", &instructions)
            .json(24, Duration::from_micros(7));
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["ir"], 2);
        assert_eq!(value["blocks"], 1);
        assert_eq!(value["guest"], 1);
        assert_eq!(value["service_us"], 7);
        assert_eq!(
            value["rows"][0],
            serde_json::json!(["Nop", "int", "body", 1, 1])
        );
        assert!(!json.contains("4660") && !json.contains("1234") && !json.contains("123"));
    }
}

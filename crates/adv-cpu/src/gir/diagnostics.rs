//! Anonymous compile-admission census. No sample bytes, PCs, or host paths.
use super::{ir::Reject, lift};
use iced_x86::Instruction;
use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
};

#[derive(Default, Debug)]
pub struct Census {
    pub regions: u64,
    pub complete_regions: u64,
    pub instructions: u64,
    pub supported: u64,
    pub verify_failures: u64,
    pub blocks: u64,
    pub inlined_calls: u64,
    pub rejected: BTreeMap<Reject, u64>,
}
impl Census {
    pub fn observe(&mut self, instructions: &BTreeMap<u32, Instruction>, start: u32) {
        self.regions += 1;
        self.instructions += instructions.len() as u64;
        match lift::lift(instructions, start) {
            Ok(lifted) => {
                self.supported += lifted.supported as u64;
                self.blocks += lifted.region.blocks.len().saturating_sub(1) as u64;
                self.inlined_calls += lifted.inlined_calls as u64;
                self.complete_regions += u64::from(lifted.rejected.is_empty());
                for (reason, count) in lifted.rejected {
                    *self.rejected.entry(reason).or_default() += count as u64;
                }
            }
            Err(error) => {
                self.verify_failures += 1;
                let form = error
                    .block
                    .and_then(|b| instructions.values().nth(b.0))
                    .map(Instruction::code);
                eprintln!("gir_lift verification_error={error} form={form:?}");
            }
        }
    }
    pub fn line(&self) -> String {
        format!(
            "gir_lift regions={} complete={} instructions={} supported={} verify_failures={} blocks={} inlined_calls={} rejected={:?}",
            self.regions,
            self.complete_regions,
            self.instructions,
            self.supported,
            self.verify_failures,
            self.blocks,
            self.inlined_calls,
            self.rejected
        )
    }
}

pub fn record(instructions: &BTreeMap<u32, Instruction>, start: u32) {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    if !*ENABLED.get_or_init(|| std::env::var("ADV_GIR_LIFT").is_ok_and(|v| v == "1")) {
        return;
    }
    static CENSUS: Mutex<Census> = Mutex::new(Census {
        regions: 0,
        complete_regions: 0,
        instructions: 0,
        supported: 0,
        verify_failures: 0,
        blocks: 0,
        inlined_calls: 0,
        rejected: BTreeMap::new(),
    });
    if let Ok(mut census) = CENSUS.lock() {
        census.observe(instructions, start);
        eprintln!("{}", census.line());
    }
}

#[cfg(feature = "gir-lower")]
pub fn record_lower(instructions: &BTreeMap<u32, Instruction>, start: u32, base: u32, len: usize) {
    if std::env::var("ADV_GIR_LOWER").as_deref() != Ok("1") {
        return;
    }
    match lift::lift(instructions, start)
        .map_err(|e| e.to_string())
        .and_then(|l| super::lower_cl::compile(&l.region, base, len, cfg!(feature = "cross-dump")))
    {
        Ok(code) => eprintln!(
            "gir_lower guest={} bytes={} arm64_bytes={:?} cold_edges={} cold_blocks={} flags_materialized={}",
            instructions.len(),
            code.machine.len(),
            code.cross.as_ref().map(|c| c.0.len()),
            code.cold_edges,
            code.cold_blocks,
            code.flags_materialized
        ),
        Err(_) => eprintln!("gir_lower failed=1"),
    }
}

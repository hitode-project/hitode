//! Owner-only publication and entry proof for the opt-in serial SSA lane.
use super::{ir, lift, lower_cl as lower};
use crate::{CpuState, Memory, memory::gir::LeaseCache};
use iced_x86::Instruction;
use std::{collections::BTreeMap, sync::atomic::AtomicBool};

#[derive(Default, Clone, Debug)]
pub struct Stats {
    pub admitted: u64,
    pub rejected: u64,
    pub entries: u64,
    pub units: u64,
    pub other_units: u64,
    pub proven: u64,
    pub checked: u64,
    pub cold: u64,
    pub invalidations: u64,
    pub mode_rejections: u64,
}

pub struct Program {
    checked: lower::Code,
    proven: Option<lower::Code>,
    pages: Vec<(usize, u64)>,
    entry: u32,
}

impl Program {
    pub fn compile(
        instructions: &BTreeMap<u32, Instruction>,
        entry: u32,
        memory: &mut Memory,
        pages: Vec<(usize, u64)>,
    ) -> Result<Self, String> {
        if !instructions
            .get(&entry)
            .is_some_and(|i| lift::classify(i).is_ok())
        {
            return Err("GIR entry form is not represented".into());
        }
        let region = lift::lift(instructions, entry)
            .map_err(|e| e.to_string())?
            .region;
        let mut checked = lower::compile(&region, memory.base, memory.len, false)?;
        checked.vcode = String::new();
        checked.table.page_dependencies = pages.clone();
        let proven = if region
            .memory
            .iter()
            .any(|m| matches!(region.values[m.address.0].kind, ir::ValueKind::Constant(_)))
        {
            let mut code = lower::compile_proven(&region, memory.base, memory.len)?;
            code.vcode = String::new();
            code.table.page_dependencies = pages.clone();
            Some(code)
        } else {
            None
        };
        // Compilation used the existing immutable decoded set. Its parent's
        // snapshot already watched every sampled code page, including tails.
        Ok(Self {
            checked,
            proven,
            pages,
            entry,
        })
    }

    pub fn valid(&self, memory: &Memory) -> bool {
        self.pages
            .iter()
            .all(|&(page, version)| memory.versions.get(page) == Some(&version))
    }

    pub fn run(
        &self,
        frame: &mut lower::Frame,
        memory: &mut Memory,
        cache: &mut LeaseCache,
        budget: u64,
        interrupt: &AtomicBool,
        revoke: &AtomicBool,
        stop: u32,
    ) -> Result<(u32, bool), String> {
        if frame.pc != self.entry || frame.flags & (1 << 8) != 0 || !self.valid(memory) {
            frame.status = lower::REVOKED;
            return Ok((lower::REVOKED, false));
        }
        let mut lease = cache.acquire(memory)?;
        let proven = self.proven.as_ref().filter(|code| code.proven(&lease));
        let status = proven.unwrap_or(&self.checked).run_lease(
            frame,
            &mut lease,
            budget,
            interrupt,
            revoke,
            u64::from(stop),
        )?;
        Ok((status, proven.is_some()))
    }
}

pub struct Engine {
    pub programs: BTreeMap<u32, Option<Program>>,
    pub lease: LeaseCache,
    pub stats: Stats,
    pub revoke: AtomicBool,
}

impl Engine {
    pub fn new() -> Result<Self, String> {
        if !cfg!(all(
            any(target_arch = "x86_64", target_arch = "aarch64"),
            any(windows, target_os = "linux", target_os = "android")
        )) {
            return Err("GIR execution is not enabled on this platform".into());
        }
        Ok(Self {
            programs: BTreeMap::new(),
            lease: LeaseCache::default(),
            stats: Stats::default(),
            revoke: AtomicBool::new(false),
        })
    }

    pub fn mode_allowed(cpu: &CpuState) -> bool {
        cpu.eflags & (1 << 8) == 0 && cpu.x87.status & 0x80 == 0 && cpu.rep.is_none()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let s = &self.stats;
        eprintln!(
            "gir_execution admitted={} rejected={} entries={} units={} other_units={} proven={} checked={} cold={} invalidations={} mode_rejections={}",
            s.admitted,
            s.rejected,
            s.entries,
            s.units,
            s.other_units,
            s.proven,
            s.checked,
            s.cold,
            s.invalidations,
            s.mode_rejections
        );
    }
}

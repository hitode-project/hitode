//! Owner-only publication and entry proof for the opt-in serial SSA lane.
use super::{hot, ir, lift, lower_cl as lower};
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
    pub tails: u64,
    pub promotions: u64,
    pub merged_regions: u64,
    pub code_bytes: u64,
    /// Complete/direct, indirect, call, return, slice, helper/form, budget,
    /// memory/guard, interrupt/revoke, stop. No guest identities are recorded.
    pub boundaries: [u64; 10],
}

pub struct Program {
    checked: lower::Code,
    proven: Option<lower::Code>,
    pages: Vec<(usize, u64)>,
    entry: u32,
    _tails: Vec<lower::Code>,
    links: Box<[usize]>,
    pub code_bytes: usize,
}

impl Program {
    pub fn compile(
        instructions: &BTreeMap<u32, Instruction>,
        entry: u32,
        memory: &mut Memory,
        pages: Vec<(usize, u64)>,
    ) -> Result<Self, String> {
        if instructions.len() > hot::MAX_INSTRUCTIONS {
            return Err("GIR instruction budget exceeded".into());
        }
        if !instructions
            .get(&entry)
            .is_some_and(|i| lift::classify(i).is_ok())
        {
            return Err("GIR entry form is not represented".into());
        }
        let region = lift::lift(instructions, entry)
            .map_err(|e| e.to_string())?
            .region;
        if region.values.len() > 65_536 {
            return Err("GIR value budget exceeded".into());
        }
        let mut checked =
            lower::compile_linked(&region, memory.base, memory.len, &BTreeMap::new(), entry)?;
        checked.vcode = String::new();
        checked.table.page_dependencies = pages.clone();
        let proven = if region
            .memory
            .iter()
            .any(|m| matches!(region.values[m.address.0].kind, ir::ValueKind::Constant(_)))
        {
            let mut code = lower::compile_proven_tail(&region, memory.base, memory.len, entry)?;
            code.vcode = String::new();
            code.table.page_dependencies = pages.clone();
            Some(code)
        } else {
            None
        };
        // Compilation used the existing immutable decoded set. Its parent's
        // snapshot already watched every sampled code page, including tails.
        let code_bytes = checked.code_bytes + proven.as_ref().map_or(0, |c| c.code_bytes);
        if code_bytes > hot::MAX_CODE_BYTES {
            return Err("GIR code budget exceeded".into());
        }
        Ok(Self {
            checked,
            proven,
            pages,
            entry,
            _tails: Vec::new(),
            links: Box::new([]),
            code_bytes,
        })
    }

    pub fn compile_family(selection: hot::Selection, memory: &mut Memory) -> Result<Self, String> {
        let root = selection.units.first().ok_or("empty GIR family")?;
        if selection.units.len() == 1 {
            return Self::compile(&root.instructions, root.entry, memory, selection.pages);
        }
        let entry = root.entry;
        let targets: BTreeMap<_, _> = selection
            .units
            .iter()
            .enumerate()
            .map(|(n, unit)| (unit.entry, n))
            .collect();
        let mut codes = Vec::new();
        let mut code_bytes = 0;
        for unit in &selection.units {
            if !unit
                .instructions
                .get(&unit.entry)
                .is_some_and(|i| lift::classify(i).is_ok())
            {
                return Err("unrepresented GIR family entry".into());
            }
            let r = lift::lift(&unit.instructions, unit.entry)
                .map_err(|e| e.to_string())?
                .region;
            if r.values.len() > 65_536 {
                return Err("GIR value budget exceeded".into());
            }
            let mut code =
                lower::compile_linked(&r, memory.base, memory.len, &targets, unit.entry)?;
            code_bytes += code.code_bytes;
            if code_bytes > hot::MAX_CODE_BYTES {
                return Err("GIR family code budget exceeded".into());
            }
            code.vcode.clear();
            code.table.page_dependencies = selection.pages.clone();
            codes.push(code);
        }
        let links = codes.iter().map(lower::Code::tail_entry).collect();
        let checked = codes.remove(0);
        Ok(Self {
            checked,
            proven: None,
            pages: selection.pages,
            entry,
            _tails: codes,
            links,
            code_bytes,
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
        let status = proven.unwrap_or(&self.checked).run_linked(
            frame,
            &mut lease,
            budget,
            interrupt,
            revoke,
            u64::from(stop),
            &self.links,
        )?;
        Ok((status, proven.is_some()))
    }
}

pub struct Engine {
    pub programs: BTreeMap<u32, Option<Program>>,
    pub lease: LeaseCache,
    pub stats: Stats,
    pub revoke: AtomicBool,
    pub heat: BTreeMap<u32, u64>,
    pub promoted: std::collections::BTreeSet<u32>,
}

impl Engine {
    pub fn publish(&mut self, owner: u32, program: Option<Program>) {
        const CACHE_BYTES: usize = 32 * 1024 * 1024;
        let bytes = self
            .programs
            .iter()
            .filter(|(entry, _)| **entry != owner)
            .filter_map(|(_, p)| p.as_ref())
            .map(|p| p.code_bytes)
            .sum::<usize>();
        if bytes + program.as_ref().map_or(0, |p| p.code_bytes) > CACHE_BYTES {
            // Only the serial owner publishes, between leases. No generated
            // pointer can outlive the owning Program or this cache eviction.
            self.programs.clear();
            self.promoted.clear();
        }
        self.programs.insert(owner, program);
    }

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
            heat: BTreeMap::new(),
            promoted: std::collections::BTreeSet::new(),
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
            "gir_execution admitted={} rejected={} entries={} units={} other_units={} proven={} checked={} cold={} invalidations={} mode_rejections={} tails={} promotions={} merged_regions={} code_bytes={} boundaries={:?}",
            s.admitted,
            s.rejected,
            s.entries,
            s.units,
            s.other_units,
            s.proven,
            s.checked,
            s.cold,
            s.invalidations,
            s.mode_rejections,
            s.tails,
            s.promotions,
            s.merged_regions,
            s.code_bytes,
            s.boundaries
        );
    }
}

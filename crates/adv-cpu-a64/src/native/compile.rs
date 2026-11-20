//! Build entry points and reusable Tier-1 translation buffers (A64A-ROLE-4).

#[cfg(native)]
mod pool;
#[cfg(native)]
pub(super) use pool::{Compiler, Job, JobForm, Request, ResidentForm, Task};

use crate::jit_map::Mark;
use crate::translate::jit::{self, Code};
use crate::translate::lower::Site;

#[cfg(native)]
use crate::facts::Key;
#[cfg(test)]
use crate::translate::front::trace::Redirect;
#[cfg(native)]
use crate::translate::lower::{InternalLink, Link};
#[cfg(native)]
use crate::translate::opt::computation::Proof;
use crate::translate::{front::Unit, jit::Features, lower::Stubs};

/// The last unit translated, in buffers the next translation reuses.
#[derive(Default)]
pub struct Workspace {
    #[cfg(test)]
    pub(crate) unit: Unit,
    pub(crate) code: Code,
    /// Translate the next entry as cold: one path, no loop trace.
    #[cfg(test)]
    pub(crate) cold: bool,
    #[cfg(test)]
    pub(crate) resident_links: bool,
}

impl Workspace {
    pub fn words(&self) -> &[u32] {
        self.code.asm.words()
    }

    /// Byte offset of each site in `words`.
    pub fn sites(&self) -> &[(usize, Site)] {
        &self.code.sites
    }

    /// Instructions the entry prepays: the unit runs only with that much
    /// countdown.
    #[cfg(native)]
    pub fn retired(&self) -> u32 {
        self.code.retired
    }

    /// Guest bytes the unit depends on, as sorted
    /// (address, length) spans.
    #[cfg(test)]
    pub fn code(&self) -> &[(u32, u32)] {
        self.unit.code()
    }

    #[cfg(native)]
    pub fn links(&self) -> &[Link] {
        &self.code.links
    }

    #[cfg(native)]
    pub(crate) fn internal_links(&self) -> &[InternalLink] {
        &self.code.internal_links
    }

    /// Where each stretch of the unit starts, for samplers.
    pub fn marks(&self) -> &[Mark] {
        &self.code.marks
    }
}

/// Translates the unit at `eip` into `work`, to be placed at host address
/// `at`. Units end before any of `stops`, the accesses of instructions
/// `checked` accepts check their page first, and the unit's helpers are
/// numbered from `helpers`. x87 arithmetic runs natively under the
/// control word `control`, the guest's at translation. Each translated byte is
/// registered with `watch_code`, so a store to it reaches the owner.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn translate(
    work: &mut Workspace,
    memory: &mut adv_cpu_core::memory::Memory,
    eip: u32,
    stops: &[u32],
    checked: &dyn Fn(u32) -> bool,
    helpers: u32,
    features: Features,
    at: usize,
    stubs: Stubs,
    host: &dyn Fn(u32, u32) -> Option<u32>,
    redirect: &dyn Fn(u32) -> Option<Redirect>,
    admit: &dyn Fn(u32, &[u8]) -> Vec<adv_cpu_core::native_region::NativeRegion>,
    control: u16,
    pe: bool,
) {
    work.unit
        .build(memory, eip, stops, host, redirect, !work.cold);
    work.unit.prepare_regions(memory, admit);
    jit::lower_unit(
        &mut work.code,
        &work.unit,
        eip,
        checked,
        helpers,
        features,
        at,
        stubs,
        control,
        pe,
        work.resident_links,
    );
}

impl Workspace {
    #[cfg(native)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn lower_tier1(
        &mut self,
        unit: &Unit,
        eip: u32,
        checked: &dyn Fn(u32) -> bool,
        helpers: u32,
        features: Features,
        at: usize,
        stubs: Stubs,
        resident_links: bool,
        facts: Key,
        arrivals: usize,
    ) {
        jit::lower_tier1(
            &mut self.code,
            unit,
            eip,
            checked,
            helpers,
            features,
            at,
            stubs,
            resident_links,
            facts,
            arrivals,
        );
    }
}

#[cfg(native)]
pub(crate) fn lower_computation<'a>(
    unit: &Unit,
    code: &'a mut Code,
    eip: u32,
    features: Features,
    at: usize,
    stubs: Stubs,
    (control, pe, proof): (u16, bool, Option<&Proof>),
) -> Option<jit::ComputationCode<'a>> {
    let assumptions = proof.map(|proof| jit::ComputationFacts {
        frame_cache: proof.frame_cache(),
        divisions: proof.divisions(),
    });
    jit::lower_computation(
        unit,
        code,
        eip,
        features,
        at,
        stubs,
        (control, pe, assumptions),
    )
}

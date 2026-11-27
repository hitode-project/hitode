//! Static resident loop costs for the owner's probation decision and
//! microarchitecture-aware FP validation strategy selection (A64-OPT-4/5).

use super::{Block, Func};
use std::sync::OnceLock;

/// Host AArch64 core class for FP validation cost modeling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CoreClass {
    /// Apple Silicon (M1–M4) / Qualcomm Oryon: expensive `mrs/msr fpsr` (41–57 cyc),
    /// cheap vector witness (0–3 cyc per 4–8 ops).
    AppleOryon,
    /// Arm Cortex-X / Cortex-A7xx big cores: cheap `mrs/msr fpsr` (1–13 cyc).
    CortexBig,
    /// Arm Cortex-A5xx little cores: moderate `mrs/msr fpsr` and narrower SIMD.
    CortexLittle,
}

/// Relative cycle estimates used to pick between FP validation strategies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ValidationCost {
    pub mrs_fpsr: u32,
    pub mrs_msr_fpsr: u32,
    pub vec_witness_4: u32,
    pub scalar_witness: u32,
}

/// Concrete validation lowering strategy selected for an `Op::FpValidate` node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ValidationStrategy {
    /// Statically proved in-range by `fp_range` (0 runtime instructions).
    StaticProof,
    /// Batched NEON `.4s` exponent-window witness (`ins` + shifts + `uqsub` + `umaxv`).
    VectorWitness,
    /// Hardware `FPSR` mask (`0x8f` including `IDC`), masked exceptions only.
    FpsrMask,
    /// Scalar GPR exponent-window witness (`fmov` + `ubfx` + `sub` + `cmp`).
    ScalarWitness,
}

#[cfg(test)]
std::thread_local! {
    static TEST_CORE_CLASS: std::cell::Cell<Option<CoreClass>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn with_core_class<R>(class: CoreClass, f: impl FnOnce() -> R) -> R {
    TEST_CORE_CLASS.with(|cell| {
        let prev = cell.replace(Some(class));
        let out = f();
        cell.set(prev);
        out
    })
}

impl CoreClass {
    /// Detects the host core class once per process.
    pub(crate) fn detect() -> Self {
        #[cfg(test)]
        if let Some(class) = TEST_CORE_CLASS.with(std::cell::Cell::get) {
            return class;
        }
        static CLASS: OnceLock<CoreClass> = OnceLock::new();
        *CLASS.get_or_init(detect_core_class)
    }

    pub(crate) const fn validation_cost(self) -> ValidationCost {
        match self {
            Self::AppleOryon => ValidationCost {
                mrs_fpsr: 12,
                mrs_msr_fpsr: 49,
                vec_witness_4: 3,
                scalar_witness: 2,
            },
            Self::CortexBig => ValidationCost {
                mrs_fpsr: 3,
                mrs_msr_fpsr: 8,
                vec_witness_4: 4,
                scalar_witness: 3,
            },
            Self::CortexLittle => ValidationCost {
                mrs_fpsr: 6,
                mrs_msr_fpsr: 14,
                vec_witness_4: 6,
                scalar_witness: 4,
            },
        }
    }
}

fn detect_core_class() -> CoreClass {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        CoreClass::AppleOryon
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    {
        #[cfg(target_os = "linux")]
        if let Ok(text) =
            std::fs::read_to_string("/sys/devices/system/cpu/cpu0/regs/identification/midr_el1")
            && let Some(hex) = text.trim().strip_prefix("0x")
            && let Ok(midr) = u64::from_str_radix(hex, 16)
        {
            let implementer = ((midr >> 24) & 0xff) as u8;
            let part = ((midr >> 4) & 0xfff) as u16;
            return match implementer {
                0x61 | 0x51 => CoreClass::AppleOryon,
                0x41 if matches!(part, 0xd03 | 0xd04 | 0xd05 | 0xd46 | 0xd80) => {
                    CoreClass::CortexLittle
                }
                _ => CoreClass::CortexBig,
            };
        }
        if adv_cpu_core::HostFeatures::get().contains(adv_cpu_core::HostFeatures::RPRES) {
            CoreClass::AppleOryon
        } else {
            CoreClass::CortexBig
        }
    }
}

/// Selects the lowest-cost sound FP validation strategy for `count` validated
/// values (of which `strict_muls` are unproved multiplications).
///
/// `fpsr_eligible` must be true only when all exceptions are masked and the
/// hardware FPCR tininess rule matches the validated operations (e.g. `AH=0` for
/// x87 tininess-before-rounding or `AH=1` for SSE).
pub(crate) fn select_validation(
    class: CoreClass,
    count: usize,
    strict_muls: usize,
    fpsr_eligible: bool,
    sse_live: bool,
) -> ValidationStrategy {
    if count == 0 {
        return ValidationStrategy::StaticProof;
    }
    let cost = class.validation_cost();
    let fpsr_cost = if sse_live {
        cost.mrs_msr_fpsr + cost.mrs_fpsr
    } else {
        cost.mrs_msr_fpsr
    };
    let non_strict = count.saturating_sub(strict_muls);
    let witness_cost = if non_strict == 0 {
        (strict_muls as u32) * cost.scalar_witness
    } else if non_strict == 1 {
        cost.scalar_witness + (strict_muls as u32) * cost.scalar_witness
    } else {
        (non_strict as u32).div_ceil(4) * cost.vec_witness_4
            + (strict_muls as u32) * cost.scalar_witness
    };
    if fpsr_eligible && fpsr_cost < witness_cost {
        ValidationStrategy::FpsrMask
    } else if count >= 2 {
        ValidationStrategy::VectorWitness
    } else {
        ValidationStrategy::ScalarWitness
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct BlockCost {
    pub body: u32,
    pub edges: [u32; 2],
}

#[derive(Default)]
pub(super) struct Iteration {
    pub words: u32,
    pub retired: u32,
    pub pcs: Vec<u32>,
}

#[derive(Clone, Copy)]
struct Path {
    words: u32,
    retired: i64,
    next: Block,
}

pub(super) fn iteration(func: &Func, costs: &[BlockCost]) -> Iteration {
    let Some(head) = func.head_exit else {
        return Iteration::default();
    };
    // Estimate the longest path back to this head, omitting cycles at
    // other heads. Keep the guest instructions and charges on that path.
    let mut paths = vec![None; func.blocks.len()];
    let Some(total) = longest(func, costs, &mut paths, head, head) else {
        return Iteration::default();
    };
    let mut pcs = Vec::new();
    let mut block = head;
    loop {
        let i = block.0 as usize;
        pcs.extend_from_slice(&func.blocks[i].pcs);
        block = paths[i].flatten().unwrap().next;
        if block == head {
            break;
        }
    }
    Iteration {
        words: total.words,
        retired: total.retired as u32,
        pcs,
    }
}

fn longest(
    func: &Func,
    costs: &[BlockCost],
    paths: &mut [Option<Option<Path>>],
    block: Block,
    head: Block,
) -> Option<Path> {
    let b = block.0 as usize;
    if let Some(path) = paths[b] {
        return path;
    }
    paths[b] = Some(None);
    let cost = costs[b];
    let best = func.blocks[b]
        .end
        .targets()
        .enumerate()
        .filter_map(|(i, t)| {
            let (words, retired) = if t.block == head {
                (0, 0)
            } else {
                let rest = longest(func, costs, paths, t.block, head)?;
                (rest.words, rest.retired)
            };
            Some(Path {
                words: cost.body + cost.edges[i] + words,
                retired: i64::from(t.charge) + retired,
                next: t.block,
            })
        })
        .max_by_key(|path| path.words);
    paths[b] = Some(best);
    best
}

/// The cheapest returning path pays for the check and the batch, including
/// cases where a scalar branch skips all effects.
pub(super) fn cheapest(func: &Func, costs: &[BlockCost], lp: &super::loops::Loop) -> u32 {
    let mut work = vec![0; func.blocks.len()];
    for &block in lp.blocks.iter().rev() {
        let i = block.0 as usize;
        work[i] = costs[i].body
            + if block == lp.latch {
                costs[i].edges[usize::from(!lp.latch_taken)]
            } else {
                func.blocks[i]
                    .end
                    .targets()
                    .enumerate()
                    .map(|(edge, target)| costs[i].edges[edge] + work[target.block.0 as usize])
                    .min()
                    .unwrap()
            };
    }
    work[lp.head.0 as usize]
}

pub(super) fn batch_savings(
    func: &Func,
    costs: &[BlockCost],
    batch: &super::loops::Batch,
    words: u32,
    price: Option<&super::batch::Price>,
) -> u32 {
    if let Some(price) = price.filter(|price| !price.classes.is_empty()) {
        price
            .classes
            .iter()
            .map(|class| {
                let scalar = class
                    .first
                    .iter()
                    .chain(&class.second)
                    .map(|(block, edge)| {
                        let cost = costs[block.0 as usize];
                        cost.body + cost.edges[*edge]
                    })
                    .sum::<u32>();
                scalar.saturating_sub(price.common + class.words)
            })
            .min()
            .unwrap()
    } else {
        let scalar = cheapest(func, costs, &batch.loop_) * u32::from(batch.plan.lanes);
        scalar.saturating_sub(words)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: A64-OPT-4, A64-OPT-5
    #[test]
    fn validation_strategy_tracks_core_class_costs() {
        assert_eq!(
            select_validation(CoreClass::AppleOryon, 0, 0, true, false),
            ValidationStrategy::StaticProof
        );
        // Short/medium DAGs on Apple/Oryon avoid expensive mrs+msr and use vector witness.
        assert_eq!(
            select_validation(CoreClass::AppleOryon, 4, 0, true, false),
            ValidationStrategy::VectorWitness
        );
        assert_eq!(
            select_validation(CoreClass::AppleOryon, 1, 0, true, false),
            ValidationStrategy::ScalarWitness
        );
        // Cortex big cores switch to FPSR mask once witness cost exceeds mrs+msr.
        assert_eq!(
            select_validation(CoreClass::CortexBig, 4, 3, true, false),
            ValidationStrategy::FpsrMask
        );
        // Unmasked / ineligible contexts never select FpsrMask.
        assert_eq!(
            select_validation(CoreClass::CortexBig, 4, 3, false, false),
            ValidationStrategy::VectorWitness
        );
    }
}

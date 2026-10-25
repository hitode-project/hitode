//! Admission data for source-assisted native regions.
//!
//! Contracts describe computations without guest addresses. An analyzer proposes
//! a mapping against an immutable image snapshot; only [`verify::verify`] can
//! produce a [`VerifiedBinding`]. Neither recognition nor differential samples
//! constitute admission. These types contain no execution or device services.

pub mod analyze;
pub mod expression;
mod frame;
mod paeth;
pub mod verify;

use sha2::{Digest, Sha256};
use std::sync::Arc;

pub use expression::{Binary, Expression, MemoryVersion, Width};
pub use frame::{FrameInvariant, FrameState};
pub use verify::VerifiedBinding;

/// Identity of a computation, independently of the implementation that runs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Computation {
    /// Add the reconstructed byte `bpp` positions earlier, modulo 256.
    PngSub,
    /// Add the corresponding previous-row byte, modulo 256.
    PngUp,
    /// Add floor((left + above) / 2), using a wide sum before byte truncation.
    PngAverage,
    /// For p=left+above-upper_left, choose the closest input, breaking ties
    /// in the order left, above, upper_left; then add modulo 256.
    PngPaeth,
    /// Copy from a positive backward distance, reading the updated window
    /// at every step rather than an immutable snapshot of the source range.
    Lz77,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntegerSemantics {
    /// Byte stores truncate; predictor intermediates are mathematical integers.
    ByteRecurrence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultCachePolicy {
    /// Hashing and copying are not justified for these inexpensive primitives.
    Direct,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    FilteredRow,
    ReconstructedPrefix,
    PreviousRow,
    BytesPerPixel,
    History,
    Length,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemorySemantics {
    /// In-place, ascending byte updates; predictor loads see earlier updates.
    PngRow,
    /// Each byte reads the current window, including bytes just expanded.
    ForwardHistory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    /// A completed byte publishes its ordered memory effect and explicit state.
    ByteBoundary,
}

/// A computation's library-independent definition. `kernel` names a device
/// implementation; it is neither a guest ABI nor a host function pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReferenceContract {
    pub computation: Computation,
    pub version: u32,
    pub kernel: &'static str,
    pub integers: IntegerSemantics,
    pub inputs: &'static [Input],
    pub memory: MemorySemantics,
    pub progress: Progress,
    pub result_cache: ResultCachePolicy,
}

impl ReferenceContract {
    pub const fn get(computation: Computation) -> Self {
        Self {
            computation,
            version: 1,
            kernel: match computation {
                Computation::PngSub => "png-sub-u8-v1",
                Computation::PngUp => "png-up-u8-v1",
                Computation::PngAverage => "png-average-u8-v1",
                Computation::PngPaeth => "png-paeth-u8-v1",
                Computation::Lz77 => "lz77-forward-u8-v1",
            },
            integers: IntegerSemantics::ByteRecurrence,
            inputs: match computation {
                Computation::PngSub => &[
                    Input::FilteredRow,
                    Input::ReconstructedPrefix,
                    Input::BytesPerPixel,
                    Input::Length,
                ],
                Computation::PngUp => &[Input::FilteredRow, Input::PreviousRow, Input::Length],
                Computation::PngAverage | Computation::PngPaeth => &[
                    Input::FilteredRow,
                    Input::ReconstructedPrefix,
                    Input::PreviousRow,
                    Input::BytesPerPixel,
                    Input::Length,
                ],
                Computation::Lz77 => &[Input::History, Input::Length],
            },
            memory: match computation {
                Computation::Lz77 => MemorySemantics::ForwardHistory,
                _ => MemorySemantics::PngRow,
            },
            progress: Progress::ByteBoundary,
            result_cache: ResultCachePolicy::Direct,
        }
    }
}

/// x86 encoding order, without an execution-engine or CPU reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Register(u8);

impl Register {
    pub fn new(index: u8) -> Option<Self> {
        (index < 8).then_some(Self(index))
    }

    pub const fn index(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemorySpan {
    pub address: Expression,
    pub length: Expression,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    ReadWrite,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateView {
    pub inputs: Vec<(Input, Expression)>,
    pub output: MemorySpan,
}

impl StateView {
    pub fn input(&self, input: Input) -> Option<&Expression> {
        self.inputs
            .iter()
            .find_map(|(role, value)| (*role == input).then_some(value))
    }
}

/// Parameterized obligations, not declarations that the current guest is safe.
/// A publication bridge must establish every predicate before any side effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Predicate {
    Positive(Expression),
    Equal(Expression, Expression),
    AtMost(Expression, Expression),
    NoWrap(MemorySpan),
    Ordinary {
        span: MemorySpan,
        access: Access,
    },
    Disjoint(MemorySpan, MemorySpan),
    BackwardDistance {
        source: Expression,
        destination: Expression,
        distance: Expression,
    },
    WritesOutsideDependencies(MemorySpan),
    NoInstructionObservers {
        entry: u32,
        end: u32,
    },
}

/// A reference-rule hint. The checker reconstructs the recurrence and verifies
/// the actual decreasing quantity; the hint conveys no execution permission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProofHint {
    ForwardBytes { remaining: Expression },
}

pub(crate) fn byte_view(
    computation: Computation,
    source: Expression,
    destination: Expression,
    remaining: Expression,
) -> Option<StateView> {
    let distance = Expression::binary(Binary::Subtract, destination.clone(), source.clone())?;
    let inputs = match computation {
        Computation::Lz77 => vec![(Input::History, source), (Input::Length, remaining.clone())],
        Computation::PngSub => vec![
            (Input::FilteredRow, destination.clone()),
            (Input::ReconstructedPrefix, source),
            (Input::BytesPerPixel, distance),
            (Input::Length, remaining.clone()),
        ],
        Computation::PngUp => vec![
            (Input::FilteredRow, destination.clone()),
            (Input::PreviousRow, source),
            (Input::Length, remaining.clone()),
        ],
        _ => return None,
    };
    Some(StateView {
        inputs,
        output: MemorySpan {
            address: destination,
            length: remaining,
        },
    })
}

pub(crate) fn row_view(
    computation: Computation,
    previous: Expression,
    left: Expression,
    destination: Expression,
    remaining: Expression,
) -> Option<StateView> {
    if !matches!(computation, Computation::PngAverage | Computation::PngPaeth) {
        return None;
    }
    let distance = Expression::binary(Binary::Subtract, destination.clone(), left.clone())?;
    Some(StateView {
        inputs: vec![
            (Input::FilteredRow, destination.clone()),
            (Input::ReconstructedPrefix, left),
            (Input::PreviousRow, previous),
            (Input::BytesPerPixel, distance),
            (Input::Length, remaining.clone()),
        ],
        output: MemorySpan {
            address: destination,
            length: remaining,
        },
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DependencyKind {
    Code,
    Constant,
}

/// Module-relative identity dependency, including all bytes, never just a prefix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependency {
    pub offset: u32,
    pub bytes: Arc<[u8]>,
    pub kind: DependencyKind,
}

/// Owned, immutable preparation input. No live guest pointers cross this boundary.
#[derive(Clone, Debug)]
pub struct ImageSnapshot {
    identity: [u8; 32],
    dependencies: Vec<Dependency>,
}

impl ImageSnapshot {
    pub fn new(dependencies: Vec<Dependency>) -> Self {
        let mut hash = Sha256::new();
        for dependency in &dependencies {
            hash.update(dependency.offset.to_le_bytes());
            hash.update([match dependency.kind {
                DependencyKind::Code => 0,
                DependencyKind::Constant => 1,
            }]);
            hash.update((dependency.bytes.len() as u64).to_le_bytes());
            hash.update(&dependency.bytes);
        }
        Self {
            identity: hash.finalize().into(),
            dependencies,
        }
    }

    pub fn identity(&self) -> [u8; 32] {
        self.identity
    }

    pub fn dependencies(&self) -> &[Dependency] {
        &self.dependencies
    }

    pub fn bytes(&self, offset: u32, len: u32) -> Option<&[u8]> {
        self.dependencies.iter().find_map(|dependency| {
            let start = offset.checked_sub(dependency.offset)? as usize;
            let end = start.checked_add(len as usize)?;
            dependency.bytes.get(start..end)
        })
    }
}

/// A proposal, never an executable capability. Every address is relative to
/// `image`; a mapping instance and its lifetime are supplied during publication.
#[derive(Clone, Debug)]
pub struct CandidateBinding {
    pub image: [u8; 32],
    pub contract: ReferenceContract,
    pub entry: u32,
    pub end: u32,
    pub view: StateView,
    pub cutpoints: Vec<u32>,
    pub exits: Vec<u32>,
    pub proof: ProofHint,
}

/// Deterministic preparation budget. Exhaustion is distinct from a counterexample
/// or a form outside the supported proof rules.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub instructions: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rejection {
    Counterexample { offset: u32, reason: &'static str },
    Timeout,
    Unsupported { offset: u32, reason: &'static str },
}

#[cfg(test)]
mod tests;

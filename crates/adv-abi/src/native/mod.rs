//! Admission data for source-assisted native regions.
//!
//! Contracts describe computations without guest addresses. An analyzer proposes
//! a mapping against an immutable image snapshot; only [`verify::verify`] can
//! produce a [`VerifiedBinding`]. Neither recognition nor differential samples
//! constitute admission. These types contain no execution or device services.

pub mod analyze;
pub mod verify;

use sha2::{Digest, Sha256};
use std::sync::Arc;

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

/// Restricted state view for a one-byte forward loop. Offset addition uses
/// x86 wrapping arithmetic; entry conditions constrain the resulting ranges.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ByteLoopMapping {
    pub source: Register,
    pub destination: Register,
    pub remaining: Register,
    pub source_offset: u32,
    pub destination_offset: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aliasing {
    /// A positive backward distance; later reads can consume earlier stores.
    ForwardHistory,
    /// The previous PNG row cannot overlap the row being reconstructed.
    Disjoint,
}

/// Conditions the eventual execution bridge must establish before any effect.
/// Zero remaining bytes decline: this region is a positive-count loop body,
/// while its enclosing guest function owns the zero-iteration branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryConditions {
    pub positive_count: bool,
    pub nonwrapping_ranges: bool,
    pub ordinary_readable_source: bool,
    pub ordinary_readable_writable_destination: bool,
    pub destination_disjoint_dependencies: bool,
    pub no_instruction_observers: bool,
    pub aliasing: Aliasing,
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
    pub mapping: ByteLoopMapping,
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

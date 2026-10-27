//! Algorithm-independent lowering data for an admitted byte region.
//! Preparation supplies a proved recurrence; engines specialize its affine
//! inputs and cursor updates into code, then run one original scalar iteration.

/// A modulo-2^32 sum of guest registers and a constant. Coefficients are
/// limited to -1, 0 and 1 by admission, so bridges need only adds/subtracts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Affine {
    pub registers: [i8; 8],
    pub constant: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aliasing {
    Disjoint,
    Backward,
}

#[derive(Clone, Copy, Debug)]
pub struct ReadSpan {
    pub address: Affine,
    pub aliasing: Aliasing,
}

/// A fixed scalar argument with explicit pointer bounds for prefix arithmetic.
#[derive(Clone, Copy, Debug)]
pub struct Scalar {
    pub value: Affine,
    pub upper_bounds: [Option<Affine>; 2],
}

#[derive(Clone, Copy, Debug)]
pub struct FrameInvariant {
    pub offset: u32,
    pub value: Affine,
}

/// Fixed private frame effects restored by the checked scalar tail.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub address: Affine,
    pub bytes: u32,
    pub writes: bool,
    pub invariants: [Option<FrameInvariant>; 8],
}

/// Host ABI: source, destination, byte count, scalar. All accesses are prevalidated;
/// the routine must not allocate, unwind, call guest code or touch CPU state.
pub type Kernel = unsafe extern "C" fn(*const u8, *mut u8, u32, u32);

#[derive(Clone, Copy, Debug)]
pub struct NativeRegion {
    inputs: [Affine; 3],
    advances: [i8; 8],
    instructions: u32,
    reads: [Option<ReadSpan>; 3],
    scalar: Option<Scalar>,
    frame: Option<Frame>,
    kernel: Kernel,
}

impl NativeRegion {
    /// # Safety
    /// The offered code must implement the kernel's ordered byte recurrence.
    /// Inputs are source, destination and remaining bytes. Every replaced
    /// iteration advances the specified registers; other registers and flags
    /// must be restored by one non-faulting original scalar iteration. All
    /// memory effects must be confined to the output and declared read spans,
    /// each of the remaining byte count, and the declared fixed frame. All
    /// mutable slots must be defined before reading prior history and restored
    /// exactly by the non-faulting tail. Invariant slots must retain their
    /// checked values across the backedge. There are no constants
    /// or external effects beyond the offered code and these buffers.
    /// Coefficients and advances must be -1, 0 or 1; ESP must not advance.
    pub unsafe fn new(
        inputs: [Affine; 3],
        advances: [i8; 8],
        instructions: u32,
        reads: [Option<ReadSpan>; 3],
        scalar: Option<Scalar>,
        frame: Option<Frame>,
        kernel: Kernel,
    ) -> Self {
        Self {
            inputs,
            advances,
            instructions,
            reads,
            scalar,
            frame,
            kernel,
        }
    }

    pub fn inputs(&self) -> &[Affine; 3] {
        &self.inputs
    }
    pub fn advances(&self) -> &[i8; 8] {
        &self.advances
    }
    pub fn instructions(&self) -> u32 {
        self.instructions
    }
    pub fn reads(&self) -> &[Option<ReadSpan>; 3] {
        &self.reads
    }
    pub fn scalar(&self) -> Option<Scalar> {
        self.scalar
    }
    pub fn frame(&self) -> Option<Frame> {
        self.frame
    }
    pub fn kernel(&self) -> Kernel {
        self.kernel
    }
}

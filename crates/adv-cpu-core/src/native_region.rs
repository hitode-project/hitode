//! Algorithm-independent lowering data for an admitted byte region.
//! Preparation supplies a proved recurrence; engines specialize its affine
//! inputs and cursor updates into code, then run one original scalar iteration.

/// A modulo-2^32 sum of guest registers and a constant. Coefficients are
/// limited to -1, 0 and 1 by admission, so bridges need only adds/subtracts.
#[derive(Clone, Copy, Debug)]
pub struct Affine {
    pub registers: [i8; 8],
    pub constant: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aliasing {
    Disjoint,
    Backward,
}

/// Host ABI: source, destination, byte count. All accesses are prevalidated;
/// the routine must not allocate, unwind, call guest code or touch CPU state.
pub type Kernel = unsafe extern "C" fn(*const u8, *mut u8, u32);

#[derive(Clone, Copy, Debug)]
pub struct NativeRegion {
    inputs: [Affine; 3],
    advances: [i8; 8],
    instructions: u32,
    aliasing: Aliasing,
    kernel: Kernel,
}

impl NativeRegion {
    /// # Safety
    /// The offered code must implement the kernel's ordered byte recurrence.
    /// Inputs are source, destination and remaining bytes. Every replaced
    /// iteration advances the specified registers; other registers and flags
    /// must be restored by one non-faulting original scalar iteration. All
    /// memory effects must be confined to these byte ranges, with no constants
    /// or external effects beyond the offered code and these buffers.
    /// Coefficients and advances must be -1, 0 or 1; ESP/EBP must not advance.
    pub unsafe fn new(
        inputs: [Affine; 3],
        advances: [i8; 8],
        instructions: u32,
        aliasing: Aliasing,
        kernel: Kernel,
    ) -> Self {
        Self {
            inputs,
            advances,
            instructions,
            aliasing,
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
    pub fn aliasing(&self) -> Aliasing {
        self.aliasing
    }
    pub fn kernel(&self) -> Kernel {
        self.kernel
    }
}

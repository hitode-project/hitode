//! Wine 11.0's FAudio reverb DSP, owned by one device graph.
use std::ffi::c_void;

unsafe extern "C" {
    fn adv_reverb_create(rate: u32, input: u32, output: u32) -> *mut c_void;
    fn adv_reverb_parameters(reverb: *mut c_void, parameters: *const u8, size: u32);
    fn adv_reverb_process(reverb: *mut c_void, input: *mut f32, output: *mut f32, frames: u32);
    fn adv_reverb_destroy(reverb: *mut c_void);
}
pub(super) struct Reverb(*mut c_void);
// The audio ownership boundary moves the entire graph, never shares its DSP.
unsafe impl Send for Reverb {}
impl Reverb {
    pub fn new(rate: u32, input: usize, output: usize) -> Self {
        // The C network has five reverb channels and a 300 ms delay allocation.
        assert!(
            rate >= 4 && matches!((input, output), (1, 1 | 6) | (2, 2 | 6) | (6, 6)),
            "reverb DSP layout"
        );
        // SAFETY: the C constructor allocates a private DSP instance.
        Self(unsafe { adv_reverb_create(rate, input as u32, output as u32) })
    }
    pub fn parameters(&mut self, parameters: &[u8]) {
        // SAFETY: the bridge copies only the recognized packed parameter sizes.
        unsafe { adv_reverb_parameters(self.0, parameters.as_ptr(), parameters.len() as u32) }
    }
    pub fn process(
        &mut self,
        input: &mut [f32],
        input_channels: usize,
        output_channels: usize,
    ) -> Vec<f32> {
        let frames = input.len() / input_channels;
        let mut output = vec![0.0; frames * output_channels];
        // SAFETY: input/output cover the DSP's negotiated frames and channels.
        unsafe {
            adv_reverb_process(
                self.0,
                input.as_mut_ptr(),
                output.as_mut_ptr(),
                frames as u32,
            )
        };
        output
    }
}
impl Drop for Reverb {
    fn drop(&mut self) {
        // SAFETY: this owner releases the instance exactly once.
        unsafe { adv_reverb_destroy(self.0) }
    }
}

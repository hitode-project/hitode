//! Explicit regions make short authored guests exercise compiled execution too.
use adv_cpu::{JitCpu, Region, backend::Backend};
use adv_unicorn::UnicornCpu;

pub trait TestCpu: Backend {
    fn code_region(&mut self, _start: u32, _length: usize) {}
}
impl TestCpu for UnicornCpu {}
impl TestCpu for JitCpu {
    fn code_region(&mut self, start: u32, length: usize) {
        self.executor_mut()
            .add_region(Region::new(start, length))
            .unwrap();
    }
}

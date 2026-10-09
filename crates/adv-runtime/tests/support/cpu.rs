//! Dev-only runtime backend selection. `unicorn` always owns a real UnicornCpu.
#![allow(dead_code)]
use adv_cpu::backend::{Backend, Exit, Guest, MemFault, Prot, Reg, SegmentSetup, TrapHandler};
use adv_cpu::{ArchitecturalState, CompileStrategy, JitCpu, StateError, Stats};
use adv_unicorn::UnicornCpu;
use std::time::Duration;

pub enum Cpu {
    Jit(Box<JitCpu>),
    Unicorn(UnicornCpu),
}
pub enum Context {
    Jit(Box<<JitCpu as Backend>::Context>),
    Unicorn(<UnicornCpu as Backend>::Context),
}

impl Cpu {
    pub fn new(name: &str) -> Result<Self, String> {
        let park = adv_machine::layout::PARK_ADDR;
        match name {
            "jit" => Ok(Self::Jit(Box::new(JitCpu::new(park, true)?))),
            "interpreter" => Ok(Self::Jit(Box::new(JitCpu::new(park, false)?))),
            "unicorn" => Ok(Self::Unicorn(
                UnicornCpu::new(park).map_err(|e| e.to_string())?,
            )),
            _ => Err("backend must be jit, interpreter, or unicorn".into()),
        }
    }
    pub fn jit_mut(&mut self) -> Option<&mut JitCpu> {
        match self {
            Self::Jit(cpu) => Some(cpu),
            _ => None,
        }
    }
    pub fn stats(&self) -> Stats {
        match self {
            Self::Jit(cpu) => cpu.stats(),
            _ => Stats::default(),
        }
    }
    pub fn profile(&self) -> std::collections::BTreeMap<u32, adv_cpu::ProfileSample> {
        match self {
            Self::Jit(cpu) => cpu.profile(),
            Self::Unicorn(_) => std::collections::BTreeMap::new(),
        }
    }
    pub fn set_profiling(&mut self, enabled: bool) {
        match self {
            Self::Jit(cpu) => cpu.set_profiling(enabled),
            Self::Unicorn(cpu) => cpu.set_block_profile(enabled),
        }
    }
    pub fn set_auto_compile_strategy(&mut self, strategy: CompileStrategy) {
        if let Self::Jit(cpu) = self {
            cpu.set_auto_compile_strategy(strategy);
        }
    }
    pub fn set_region_linking(&mut self, enabled: bool) {
        if let Self::Jit(cpu) = self {
            cpu.set_region_linking(enabled);
        }
    }
    pub fn set_async_compilation(&mut self, enabled: bool) {
        if let Self::Jit(cpu) = self {
            cpu.set_async_compilation(enabled);
        }
    }
    fn guest(&self) -> &dyn Guest {
        match self {
            Self::Jit(cpu) => cpu.as_ref(),
            Self::Unicorn(cpu) => cpu,
        }
    }
    fn guest_mut(&mut self) -> &mut dyn Guest {
        match self {
            Self::Jit(cpu) => cpu.as_mut(),
            Self::Unicorn(cpu) => cpu,
        }
    }
}
impl Guest for Cpu {
    fn wants_code_images(&self) -> bool {
        self.guest().wants_code_images()
    }
    fn register_code_image(&mut self, image: adv_cpu::AotImage) {
        self.guest_mut().register_code_image(image);
    }
    fn capture_architecture(&mut self) -> Result<ArchitecturalState, StateError> {
        self.guest_mut().capture_architecture()
    }
    fn restore_architecture(&mut self, state: &ArchitecturalState) -> Result<(), StateError> {
        self.guest_mut().restore_architecture(state)
    }
    fn last_indirect_branch(&self) -> Option<adv_cpu::IndirectBranch> {
        self.guest().last_indirect_branch()
    }
    fn file_page_copy_on_write(&self, addr: u32) -> Option<bool> {
        self.guest().file_page_copy_on_write(addr)
    }
    fn map_private_copy_pages(
        &mut self,
        addr: u32,
        bytes: &[u8],
        prot: Prot,
    ) -> Result<(), MemFault> {
        self.guest_mut().map_private_copy_pages(addr, bytes, prot)
    }
    fn privatize_copy_pages(&mut self, addr: u32, len: u32) -> Result<(), MemFault> {
        self.guest_mut().privatize_copy_pages(addr, len)
    }
    fn map_file_pages(
        &mut self,
        addr: u32,
        pages: &[std::rc::Rc<adv_cpu::backend::FilePage>],
        prot: Prot,
        copy_on_write: bool,
    ) -> Result<(), MemFault> {
        self.guest_mut()
            .map_file_pages(addr, pages, prot, copy_on_write)
    }
    fn cpuid_features(&self) -> u32 {
        self.guest().cpuid_features()
    }
    fn push_f64(&mut self, value: f64) {
        self.guest_mut().push_f64(value);
    }
    fn pop_f64(&mut self) -> Option<f64> {
        self.guest_mut().pop_f64()
    }
    fn reg(&self, reg: Reg) -> u32 {
        self.guest().reg(reg)
    }
    fn set_reg(&mut self, reg: Reg, value: u32) {
        self.guest_mut().set_reg(reg, value);
    }
    fn read(&self, address: u32, bytes: &mut [u8]) -> Result<(), MemFault> {
        self.guest().read(address, bytes)
    }
    fn write(&mut self, address: u32, bytes: &[u8]) -> Result<(), MemFault> {
        self.guest_mut().write(address, bytes)
    }
    fn shared_bytes(&self, address: u32, len: usize) -> Option<&[u8]> {
        self.guest().shared_bytes(address, len)
    }
    fn copy_memory(&mut self, src: u32, dst: u32, len: usize) -> Result<(), MemFault> {
        self.guest_mut().copy_memory(src, dst, len)
    }
    fn accessible(&self, address: u32, len: u32, prot: Prot) -> bool {
        self.guest().accessible(address, len, prot)
    }
    fn next_mapped_page(&self, address: u32, limit: u64) -> Result<Option<u32>, MemFault> {
        self.guest().next_mapped_page(address, limit)
    }
    fn map(&mut self, address: u32, size: u32, prot: Prot) -> Result<(), MemFault> {
        self.guest_mut().map(address, size, prot)
    }
    fn protect(&mut self, address: u32, size: u32, prot: Prot) -> Result<(), MemFault> {
        self.guest_mut().protect(address, size, prot)
    }
    fn unmap(&mut self, address: u32, size: u32) -> Result<(), MemFault> {
        self.guest_mut().unmap(address, size)
    }
}
macro_rules! backend {
    ($self:expr, $method:ident $(, $argument:expr)*) => {
        match $self { Cpu::Jit(cpu) => cpu.$method($($argument),*), Cpu::Unicorn(cpu) => cpu.$method($($argument),*) }
    };
}
impl Backend for Cpu {
    fn new_process(&mut self) -> Result<Self, String> {
        match self {
            Self::Jit(cpu) => Ok(Self::Jit(Box::new(cpu.new_process()?))),
            Self::Unicorn(cpu) => Ok(Self::Unicorn(cpu.new_process()?)),
        }
    }

    type Context = Context;
    fn setup_segments(&mut self, setup: SegmentSetup) -> Result<(), MemFault> {
        backend!(self, setup_segments, setup)
    }
    fn set_fs_base(&mut self, base: u32) -> Result<(), MemFault> {
        backend!(self, set_fs_base, base)
    }
    fn park_addr(&self) -> u32 {
        backend!(self, park_addr)
    }
    fn new_context(&self) -> Context {
        match self {
            Self::Jit(cpu) => Context::Jit(Box::new(cpu.new_context())),
            Self::Unicorn(cpu) => Context::Unicorn(cpu.new_context()),
        }
    }
    fn save(&self, context: &mut Context) {
        match (self, context) {
            (Self::Jit(cpu), Context::Jit(ctx)) => cpu.save(ctx),
            (Self::Unicorn(cpu), Context::Unicorn(ctx)) => cpu.save(ctx),
            _ => panic!("backend context mismatch"),
        }
    }
    fn restore(&mut self, context: &Context) {
        match (self, context) {
            (Self::Jit(cpu), Context::Jit(ctx)) => cpu.restore(ctx),
            (Self::Unicorn(cpu), Context::Unicorn(ctx)) => cpu.restore(ctx),
            _ => panic!("backend context mismatch"),
        }
    }
    fn set_execution_budget(&mut self, budget: Option<Duration>) {
        backend!(self, set_execution_budget, budget);
    }
    fn execution_budget(&self) -> Option<Duration> {
        backend!(self, execution_budget)
    }
    fn run(&mut self, handler: &mut dyn TrapHandler) -> Exit {
        backend!(self, run, handler)
    }
    fn invalidate_code(&mut self, address: u32, len: u32) {
        backend!(self, invalidate_code, address, len);
    }
    fn set_block_counting(&mut self, on: bool) {
        backend!(self, set_block_counting, on);
    }
    fn executed_blocks(&self) -> Option<(u64, u64)> {
        backend!(self, executed_blocks)
    }
    fn set_block_profile(&mut self, on: bool) {
        backend!(self, set_block_profile, on);
    }
    fn block_profile(&self) -> Vec<(u32, u64, u32)> {
        backend!(self, block_profile)
    }
}

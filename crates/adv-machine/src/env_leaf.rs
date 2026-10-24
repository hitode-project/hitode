//! Full service access with only the CPU state synchronized by the leaf stub.
use crate::Env;
use adv_cpu::backend::{Guest, MemFault, Prot, Reg};

/// None declines before any observable service or memory side effect.
pub type EnvLeafHandler = fn(&mut Env<'_>) -> Option<u64>;

pub(crate) struct Cpu<'a>(pub &'a mut dyn Guest);

fn register(r: Reg) {
    if !matches!(r, Reg::Esp | Reg::Edx) {
        if cfg!(debug_assertions) {
            panic!("EnvLeaf accessed unsynchronized register {r:?}");
        }
        static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            log::warn!("EnvLeaf accessed unsynchronized register {r:?}");
        }
    }
}

impl Guest for Cpu<'_> {
    fn reg(&self, r: Reg) -> u32 {
        register(r);
        self.0.reg(r)
    }
    fn set_reg(&mut self, r: Reg, v: u32) {
        assert!(
            matches!(r, Reg::Esp | Reg::Edx),
            "EnvLeaf register write {r:?}"
        );
        self.0.set_reg(r, v);
    }
    fn read(&self, addr: u32, buf: &mut [u8]) -> Result<(), MemFault> {
        self.0.read(addr, buf)
    }
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), MemFault> {
        self.0.write(addr, data)
    }
    fn accessible(&self, addr: u32, len: u32, prot: Prot) -> bool {
        self.0.accessible(addr, len, prot)
    }
    fn shared_bytes(&self, addr: u32, len: usize) -> Option<&[u8]> {
        self.0.shared_bytes(addr, len)
    }
    fn file_page_copy_on_write(&self, addr: u32) -> Option<bool> {
        self.0.file_page_copy_on_write(addr)
    }
    fn private_copy_epoch(&self) -> Option<u64> {
        self.0.private_copy_epoch()
    }
    fn copy_memory(&mut self, src: u32, dst: u32, len: usize) -> Result<(), MemFault> {
        self.0.copy_memory(src, dst, len)
    }
    fn fill_memory(&mut self, addr: u32, byte: u8, len: usize) -> Result<(), MemFault> {
        self.0.fill_memory(addr, byte, len)
    }
    fn next_mapped_page(&self, addr: u32, limit: u64) -> Result<Option<u32>, MemFault> {
        self.0.next_mapped_page(addr, limit)
    }
    fn has_locked_pages(&self, addr: u32, len: u32) -> bool {
        self.0.has_locked_pages(addr, len)
    }
    fn map(&mut self, _: u32, _: u32, _: Prot) -> Result<(), MemFault> {
        panic!("EnvLeaf mapping change")
    }
    fn protect(&mut self, _: u32, _: u32, _: Prot) -> Result<(), MemFault> {
        panic!("EnvLeaf mapping change")
    }
    fn unmap(&mut self, _: u32, _: u32) -> Result<(), MemFault> {
        panic!("EnvLeaf mapping change")
    }
}

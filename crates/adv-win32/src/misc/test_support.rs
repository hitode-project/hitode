//! Shared authored API fixtures: real USER/GDI owners and a tiny guest canvas.
use crate::{Win32, Win32Config};
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, Reg},
};
use adv_device::surface::{MemoryOperations, MemoryView, Snapshot, SurfaceStore};
use adv_machine::{Clock, Env, Machine, Outcome, layout::PARK_ADDR};
use std::{
    borrow::Cow,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
pub(super) const CODE: u32 = 0x400000;
pub(super) const DATA: u32 = 0x410000;
pub(super) struct Fixture {
    pub m: Machine<JitCpu>,
    pub root: PathBuf,
}
pub(super) fn env(m: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    }
}
fn memory(cpu: &mut dyn Guest) -> MemoryView<'_, dyn Guest + '_> {
    MemoryView::new(
        cpu,
        MemoryOperations {
            read: |cpu, p, n| {
                let mut b = vec![0; n];
                cpu.read(p, &mut b).map_err(|e| e.to_string())?;
                Ok(Cow::Owned(b))
            },
            write: |cpu, p, b| cpu.write(p, b).map_err(|e| e.to_string()),
            writable: |cpu, p, n| {
                adv_abi::guest::check_range(cpu, p, n as u32, Prot::WRITE)
                    .map_err(|e| e.to_string())
            },
            copy: |cpu, s, d, n| cpu.copy_memory(s, d, n).map_err(|e| e.to_string()),
        },
    )
}
impl Fixture {
    pub fn new(name: &str) -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/scratch/oretsuba-tests")
            .join(format!(
                "{name}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(root.join("game")).unwrap();
        let mut registry = Registry::default();
        crate::register(&mut registry);
        assert!(registry.signature_conflicts().is_empty());
        let mut m = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        Win32::install(
            &mut m,
            Win32Config::new(root.join("game"), "authored.exe", root.join("data")),
        )
        .unwrap();
        m.cpu.map(CODE, 4096, Prot::RX).unwrap();
        m.cpu.map(DATA, 65536, Prot::RW).unwrap();
        m.spawn("authored-api", CODE, &[], 65536).unwrap();
        m.cpu.set_reg(Reg::Esp, DATA + 0xff00);
        let arena = m.sys.vm.alloc(&mut m.cpu, 65536, Prot::RW).unwrap();
        m.ext
            .insert(SurfaceStore::new(8, 8, arena, 65536, Snapshot::default()).unwrap());
        crate::gdi32::raster::front::set_provider(
            &mut m.ext,
            crate::gdi32::raster::front::DrawingProvider {
                memory,
                completed: |_, _| None,
            },
        );
        Self { m, root }
    }
    pub fn invoke(&mut self, module: &str, name: &str, args: &[u32]) -> Outcome {
        self.m.resolve_import(module, name).unwrap();
        let index = self.m.sys.thunks.published_index(module, name).unwrap();
        let handler = self.m.sys.thunks.get(index).f.handler;
        self.m.cpu.set_reg(Reg::Esp, DATA + 0xff00);
        for (i, arg) in args.iter().enumerate() {
            self.m
                .cpu
                .write_u32(DATA + 0xff04 + i as u32 * 4, *arg)
                .unwrap();
        }
        handler(&mut Env {
            context: self.m.sys.current,
            cpu: &mut self.m.cpu,
            sys: &mut self.m.sys,
            ext: &mut self.m.ext,
            thunk: index,
        })
    }
    pub fn call(&mut self, module: &str, name: &str, args: &[u32]) -> u32 {
        match self.invoke(module, name, args) {
            Outcome::Return(v) => v,
            _ => panic!("{module}!{name} must return synchronously"),
        }
    }
    pub fn cleanup(self) {
        let root = self.root.clone();
        drop(self);
        std::fs::remove_dir_all(root).unwrap();
    }
}

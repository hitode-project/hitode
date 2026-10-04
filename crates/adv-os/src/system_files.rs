//! Immutable synthetic System32 files, independent of module load references.
use crate::loader::{LoadError, is_host_module, is_system_path, system_image};
use adv_cpu::backend::Guest;
use adv_machine::{Sys, layout::HOST_MODULE_BASE};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Default)]
pub struct SystemFiles {
    images: BTreeMap<String, Arc<[u8]>>,
}
impl SystemFiles {
    /// A file probe never loads a module, changes a reference count or runs DLL
    /// initialization. Trap bindings are published without recording imports.
    pub fn open(
        &mut self,
        cpu: &mut dyn Guest,
        sys: &mut Sys,
        path: &str,
    ) -> Result<Option<Arc<[u8]>>, LoadError> {
        if !is_system_path(path) {
            return Ok(None);
        }
        let key = crate::module_key(path);
        if let Some(image) = self.images.get(&key) {
            return Ok(Some(image.clone()));
        }
        if !is_host_module(sys, &key) {
            return Ok(None);
        }
        let image: Arc<[u8]> = system_image(cpu, sys, &key, HOST_MODULE_BASE)?.bytes.into();
        self.images.insert(key, image.clone());
        Ok(Some(image))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileSource, Process};
    use adv_cpu::JitCpu;
    use adv_machine::{Clock, Machine, layout::PARK_ADDR};
    struct Empty;
    impl FileSource for Empty {
        fn read(&self, _: &str) -> Option<Vec<u8>> {
            None
        }
    }

    #[test]
    fn immutable_file_probes_do_not_create_or_reference_loaded_modules() {
        let mut m = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            adv_abi::Registry::default(),
            Clock::virtual_time(),
        )
        .unwrap();
        let mut files = SystemFiles::default();
        let path = "C:\\Windows\\system32\\version.dll";
        let first = files.open(&mut m.cpu, &mut m.sys, path).unwrap().unwrap();
        let alias = files
            .open(&mut m.cpu, &mut m.sys, "c:/windows/SYSTEM32/VERSION.DLL")
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&first, &alias));
        assert!(crate::try_modules(&m.ext).is_none());
        let id = m.load(&Empty, path).unwrap();
        let module = crate::modules(&m.ext).get(id);
        let base = module.base;
        assert_eq!(module.load_count, 1);
        m.cpu.write(base, b"XX").unwrap();
        assert_eq!(
            &files.open(&mut m.cpu, &mut m.sys, path).unwrap().unwrap()[..2],
            b"MZ"
        );
        assert_eq!(crate::modules(&m.ext).get(id).load_count, 1);
        assert!(
            files
                .open(
                    &mut m.cpu,
                    &mut m.sys,
                    "C:\\Windows\\system32\\never-provided.dll"
                )
                .unwrap()
                .is_none()
        );
        assert!(
            m.load(&Empty, "C:\\Windows\\system32\\never-provided.dll")
                .is_err()
        );
    }
}

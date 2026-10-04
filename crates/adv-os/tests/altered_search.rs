//! Authored PE dependency graph; no local sample files or native execution.
use adv_abi::Registry;
use adv_cpu::{JitCpu, backend::GuestExt};
use adv_machine::{Clock, Machine, layout::PARK_ADDR};
use adv_os::{FileSource, Loader, Modules};
use std::collections::BTreeMap;

#[path = "../../adv-pe/tests/support/delay_fixture.rs"]
mod pe_fixture;

struct Files(BTreeMap<String, Vec<u8>>);
impl FileSource for Files {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        self.0.get(path).cloned()
    }
}
fn root() -> Vec<u8> {
    let mut image = pe_fixture::image(true);
    pe_fixture::word(&mut image, 0x98 + 96 + 13 * 8, 0);
    pe_fixture::word(&mut image, 0x98 + 96 + 8, 0x400);
    pe_fixture::word(&mut image, 0x98 + 100 + 8, 40);
    image[0x400..0x440].fill(0);
    image[0x500..0x50c].fill(0);
    for (at, value) in [
        (0x400, 0x500),
        (0x40c, 0x700),
        (0x410, 0x540),
        (0x500, 0x600),
    ] {
        pe_fixture::word(&mut image, at, value);
    }
    image[0x600..0x610].fill(0);
    image[0x602..0x608].copy_from_slice(b"Value\0");
    image[0x700..0x714].fill(0);
    image[0x700..0x70e].copy_from_slice(b"owned_dep.dll\0");
    image
}
fn dependency(value: u8) -> Vec<u8> {
    adv_os::synthetic::build(
        0x800000,
        "owned_dep.dll",
        &[adv_os::synthetic::Export {
            name: Some("Value".into()),
            ordinal: Some(1),
            target: adv_os::synthetic::Target::Code(vec![0xb8, value, 0, 0, 0, 0xc3]),
        }],
    )
    .unwrap()
    .bytes
}
fn fixture() -> Machine<JitCpu> {
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        Registry::default(),
        Clock::virtual_time(),
    )
    .unwrap();
    machine.ext.insert(Modules::default());
    machine
}

#[test]
fn absolute_altered_load_prefers_root_directory_and_keeps_existing_module_identity() {
    let files = Files(BTreeMap::from([
        ("C:\\plugins\\root.dll".into(), root()),
        ("owned_dep.dll".into(), dependency(11)),
        ("C:\\plugins\\owned_dep.dll".into(), dependency(22)),
    ]));
    for altered in [false, true] {
        let mut machine = fixture();
        let mut loader = Loader::new(
            &mut machine.cpu,
            &mut machine.sys,
            machine.ext.get_mut::<Modules>(),
            &files,
        );
        let root = if altered {
            loader.load_altered("C:\\plugins\\root.dll")
        } else {
            loader.load("C:\\plugins\\root.dll")
        }
        .unwrap();
        let dep = loader.modules.by_name("owned_dep.dll").unwrap();
        assert_eq!(
            loader.modules.get(dep).path,
            if altered {
                "C:\\plugins\\owned_dep.dll"
            } else {
                "owned_dep.dll"
            }
        );
        let value = &loader.modules.get(dep).exports_by_name["Value"];
        let adv_pe::ExportTarget::Rva(rva) = value else {
            panic!("code export")
        };
        assert_eq!(
            loader
                .cpu
                .read_u32(loader.modules.get(root).base + 0x540)
                .unwrap(),
            loader.modules.get(dep).base + rva
        );
        assert_eq!(loader.modules.get(dep).lifetime.external_refs, 0);
        assert_eq!(loader.load_altered("C:\\plugins\\root.dll").unwrap(), root);
        assert_eq!(loader.modules.get(root).lifetime.external_refs, 2);
        assert_eq!(loader.modules.iter().count(), 2);
    }
}

#[test]
fn malformed_altered_dependency_rolls_back_and_does_not_leak_search_policy() {
    let files = Files(BTreeMap::from([
        ("C:\\plugins\\root.dll".into(), root()),
        ("owned_dep.dll".into(), dependency(11)),
        ("C:\\plugins\\owned_dep.dll".into(), vec![1, 2, 3]),
    ]));
    let mut machine = fixture();
    let mut loader = Loader::new(
        &mut machine.cpu,
        &mut machine.sys,
        machine.ext.get_mut::<Modules>(),
        &files,
    );
    assert!(loader.load_altered("C:\\plugins\\root.dll").is_err());
    assert_eq!(loader.modules.iter().count(), 0);
    assert!(loader.inits.is_empty());
    assert!(
        !loader
            .cpu
            .accessible(0x400000, 4096, adv_cpu::backend::Prot::READ)
    );
    // Reuse this same Loader after failure: ordinary lookup must not inherit
    // the temporary plugins-first policy.
    let id = loader.load("C:\\plugins\\root.dll").unwrap();
    assert_eq!(loader.modules.get(id).lifetime.external_refs, 1);
    let dep = loader.modules.by_name("owned_dep.dll").unwrap();
    assert_eq!(loader.modules.get(dep).path, "owned_dep.dll");
}

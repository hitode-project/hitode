//! Authored PE dependency graph; no local sample files or native execution.
use adv_abi::Registry;
use adv_cpu::{JitCpu, backend::GuestExt};
use adv_machine::{Clock, Machine, layout::PARK_ADDR};
use adv_os::{FileSource, LoadError, Loader, Modules};
use std::cell::{Cell, RefCell};
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

#[test]
fn absolute_path_to_a_relatively_loaded_image_reuses_the_module() {
    let files = Files(BTreeMap::from([
        ("dll\\owned_dep.dll".into(), dependency(11)),
        ("C:\\Game\\dll\\owned_dep.dll".into(), dependency(11)),
    ]));
    let mut machine = fixture();
    let mut loader = Loader::new(
        &mut machine.cpu,
        &mut machine.sys,
        machine.ext.get_mut::<Modules>(),
        &files,
    );
    let first = loader.load("dll\\owned_dep.dll").unwrap();
    for path in ["C:\\Game\\dll\\owned_dep.dll", "c:/game/DLL/owned_dep.dll"] {
        assert_eq!(loader.load_altered(path).unwrap(), first);
    }
    assert_eq!(loader.load("C:\\Game\\dll\\owned_dep.dll").unwrap(), first);
    assert_eq!(loader.modules.iter().count(), 1);
    assert_eq!(loader.modules.get(first).lifetime.external_refs, 4);
}

struct DeferredFiles {
    files: Files,
    deferred: String,
    ready: Cell<bool>,
    reads: RefCell<Vec<String>>,
}
impl FileSource for DeferredFiles {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        self.files.read(path)
    }
    fn read_for_loader(&self, path: &str) -> Result<Option<Vec<u8>>, LoadError> {
        self.reads.borrow_mut().push(path.to_owned());
        if path == self.deferred && !self.ready.get() {
            Err(LoadError::DeferredRead(path.to_owned()))
        } else {
            Ok(self.read(path))
        }
    }
}

#[test]
fn deferred_dependency_rolls_back_images_and_initializers_before_retry() {
    let mut image = root();
    pe_fixture::word(&mut image, 0x98 + 100 + 8, 60);
    image[0x700..0x714].fill(0);
    image[0x700..0x70e].copy_from_slice(b"ready_dep.dll\0");
    for (at, value) in [
        (0x414, 0x510),
        (0x420, 0x720),
        (0x424, 0x550),
        (0x510, 0x600),
    ] {
        pe_fixture::word(&mut image, at, value);
    }
    image[0x720..0x72d].copy_from_slice(b"late_dep.dll\0");
    let mut first = dependency(11);
    pe_fixture::word(&mut first, 0x98 + 16, 0x2008);
    let mut second = dependency(22);
    pe_fixture::word(&mut second, 0x98 + 28, 0x900000);
    let files = DeferredFiles {
        files: Files(BTreeMap::from([
            ("root.dll".into(), image),
            ("ready_dep.dll".into(), first),
            ("late_dep.dll".into(), second),
        ])),
        deferred: "late_dep.dll".into(),
        ready: Cell::new(false),
        reads: RefCell::new(Vec::new()),
    };
    let mut machine = fixture();
    let mut loader = Loader::new(
        &mut machine.cpu,
        &mut machine.sys,
        machine.ext.get_mut::<Modules>(),
        &files,
    );
    assert!(
        matches!(loader.load("root.dll"), Err(LoadError::DeferredRead(path)) if path == "late_dep.dll")
    );
    assert_eq!(
        files.reads.borrow().as_slice(),
        ["root.dll", "ready_dep.dll", "late_dep.dll"]
    );
    assert_eq!(loader.modules.iter().count(), 0);
    assert!(loader.inits.is_empty());
    for base in [0x400000, 0x800000] {
        assert!(loader.sys.vm.region(base).is_none());
        assert!(
            !loader
                .cpu
                .accessible(base, 4096, adv_cpu::backend::Prot::READ)
        );
    }
    files.ready.set(true);
    let root = loader.load("root.dll").unwrap();
    assert_eq!(loader.modules.iter().count(), 3);
    assert_eq!(loader.modules.get(root).lifetime.external_refs, 1);
    assert_eq!(loader.inits.len(), 1);
    for (name, slot) in [("ready_dep.dll", 0x540), ("late_dep.dll", 0x550)] {
        let dependency = loader.modules.by_name(name).unwrap();
        let adv_pe::ExportTarget::Rva(rva) =
            loader.modules.get(dependency).exports_by_name["Value"]
        else {
            panic!("code export")
        };
        assert_eq!(
            loader
                .cpu
                .read_u32(loader.modules.get(root).base + slot)
                .unwrap(),
            loader.modules.get(dependency).base + rva
        );
    }
    let reads = files.reads.borrow().len();
    assert_eq!(loader.load("root.dll").unwrap(), root);
    assert_eq!(files.reads.borrow().len(), reads);
}

#[test]
fn deferred_reads_cover_each_search_candidate_and_preserve_plain_probes() {
    for (candidate, altered, expected) in [
        ("owned_dep.dll", false, vec!["owned_dep.dll"]),
        (
            "C:\\plugins\\owned_dep.dll",
            false,
            vec!["owned_dep.dll", "C:\\plugins\\owned_dep.dll"],
        ),
        (
            "dll\\owned_dep.dll",
            false,
            vec![
                "owned_dep.dll",
                "C:\\plugins\\owned_dep.dll",
                "dll\\owned_dep.dll",
            ],
        ),
        (
            "C:\\plugins\\owned_dep.dll",
            true,
            vec!["C:\\plugins\\owned_dep.dll"],
        ),
    ] {
        let files = DeferredFiles {
            files: Files(BTreeMap::from([
                ("C:\\plugins\\root.dll".into(), root()),
                (candidate.into(), dependency(11)),
            ])),
            deferred: candidate.into(),
            ready: Cell::new(false),
            reads: RefCell::new(Vec::new()),
        };
        // Ordinary capability reads keep the synchronous contract and order.
        assert_eq!(
            adv_os::module_file(&files, "owned_dep.dll", Some("C:\\plugins"))
                .unwrap()
                .0,
            candidate
        );
        assert!(files.reads.borrow().is_empty());
        let mut machine = fixture();
        let mut loader = Loader::new(
            &mut machine.cpu,
            &mut machine.sys,
            machine.ext.get_mut::<Modules>(),
            &files,
        );
        let result = if altered {
            loader.load_altered("C:\\plugins\\root.dll")
        } else {
            loader.load("C:\\plugins\\root.dll")
        };
        assert!(matches!(result, Err(LoadError::DeferredRead(path)) if path == candidate));
        assert_eq!(&files.reads.borrow()[1..], expected);
        assert_eq!(loader.modules.iter().count(), 0);
        files.ready.set(true);
        let result = if altered {
            loader.load_altered("C:\\plugins\\root.dll")
        } else {
            loader.load("C:\\plugins\\root.dll")
        };
        let id = result.unwrap();
        assert_eq!(loader.modules.get(id).name, "root.dll");
        let dependency = loader.modules.by_name("owned_dep.dll").unwrap();
        assert_eq!(loader.modules.get(dependency).path, candidate);
    }
}

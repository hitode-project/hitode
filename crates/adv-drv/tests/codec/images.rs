use super::*;
use adv_abi::dll::ModuleImage;
use adv_cpu::JitCpu;
use adv_machine::{Clock, Machine, Resolver, layout::PARK_ADDR};
use std::path::Path;

struct Platform;
impl Resolver for Platform {
    fn provides(&self, _: &str) -> bool {
        true
    }
    fn host_fn(&self, module: &str, name: &str) -> Option<HostFn> {
        host_fn(module, name)
    }
}

fn machine(mode: Mode) -> Machine<JitCpu> {
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        Platform,
        Clock::virtual_time(),
    )
    .unwrap();
    install(&mut m.ext, mode);
    m
}

// spec: CODEC-MAP-1
#[test]
fn distribution_images_are_not_scanned() {
    let mut m = machine(Mode::Native);
    let image = ModuleImage::build(&mut m, "fixture.dll", &[], &[]).unwrap();
    adv_nt::identity::note_ntdll(
        &mut m,
        Path::new("dist/lib/wine/i386-windows/fixture.dll"),
        image.bytes(),
    );
    let base = *m
        .ext
        .get::<adv_nt::identity::Images>()
        .0
        .keys()
        .next()
        .unwrap();
    m.visit_context(None, |e| note_image(e, base, PAGE, &[]))
        .unwrap();
    assert!(m.ext.get::<Images>().mapped.is_empty());
    assert!(stats(&m.ext).unwrap().last_rejection.is_none());
    assert!(m.sys.redirects.is_empty());
}

// spec: CODEC-MAP-1
#[test]
fn each_mapped_image_is_scanned_once_and_retirement_allows_remapping() {
    let mut m = machine(Mode::Native);
    let base = 0x400000;
    m.cpu.map(base, PAGE, Prot::RX).unwrap();
    let ranges = [ExecutableRange {
        start: base,
        len: PAGE,
    }];
    m.visit_context(None, |e| note_image(e, base, PAGE, &ranges))
        .unwrap();
    assert_eq!(m.ext.get::<Images>().mapped.len(), 1);
    assert!(stats(&m.ext).unwrap().last_rejection.is_none());
    // An invalid second scan would record an executable-range rejection.
    m.visit_context(None, |e| note_image(e, base, PAGE, &[]))
        .unwrap();
    assert!(stats(&m.ext).unwrap().last_rejection.is_none());
    m.visit_context(None, |e| forget_image(e, base)).unwrap();
    assert!(m.ext.get::<Images>().mapped.is_empty());
    m.visit_context(None, |e| note_image(e, base, PAGE, &ranges))
        .unwrap();
    assert_eq!(m.ext.get::<Images>().mapped.len(), 1);
}

// spec: CODEC-MAP-1
#[test]
fn guest_mode_does_not_scan_or_bind() {
    let mut m = machine(Mode::Guest);
    m.visit_context(None, |e| note_image(e, 0x400000, PAGE, &[]))
        .unwrap();
    assert!(m.ext.get::<Images>().mapped.is_empty());
    assert!(stats(&m.ext).unwrap().last_rejection.is_none());
    assert!(m.sys.redirects.is_empty());
}

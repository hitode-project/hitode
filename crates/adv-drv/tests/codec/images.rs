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
fn recorded_guest_images_are_scanned() {
    let mut m = machine(Mode::Native);
    let base = 0x400000;
    let mut writer = adv_pe::write::DllWriter::new("fixture.dll", base);
    // Framed x86 helper returning its register argument.
    let code = vec![0x55, 0x8b, 0xec, 0x83, 0xec, 0x10, 0x8b, 0xc1, 0xc9, 0xc3];
    let size = code.len() as u32;
    writer
        .section(
            ".text",
            code,
            size,
            adv_pe::scn::CNT_CODE | adv_pe::scn::MEM_READ | adv_pe::scn::MEM_EXECUTE,
        )
        .unwrap();
    let bytes = writer.finish().unwrap();
    let pe = adv_pe::Pe::parse(&bytes).unwrap();
    m.cpu.map(base, pe.size_of_image, Prot::RX).unwrap();
    m.cpu.write(base, &pe.image).unwrap();
    let path = Path::new("fixture.dll");
    adv_nt::identity::note_ntdll(&mut m, path, &bytes);
    m.visit_context(None, |e| {
        let binding = *e.ext.get::<adv_nt::section::ImageBinding>();
        (binding.mapped)(e, base, &pe, Some(path));
    })
    .unwrap();
    assert_eq!(
        m.ext.get::<Images>().mapped.get(&base),
        Some(&pe.size_of_image)
    );
    assert!(stats(&m.ext).unwrap().last_rejection.is_none());
}

// spec: CODEC-MAP-1
#[test]
fn local_static_codec_image_binds_bundled_families() {
    let Some(path) = std::env::var_os("ADV_STATIC_CODEC_TEST_IMAGE") else {
        return;
    };
    let path = Path::new(&path);
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => panic!("reading local codec fixture: {error}"),
    };
    let pe = adv_pe::Pe::parse(&bytes).unwrap();
    let mut m = machine(Mode::Verify);
    let base = pe.image_base;
    let size = pe.size_of_image.next_multiple_of(PAGE);
    m.cpu.map(base, size, Prot::RX).unwrap();
    m.cpu.write(base, &pe.image).unwrap();
    adv_nt::identity::note_ntdll(&mut m, path, &bytes);
    m.visit_context(None, |e| {
        let binding = *e.ext.get::<adv_nt::section::ImageBinding>();
        (binding.mapped)(e, base, &pe, Some(path));
    })
    .unwrap();
    let stats = stats(&m.ext).unwrap();
    assert!(stats.installed > 0, "{stats:?}");
    assert!(stats.theora.installed > 0, "{stats:?}");
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

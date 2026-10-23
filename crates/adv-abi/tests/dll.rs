use adv_abi::{
    Conv, Env, Outcome, Registry,
    dll::{Export, Import, ModuleImage},
};
use adv_cpu::backend::{Guest, GuestExt, Prot, asm::Asm};
use adv_machine::{
    Clock, Machine, RunResult,
    layout::{PAGE, PARK_ADDR},
};
use adv_pe::{ExportTarget, ImportModule, ImportName, Pe};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x0040_0000;
const DATA: u32 = CODE + PAGE;

fn machine(mut registry: Registry) -> Machine<UnicornCpu> {
    registry.add("fixture.dll", "done", Conv::Cdecl { argc: 0 }, |_| {
        Outcome::ExitProcess(0)
    });
    let mut machine = Machine::new(
        UnicornCpu::new(PARK_ADDR).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    machine.cpu.map(CODE, PAGE, Prot::RWX).unwrap();
    machine.cpu.map(DATA, PAGE, Prot::RW).unwrap();
    machine
}

fn load(machine: &mut Machine<UnicornCpu>, image: &ModuleImage, base: u32) -> Pe {
    let mut pe = Pe::parse(image.bytes()).unwrap();
    pe.rebase(base).unwrap();
    machine.cpu.map(base, pe.size_of_image, Prot::RWX).unwrap();
    machine.cpu.write(base, &pe.image).unwrap();
    pe
}

fn entry(pe: &Pe, base: u32) -> u32 {
    let ExportTarget::Rva(rva) = pe.exports().unwrap()[0].target else {
        panic!("code export")
    };
    base + rva
}

fn run(machine: &mut Machine<UnicornCpu>, mut code: Asm) {
    let done = machine.resolve_import("fixture.dll", "done").unwrap();
    code.call(done);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("fixture", CODE, &[], 0x1_0000).unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
}

// spec: WINE-DLL-1
#[test]
fn host_entries_obey_all_registered_conventions_at_different_bases() {
    for base in [0x2000_0000, 0x5000_0000] {
        for conv in [
            Conv::Stdcall { argc: 4 },
            Conv::Cdecl { argc: 4 },
            Conv::CdeclVariadic { fixed: 1 },
        ] {
            let mut registry = Registry::default();
            registry.add("fixture.dll", "Compute", conv, |env| {
                Outcome::Return(env.arg(0) * 1000 + env.arg(1) * 100 + env.arg(2) * 10 + env.arg(3))
            });
            let mut machine = machine(registry);
            let image = ModuleImage::build(
                &mut machine,
                "fixture.dll",
                &[Export {
                    name: "Compute",
                    ordinal: 7,
                }],
                &[],
            )
            .unwrap();
            let pe = load(&mut machine, &image, base);
            let mut code = Asm::new(CODE);
            code.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax,esp
            code.mov_mem_eax(DATA)
                .push_imm(4)
                .push_imm(3)
                .push_imm(2)
                .push_imm(1)
                .call(entry(&pe, base))
                .mov_mem_eax(DATA + 4);
            if !matches!(conv, Conv::Stdcall { .. }) {
                code.bytes.extend_from_slice(&[0x83, 0xc4, 16]);
            }
            code.bytes.extend_from_slice(&[0x89, 0xe0]);
            code.mov_mem_eax(DATA + 8);
            run(&mut machine, code);
            assert_eq!(machine.cpu.read_u32(DATA + 4).unwrap(), 1234);
            assert_eq!(
                machine.cpu.read_u32(DATA).unwrap(),
                machine.cpu.read_u32(DATA + 8).unwrap()
            );
        }
    }
}

fn import_twice(env: &mut Env<'_>) -> Outcome {
    let import = Import::bind(env, "guest.dll", "Value").unwrap();
    import.call(env, [123], move |env, first| {
        import.call(env, [456], move |_, second| {
            Outcome::Return(first.value() + second.value())
        })
    })
}

// spec: WINE-DLL-2
#[test]
fn copied_entry_captures_the_image_iat_and_rereads_it_from_a_continuation() {
    let mut registry = Registry::default();
    registry.add(
        "fixture.dll",
        "ImportTwice",
        Conv::Stdcall { argc: 0 },
        import_twice,
    );
    let mut machine = machine(registry);
    let image = ModuleImage::build(
        &mut machine,
        "fixture.dll",
        &[Export {
            name: "ImportTwice",
            ordinal: 1,
        }],
        &[ImportModule {
            dll: "guest.dll".into(),
            imports: vec![adv_pe::Import {
                name: ImportName::Name {
                    hint: 0,
                    name: "Value".into(),
                },
                iat_rva: 0,
            }],
        }],
    )
    .unwrap();
    let base = 0x3000_0000;
    let pe = load(&mut machine, &image, base);
    let slot = base + pe.imports().unwrap()[0].imports[0].iat_rva;
    let first = CODE + 0x100;
    let second = CODE + 0x120;
    machine.cpu.write_u32(slot, first).unwrap();
    let mut a = Asm::new(first);
    a.bytes.extend_from_slice(&[0xc7, 0x05]); // mov dword [slot],second
    a.bytes.extend_from_slice(&slot.to_le_bytes());
    a.bytes.extend_from_slice(&second.to_le_bytes());
    a.bytes.extend_from_slice(&[0xb8, 7, 0, 0, 0]);
    a.ret_n(4);
    machine.cpu.write(first, &a.bytes).unwrap();
    let mut b = Asm::new(second);
    b.bytes.extend_from_slice(&[0xb8, 13, 0, 0, 0]);
    b.ret();
    machine.cpu.write(second, &b.bytes).unwrap();
    // A detour may copy the entire entry outside its PE image.
    let copied = CODE + 0x200;
    let text = &pe.sections[0];
    machine
        .cpu
        .write(
            copied,
            pe.slice(text.virtual_address, text.virtual_size).unwrap(),
        )
        .unwrap();
    let mut code = Asm::new(CODE);
    code.call(copied).mov_mem_eax(DATA);
    run(&mut machine, code);
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 20);
    assert_eq!(machine.cpu.read_u32(slot).unwrap(), second);
}

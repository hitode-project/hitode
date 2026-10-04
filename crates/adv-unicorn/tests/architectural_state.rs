use adv_cpu::backend::{Backend, Exit, Guest, GuestExt, Prot, TrapAction, TrapHandler};
use adv_cpu::{ArchitecturalState, JitCpu, StateError, X87Register};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const STACK: u32 = 0x420000;
const PARK: u32 = 0x430000;
struct Unexpected;
impl TrapHandler for Unexpected {
    fn on_trap(&mut self, _: &mut dyn Guest, vector: u8) -> TrapAction {
        panic!("unexpected trap {vector}");
    }
}
fn setup<B: Backend>(mut cpu: B) -> B {
    cpu.map(CODE, 4096, Prot::RX).unwrap();
    cpu.map(DATA, 4096, Prot::RW).unwrap();
    cpu.map(STACK, 4096, Prot::RW).unwrap();
    cpu.map(PARK, 4096, Prot::RX).unwrap();
    cpu
}
fn modified(cpu: &mut dyn Guest) -> ArchitecturalState {
    let mut state = cpu.capture_architecture().unwrap();
    state.regs = [1, 2, 3, 4, STACK + 512, 6, 7, 8];
    state.eip = CODE;
    state.eflags = 0x202;
    state.x87.control = 0x077f;
    state.x87.status = 0x4001 | (5 << 11);
    state.x87.opcode = 0x345;
    state.x87.ip = 0x12345678;
    state.x87.dp = 0x87654321;
    state.x87.cs = 0x23;
    state.x87.ds = 0x2b;
    state.x87.tag = 0xffff;
    for physical in 0..8 {
        state.x87.regs[physical] =
            X87Register::from_parts(0xa000000000000000 + physical as u64, 0x3fff);
    }
    state.x87.set_physical_tag(2, 0);
    state.x87.set_physical_tag(5, 0);
    state.xmm = std::array::from_fn(|i| std::array::from_fn(|j| (i * 17 + j) as u8));
    state.mxcsr = 0x5f80;
    state
}
fn roundtrip<B: Backend>(mut cpu: B) {
    let state = modified(&mut cpu);
    cpu.restore_architecture(&state).unwrap();
    assert_eq!(cpu.capture_architecture().unwrap(), state);
    for field in 0..6 {
        let mut invalid = state.clone();
        invalid.regs[0] = 999;
        match field {
            0 => invalid.mxcsr |= 0x10000,
            1 => invalid.x87.opcode |= 0x800,
            2 => invalid.fs_base ^= 1,
            3 => invalid.selectors[1] ^= 8,
            4 => invalid.eflags ^= 1 << 17,
            5 => invalid.x87.control ^= 0x8000,
            _ => unreachable!(),
        }
        assert!(cpu.restore_architecture(&invalid).is_err());
        assert_eq!(cpu.capture_architecture().unwrap(), state);
    }
    // FXSAVE observes the actual instruction path, not just register API reads.
    let mut bytes = vec![0x0f, 0xae, 0x05];
    bytes.extend(DATA.to_le_bytes());
    bytes.push(0xc3);
    cpu.write(CODE, &bytes).unwrap();
    cpu.write_u32(STACK + 512, PARK).unwrap();
    assert_eq!(cpu.run(&mut Unexpected), Exit::Parked);
    let mut image = [0; 288];
    cpu.read(DATA, &mut image).unwrap();
    assert_eq!(
        u16::from_le_bytes(image[0..2].try_into().unwrap()),
        state.x87.control
    );
    assert_eq!(
        u16::from_le_bytes(image[2..4].try_into().unwrap()),
        state.x87.status
    );
    assert_eq!(
        u32::from_le_bytes(image[24..28].try_into().unwrap()),
        state.mxcsr
    );
    for logical in 0..8 {
        assert_eq!(
            &image[32 + logical * 16..42 + logical * 16],
            &state.x87.regs[state.x87.physical_index(logical)].0
        );
    }
    for slot in 0..8 {
        assert_eq!(&image[160 + slot * 16..176 + slot * 16], &state.xmm[slot]);
    }
}
#[test]
fn complete_architecture_roundtrip_reaches_real_fxsave_on_three_backends() {
    for jit in [false, true] {
        roundtrip(setup(JitCpu::new(PARK, jit).unwrap()));
    }
    roundtrip(setup(UnicornCpu::new(PARK).unwrap()));
}
#[test]
fn reference_rejects_inconsistent_full_tags_before_any_register_write() {
    let mut cpu = setup(UnicornCpu::new(PARK).unwrap());
    let mut state = modified(&mut cpu);
    cpu.restore_architecture(&state).unwrap();
    let before = cpu.capture_architecture().unwrap();
    state.regs[0] = 999;
    state.x87.set_physical_tag(2, 1);
    assert!(matches!(
        cpu.restore_architecture(&state),
        Err(StateError::Backend(_))
    ));
    assert_eq!(cpu.capture_architecture().unwrap(), before);
}

#[test]
#[ignore = "explicit boundary cost diagnostic; no guest hot-loop change"]
fn explicit_architectural_boundary_cost() {
    use std::{hint::black_box, time::Instant};
    println!(
        "snapshot_bytes={}",
        std::mem::size_of::<ArchitecturalState>()
    );
    for (name, mut cpu) in [
        (
            "interpreter",
            Box::new(setup(JitCpu::new(PARK, false).unwrap())) as Box<dyn Guest>,
        ),
        (
            "jit",
            Box::new(setup(JitCpu::new(PARK, true).unwrap())) as Box<dyn Guest>,
        ),
        (
            "unicorn",
            Box::new(setup(UnicornCpu::new(PARK).unwrap())) as Box<dyn Guest>,
        ),
    ] {
        const N: usize = 100_000;
        let snapshot = cpu.capture_architecture().unwrap();
        let start = Instant::now();
        for _ in 0..N {
            black_box(cpu.capture_architecture().unwrap());
        }
        let capture = start.elapsed().as_secs_f64() * 1e9 / N as f64;
        let start = Instant::now();
        for _ in 0..N {
            cpu.restore_architecture(black_box(&snapshot)).unwrap();
        }
        let restore = start.elapsed().as_secs_f64() * 1e9 / N as f64;
        println!("{name} capture_ns={capture:.1} restore_ns={restore:.1}");
    }
}

//! Public control changes must reach guest instructions and survive CPU contexts.
use adv_cpu::JitCpu;
use adv_cpu::backend::{
    Backend, Exit, FpuEnvironment, FpuEnvironmentError, Guest, GuestExt, Prot, Reg, TrapAction,
    TrapHandler,
};
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
fn observe<B: Backend>(cpu: &mut B) -> Vec<u8> {
    // FXSAVE [DATA]; RET. This observes the actual instruction path, including
    // control/status, physical tags, x87 registers, and raw MXCSR status bits.
    let mut code = vec![0x0f, 0xae, 0x05];
    code.extend(DATA.to_le_bytes());
    code.push(0xc3);
    cpu.write(CODE, &code).unwrap();
    cpu.write_u32(STACK + 512, PARK).unwrap();
    cpu.set_reg(Reg::Esp, STACK + 512);
    cpu.set_reg(Reg::Eip, CODE);
    assert_eq!(cpu.run(&mut Unexpected), Exit::Parked);
    let mut bytes = vec![0; 288];
    cpu.read(DATA, &mut bytes).unwrap();
    bytes
}
fn roundtrip<B: Backend>(mut cpu: B) {
    let initial = FpuEnvironment {
        x87_control: 0x077f,
        mxcsr: 0x3fc3,
    };
    cpu.set_fpu_environment(initial).unwrap();
    cpu.push_f64(1.25);
    cpu.push_f64(-2.5);
    let before = observe(&mut cpu);
    let mut context = cpu.new_context();
    cpu.save(&mut context);
    let changed = FpuEnvironment {
        x87_control: 0x0f7f,
        mxcsr: 0xffe3,
    };
    cpu.set_fpu_environment(changed).unwrap();
    assert_eq!(cpu.fpu_environment().unwrap(), changed);
    let after = observe(&mut cpu);
    assert_eq!(
        &after[2..24],
        &before[2..24],
        "status, tags, opcode and pointers preserved"
    );
    assert_eq!(
        &after[32..],
        &before[32..],
        "floating/vector registers preserved"
    );
    assert_eq!(
        u16::from_le_bytes(after[..2].try_into().unwrap()),
        changed.x87_control
    );
    assert_eq!(
        u32::from_le_bytes(after[24..28].try_into().unwrap()),
        changed.mxcsr
    );
    assert_eq!(
        cpu.set_fpu_environment(FpuEnvironment {
            mxcsr: 0x1_0000,
            ..initial
        }),
        Err(FpuEnvironmentError::InvalidMxcsr(0x1_0000))
    );
    assert_eq!(
        cpu.fpu_environment().unwrap(),
        changed,
        "invalid change is atomic"
    );
    cpu.restore(&context);
    assert_eq!(cpu.fpu_environment().unwrap(), initial);
    assert_eq!(cpu.pop_f64(), Some(-2.5));
    assert_eq!(cpu.pop_f64(), Some(1.25));
}
#[test]
fn guest_environment_roundtrip_and_contexts_match_reference() {
    for jit in [false, true] {
        roundtrip(setup(JitCpu::new(PARK, jit).unwrap()));
    }
    roundtrip(setup(UnicornCpu::new(PARK).unwrap()));
}

struct SetAtTrap;
impl TrapHandler for SetAtTrap {
    fn on_trap(&mut self, cpu: &mut dyn Guest, vector: u8) -> TrapAction {
        assert_eq!(vector, 0x2e);
        cpu.set_fpu_environment(FpuEnvironment {
            x87_control: 0x077f,
            mxcsr: 0x3fa0,
        })
        .unwrap();
        TrapAction::Continue
    }
}
fn trap_update<B: Backend>(mut cpu: B) {
    // Host changes the environment through the borrowed trap view. The guest
    // then computes floor(1.75) with FISTP and reads MXCSR without changing it.
    let mut code = vec![0xcd, 0x2e, 0xdd, 0x05];
    code.extend((DATA + 512).to_le_bytes()); // FLD double
    code.extend([0xdb, 0x1d]);
    code.extend((DATA + 520).to_le_bytes()); // FISTP dword
    code.extend([0x0f, 0xae, 0x1d]);
    code.extend((DATA + 524).to_le_bytes()); // STMXCSR
    code.push(0xc3);
    cpu.write(CODE, &code).unwrap();
    cpu.write(DATA + 512, &1.75f64.to_le_bytes()).unwrap();
    cpu.write_u32(STACK + 512, PARK).unwrap();
    cpu.set_reg(Reg::Esp, STACK + 512);
    cpu.set_reg(Reg::Eip, CODE);
    assert_eq!(cpu.run(&mut SetAtTrap), Exit::Parked);
    assert_eq!(cpu.read_u32(DATA + 520).unwrap(), 1);
    assert_eq!(cpu.read_u32(DATA + 524).unwrap(), 0x3fa0);
}
#[test]
fn trap_view_control_change_affects_the_next_guest_instruction() {
    for jit in [false, true] {
        trap_update(setup(JitCpu::new(PARK, jit).unwrap()));
    }
    trap_update(setup(UnicornCpu::new(PARK).unwrap()));
}

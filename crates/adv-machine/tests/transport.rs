//! Authored relocation/reconstruction exercises the private host transport, not
//! a Windows numeric syscall table. Every caller obeys its normal guest ABI.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu, Region,
    backend::{Backend, GuestExt, Prot, Reg, asm::Asm},
};
use adv_machine::{
    Clock, Conv, Env, Machine, Outcome, RunResult, Wait,
    exception::{Checkpoint, Exception},
    layout::{PARK_ADDR, THUNK_BASE, THUNK_STRIDE, thunk_addr},
};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const COPY: u32 = CODE + 0x900;
const CALLBACK: u32 = CODE + 0xa00;

#[derive(Default)]
struct Seen {
    apis: Vec<String>,
    raw: Vec<u32>,
}

fn raw(env: &mut Env<'_>, event: Exception, mut checkpoint: Checkpoint) -> Outcome {
    assert!(matches!(event, Exception::Trap { vector: 0x2e, .. }));
    let word = env.cpu.read_u32(env.cpu.reg(Reg::Esp)).unwrap();
    env.ext.get_mut::<Seen>().raw.push(word);
    checkpoint.state.regs[0] = 0xc000001c;
    Outcome::ResumeExecution(checkpoint)
}

fn token<B: Backend>(machine: &Machine<B>, address: u32) -> u32 {
    let index = (address - THUNK_BASE) / THUNK_STRIDE;
    let token = machine
        .sys
        .thunks
        .get(index)
        .transport_token
        .unwrap()
        .word();
    assert_ne!(token, index);
    assert_eq!(machine.sys.thunks.transport_index(token), Some(index));
    token
}

fn rebuilt<B: Backend>(machine: &mut Machine<B>, entry: u32, gate: u32, token: u32, pop: u16) {
    let seed = 0x10203040;
    let mut code = Asm::new(entry);
    code.push_imm(seed);
    code.bytes.extend_from_slice(&[0x81, 0x04, 0x24]); // add dword [esp], delta
    code.bytes
        .extend_from_slice(&token.wrapping_sub(seed).to_le_bytes());
    code.jmp(gate);
    machine.cpu.write(entry, &code.bytes).unwrap();
    let mut tail = Asm::new(gate);
    tail.trap().ret_n(pop);
    machine.cpu.write(gate, &tail.bytes).unwrap();
}

fn transport<B: Backend>(cpu: B, prepare: impl FnOnce(&mut Machine<B>)) -> Machine<B> {
    let mut registry = Registry::default();
    for (name, conv) in [
        ("sum", Conv::Stdcall { argc: 2 }),
        ("cdecl", Conv::Cdecl { argc: 2 }),
    ] {
        registry.add("fixture", name, conv, |env| {
            let name = env.sys.thunks.get(env.thunk).name.clone();
            env.ext.get_mut::<Seen>().apis.push(name);
            Outcome::Return(env.arg(0) + env.arg(1))
        });
    }
    registry.add("fixture", "store_pair", Conv::Stdcall { argc: 1 }, |env| {
        let out = env.arg(0);
        let context = env.ctx();
        assert!(out >= context.stack_lo && out + 8 <= context.stack_hi);
        assert!(
            (CODE..CODE + 0x200).contains(&env.caller()),
            "original return slot survives token consumption"
        );
        env.ext.get_mut::<Seen>().apis.push("store_pair".into());
        env.cpu.write_u32(out, 0x12345678).unwrap();
        env.cpu.write_u32(out + 4, 0x24681357).unwrap();
        Outcome::Return(0)
    });
    registry.add("fixture", "callback", Conv::Stdcall { argc: 1 }, |env| {
        env.ext.get_mut::<Seen>().apis.push("callback".into());
        Outcome::call(CALLBACK, [env.arg(0)], |_, result| {
            Outcome::block_then_return(Wait::Yield, result.value() + 1)
        })
    });
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    machine.cpu.map(CODE, 4096, Prot::RWX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.ext.insert(Seen::default());
    machine.sys.exception_handler = Some(raw);
    let sum = machine.resolve_import("fixture", "sum").unwrap();
    let cdecl = machine.resolve_import("fixture", "cdecl").unwrap();
    let store = machine.resolve_import("fixture", "store_pair").unwrap();
    let callback = machine.resolve_import("fixture", "callback").unwrap();
    let sum_token = token(&machine, sum);
    let cdecl_token = token(&machine, cdecl);
    let store_token = token(&machine, store);
    let callback_token = token(&machine, callback);
    assert!(machine.sys.thunks.get(0).transport_token.is_none());
    assert!(machine.sys.thunks.get(1).transport_token.is_none());
    let mut pad = [0; 3];
    machine.cpu.read(thunk_addr(0), &mut pad).unwrap();
    assert_eq!(machine.sys.thunks.pristine_entry(0), pad);
    assert!(machine.sys.thunks.pristine_entry(1).is_empty());
    for reserved in [sum_token & !0x3fff, (sum_token & !0x3fff) | 1] {
        assert_eq!(machine.sys.thunks.transport_index(reserved), None);
    }
    let unregistered = sum_token + 0x100;
    assert_eq!(machine.sys.thunks.transport_index(unregistered), None);
    for (entry, gate, word, pop) in [
        (CODE + 0x200, CODE + 0x280, sum_token, 8),
        (CODE + 0x300, CODE + 0x380, cdecl_token, 0),
        (CODE + 0x400, CODE + 0x480, store_token, 4),
        (CODE + 0x500, CODE + 0x580, callback_token, 4),
    ] {
        rebuilt(&mut machine, entry, gate, word, pop);
    }
    let mut bytes = [0; THUNK_STRIDE as usize];
    machine.cpu.read(sum, &mut bytes).unwrap();
    machine.cpu.write(COPY, &bytes).unwrap();
    let mut replacement = Asm::new(CODE + 0xb00);
    replacement
        .bytes
        .extend_from_slice(&[0xb8, 0xe7, 0x03, 0, 0]);
    replacement.ret_n(8);
    machine.cpu.write(CODE + 0xb00, &replacement.bytes).unwrap();
    let mut patch = Asm::new(sum);
    patch.jmp(CODE + 0xb00);
    machine.cpu.write(sum, &patch.bytes).unwrap();
    let mut body = Asm::new(CALLBACK);
    body.mov_eax_esp(4).add_eax_imm(7).ret_n(4);
    machine.cpu.write(CALLBACK, &body.bytes).unwrap();
    let mut main = Asm::new(CODE);
    main.bytes.extend_from_slice(&[0x89, 0xe0]);
    main.mov_mem_eax(DATA);
    main.push_imm(5).push_imm(7).call(sum).mov_mem_eax(DATA + 4);
    main.push_imm(5)
        .push_imm(7)
        .call(COPY)
        .mov_mem_eax(DATA + 8);
    main.push_imm(13)
        .push_imm(17)
        .call(CODE + 0x200)
        .mov_mem_eax(DATA + 12);
    main.push_imm(19)
        .push_imm(23)
        .call(CODE + 0x300)
        .mov_mem_eax(DATA + 16)
        .add_esp(8);
    // The stdcall1 caller allocates its own stack output and creates a normal
    // return/argument frame before the private token is reconstructed.
    main.bytes
        .extend_from_slice(&[0x83, 0xec, 8, 0x89, 0xe0, 0x50]);
    main.call(CODE + 0x400)
        .mov_eax_esp(0)
        .mov_mem_eax(DATA + 20);
    main.mov_eax_esp(4).mov_mem_eax(DATA + 24).add_esp(8);
    main.push_imm(31).call(CODE + 0x500).mov_mem_eax(DATA + 28);
    let bare_index = (sum - THUNK_BASE) / THUNK_STRIDE;
    for (word, offset) in [(bare_index, 32), (unregistered, 36)] {
        main.push_imm(word)
            .trap()
            .mov_mem_eax(DATA + offset)
            .add_esp(4);
    }
    main.bytes.extend_from_slice(&[0x89, 0xe0]);
    main.mov_mem_eax(DATA + 40);
    main.bytes.extend_from_slice(&[0x31, 0xc0]);
    main.ret();
    machine.cpu.write(CODE, &main.bytes).unwrap();
    prepare(&mut machine);
    machine
        .spawn("authored reconstructed host transport", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (offset, expected) in [
        (4, 999),
        (8, 12),
        (12, 30),
        (16, 42),
        (20, 0x12345678),
        (24, 0x24681357),
        (28, 39),
        (32, 0xc000001c),
        (36, 0xc000001c),
    ] {
        assert_eq!(
            machine.cpu.read_u32(DATA + offset).unwrap(),
            expected,
            "offset {offset}"
        );
    }
    assert_eq!(
        machine.cpu.read_u32(DATA).unwrap(),
        machine.cpu.read_u32(DATA + 40).unwrap(),
        "one token pop plus correct stdcall/cdecl cleanup"
    );
    assert_eq!(
        machine.ext.get::<Seen>().apis,
        ["sum", "sum", "cdecl", "store_pair", "callback"]
    );
    assert_eq!(machine.ext.get::<Seen>().raw, [bare_index, unregistered]);
    machine
}

#[test]
fn registered_transport_survives_copy_and_arithmetic_reconstruction_with_normal_abi_frames() {
    transport(UnicornCpu::new(PARK_ADDR).unwrap(), |_| {});
    transport(JitCpu::new(PARK_ADDR, false).unwrap(), |_| {});
    let mut jit = JitCpu::new(PARK_ADDR, true).unwrap();
    jit.set_async_compilation(false);
    let machine = transport(jit, |machine| {
        machine.cpu.executor_mut().enable_auto_jit(false);
        let mut guest = Region::new(CODE, 4096);
        guest.entries.extend([
            CODE,
            CODE + 0x200,
            CODE + 0x280,
            CODE + 0x300,
            CODE + 0x380,
            CODE + 0x400,
            CODE + 0x480,
            CODE + 0x500,
            CODE + 0x580,
            COPY,
            CALLBACK,
            CODE + 0xb00,
        ]);
        machine.cpu.executor_mut().add_region(guest).unwrap();
        let mut thunks = Region::new(
            THUNK_BASE,
            (machine.sys.thunks.len() * THUNK_STRIDE) as usize,
        );
        thunks
            .entries
            .extend((0..machine.sys.thunks.len()).map(thunk_addr));
        machine.cpu.executor_mut().add_region(thunks).unwrap();
    });
    let stats = machine.cpu.stats();
    assert!(stats.native_entries > 0, "fixture must enter compiled code");
    assert_eq!(
        stats.interpreter_instructions, 0,
        "all transport gates and return tails must execute in compiled code"
    );
}

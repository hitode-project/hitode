//! Machine dispatch keeps trace and diagnostic contracts around light calls.
use super::*;
use crate::{Conv, Dispatch, Machine, Trace, layout::PARK_ADDR};
use adv_cpu::{
    JitCpu,
    backend::{Backend, Exit, Prot, asm::Asm},
};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;

struct Binding {
    conv: Conv,
    wide: bool,
    env_leaf: Option<crate::EnvLeafHandler>,
}
impl crate::Resolver for Binding {
    fn provides(&self, _: &str) -> bool {
        true
    }
    fn host_fn(&self, _: &str, _: &str) -> Option<crate::HostFn> {
        Some(crate::HostFn {
            conv: self.conv,
            env_leaf: self.env_leaf,
            handler: if self.env_leaf.is_some() {
                ordinary_env
            } else {
                trap
            },
            data: 0,
            trace: false,
            guest: None,
            leaf: Some(if self.wide {
                |e| {
                    let value = e.arg(1);
                    e.write_u32(e.arg(0), value).unwrap();
                    0xfedcba98_12345678
                }
            } else {
                |e| {
                    let value = e.arg(1);
                    e.write_u32(e.arg(0), value).unwrap();
                    e.return32(value)
                }
            }),
        })
    }
}

fn ordinary_env(env: &mut Env<'_>) -> Outcome {
    let value = env.arg(1);
    env.cpu.write_u32(env.arg(0), value).unwrap();
    Outcome::Return(value)
}

fn fixture(conv: Conv, wide: bool) -> Machine<JitCpu> {
    fixture_binding(conv, wide, None)
}
fn fixture_binding(
    conv: Conv,
    wide: bool,
    env_leaf: Option<crate::EnvLeafHandler>,
) -> Machine<JitCpu> {
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, true).unwrap(),
        Binding {
            conv,
            wide,
            env_leaf,
        },
        Clock::virtual_time(),
    )
    .unwrap();
    machine.spawn("leaf test", CODE, &[], 65536).unwrap();
    machine.cpu.map(CODE, 0x4000, Prot::RX).unwrap();
    machine.cpu.map(DATA, 0x4000, Prot::RW).unwrap();
    let target = machine.resolve_import("fixture.dll", "leaf").unwrap();
    machine.cpu.write_u32(DATA + 16, target).unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(17).push_imm(DATA);
    code.bytes.extend_from_slice(&[0xff, 0x15]);
    code.bytes.extend_from_slice(&(DATA + 16).to_le_bytes());
    if matches!(conv, Conv::Cdecl { .. }) {
        code.add_esp(8);
    }
    code.jmp(PARK_ADDR);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
}

fn run(machine: &mut Machine<JitCpu>) {
    machine.cpu.set_reg(Reg::Eip, CODE);
    machine.cpu.set_reg(Reg::Esp, DATA + 0x3f00);
    machine.cpu.set_reg(Reg::Edx, 0x87654321);
    machine.cpu.set_reg(Reg::Eflags, 0xed7);
    let mut dispatch = Dispatch {
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        parked: None,
        yield_on_frame: false,
        defer_work: false,
    };
    assert!(matches!(machine.cpu.run(&mut dispatch), Exit::Parked));
    assert!(dispatch.parked.is_none());
}

// spec: A64-CTL-12, A64-REG-4
#[test]
fn machine_leaf_and_trace_match_registers_stack_memory_and_flags() {
    for conv in [Conv::Stdcall { argc: 2 }, Conv::Cdecl { argc: 2 }] {
        for wide in [false, true] {
            let mut light = fixture(conv, wide);
            let mut traced = fixture(conv, wide);
            traced.sys.trace = Trace::to_writer(Box::new(std::io::sink()));
            traced.sys.trace.all = true;
            for _ in 0..3 {
                run(&mut light);
                run(&mut traced);
            }
            assert_eq!(
                light.cpu.capture_architecture().unwrap(),
                traced.cpu.capture_architecture().unwrap()
            );
            let mut a = vec![0; 0x4000];
            let mut b = a.clone();
            light.cpu.read(DATA, &mut a).unwrap();
            traced.cpu.read(DATA, &mut b).unwrap();
            assert_eq!(a, b);
            assert!(light.sys.stats.leaf_calls > 0);
            assert_eq!(traced.sys.stats.leaf_calls, 0);
            assert_eq!(light.sys.host_calls, 3);
            assert_eq!(traced.sys.host_calls, 3);
            assert_eq!(light.cpu.reg(Reg::Esp), DATA + 0x3f00);
        }
    }
}

// spec: A64-CTL-12
#[test]
fn trace_enabled_after_translation_uses_the_ordinary_handler() {
    let mut machine = fixture(Conv::Stdcall { argc: 2 }, false);
    for _ in 0..3 {
        run(&mut machine);
    }
    let leaves = machine.sys.stats.leaf_calls;
    machine.sys.trace = Trace::to_writer(Box::new(std::io::sink()));
    machine.sys.trace.all = true;
    run(&mut machine);
    assert_eq!(machine.sys.stats.leaf_calls, leaves);
    assert_eq!(machine.sys.host_calls, 4);
}

// spec: A64-CTL-10, A64-CTL-12
#[test]
fn published_thunk_copy_uses_the_leaf_route_and_remains_patchable() {
    let mut machine = fixture(Conv::Stdcall { argc: 2 }, false);
    let target = machine.cpu.read_u32(DATA + 16).unwrap();
    let index = (target - crate::layout::THUNK_BASE) / crate::layout::THUNK_STRIDE;
    let bytes = machine.sys.thunks.pristine_entry(index);
    let mut image = vec![0; 0x4000];
    image[..bytes.len()].copy_from_slice(&bytes);
    machine
        .cpu
        .map_private_copy_pages(CODE + 0x30000, &image, Prot::RX)
        .unwrap();
    machine
        .sys
        .thunks
        .register_thunk_pages(CODE + 0x30000, 0x4000);
    machine.cpu.write_u32(DATA + 16, CODE + 0x30000).unwrap();
    // Normal guest entries and callbacks can retain a host continuation.
    machine.sys.contexts[0].frames.push(crate::sched::Frame {
        id: 1,
        stack: DATA + 0x3ff0,
        return_floor: DATA + 0x3e00,
        unwindable: false,
        then: Box::new(|_, _| Outcome::Return(0)),
        origin: crate::sched::Origin {
            esp: DATA + 0x3ff0,
            ret_eip: PARK_ADDR,
            thunk: index,
            trace_args: None,
            interrupted: false,
        },
    });
    for _ in 0..3 {
        run(&mut machine);
    }
    assert!(machine.sys.stats.leaf_calls > 0);
    let leaves = machine.sys.stats.leaf_calls;
    // An ordinary replacement returns directly and cleans the same arguments.
    assert_eq!(machine.sys.contexts[0].frames.len(), 1);
    machine
        .cpu
        .write(CODE + 0x30000, &[0xb8, 42, 0, 0, 0, 0xc2, 8, 0])
        .unwrap();
    run(&mut machine);
    assert_eq!(machine.cpu.reg(Reg::Eax), 42);
    assert_eq!(machine.sys.stats.leaf_calls, leaves);
}

// spec: A64-CTL-12
#[test]
fn leaf_counts_reach_host_services_and_frame_budget_without_timing() {
    let mut machine = fixture(Conv::Stdcall { argc: 2 }, false);
    for _ in 0..3 {
        run(&mut machine);
    }
    machine.sys.stats.frame_budget = Some(crate::frame_budget::FrameBudget::new(
        machine.sys.stats.total,
        None,
    ));
    let leaves = machine.sys.stats.leaf_calls;
    run(&mut machine);
    assert_eq!(machine.sys.stats.leaf_calls, leaves + 1);
    let mut csv = Vec::new();
    machine
        .sys
        .stats
        .write_host_csv(&machine.cpu, &machine.sys.thunks, &mut csv)
        .unwrap();
    assert!(
        String::from_utf8(csv)
            .unwrap()
            .contains("\"fixture.dll\",\"leaf\",1,0,0")
    );
    // Close the API row through the usual frame accounting boundary.
    let budget = machine.sys.stats.frame_budget.as_mut().unwrap();
    budget.mark_frame(1);
    budget.finish(machine.sys.stats.total, None);
    assert!(
        budget
            .json()
            .contains("\"api\":\"fixture.dll!leaf\",\"ns\":0,\"calls\":1")
    );
}

// spec: A64-CTL-12
#[test]
fn ordinary_leaf_handler_uses_owned_invocation_arguments() {
    let mut machine = fixture(Conv::Stdcall { argc: 2 }, false);
    let target = machine.resolve_import("fixture.dll", "leaf").unwrap();
    let thunk = (target - crate::layout::THUNK_BASE) / crate::layout::THUNK_STRIDE;
    machine.sys.invocation_arguments = Some(crate::invocation::Arguments {
        context: machine.sys.current,
        thunk,
        values: vec![DATA, 91].into(),
    });
    let result = trap(&mut Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk,
    });
    assert!(matches!(result, Outcome::Return(91)));
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 91);
}

// spec: A64-CTL-12
#[test]
fn env_leaf_declines_restore_transport_and_count_each_call_once() {
    for abandon in [false, true] {
        let handler: crate::EnvLeafHandler = if abandon {
            |_| None
        } else {
            |e| {
                let value = e.arg(1);
                e.cpu.write_u32(e.arg(0), value).unwrap();
                Some((u64::from(e.cpu.reg(Reg::Edx)) << 32) | u64::from(value))
            }
        };
        let mut light = fixture_binding(Conv::Stdcall { argc: 2 }, false, Some(handler));
        let mut traced = fixture_binding(Conv::Stdcall { argc: 2 }, false, Some(handler));
        traced.sys.trace = Trace::to_writer(Box::new(std::io::sink()));
        for _ in 0..3 {
            run(&mut light);
            run(&mut traced);
        }
        assert_eq!(
            light.cpu.capture_architecture().unwrap(),
            traced.cpu.capture_architecture().unwrap()
        );
        assert_eq!(light.cpu.read_u32(DATA), traced.cpu.read_u32(DATA));
        assert_eq!(light.sys.host_calls, 3);
        assert_eq!(traced.sys.stats.leaf_calls, 0);
        assert_eq!(traced.sys.stats.leaf_abandons, 0);
        light.sys.stats.services_enabled = true;
        light.sys.stats.frame_budget = Some(crate::frame_budget::FrameBudget::new(
            light.sys.stats.total,
            None,
        ));
        let before = (light.sys.stats.leaf_calls, light.sys.stats.leaf_abandons);
        run(&mut light);
        assert_eq!(light.sys.stats.leaf_calls - before.0, u64::from(!abandon));
        assert_eq!(light.sys.stats.leaf_abandons - before.1, u64::from(abandon));
        let mut csv = Vec::new();
        light
            .sys
            .stats
            .write_host_csv(&light.cpu, &light.sys.thunks, &mut csv)
            .unwrap();
        let csv = String::from_utf8(csv).unwrap();
        assert!(csv.ends_with(&format!(
            ",{},{}\n",
            light.sys.stats.leaf_calls, light.sys.stats.leaf_abandons
        )));
        let budget = light.sys.stats.frame_budget.as_mut().unwrap();
        budget.mark_frame(1);
        budget.finish(light.sys.stats.total, None);
        assert!(budget.json().contains(&format!(
            "\"calls\":1,\"leaf_calls\":{},\"leaf_abandons\":{}",
            u64::from(!abandon),
            u64::from(abandon)
        )));
    }
}

// spec: A64-CTL-12
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "unsynchronized register Eax")]
fn env_leaf_rejects_unsynchronized_register_reads() {
    let mut machine = fixture(Conv::Stdcall { argc: 2 }, false);
    crate::env_leaf::Cpu(&mut machine.cpu).reg(Reg::Eax);
}

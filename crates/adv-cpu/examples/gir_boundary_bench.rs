//! Portable synthetic boundary fixture; independent of the adv-cpu execution path.
//! Usage: cargo run -p adv-cpu --release --example gir_boundary_bench.
//! Inputs: ADV_BOUNDARY_SAMPLES (default 7), ADV_BOUNDARY_LOOPS (default 1000000).
//! Output: anonymous CSV timing, code sizes, full-state assertion counts and rough
//! static vcode counts. Never emits code, disassembly, addresses or private inputs.
//! Compilation, differential checks and warmup are outside the timed interval.

use cranelift_codegen::ir::{
    AbiParam, InstBuilder, MemFlags, Signature, Value, condcodes::IntCC, types,
};
use cranelift_codegen::isa::CallConv;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module, default_libcall_names};
use std::hint::black_box;
use std::mem::offset_of;
use std::time::Instant;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Frame {
    gpr: [u64; 8],
    flags: u64,
    retired: u64,
}

#[repr(C)]
struct Control {
    // All fields are immutable for the duration of a native invocation.
    stop: u64,
    fuel: u64,
    interrupt: u64,
    expected_version: u64,
    actual_version: u64,
    output: *mut Frame,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Route {
    Empty,
    RustFrame,
    RustSsa,
    FrameLoop,
    FrameTail,
    SsaLoop,
    SsaTail,
}

#[derive(Clone, Copy)]
struct Spec {
    name: &'static str,
    route: Route,
    checks: u8,
    frame_pointers: bool,
}

#[derive(Default, Clone, Copy)]
struct StaticCounts {
    bytes: usize,
    move_like: usize,
    stack_ref: usize,
    frame_like: usize,
}

impl StaticCounts {
    fn add(&mut self, other: Self) {
        self.bytes += other.bytes;
        self.move_like += other.move_like;
        self.stack_ref += other.stack_ref;
        self.frame_like += other.frame_like;
    }
}

struct Ready {
    spec: Spec,
    module: Option<JITModule>,
    entry: unsafe extern "C" fn(*mut Frame, *const Control),
    body: StaticCounts,
    wrapper: StaticCounts,
    bodies: usize,
}

impl Drop for Ready {
    fn drop(&mut self) {
        // SAFETY: no invocation survives Ready, and no entry is shared externally.
        unsafe { self.module.take().unwrap().free_memory() };
    }
}

fn seed() -> Frame {
    Frame {
        gpr: [3, 5, 7, 11, 13, 17, 19, 23],
        flags: 0x202,
        retired: 0,
    }
}

fn reference(steps: u64) -> Frame {
    let mut frame = seed();
    for _ in 0..steps {
        for (i, lane) in frame.gpr.iter_mut().enumerate() {
            *lane = lane.wrapping_add(i as u64 + 1);
        }
        frame.flags = frame.flags.wrapping_add(frame.gpr[0]);
        frame.retired += 1;
    }
    frame
}

fn control(frame: &mut Frame, stop: u64) -> Control {
    Control {
        stop,
        fuel: stop + 1,
        interrupt: 0,
        expected_version: 9,
        actual_version: 9,
        output: frame,
    }
}

fn load_state(b: &mut FunctionBuilder<'_>, frame: Value) -> Vec<Value> {
    (0..10)
        .map(|i| b.ins().load(types::I64, MemFlags::trusted(), frame, i * 8))
        .collect()
}

fn store_state(b: &mut FunctionBuilder<'_>, frame: Value, values: &[Value]) {
    for (i, &value) in values.iter().enumerate() {
        b.ins()
            .store(MemFlags::trusted(), value, frame, (i * 8) as i32);
    }
}

fn update(b: &mut FunctionBuilder<'_>, values: &mut [Value]) {
    for (i, value) in values[..8].iter_mut().enumerate() {
        *value = b.ins().iadd_imm(*value, i as i64 + 1);
    }
    values[8] = b.ins().iadd(values[8], values[0]);
    values[9] = b.ins().iadd_imm(values[9], 1);
}

fn readonly(b: &mut FunctionBuilder<'_>, context: Value, offset: usize) -> Value {
    b.ins().load(
        types::I64,
        MemFlags::trusted().with_readonly(),
        context,
        offset as i32,
    )
}

fn keep_running(b: &mut FunctionBuilder<'_>, context: Value, retired: Value, checks: u8) -> Value {
    let stop = readonly(b, context, offset_of!(Control, stop));
    let mut valid = b.ins().icmp(IntCC::UnsignedLessThan, retired, stop);
    if checks >= 1 {
        let fuel = readonly(b, context, offset_of!(Control, fuel));
        let within = b.ins().icmp(IntCC::UnsignedLessThan, retired, fuel);
        valid = b.ins().band(valid, within);
    }
    if checks >= 2 {
        let interrupt = readonly(b, context, offset_of!(Control, interrupt));
        let clear = b.ins().icmp_imm(IntCC::Equal, interrupt, 0);
        valid = b.ins().band(valid, clear);
    }
    if checks >= 3 {
        let expected = readonly(b, context, offset_of!(Control, expected_version));
        let actual = readonly(b, context, offset_of!(Control, actual_version));
        let same = b.ins().icmp(IntCC::Equal, expected, actual);
        valid = b.ins().band(valid, same);
    }
    valid
}

fn counts(context: &cranelift_codegen::Context) -> StaticCounts {
    let compiled = context.compiled_code().unwrap();
    let mut out = StaticCounts {
        bytes: compiled.code_buffer().len(),
        ..StaticCounts::default()
    };
    // Text stays in memory. These broad categories are estimates, not exact
    // regalloc spill counts, dynamic instruction counts, or a disassembler API.
    for line in compiled.vcode.as_deref().unwrap_or_default().lines() {
        let line = line.trim().to_ascii_lowercase();
        let instruction = line
            .split_once(':')
            .map_or(line.as_str(), |(_, rest)| rest.trim());
        let op = instruction.split_whitespace().next().unwrap_or_default();
        out.move_like += usize::from(op.starts_with("mov") || op == "orr");
        let stack = instruction.contains("%rsp")
            || instruction.contains("%rbp")
            || instruction.contains("[sp")
            || instruction.contains("[fp")
            || instruction.contains("[x29")
            || instruction.contains("[x31");
        let memory = op.starts_with("ld")
            || op.starts_with("st")
            || (op.starts_with("mov") && instruction.contains('('));
        out.stack_ref += usize::from(stack && memory);
        let stack_operand = stack || instruction.contains(" sp") || instruction.contains(",sp");
        out.frame_like += usize::from(
            ((op == "pushq" || op == "popq") && instruction.contains("%rbp"))
                || ((op == "stp" || op == "ldp") && instruction.contains("x29"))
                || ((op.starts_with("sub") || op.starts_with("add")) && stack_operand)
                || (op.starts_with("mov")
                    && instruction.contains("%rsp")
                    && instruction.contains("%rbp"))
                || (op == "mov" && instruction.contains("x29") && instruction.contains("sp")),
        );
    }
    out
}

fn define_body(
    module: &mut JITModule,
    id: FuncId,
    signature: &Signature,
    spec: Spec,
    next: Option<FuncId>,
) -> StaticCounts {
    let ssa = matches!(spec.route, Route::RustSsa | Route::SsaLoop | Route::SsaTail);
    let local_loop = matches!(spec.route, Route::FrameLoop | Route::SsaLoop);
    let single = matches!(spec.route, Route::RustFrame | Route::RustSsa);
    let mut context = module.make_context();
    context.func.signature = signature.clone();
    context.set_disasm(true);
    let callee = next.map(|id| module.declare_func_in_func(id, &mut context.func));
    let mut fc = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut context.func, &mut fc);
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    if spec.route == Route::Empty {
        b.ins().return_(&[]);
    } else {
        let args = b.block_params(entry).to_vec();
        let context_ptr = args[if ssa { 10 } else { 1 }];
        let frame = (!ssa).then_some(args[0]);
        let loop_block = b.create_block();
        if ssa {
            for _ in 0..10 {
                b.append_block_param(loop_block, types::I64);
            }
        }
        let input: Vec<_> = if ssa {
            args[..10].iter().copied().map(Into::into).collect()
        } else {
            vec![]
        };
        b.ins().jump(loop_block, &input);
        b.switch_to_block(loop_block);
        let mut values = if ssa {
            b.block_params(loop_block).to_vec()
        } else {
            load_state(&mut b, frame.unwrap())
        };
        update(&mut b, &mut values);
        if !ssa {
            store_state(&mut b, frame.unwrap(), &values);
        }
        let finish = b.create_block();
        if single {
            // Exercise check loads/branches even though Rust selects the next entry.
            if spec.checks != 0 {
                let valid = keep_running(&mut b, context_ptr, values[9], spec.checks);
                let accepted = b.create_block();
                b.ins().brif(valid, accepted, &[], finish, &[]);
                b.switch_to_block(accepted);
                b.ins().jump(finish, &[]);
            } else {
                b.ins().jump(finish, &[]);
            }
        } else {
            let valid = keep_running(&mut b, context_ptr, values[9], spec.checks);
            let next_block = b.create_block();
            b.ins().brif(valid, next_block, &[], finish, &[]);
            b.switch_to_block(next_block);
            if local_loop {
                let input: Vec<_> = if ssa {
                    values.iter().copied().map(Into::into).collect()
                } else {
                    vec![]
                };
                b.ins().jump(loop_block, &input);
            } else {
                let tail_args = if ssa {
                    let mut params = values.clone();
                    params.push(context_ptr);
                    params
                } else {
                    vec![frame.unwrap(), context_ptr]
                };
                b.ins().return_call(callee.unwrap(), &tail_args);
            }
        }
        b.switch_to_block(finish);
        if ssa {
            let output = readonly(&mut b, context_ptr, offset_of!(Control, output));
            store_state(&mut b, output, &values);
        }
        b.ins().return_(&[]);
    }
    b.seal_all_blocks();
    b.finalize();
    module
        .define_function(id, &mut context)
        .expect("fixture body compilation");
    counts(&context)
}

fn compile(spec: Spec) -> Ready {
    let builder = JITBuilder::with_flags(
        &[
            ("opt_level", "speed"),
            (
                "preserve_frame_pointers",
                if spec.frame_pointers { "true" } else { "false" },
            ),
            ("unwind_info", "false"),
        ],
        default_libcall_names(),
    )
    .unwrap();
    let mut module = JITModule::new(builder);
    assert_eq!(module.target_config().pointer_type(), types::I64);
    let mut c_signature = module.make_signature();
    c_signature.params.extend([AbiParam::new(types::I64); 2]);
    let ssa = matches!(spec.route, Route::RustSsa | Route::SsaLoop | Route::SsaTail);
    let tail = !matches!(spec.route, Route::Empty | Route::RustFrame);
    let mut signature = if tail {
        Signature::new(CallConv::Tail)
    } else {
        c_signature.clone()
    };
    if tail {
        signature
            .params
            .extend(vec![AbiParam::new(types::I64); if ssa { 11 } else { 2 }]);
    }
    let first = module
        .declare_function("fixture_first", Linkage::Local, &signature)
        .unwrap();
    let linked = matches!(spec.route, Route::FrameTail | Route::SsaTail);
    let second = linked.then(|| {
        module
            .declare_function("fixture_second", Linkage::Local, &signature)
            .unwrap()
    });
    let mut body = define_body(&mut module, first, &signature, spec, second);
    if let Some(second) = second {
        body.add(define_body(
            &mut module,
            second,
            &signature,
            spec,
            Some(first),
        ));
    }
    let mut wrapper = StaticCounts::default();
    let entry_id = if tail {
        let id = module
            .declare_function("fixture_bridge", Linkage::Local, &c_signature)
            .unwrap();
        let mut context = module.make_context();
        context.func.signature = c_signature;
        context.set_disasm(true);
        let callee = module.declare_func_in_func(first, &mut context.func);
        let mut fc = FunctionBuilderContext::new();
        let mut b = FunctionBuilder::new(&mut context.func, &mut fc);
        let block = b.create_block();
        b.append_block_params_for_function_params(block);
        b.switch_to_block(block);
        let frame = b.block_params(block)[0];
        let context_ptr = b.block_params(block)[1];
        let args = if ssa {
            let mut args = load_state(&mut b, frame);
            args.push(context_ptr);
            args
        } else {
            vec![frame, context_ptr]
        };
        b.ins().call(callee, &args);
        b.ins().return_(&[]);
        b.seal_all_blocks();
        b.finalize();
        module
            .define_function(id, &mut context)
            .expect("C to Tail bridge compilation");
        wrapper = counts(&context);
        id
    } else {
        first
    };
    module.finalize_definitions().expect("fixture linking");
    // SAFETY: only the generated C bridge is exposed to Rust, never a Tail ABI.
    let entry = unsafe {
        std::mem::transmute::<*const u8, unsafe extern "C" fn(*mut Frame, *const Control)>(
            module.get_finalized_function(entry_id),
        )
    };
    Ready {
        spec,
        module: Some(module),
        entry,
        body,
        wrapper,
        bodies: if linked { 2 } else { 1 },
    }
}

fn run(ready: &Ready, frame: &mut Frame, context: &Control, loops: u64) {
    let dispatch = matches!(
        ready.spec.route,
        Route::Empty | Route::RustFrame | Route::RustSsa
    );
    // SAFETY: Frame/Control match the generated layouts; JIT storage is live.
    unsafe {
        if dispatch {
            let entry = black_box(ready.entry);
            for _ in 0..loops {
                entry(black_box(frame), black_box(context));
            }
        } else {
            (black_box(ready.entry))(black_box(frame), black_box(context));
        }
    }
}

fn validate(ready: &Ready, loops: u64, expected: Frame) -> usize {
    let mut assertions = 0;
    // Materialize and compare every complete boundary state in a short prefix.
    // Linked timing preserves those same ten live values at every edge, without
    // adding a diagnostic memory spill or a Rust callback to the hot path.
    for stop in 1..=17 {
        let mut frame = seed();
        let context = control(&mut frame, stop);
        run(ready, &mut frame, &context, stop);
        assert_eq!(
            frame,
            if ready.spec.route == Route::Empty {
                seed()
            } else {
                reference(stop)
            }
        );
        assertions += 1;
    }
    if ready.spec.checks > 0 {
        for invalid in 1..=ready.spec.checks {
            let mut frame = seed();
            let mut context = control(&mut frame, 17);
            match invalid {
                1 => context.fuel = 1,
                2 => context.interrupt = 1,
                3 => context.actual_version += 1,
                _ => unreachable!(),
            }
            run(ready, &mut frame, &context, 1);
            assert_eq!(frame, reference(1));
            assertions += 1;
        }
    }
    let mut frame = seed();
    let context = control(&mut frame, loops);
    run(ready, &mut frame, &context, loops);
    assert_eq!(
        frame,
        if ready.spec.route == Route::Empty {
            seed()
        } else {
            expected
        }
    );
    assertions + 1
}

fn option(name: &str, default: u64, max: u64) -> u64 {
    let value = std::env::var(name).map_or(default, |value| value.parse().expect("integer option"));
    assert!(value > 0 && value <= max, "option outside bounded range");
    value
}

fn main() {
    let samples = option("ADV_BOUNDARY_SAMPLES", 7, 31);
    let loops = option("ADV_BOUNDARY_LOOPS", 1_000_000, 100_000_000);
    // Cranelift 0.125's x64 return_call emitter requires frame pointers. Keep
    // its no-FP controls on the C ABI; AArch64 can compare the Tail bodies.
    let no_fp_controls = if cfg!(target_arch = "x86_64") {
        [
            ("empty_c_no_fp", Route::Empty, 0, false),
            ("frame_rust_no_fp", Route::RustFrame, 0, false),
        ]
    } else {
        [
            ("frame_tail_no_fp", Route::FrameTail, 0, false),
            ("ssa11_tail_no_fp", Route::SsaTail, 0, false),
        ]
    };
    let specs = [
        ("empty_c", Route::Empty, 0, true),
        ("frame_rust", Route::RustFrame, 0, true),
        ("ssa11_rust", Route::RustSsa, 0, true),
        ("frame_loop", Route::FrameLoop, 0, true),
        ("frame_tail", Route::FrameTail, 0, true),
        ("frame_tail_fuel", Route::FrameTail, 1, true),
        ("frame_tail_interrupt", Route::FrameTail, 2, true),
        ("frame_tail_version", Route::FrameTail, 3, true),
        ("ssa11_loop", Route::SsaLoop, 0, true),
        ("ssa11_tail", Route::SsaTail, 0, true),
        ("ssa11_tail_fuel", Route::SsaTail, 1, true),
        ("ssa11_tail_interrupt", Route::SsaTail, 2, true),
        ("ssa11_tail_version", Route::SsaTail, 3, true),
        no_fp_controls[0],
        no_fp_controls[1],
    ];
    let expected = reference(loops);
    let ready: Vec<_> = specs
        .into_iter()
        .map(|(name, route, checks, frame_pointers)| {
            compile(Spec {
                name,
                route,
                checks,
                frame_pointers,
            })
        })
        .collect();
    println!(
        "# arch={} os={} loops={loops} samples={samples} opt=speed unwind=false",
        std::env::consts::ARCH,
        std::env::consts::OS
    );
    println!(
        "# denominator=state_updates empty_c_denominator=C_calls; linked_transfers=loops-1; one_bridge_per_chain"
    );
    println!(
        "# static_counts=rough_vcode_text_estimates summed_over_bodies; not_dynamic_or_exact_spill_counts"
    );
    println!(
        "# state=8xI64_GPR_flags_retired; ssa_signature=GPR0..7_flags_retired_context; readonly_control=true"
    );
    println!(
        "# no_fp_controls={} x64_tail_requires_fp=true",
        if cfg!(target_arch = "x86_64") {
            "C_ABI"
        } else {
            "Tail_ABI"
        }
    );
    println!(
        "case,sample,ns_per_boundary,code_bytes,body_bytes,bridge_bytes,bodies,move_like_est,stack_memory_est,frame_like_est,verified_boundaries,full_state_assertions,checks,frame_pointers"
    );
    let assertions: Vec<_> = ready
        .iter()
        .map(|ready| validate(ready, loops, expected))
        .collect();
    let mut completed = 0;
    // Rotate ordering deterministically to reduce monotonic thermal/order bias.
    for sample in 0..samples as usize {
        for index in 0..ready.len() {
            let position = (index + sample * 7) % ready.len();
            let ready = &ready[position];
            let mut frame = seed();
            let context = control(&mut frame, loops);
            let start = Instant::now();
            run(ready, &mut frame, &context, loops);
            let elapsed = start.elapsed();
            assert_eq!(
                black_box(frame),
                if ready.spec.route == Route::Empty {
                    seed()
                } else {
                    expected
                }
            );
            println!(
                "{},{sample},{:.6},{},{},{},{},{},{},{},{loops},{},{},{}",
                ready.spec.name,
                elapsed.as_nanos() as f64 / loops as f64,
                ready.body.bytes + ready.wrapper.bytes,
                ready.body.bytes,
                ready.wrapper.bytes,
                ready.bodies,
                ready.body.move_like,
                ready.body.stack_ref,
                ready.body.frame_like,
                assertions[position] + 1,
                ready.spec.checks,
                ready.spec.frame_pointers
            );
            completed += 1;
        }
    }
    println!(
        "# completed samples={completed} cases={} verified_boundaries={} full_state_assertions={}",
        ready.len(),
        loops * completed,
        assertions.iter().sum::<usize>() + completed as usize
    );
}

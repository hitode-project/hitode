//! Executable checks of the pinned backend's recovery metadata contracts.
use cranelift_codegen::{
    Context,
    ir::{self, AbiParam, InstBuilder, MemFlags, types},
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};

fn probe(
    architecture: &str,
    label: bool,
    map: bool,
    call: bool,
) -> cranelift_codegen::CompiledCode {
    let mut flags = settings::builder();
    flags.set("opt_level", "speed").unwrap();
    let isa = cranelift_codegen::isa::lookup_by_name(architecture)
        .unwrap()
        .finish(settings::Flags::new(flags))
        .unwrap();
    let mut f = ir::Function::new();
    f.signature.call_conv = isa.default_call_conv();
    f.signature
        .params
        .extend([AbiParam::new(types::I64), AbiParam::new(types::I64)]);
    f.signature.returns.push(AbiParam::new(types::I64));
    f.dfg.collect_debug_info();
    let mut fc = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut f, &mut fc);
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    let pointer = b.block_params(entry)[0];
    let value = b.block_params(entry)[1];
    b.set_srcloc(ir::SourceLoc::new(10));
    let dead = b.ins().iadd_imm(value, 12345);
    if label {
        b.set_val_label(dead, ir::ValueLabel::from_u32(1));
    }
    let slot = b.create_sized_stack_slot(ir::StackSlotData::new(
        ir::StackSlotKind::ExplicitSlot,
        8,
        3,
    ));
    if map {
        b.ins().stack_store(value, slot, 0);
    }
    b.set_srcloc(ir::SourceLoc::new(20));
    let (inst, result) = if call {
        let mut signature = ir::Signature::new(isa.default_call_conv());
        signature.returns.push(AbiParam::new(types::I64));
        let signature = b.import_signature(signature);
        let inst = b.ins().call_indirect(signature, pointer, &[]);
        (inst, b.inst_results(inst)[0])
    } else {
        let result = b.ins().load(types::I64, MemFlags::new(), pointer, 0);
        (b.func.dfg.value_def(result).inst().unwrap(), result)
    };
    if map {
        b.func.dfg.append_user_stack_map_entry(
            inst,
            ir::UserStackMapEntry {
                ty: types::I64,
                slot,
                offset: 0,
            },
        );
    }
    b.ins().return_(&[result]);
    b.seal_all_blocks();
    b.finalize();
    let mut context = Context::for_function(f);
    context
        .compile(&*isa, &mut Default::default())
        .unwrap()
        .clone()
}

#[test]
fn value_labels_do_not_keep_dead_recovery_values_alive() {
    for arch in architectures() {
        let plain = probe(arch, false, false, false);
        let labeled = probe(arch, true, false, false);
        assert_eq!(plain.code_buffer(), labeled.code_buffer());
        assert!(
            labeled
                .value_labels_ranges
                .get(&ir::ValueLabel::from_u32(1))
                .is_none_or(Vec::is_empty)
        );
    }
}

#[test]
fn stack_maps_cover_calls_but_not_scalar_loads() {
    for arch in architectures() {
        assert_eq!(
            probe(arch, false, true, true)
                .buffer
                .user_stack_maps()
                .len(),
            1
        );
        // The public API rejects a non-safepoint before lowering, including
        // release builds. This is a capability rejection, not a compiler bug.
        assert!(std::panic::catch_unwind(|| probe(arch, false, true, false)).is_err());
    }
}

fn architectures() -> Vec<&'static str> {
    let mut result = vec![if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    }];
    if cfg!(all(feature = "cross-dump", target_arch = "x86_64")) {
        result.push("aarch64");
    }
    result
}

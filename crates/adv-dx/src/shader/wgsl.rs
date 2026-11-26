//! Direct WGSL emission for vertex, pixel and compute shaders.

mod alu;
mod interface;
mod memory;
#[cfg(feature = "gpu")]
pub(crate) mod pulled;
mod variant_interface;

pub use variant_interface::STATISTICS_SLOT;

use super::{analysis::Reflection, ir::*};
use std::{
    collections::{HashMap, HashSet},
    fmt::Write,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BindingLocation {
    pub register: Register,
    pub slot: u32,
    pub group: u32,
    pub binding: u32,
}

#[derive(Clone, Debug)]
pub struct Translation {
    pub source: String,
    pub entry_point: &'static str,
    pub reflection: Reflection,
    pub locations: Vec<BindingLocation>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Options<'a> {
    /// Formats of the typed UAV views, keyed by their DXBC register slot.
    pub storage_formats: &'a [(u32, naga::StorageFormat)],
}

pub fn emit(module: &Module, options: Options<'_>) -> Result<Translation, Error> {
    let (module, reflection) = passes::prepare(module)?;
    emit_entry(module, reflection, options, None)
}

/// Emit a raster or compute entry using the variant selected for this draw.
#[cfg(feature = "gpu")]
pub fn emit_variant(
    module: &Module,
    variant: &super::variant::Variant,
    fragment: Option<&Reflection>,
) -> Result<Translation, Error> {
    let (mut module, mut reflection) = passes::prepare(module)?;
    let interface = passes::variant::apply(&mut module, &reflection, variant, fragment)?;
    if interface.pulled || variant.storage_varyings() {
        return Err(Error::Invalid("pulled shader requires an expansion plan"));
    }
    if variant.early_tests() {
        reflection.global_flags |=
            super::dxbc::fields::D3D11_SB_GLOBAL_FLAG_FORCE_EARLY_DEPTH_STENCIL;
    }
    emit_entry(
        module,
        reflection,
        Options {
            storage_formats: variant.storage_formats(),
        },
        Some(&interface),
    )
}

/// Emit the pixel stage for an expanded primitive with storage-backed inputs.
#[cfg(feature = "gpu")]
pub fn emit_expanded_fragment(
    module: &Module,
    variant: &super::variant::Variant,
    producer: &Reflection,
    vertices: u32,
    registers: u32,
) -> Result<Translation, Error> {
    let (mut module, mut reflection) = passes::prepare(module)?;
    let mut interface = passes::variant::apply(&mut module, &reflection, variant, None)?;
    if variant.storage_varyings() {
        passes::variant::storage_varyings(
            &mut interface,
            &reflection,
            producer,
            &reflection,
            vertices,
            registers,
        );
    }
    if variant.early_tests() {
        reflection.global_flags |=
            super::dxbc::fields::D3D11_SB_GLOBAL_FLAG_FORCE_EARLY_DEPTH_STENCIL;
    }
    emit_entry(
        module,
        reflection,
        Options {
            storage_formats: variant.storage_formats(),
        },
        Some(&interface),
    )
}

fn emit_entry(
    module: Module,
    reflection: Reflection,
    options: Options<'_>,
    interface: Option<&passes::variant::Interface>,
) -> Result<Translation, Error> {
    if reflection.doubles {
        return Err(Error::DoublePrecision);
    }
    let entry_point = match module.stage {
        Stage::Vertex => "vertex_main",
        Stage::Pixel => "fragment_main",
        Stage::Compute => "compute_main",
        Stage::Geometry | Stage::Hull | Stage::Domain => {
            return Err(Error::UnsupportedStage(module.stage));
        }
    };
    let mut writer = Writer::new(&module, &reflection, options.storage_formats);
    if module.stage == Stage::Vertex && interface.is_some_and(|i| i.native_clip) {
        writer.source.push_str("enable clip_distances;\n");
    }
    writer.source.push_str(alu::HELPERS);
    writer.declarations(&module, &reflection)?;
    writer.entry(entry_point, &module, &reflection, interface)?;
    let terminates = writer.block(&module.body)?;
    if !terminates && !writer.result.is_empty() {
        writer.line(&format!("return {};", writer.result));
    }
    writer.source.push_str("}\n");
    let Writer {
        source, locations, ..
    } = writer;
    Ok(Translation {
        source,
        entry_point,
        reflection,
        locations,
    })
}

struct Writer<'a> {
    source: String,
    stage: Stage,
    result: String,
    serial: u32,
    bindings: HashMap<(Register, u32), BindingKind>,
    locations: Vec<BindingLocation>,
    storage_formats: &'a [(u32, naga::StorageFormat)],
    atomic_bindings: &'a HashSet<(Register, u32)>,
    depth: usize,
}

impl<'a> Writer<'a> {
    fn new(
        module: &Module,
        reflection: &'a Reflection,
        storage_formats: &'a [(u32, naga::StorageFormat)],
    ) -> Self {
        Self {
            source: String::new(),
            stage: module.stage,
            result: String::new(),
            serial: 0,
            bindings: HashMap::new(),
            locations: Vec::new(),
            storage_formats,
            atomic_bindings: &reflection.atomic_bindings,
            depth: 0,
        }
    }

    fn line(&mut self, line: &str) {
        for _ in 0..self.depth {
            self.source.push_str("    ");
        }
        writeln!(self.source, "{line}").unwrap();
    }

    fn open(&mut self, line: &str) {
        self.line(&format!("{line} {{"));
        self.depth += 1;
    }

    fn close(&mut self) {
        self.depth -= 1;
        self.line("}");
    }

    fn block(&mut self, block: &[Statement]) -> Result<bool, Error> {
        for statement in block {
            match statement {
                Statement::Execute(op) => {
                    self.instruction(op)?;
                    if matches!(op.opcode, Opcode::Break | Opcode::Continue | Opcode::Ret) {
                        return Ok(true);
                    }
                }
                Statement::If {
                    test,
                    accept,
                    reject,
                } => {
                    self.open(&format!("if {}", self.test(test)?));
                    self.block(accept)?;
                    self.close();
                    if let Some(reject) = reject {
                        self.open("else");
                        self.block(reject)?;
                        self.close();
                    }
                }
                Statement::Loop(body) => {
                    self.open("loop");
                    self.block(body)?;
                    self.close();
                }
                Statement::Switch { selector, cases } => {
                    let selector = self.read(selector, ValueType::Uint, false)?;
                    self.switch(&format!("({selector}).x"), cases)?;
                }
                Statement::Declare(_)
                | Statement::ImmediateConstants(_)
                | Statement::CustomData { .. } => {}
            }
        }
        Ok(false)
    }

    fn switch(&mut self, selector: &str, cases: &[Case]) -> Result<(), Error> {
        self.open(&format!("switch {selector}"));
        for (index, case) in cases.iter().enumerate() {
            let label = case
                .value
                .map_or("default:".into(), |value| format!("case {value}u:"));
            self.open(&label);
            // WGSL has no fallthrough. Emit the suffix reachable from this label.
            for case in &cases[index..] {
                if self.block(&case.body)? {
                    break;
                }
            }
            self.close();
        }
        if cases.iter().all(|case| case.value.is_some()) {
            self.line("default: {}");
        }
        self.close();
        Ok(())
    }

    fn instruction(&mut self, op: &Instruction) -> Result<(), Error> {
        use Opcode::*;
        if op.opcode == Nop {
            return Ok(());
        }
        if matches!(
            op.opcode,
            Break | Breakc | Continue | Continuec | Ret | Retc | Discard
        ) {
            let text = if matches!(op.opcode, Break | Breakc) {
                "break;".into()
            } else if matches!(op.opcode, Continue | Continuec) {
                "continue;".into()
            } else if op.opcode == Discard {
                "discard;".into()
            } else if self.result.is_empty() {
                "return;".into()
            } else {
                format!("return {};", self.result)
            };
            if matches!(op.opcode, Breakc | Continuec | Retc | Discard) {
                self.open(&format!("if {}", self.test(op)?));
                self.line(&text);
                self.close();
            } else {
                self.line(&text);
            }
            return Ok(());
        }
        if self.stage == Stage::Geometry
            && matches!(
                op.opcode,
                Emit | Cut | Emitthencut | EmitStream | CutStream | EmitthencutStream
            )
        {
            let stream = match op.sources.first() {
                Some(source) => source
                    .register()
                    .and_then(|(_, indices)| indices.first().and_then(Index::direct))
                    .ok_or(Error::Invalid("geometry stream index"))?,
                None => 0,
            };
            if matches!(
                op.opcode,
                Emit | Emitthencut | EmitStream | EmitthencutStream
            ) {
                self.line(&format!("dx_emit({stream}u, o);"));
            }
            if matches!(op.opcode, Cut | Emitthencut | CutStream | EmitthencutStream) {
                self.line(&format!("dx_cut({stream}u);"));
            }
            return Ok(());
        }
        if op.opcode == Sync {
            return self.barrier(op);
        }
        let (dst_types, src_types) = op.opcode.types();
        let serial = self.serial;
        self.serial += 1;
        let mut args = Vec::new();
        for (i, (operand, ty)) in op.sources.iter().zip(src_types).enumerate() {
            if *ty == ValueType::Binding {
                continue;
            }
            let value = self.read(operand, *ty, op.flush_inputs)?;
            let name = format!("a{serial}_{i}");
            self.line(&format!("let {name} = {value};"));
            args.push(name);
        }
        let mut writes = Vec::new();
        let resource =
            if src_types.contains(&ValueType::Binding) || dst_types.contains(&ValueType::Binding) {
                self.resource(op, &args)?
            } else {
                None
            };
        for (i, (operand, ty)) in op.destinations.iter().zip(dst_types).enumerate() {
            if *ty == ValueType::Binding || matches!(operand.register(), Some((Register::Null, _)))
            {
                continue;
            }
            let destination = self.register(operand)?;
            let name = format!("d{serial}_{i}");
            let mut values = Vec::new();
            for (lane, component) in LANES.chars().enumerate() {
                if operand.components.mask() & (1 << lane) == 0 {
                    continue;
                }
                let mut value = if let Some(resource) = &resource {
                    if i != 0 {
                        return Err(Error::UnsupportedOpcode(op.opcode));
                    }
                    format!("({resource}).{component}")
                } else {
                    alu::expression(op.opcode, &args, i, lane)?
                };
                if op.flush_outputs && *ty == ValueType::Float {
                    value = format!("dx_flush({value})");
                }
                if op.saturate {
                    value = format!("dx_sat({value})");
                }
                values.push((component, value));
            }
            for (component, value) in values {
                self.line(&format!("let {name}_{component} = {value};"));
                writes.push(format!("{destination}.{component} = {name}_{component};"));
            }
        }
        if op.destinations.is_empty() && resource.is_none() {
            return Err(Error::UnsupportedOpcode(op.opcode));
        }
        for write in writes {
            self.line(&write);
        }
        Ok(())
    }

    fn test(&self, test: &Instruction) -> Result<String, Error> {
        let value = self.read(&test.sources[0], ValueType::Uint, false)?;
        let comparison = if test.test_nonzero { "!=" } else { "==" };
        Ok(format!("({value}).x {comparison} 0u"))
    }

    fn index(&self, index: &Index, stride: u64) -> Result<String, Error> {
        match index {
            Index::Direct(value) => Ok(format!("{}u", (*value / stride) as u32)),
            Index::Relative { base, register } => {
                let register = self.read(register, ValueType::Uint, false)?;
                Ok(format!("({}u + ({register}).x)", (*base / stride) as u32))
            }
        }
    }

    fn register(&self, operand: &Operand) -> Result<String, Error> {
        let (register, indices) = operand
            .register()
            .ok_or(Error::Invalid("expected register"))?;
        let name = match register {
            Register::Temp => format!("r[{}]", self.index(&indices[0], 1)?),
            Register::Input if self.stage == Stage::Geometry => {
                format!(
                    "v[{}][{}]",
                    self.index(&indices[0], 1)?,
                    self.index(&indices[1], 1)?
                )
            }
            Register::Input => format!("v[{}]", self.index(&indices[0], 1)?),
            Register::InputControlPoint => {
                format!(
                    "v_cp[{}][{}]",
                    self.index(&indices[0], 1)?,
                    self.index(&indices[1], 1)?
                )
            }
            Register::InputPatchConstant => format!("v_patch[{}]", self.index(&indices[0], 1)?),
            Register::InputDomainPoint => "v_domain".into(),
            Register::InputGsInstanceId => "v_gs_instance".into(),
            Register::Output => format!("o[{}]", self.index(&indices[0], 1)?),
            Register::IndexableTemp => {
                let array = indices[0]
                    .direct()
                    .ok_or(Error::UnsupportedRegister(register))?;
                format!("x{array}[{}]", self.index(&indices[1], 1)?)
            }
            Register::ImmediateConstantBuffer => format!("dx_icb({})", self.index(&indices[0], 1)?),
            Register::OutputDepth
            | Register::OutputDepthGreaterEqual
            | Register::OutputDepthLessEqual => "o_depth".into(),
            Register::OutputCoverageMask => "o_coverage".into(),
            Register::InputCoverageMask => "v_coverage".into(),
            Register::InputPrimitiveId => "v_primitive".into(),
            Register::InputThreadId => "dx_dispatch_id".into(),
            Register::InputThreadGroupId => "dx_group_id".into(),
            Register::InputThreadIdInGroup => "dx_local_id".into(),
            Register::InputThreadIdInGroupFlattened => "dx_local_index".into(),
            other => return Err(Error::UnsupportedRegister(other)),
        };
        Ok(name)
    }

    fn read(&self, operand: &Operand, ty: ValueType, flush: bool) -> Result<String, Error> {
        let value = match &operand.value {
            OperandValue::Immediate(value) => vector(*value),
            OperandValue::Constant { slot, byte_offset } => {
                let slot = slot
                    .direct()
                    .ok_or(Error::UnsupportedRegister(Register::ConstantBuffer))?;
                format!("dx_cb{slot}({})", self.index(byte_offset, 16)?)
            }
            OperandValue::Register { .. } => self.register(operand)?,
            OperandValue::Immediate64(_) => {
                return Err(Error::DoublePrecision);
            }
        };
        let swizzle: String = operand
            .components
            .swizzle()
            .map(|lane| LANES.as_bytes()[lane as usize] as char)
            .into_iter()
            .collect();
        let value = format!("({value}).{swizzle}");
        let value = match (operand.modifier, ty) {
            (Modifier::None, _) => value,
            (Modifier::Negate, ValueType::Int | ValueType::Uint) => {
                format!("(vec4<u32>(0u) - {value})")
            }
            (Modifier::Absolute, ValueType::Int | ValueType::Uint) => {
                format!("bitcast<vec4<u32>>(abs(bitcast<vec4<i32>>({value})))")
            }
            (Modifier::AbsoluteNegate, ValueType::Int | ValueType::Uint) => {
                format!("(vec4<u32>(0u) - bitcast<vec4<u32>>(abs(bitcast<vec4<i32>>({value}))))")
            }
            (Modifier::Negate, _) => format!("(dx_flush4({value}) ^ vec4<u32>(0x80000000u))"),
            (Modifier::Absolute, _) => format!("(dx_flush4({value}) & vec4<u32>(0x7fffffffu))"),
            (Modifier::AbsoluteNegate, _) => {
                format!("(dx_flush4({value}) | vec4<u32>(0x80000000u))")
            }
        };
        Ok(if flush && ty == ValueType::Float {
            format!("dx_flush4({value})")
        } else {
            value
        })
    }
}

const LANES: &str = "xyzw";

fn vector([a, b, c, d]: Vector) -> String {
    format!("vec4<u32>({a}u, {b}u, {c}u, {d}u)")
}

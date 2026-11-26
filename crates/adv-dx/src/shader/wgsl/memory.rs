mod border;
mod typed_buffer;

#[cfg(feature = "gpu")]
pub(crate) use typed_buffer::clear_pattern;

use super::*;
use crate::shader::dxbc::fields::{
    D3D10_SB_INSTRUCTION_RETURN_UINT, D3D11_SB_SYNC_THREAD_GROUP_SHARED_MEMORY,
    D3D11_SB_SYNC_THREADS_IN_GROUP, D3D11_SB_SYNC_UNORDERED_ACCESS_VIEW_MEMORY_GLOBAL,
    D3D11_SB_SYNC_UNORDERED_ACCESS_VIEW_MEMORY_GROUP,
};
use naga::common::wgsl::ToWgsl;

pub(super) const EXACT_UNORM: &str = r#"
fn dx_unorm_exact(value: f32, bits: u32) -> f32 {
    let maximum = (1u << bits) - 1u;
    let code = u32(round(value * f32(maximum)));
    if code == 0u { return 0.0; }
    if code == maximum { return 1.0; }
    let top = 31u - countLeadingZeros(code);
    let significand = code << (23u - top);
    let rounded = significand + (significand + maximum / 2u) / maximum;
    return bitcast<f32>(((126u + top - bits) << 23u) + rounded);
}
"#;

impl Writer<'_> {
    pub(super) fn declarations(
        &mut self,
        module: &Module,
        reflection: &Reflection,
    ) -> Result<(), Error> {
        let mut storage_slot = reflection
            .bindings
            .iter()
            .filter(|b| b.register == Register::UnorderedAccessView)
            .map(|b| b.slot + 1)
            .max()
            .unwrap_or(0);
        if self
            .options
            .storage_formats
            .iter()
            .any(|&(_, format)| format == naga::StorageFormat::Rg11b10Ufloat)
        {
            self.source.push_str(typed_buffer::PACK_UFLOAT);
        }
        for binding in &reflection.bindings {
            let slot = binding.slot;
            self.bindings.insert((binding.register, slot), binding.kind);
            let name = resource_name(binding.register, slot)?;
            let (group, physical_slot) = match binding.kind {
                BindingKind::ConstantBuffer { bytes, .. } => {
                    let rows = (bytes / 16).max(1);
                    self.line(&format!(
                        "@group(0) @binding({slot}) var<uniform> {name}: array<vec4<u32>, {rows}>;"
                    ));
                    self.line(&format!("fn dx_cb{slot}(i: u32) -> vec4<u32> {{ if i < {}u {{ return {name}[i]; }} return vec4<u32>(); }}", bytes / 16));
                    (0, slot)
                }
                BindingKind::Sampler(mode) => {
                    let ty = match mode {
                        SamplerMode::Default => "sampler",
                        SamplerMode::Comparison => "sampler_comparison",
                        SamplerMode::Mono => {
                            return Err(Error::UnsupportedRegister(binding.register));
                        }
                    };
                    self.line(&format!("@group(2) @binding({slot}) var {name}: {ty};"));
                    (2, slot)
                }
                BindingKind::Typed {
                    dimension: ResourceDimension::Buffer,
                    returns,
                    ..
                } => {
                    let writable = binding.register == Register::UnorderedAccessView;
                    let format = self.buffer_format(binding.register, slot)?;
                    self.check_typed_format(returns[0], format)?;
                    let physical_slot = if writable {
                        slot
                    } else {
                        let current = storage_slot;
                        storage_slot += 1;
                        current
                    };
                    self.declare_buffer(
                        &name,
                        physical_slot,
                        writable,
                        self.atomic_bindings.contains(&(binding.register, slot))
                            || (writable && typed_buffer::Layout::new(format)?.bits < 32),
                    );
                    (3, physical_slot)
                }
                BindingKind::Typed {
                    dimension, returns, ..
                } if binding.register == Register::Resource => {
                    let dimension = match dimension {
                        ResourceDimension::Texture1D => "1d",
                        ResourceDimension::Texture2D => "2d",
                        ResourceDimension::Texture2DMs => "multisampled_2d",
                        ResourceDimension::Texture3D => "3d",
                        ResourceDimension::TextureCube => "cube",
                        ResourceDimension::Texture2DArray => "2d_array",
                        ResourceDimension::TextureCubeArray => "cube_array",
                        ResourceDimension::Texture1DArray
                        | ResourceDimension::Texture2DMsArray
                        | ResourceDimension::RawBuffer
                        | ResourceDimension::StructuredBuffer
                        | ResourceDimension::Buffer => {
                            return Err(Error::UnsupportedRegister(binding.register));
                        }
                    };
                    let ty = scalar(returns[0])?;
                    let texture = if self.options.depth_textures.contains(&slot)
                        || comparison_sample(&module.body, slot)
                    {
                        format!("texture_depth_{dimension}")
                    } else {
                        format!("texture_{dimension}<{ty}>")
                    };
                    self.line(&format!(
                        "@group(1) @binding({slot}) var {name}: {texture};"
                    ));
                    (1, slot)
                }
                BindingKind::Raw { .. } | BindingKind::Structured { .. } => {
                    let writable = binding.register == Register::UnorderedAccessView;
                    let physical_slot = if writable {
                        slot
                    } else {
                        let current = storage_slot;
                        storage_slot += 1;
                        current
                    };
                    let atomic = self.atomic_bindings.contains(&(binding.register, slot));
                    self.declare_buffer(&name, physical_slot, writable, atomic);
                    (3, physical_slot)
                }
                BindingKind::Typed {
                    dimension, returns, ..
                } if binding.register == Register::UnorderedAccessView => {
                    let format = self.storage_format(slot)?;
                    self.check_typed_format(returns[0], format)?;
                    let atomic = self.atomic_bindings.contains(&(binding.register, slot));
                    if self.storage_extent(slot).is_some() {
                        if !matches!(
                            format,
                            naga::StorageFormat::R32Uint
                                | naga::StorageFormat::R32Sint
                                | naga::StorageFormat::R32Float
                        ) {
                            return Err(Error::Invalid("buffer-backed UAV format"));
                        }
                        self.declare_buffer(&name, slot, true, atomic);
                        self.locations.push(BindingLocation {
                            register: binding.register,
                            slot,
                            group: 3,
                            binding: slot,
                            sampler_min_lod_bits: None,
                        });
                        continue;
                    }
                    if atomic
                        && (!self.options.image_atomics
                            || dimension != ResourceDimension::Texture2D)
                    {
                        return Err(Error::MissingStorageExtent(slot));
                    }
                    let dimension = storage_dimension(dimension)
                        .ok_or(Error::UnsupportedRegister(binding.register))?;
                    let key = (binding.register, Some(slot));
                    let access = match (
                        atomic,
                        reflection.reads.contains_key(&key),
                        reflection.writes.contains_key(&key),
                    ) {
                        (true, _, _) => "atomic",
                        (false, true, true) => "read_write",
                        (false, true, false) => "read",
                        (false, false, _) => "write",
                    };
                    self.line(&format!("@group(3) @binding({slot}) var {name}: texture_storage_{dimension}<{}, {access}>;", format.to_wgsl()));
                    (3, slot)
                }
                BindingKind::Typed { .. } => {
                    return Err(Error::UnsupportedRegister(binding.register));
                }
            };
            self.locations.push(BindingLocation {
                register: binding.register,
                slot,
                group,
                binding: physical_slot,
                sampler_min_lod_bits: None,
            });
        }
        for binding in &reflection.bindings {
            let slot = binding.slot;
            if binding.register == Register::UnorderedAccessView
                && reflection.counter_bindings.contains(&slot)
            {
                self.line(&format!("@group(3) @binding({storage_slot}) var<storage, read_write> dx_counter{slot}: atomic<u32>;"));
                self.counters.push((slot, storage_slot));
                storage_slot += 1;
            }
        }
        for statement in &module.body {
            match statement {
                Statement::ImmediateConstants(values) => {
                    if !values.is_empty() {
                        let rows: Vec<_> = values.iter().copied().map(vector).collect();
                        self.line(&format!("const dx_immediate = array({});", rows.join(", ")));
                        self.line(&format!("fn dx_icb(i: u32) -> vec4<u32> {{ if i < {}u {{ return dx_immediate[i]; }} return vec4<u32>(); }}", values.len()));
                    }
                }
                Statement::Declare(declaration) => {
                    if let Some(binding) = declaration.binding
                        && binding.register == Register::ThreadGroupSharedMemory
                    {
                        let count = match binding.kind {
                            BindingKind::Raw { bytes } => bytes / 4,
                            BindingKind::Structured { stride, elements } => stride / 4 * elements,
                            BindingKind::Sampler(_)
                            | BindingKind::ConstantBuffer { .. }
                            | BindingKind::Typed { .. } => {
                                return Err(Error::Invalid("shared memory declaration"));
                            }
                        };
                        let name = resource_name(binding.register, binding.slot)?;
                        self.bindings
                            .insert((binding.register, binding.slot), binding.kind);
                        let atomic = self
                            .atomic_bindings
                            .contains(&(binding.register, binding.slot));
                        let element = if atomic { "atomic<u32>" } else { "u32" };
                        self.line(&format!(
                            "var<workgroup> {name}: array<{element}, {count}>;"
                        ));
                        self.buffer_functions(&name, &format!("{count}u"), true, atomic);
                    }
                }
                Statement::Execute(_)
                | Statement::If { .. }
                | Statement::Loop(_)
                | Statement::Switch { .. }
                | Statement::CustomData { .. } => {}
            }
        }
        Ok(())
    }

    fn declare_buffer(&mut self, name: &str, slot: u32, writable: bool, atomic: bool) {
        let mode = if writable { "read_write" } else { "read" };
        let element = if atomic { "atomic<u32>" } else { "u32" };
        self.line(&format!(
            "@group(3) @binding({slot}) var<storage, {mode}> {name}: array<{element}>;"
        ));
        self.buffer_functions(name, &format!("arrayLength(&{name})"), writable, atomic);
    }

    fn check_typed_format(
        &self,
        ty: ResourceType,
        format: naga::StorageFormat,
    ) -> Result<(), Error> {
        if scalar(ty)? != scalar_name(format) {
            return Err(Error::Invalid(
                "typed view format disagrees with declaration",
            ));
        }
        Ok(())
    }

    fn buffer_functions(&mut self, name: &str, length: &str, writable: bool, atomic: bool) {
        let read = if atomic {
            format!("atomicLoad(&{name}[word])")
        } else {
            format!("{name}[word]")
        };
        self.line(&format!(
            "fn dx_size_{name}() -> u32 {{ return {length}; }}"
        ));
        self.line(&format!("fn dx_read_{name}(word: u32) -> u32 {{ if word < {length} {{ return {read}; }} return 0u; }}"));
        if writable {
            let write = if atomic {
                format!("atomicStore(&{name}[word], value);")
            } else {
                format!("{name}[word] = value;")
            };
            self.line(&format!(
                "fn dx_write_{name}(word: u32, value: u32) {{ if word < {length} {{ {write} }} }}"
            ));
        }
    }

    pub(super) fn barrier(&mut self, op: &Instruction) -> Result<(), Error> {
        let flags = op.controls;
        if flags & D3D11_SB_SYNC_THREADS_IN_GROUP == 0 {
            return Err(Error::UnsupportedOpcode(op.opcode));
        }
        if flags
            & (D3D11_SB_SYNC_UNORDERED_ACCESS_VIEW_MEMORY_GLOBAL
                | D3D11_SB_SYNC_UNORDERED_ACCESS_VIEW_MEMORY_GROUP)
            != 0
        {
            self.line("storageBarrier();");
        }
        if flags & D3D11_SB_SYNC_THREAD_GROUP_SHARED_MEMORY != 0
            || flags == D3D11_SB_SYNC_THREADS_IN_GROUP
        {
            self.line("workgroupBarrier();");
        }
        Ok(())
    }

    pub(super) fn resource(
        &mut self,
        op: &Instruction,
        args: &[String],
    ) -> Result<Option<String>, Error> {
        let (dst_types, src_types) = op.opcode.types();
        let mut resource = None;
        let mut sampler = None;
        let mut mask = 0;
        let mut swizzle = [0, 1, 2, 3];
        for (operand, ty) in op.destinations.iter().zip(dst_types) {
            if *ty == ValueType::Binding {
                resource = Some(binding(operand)?);
                mask = operand.components.mask();
            }
        }
        for (operand, ty) in op.sources.iter().zip(src_types) {
            if *ty == ValueType::Binding {
                let (register, slot) = binding(operand)?;
                if register == Register::Sampler {
                    sampler = Some((slot, operand.components.swizzle()[0]));
                } else {
                    resource = Some((register, slot));
                    swizzle = operand.components.swizzle();
                }
            }
        }
        let (register, slot) = resource.ok_or(Error::UnsupportedOpcode(op.opcode))?;
        let expression = if register == Register::Rasterizer {
            Some(self.sample_query(op, &format!("{}u", self.options.raster_samples), args)?)
        } else if op.opcode.is_counter() {
            let counter = format!("&dx_counter{slot}");
            Some(if op.opcode == Opcode::ImmAtomicAlloc {
                format!("vec4<u32>(atomicAdd({counter}, 1u))")
            } else {
                format!("vec4<u32>(atomicSub({counter}, 1u) - 1u)")
            })
        } else {
            let name = resource_name(register, slot)?;
            let binding = *self
                .bindings
                .get(&(register, slot))
                .ok_or(Error::Invalid("undeclared resource"))?;
            match binding {
                BindingKind::Raw { .. } | BindingKind::Structured { .. } => {
                    if op.opcode.is_atomic() {
                        self.atomic_buffer_instruction(op, args, &name, binding)?
                    } else {
                        self.buffer_instruction(op, args, &name, binding, mask)?
                    }
                }
                BindingKind::Typed { dimension, .. } => {
                    if dimension == ResourceDimension::Buffer
                        || (register == Register::UnorderedAccessView
                            && self.storage_extent(slot).is_some())
                    {
                        self.typed_buffer_instruction(op, args, &name, register, slot, dimension)?
                    } else if register == Register::UnorderedAccessView {
                        self.storage_instruction(op, args, &name, dimension, slot)?
                    } else {
                        Some(self.texture_instruction(op, args, &name, sampler, dimension, slot)?)
                    }
                }
                BindingKind::Sampler(_) | BindingKind::ConstantBuffer { .. } => {
                    return Err(Error::UnsupportedOpcode(op.opcode));
                }
            }
        };
        let lanes: String = swizzle
            .into_iter()
            .map(|lane| LANES.as_bytes()[lane as usize] as char)
            .collect();
        Ok(expression.map(|value| {
            let local = format!("resource_{}", self.serial);
            self.line(&format!("let {local} = ({value}).{lanes};"));
            local
        }))
    }

    fn buffer_instruction(
        &mut self,
        op: &Instruction,
        args: &[String],
        name: &str,
        kind: BindingKind,
        mask: u8,
    ) -> Result<Option<String>, Error> {
        use Opcode::*;
        let stride = if let BindingKind::Structured { stride, .. } = kind {
            stride / 4
        } else {
            1
        };
        if op.opcode == Bufinfo {
            return Ok(Some(format!(
                "vec4<u32>(dx_size_{name}() / {stride}u, {}u, 0u, 0u)",
                stride * 4
            )));
        }
        let (address, in_bounds) = if matches!(op.opcode, LdRaw | StoreRaw) {
            (format!("{}.x / 4u", args[0]), "true".into())
        } else if matches!(op.opcode, LdStructured | StoreStructured) {
            (
                format!("{}.x * {stride}u + {}.x / 4u", args[0], args[1]),
                format!(
                    "{}.x < dx_size_{name}() / {stride}u && {}.x / 4u < {stride}u",
                    args[0], args[1]
                ),
            )
        } else {
            return Err(Error::UnsupportedOpcode(op.opcode));
        };
        let local = format!("address_{}", self.serial);
        self.line(&format!("let {local} = {address};"));
        if matches!(op.opcode, StoreRaw | StoreStructured) {
            let value = args.last().unwrap();
            self.open(&format!("if {in_bounds}"));
            for (lane, component) in LANES.chars().enumerate() {
                if mask & (1 << lane) != 0 {
                    self.line(&format!(
                        "dx_write_{name}({local} + {lane}u, {value}.{component});"
                    ));
                }
            }
            self.close();
            Ok(None)
        } else {
            let out = format!("loaded_{}", self.serial);
            self.line(&format!("var {out}: vec4<u32>;"));
            self.open(&format!("if {in_bounds}"));
            self.line(&format!(
                "{out} = vec4<u32>(dx_read_{name}({local}), dx_read_{name}({local} + 1u), dx_read_{name}({local} + 2u), dx_read_{name}({local} + 3u));"
            ));
            self.close();
            Ok(Some(out))
        }
    }

    fn atomic_buffer_instruction(
        &mut self,
        op: &Instruction,
        args: &[String],
        name: &str,
        kind: BindingKind,
    ) -> Result<Option<String>, Error> {
        let (address, bounds) = if let BindingKind::Structured { stride, .. } = kind {
            let words = stride / 4;
            (
                format!("{}.x * {words}u + {}.y / 4u", args[0], args[0]),
                format!(
                    "{}.x < dx_size_{name}() / {words}u && {}.y / 4u < {words}u",
                    args[0], args[0]
                ),
            )
        } else {
            (
                format!("{}.x / 4u", args[0]),
                format!("{}.x / 4u < dx_size_{name}()", args[0]),
            )
        };
        self.atomic_at(op, args, name, &address, &bounds)
    }

    fn atomic_at(
        &mut self,
        op: &Instruction,
        args: &[String],
        name: &str,
        address: &str,
        bounds: &str,
    ) -> Result<Option<String>, Error> {
        use Opcode::*;
        let result = format!("atomic_result_{}", self.serial);
        let returning = op.destinations.len() > 1;
        if returning {
            self.line(&format!("var {result} = 0u;"));
        }
        self.open(&format!("if {bounds}"));
        self.line(&format!("let atomic_address = &{name}[{address}];"));
        let value = format!("{}.x", args[1]);
        let builtin = match op.opcode {
            AtomicIadd | ImmAtomicIadd => "atomicAdd",
            AtomicAnd | ImmAtomicAnd => "atomicAnd",
            AtomicOr | ImmAtomicOr => "atomicOr",
            AtomicXor | ImmAtomicXor => "atomicXor",
            AtomicUmax | ImmAtomicUmax => "atomicMax",
            AtomicUmin | ImmAtomicUmin => "atomicMin",
            ImmAtomicExch => "atomicExchange",
            AtomicImax | ImmAtomicImax | AtomicImin | ImmAtomicImin | AtomicCmpStore
            | ImmAtomicCmpExch => "",
            other => return Err(Error::UnsupportedOpcode(other)),
        };
        if !builtin.is_empty() {
            let assignment = if returning {
                format!("{result} = ")
            } else {
                "_ = ".into()
            };
            self.line(&format!("{assignment}{builtin}(atomic_address, {value});"));
        } else {
            self.open("loop");
            self.line("let observed = atomicLoad(atomic_address);");
            if returning {
                self.line(&format!("{result} = observed;"));
            }
            let update = if matches!(op.opcode, AtomicCmpStore | ImmAtomicCmpExch) {
                self.line(&format!("if observed != {value} {{ break; }}"));
                format!("{}.x", args[2])
            } else {
                let relative = if matches!(op.opcode, AtomicImax | ImmAtomicImax) {
                    "max"
                } else {
                    "min"
                };
                format!("bitcast<u32>({relative}(bitcast<i32>(observed), bitcast<i32>({value})))")
            };
            self.line(&format!("if atomicCompareExchangeWeak(atomic_address, observed, {update}).exchanged {{ break; }}"));
            self.close();
        }
        self.close();
        Ok(returning.then(|| format!("vec4<u32>({result})")))
    }

    fn storage_format(&self, slot: u32) -> Result<naga::StorageFormat, Error> {
        self.options
            .storage_formats
            .iter()
            .find_map(|&(binding, format)| (binding == slot).then_some(format))
            .ok_or(Error::MissingStorageFormat(slot))
    }

    fn storage_extent(&self, slot: u32) -> Option<[u32; 3]> {
        self.options
            .storage_extents
            .iter()
            .find_map(|&(index, extent)| (slot == index).then_some(extent))
    }

    fn storage_instruction(
        &mut self,
        op: &Instruction,
        args: &[String],
        name: &str,
        dimension: ResourceDimension,
        slot: u32,
    ) -> Result<Option<String>, Error> {
        let (coordinate, layer, condition) = match dimension {
            ResourceDimension::Texture1D => (
                format!("bitcast<i32>({}.x)", args[0]),
                String::new(),
                format!("{}.x < textureDimensions({name})", args[0]),
            ),
            ResourceDimension::Texture2D => (
                format!("bitcast<vec2<i32>>({}.xy)", args[0]),
                String::new(),
                format!("all({}.xy < textureDimensions({name}))", args[0]),
            ),
            ResourceDimension::Texture2DArray => (
                format!("bitcast<vec2<i32>>({}.xy)", args[0]),
                format!(", bitcast<i32>({}.z)", args[0]),
                format!(
                    "all({}.xy < textureDimensions({name})) && {}.z < textureNumLayers({name})",
                    args[0], args[0]
                ),
            ),
            ResourceDimension::Texture3D => (
                format!("bitcast<vec3<i32>>({}.xyz)", args[0]),
                String::new(),
                format!("all({}.xyz < textureDimensions({name}))", args[0]),
            ),
            ResourceDimension::Buffer
            | ResourceDimension::Texture2DMs
            | ResourceDimension::TextureCube
            | ResourceDimension::Texture1DArray
            | ResourceDimension::Texture2DMsArray
            | ResourceDimension::TextureCubeArray
            | ResourceDimension::RawBuffer
            | ResourceDimension::StructuredBuffer => {
                return Err(Error::UnsupportedOpcode(op.opcode));
            }
        };
        if op.opcode == Opcode::StoreUavTyped {
            let scalar = scalar_name(self.storage_format(slot)?);
            self.open(&format!("if {condition}"));
            self.line(&format!(
                "textureStore({name}, {coordinate}{layer}, bitcast<vec4<{scalar}>>({}));",
                args[1]
            ));
            self.close();
            Ok(None)
        } else if op.opcode == Opcode::LdUavTyped {
            let result = format!("typed_result_{}", self.serial);
            self.line(&format!("var {result}: vec4<u32>;"));
            self.open(&format!("if {condition}"));
            self.line(&format!(
                "{result} = bitcast<vec4<u32>>(textureLoad({name}, {coordinate}{layer}));"
            ));
            self.close();
            Ok(Some(result))
        } else if op.opcode.is_atomic() {
            use Opcode::*;
            let format = self.storage_format(slot)?;
            let builtin = match op.opcode {
                AtomicAnd => "And",
                AtomicOr => "Or",
                AtomicXor => "Xor",
                AtomicIadd => "Add",
                AtomicImax if format == naga::StorageFormat::R32Sint => "Max",
                AtomicImin if format == naga::StorageFormat::R32Sint => "Min",
                AtomicUmax if format == naga::StorageFormat::R32Uint => "Max",
                AtomicUmin if format == naga::StorageFormat::R32Uint => "Min",
                _ => return Err(Error::MissingStorageExtent(slot)),
            };
            let scalar = scalar_name(format);
            self.open(&format!("if {condition}"));
            self.line(&format!(
                "textureAtomic{builtin}({name}, {coordinate}{layer}, bitcast<{scalar}>({}.x));",
                args[1]
            ));
            self.close();
            Ok(None)
        } else {
            Err(Error::UnsupportedOpcode(op.opcode))
        }
    }

    fn texture_instruction(
        &mut self,
        op: &Instruction,
        args: &[String],
        texture: &str,
        sampler: Option<(u32, u8)>,
        dimension: ResourceDimension,
        slot: u32,
    ) -> Result<String, Error> {
        use Opcode::*;
        let lod = self
            .options
            .texture_lods
            .iter()
            .find(|lod| lod.slot == slot)
            .copied();
        let (width, lanes, array) = match dimension {
            ResourceDimension::Texture1D => (1, "x", ""),
            ResourceDimension::Texture2D | ResourceDimension::Texture2DMs => (2, "xy", ""),
            ResourceDimension::Texture2DArray => (2, "xy", "z"),
            ResourceDimension::Texture3D | ResourceDimension::TextureCube => (3, "xyz", ""),
            ResourceDimension::TextureCubeArray => (3, "xyz", "w"),
            ResourceDimension::Buffer
            | ResourceDimension::Texture1DArray
            | ResourceDimension::Texture2DMsArray
            | ResourceDimension::RawBuffer
            | ResourceDimension::StructuredBuffer => {
                return Err(Error::UnsupportedOpcode(op.opcode));
            }
        };
        let cast = |value: &str, ty: &str| {
            if width == 1 {
                format!("bitcast<{ty}>({value}.{lanes})")
            } else {
                format!("bitcast<vec{width}<{ty}>>({value}.{lanes})")
            }
        };
        let load_layer = if array.is_empty() {
            String::new()
        } else {
            format!(", bitcast<i32>({}.{})", args[0], array)
        };
        let offset = if let Some(offset) = op.resource.offset {
            if width == 1 {
                format!(", {}i", offset[0])
            } else {
                let values: Vec<_> = offset[..width].iter().map(|v| format!("{v}i")).collect();
                format!(", vec{width}<i32>({})", values.join(", "))
            }
        } else {
            String::new()
        };
        if op.opcode == Resinfo {
            return Ok(self.resource_info(op, args, texture, dimension));
        }
        if matches!(op.opcode, SampleInfo | SamplePos)
            && dimension == ResourceDimension::Texture2DMs
        {
            return self.sample_query(op, &format!("textureNumSamples({texture})"), args);
        }
        let empty = lod.is_some_and(|lod| lod.minimum > lod.last as f32);
        let default = lod.map_or(0, |lod| lod.default_alpha);
        if empty && op.opcode != Lod {
            return Ok(format!("vec4<u32>(0u, 0u, 0u, {default}u)"));
        }
        if matches!(op.opcode, Ld | LdMs) {
            let xy = if offset.is_empty() {
                cast(&args[0], "i32")
            } else {
                format!("({} + {})", cast(&args[0], "i32"), &offset[2..])
            };
            let level = if op.opcode == LdMs {
                format!("{}.x", args[1])
            } else {
                format!("{}.w", args[0])
            };
            let result = format!("texture_result_{}", self.serial);
            self.line(&format!(
                "var {result} = vec4<u32>(0u, 0u, 0u, {default}u);"
            ));
            let (count, dimensions) = if op.opcode == LdMs {
                (
                    format!("textureNumSamples({texture})"),
                    format!("textureDimensions({texture})"),
                )
            } else {
                (
                    format!("textureNumLevels({texture})"),
                    format!("textureDimensions({texture}, {level})"),
                )
            };
            let minimum = lod.map_or(0, |lod| lod.minimum.floor() as u32);
            let first = if minimum == 0 {
                String::new()
            } else {
                format!(" && {level} >= {minimum}u")
            };
            self.open(&format!("if {level} < {count}{first}"));
            let index_u32 = if width == 1 {
                format!("bitcast<u32>({xy})")
            } else {
                format!("bitcast<vec{width}<u32>>({xy})")
            };
            let in_bounds = if width == 1 {
                format!("{index_u32} < {dimensions}")
            } else {
                format!("all({index_u32} < {dimensions})")
            };
            let in_bounds = if array.is_empty() {
                in_bounds
            } else {
                format!(
                    "{in_bounds} && {}.{array} < textureNumLayers({texture})",
                    args[0]
                )
            };
            self.open(&format!("if {in_bounds}"));
            let load = format!("textureLoad({texture}, {xy}{load_layer}, {level})");
            let load = if self.options.depth24_textures.contains(&slot) {
                format!("vec4<f32>(dx_unorm_exact({load}, 24u), 0.0, 0.0, 1.0)")
            } else if self.options.depth_textures.contains(&slot) {
                format!("vec4<f32>({load}, 0.0, 0.0, 1.0)")
            } else {
                load
            };
            let load = self.exact_unorm(slot, &load);
            self.line(&format!("{result} = bitcast<vec4<u32>>({load});"));
            self.close();
            self.close();
            return Ok(result);
        }
        let (sampler, component) = sampler.ok_or(Error::UnsupportedOpcode(op.opcode))?;
        let mut coord = cast(&args[0], "f32");
        let suffix = if array.is_empty() {
            String::new()
        } else {
            format!(
                ", i32(clamp(bitcast<f32>(dx_round({}.{array}, 0u)), 0.0, f32(textureNumLayers({texture}) - 1u)))",
                args[0]
            )
        };
        if op.opcode == Lod {
            return Ok(self.lod_query(texture, sampler, dimension, width, &coord, lod));
        }
        if matches!(op.opcode, Gather4 | Gather4C | Gather4Po | Gather4PoC) {
            if lod.is_some_and(|lod| lod.minimum >= 1.) {
                return Ok(format!(
                    "vec4<u32>({}u)",
                    if component == 3 { default } else { 0 }
                ));
            }
            if matches!(op.opcode, Gather4Po | Gather4PoC) {
                let dynamic = &args[1];
                coord = format!(
                    "({coord} + vec2<f32>(bitcast<vec2<i32>>({dynamic}.xy << vec2<u32>(26u)) >> vec2<u32>(26u)) / vec2<f32>(textureDimensions({texture}, 0u)))"
                );
            }
            let call = if matches!(op.opcode, Gather4 | Gather4Po) {
                format!(
                    "textureGather({component}i, {texture}, s{sampler}, {coord}{suffix}{offset})"
                )
            } else {
                let reference = args.last().unwrap();
                format!(
                    "textureGatherCompare({texture}, s{sampler}, {coord}{suffix}, bitcast<f32>({reference}.x){offset})"
                )
            };
            return Ok(format!("bitcast<vec4<u32>>({call})"));
        }
        let border = self
            .options
            .border_samplers
            .iter()
            .find(|border| border.slot == sampler)
            .copied();
        let sampling = self
            .options
            .lod_samplers
            .iter()
            .find_map(|(slot, mode)| (*slot == sampler).then_some(*mode));
        let mip = sampling.filter(|mode| mode.round_mip);
        let sampler = if let Some(lod) = lod {
            self.clamped_sampler(sampler, lod.minimum)
        } else {
            format!("s{sampler}")
        };
        let extra = |idx: usize| format!("bitcast<f32>({}.x)", args[idx]);
        let (function, arguments, scalar) = match op.opcode {
            Sample if mip.is_some_and(|mip| mip.bias != 0.) => (
                "textureSampleBias",
                format!(", bitcast<f32>({}u)", mip.unwrap().bias.to_bits()),
                false,
            ),
            Sample => ("textureSample", String::new(), false),
            SampleB => (
                "textureSampleBias",
                match mip {
                    Some(mip) => format!(", {} + bitcast<f32>({}u)", extra(1), mip.bias.to_bits()),
                    None => format!(", {}", extra(1)),
                },
                false,
            ),
            SampleL => ("textureSampleLevel", format!(", {}", extra(1)), false),
            SampleD
                if mip.is_some()
                    && !matches!(
                        dimension,
                        ResourceDimension::TextureCube | ResourceDimension::TextureCubeArray
                    ) =>
            {
                let ty = if width == 1 {
                    "f32".into()
                } else {
                    format!("vec{width}<f32>")
                };
                let size = format!("{ty}(textureDimensions({texture}, 0u))");
                let x = format!("({} * {size})", cast(&args[1], "f32"));
                let y = format!("({} * {size})", cast(&args[2], "f32"));
                let magnitude = if width == 1 {
                    format!("max(abs({x}), abs({y}))")
                } else {
                    format!("max(length({x}), length({y}))")
                };
                let bias = mip.unwrap().bias.to_bits();
                let lambda = format!("(log2({magnitude}) + bitcast<f32>({bias}u))");
                // D3D converts LOD to at least 8.8 fixed precision before the
                // half-up mip selection. Do that before the host samples.
                (
                    "textureSampleLevel",
                    format!(", floor(round({lambda} * 256.0) / 256.0 + 0.5)"),
                    false,
                )
            }
            SampleD => (
                "textureSampleGrad",
                if let Some(mip) = mip {
                    let scale = format!("exp2(bitcast<f32>({}u))", mip.bias.to_bits());
                    format!(
                        ", {} * {scale}, {} * {scale}",
                        cast(&args[1], "f32"),
                        cast(&args[2], "f32")
                    )
                } else {
                    format!(", {}, {}", cast(&args[1], "f32"), cast(&args[2], "f32"))
                },
                false,
            ),
            SampleC => ("textureSampleCompare", format!(", {}", extra(1)), true),
            SampleCLz => ("textureSampleCompareLevel", format!(", {}", extra(1)), true),
            other => return Err(Error::UnsupportedOpcode(other)),
        };
        let mut sample =
            format!("{function}({texture}, {sampler}, {coord}{suffix}{arguments}{offset})");
        if op.opcode == SampleCLz
            && sampling.is_some_and(|mode| mode.linear_comparison)
            && matches!(
                dimension,
                ResourceDimension::Texture2D | ResourceDimension::Texture2DArray
            )
            && lod.is_none_or(|lod| lod.minimum < 1.)
        {
            let name = format!("cmp_taps_{}", self.serial);
            let weight = format!("cmp_weight_{}", self.serial);
            self.line(&format!("let {name} = textureGatherCompare({texture}, {sampler}, {coord}{suffix}{arguments}{offset});"));
            self.line(&format!(
                "let {weight} = fract({coord} * vec2<f32>(textureDimensions({texture}, 0u)) - 0.5);"
            ));
            sample = format!(
                "mix(mix({name}.w, {name}.z, {weight}.x), mix({name}.x, {name}.y, {weight}.x), {weight}.y)"
            );
        }
        if let Some(border) = border
            && !matches!(
                dimension,
                ResourceDimension::TextureCube | ResourceDimension::TextureCubeArray
            )
            && let Some(format) = self
                .options
                .border_textures
                .iter()
                .find(|format| format.slot == slot)
        {
            sample = self.border_sample(border::BorderSample {
                sampler: border,
                format: *format,
                depth: self.options.depth_textures.contains(&slot),
                width,
                texture,
                coordinate: &coord,
                arguments: args,
                offset: op.resource.offset,
                opcode: op.opcode,
                lod,
                native: &sample,
            });
        }
        if !scalar && sampling.is_some_and(|mode| mode.point) {
            sample = self.exact_unorm(slot, &sample);
        }
        Ok(if scalar {
            format!("vec4<u32>(bitcast<u32>({sample}))")
        } else if self.options.depth_textures.contains(&slot) {
            format!("bitcast<vec4<u32>>(vec4<f32>({sample}, 0.0, 0.0, 1.0))")
        } else {
            format!("bitcast<vec4<u32>>({sample})")
        })
    }

    fn exact_unorm(&mut self, slot: u32, value: &str) -> String {
        let Some((_, widths)) = self
            .options
            .exact_unorm
            .iter()
            .find(|(bound, _)| *bound == slot)
        else {
            return value.into();
        };
        let local = format!("decoded_unorm_{}", self.serial);
        self.line(&format!("let {local} = {value};"));
        let channels = widths
            .iter()
            .enumerate()
            .map(|(lane, &bits)| {
                let source = format!("{local}.{}", LANES.as_bytes()[lane] as char);
                if bits == 0 {
                    source
                } else {
                    format!("dx_unorm_exact({source}, {bits}u)")
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!("vec4<f32>({channels})")
    }

    fn clamped_sampler(&mut self, slot: u32, minimum: f32) -> String {
        let bits = Some(minimum.to_bits());
        if let Some(location) = self.locations.iter().find(|location| {
            location.register == Register::Sampler
                && location.slot == slot
                && location.sampler_min_lod_bits == bits
        }) {
            return format!("dx_s{}", location.binding);
        }
        let binding = 16
            + self
                .locations
                .iter()
                .filter(|location| location.sampler_min_lod_bits.is_some())
                .count() as u32;
        self.locations.push(BindingLocation {
            register: Register::Sampler,
            slot,
            group: 2,
            binding,
            sampler_min_lod_bits: bits,
        });
        format!("dx_s{binding}")
    }

    pub(super) fn clamped_sampler_declarations(&mut self) {
        for location in &self.locations {
            if location.sampler_min_lod_bits.is_some() {
                let binding = location.binding;
                let ty = if self.bindings[&(Register::Sampler, location.slot)]
                    == BindingKind::Sampler(SamplerMode::Comparison)
                {
                    "sampler_comparison"
                } else {
                    "sampler"
                };
                writeln!(
                    self.source,
                    "@group(2) @binding({binding}) var dx_s{binding}: {ty};"
                )
                .unwrap();
            }
        }
    }
    fn sample_query(
        &self,
        op: &Instruction,
        count: &str,
        args: &[String],
    ) -> Result<String, Error> {
        if op.opcode == Opcode::SamplePos {
            Ok(format!(
                "vec4<u32>(bitcast<vec2<u32>>(dx_sample_position({count}, {}.x)), 0u, 0u)",
                args[0]
            ))
        } else if op.opcode == Opcode::SampleInfo {
            let value = if crate::shader::dxbc::fields::decode_instruction_return_type(op.controls)
                == D3D10_SB_INSTRUCTION_RETURN_UINT
            {
                count.to_owned()
            } else {
                format!("bitcast<u32>(f32({count}))")
            };
            Ok(format!("vec4<u32>({value}, 0u, 0u, 0u)"))
        } else {
            Err(Error::UnsupportedOpcode(op.opcode))
        }
    }

    fn lod_query(
        &mut self,
        texture: &str,
        slot: u32,
        dimension: ResourceDimension,
        width: usize,
        coordinate: &str,
        resource: Option<TextureLod>,
    ) -> String {
        let sampler = self
            .options
            .lod_samplers
            .iter()
            .find_map(|&(index, sampler)| (slot == index).then_some(sampler))
            .unwrap_or(LodSampler {
                bias: 0.0,
                min: 0.0,
                max: f32::MAX,
                max_anisotropy: 1,
                round_mip: false,
                point: false,
                linear_comparison: false,
            });
        let c = format!("lod_coord_{}", self.serial);
        self.line(&format!("let {c} = {coordinate};"));
        let scalar = width == 1;
        let cube = matches!(
            dimension,
            ResourceDimension::TextureCube | ResourceDimension::TextureCubeArray
        );
        let scale = if scalar {
            "f32".to_owned()
        } else {
            format!("vec{}<f32>", if cube { 2 } else { width })
        };
        let footprint = |axis| {
            let delta = format!("dpd{axis}Coarse({c})");
            let delta = if cube {
                format!("dx_cube_gradient({c}, {delta})")
            } else {
                delta
            };
            format!("{delta} * {scale}(textureDimensions({texture}, 0u))")
        };
        let dx = format!("lod_dx_{}", self.serial);
        let dy = format!("lod_dy_{}", self.serial);
        let raw = format!("lod_raw_{}", self.serial);
        self.line(&format!("let {dx} = {};", footprint('x')));
        self.line(&format!("let {dy} = {};", footprint('y')));
        let rho = if scalar {
            format!("max(abs({dx}), abs({dy}))")
        } else if sampler.max_anisotropy > 1 {
            // The squared ellipse axes are the eigenvalues of the gradient Gram matrix.
            format!(
                "dx_anisotropic_extent(dot({dx}, {dx}), dot({dx}, {dy}), dot({dy}, {dy}), {}.0)",
                sampler.max_anisotropy
            )
        } else {
            format!("max(length({dx}), length({dy}))")
        };
        self.line(&format!(
            "let {raw} = bitcast<f32>(dx_log(bitcast<u32>({rho}))) + bitcast<f32>({}u);",
            sampler.bias.to_bits()
        ));
        let lower = format!(
            "clamp(bitcast<f32>({}u), 0.0, f32(textureNumLevels({texture}) - 1u))",
            sampler.min.to_bits()
        );
        let upper = format!(
            "clamp(bitcast<f32>({}u), 0.0, f32(textureNumLevels({texture}) - 1u))",
            sampler.max.to_bits()
        );
        let clamped = format!("clamp({raw}, {lower}, {upper})");
        let clamped = match resource {
            Some(lod) if lod.minimum > lod.last as f32 => "0.0".into(),
            Some(lod) => format!("max({clamped}, bitcast<f32>({}u))", lod.minimum.to_bits()),
            None => clamped,
        };
        format!("bitcast<vec4<u32>>(vec4<f32>({clamped}, {raw}, 0.0, 0.0))")
    }

    fn resource_info(
        &mut self,
        op: &Instruction,
        args: &[String],
        texture: &str,
        dimension: ResourceDimension,
    ) -> String {
        let multisampled = dimension == ResourceDimension::Texture2DMs;
        let count = if multisampled {
            "1u".into()
        } else {
            format!("textureNumLevels({texture})")
        };
        let level = format!("{}.x", args[0]);
        let size = if multisampled {
            format!("textureDimensions({texture})")
        } else {
            format!("textureDimensions({texture}, {level})")
        };
        let name = format!("dimensions_{}", self.serial);
        self.line(&format!("var {name} = vec4<u32>(0u, 0u, 0u, {count});"));
        self.open(&format!("if {level} < {count}"));
        let dimensions = match dimension {
            ResourceDimension::Texture1D => format!("vec3<u32>({size}, 0u, 0u)"),
            ResourceDimension::Texture3D => size,
            ResourceDimension::Texture2DArray | ResourceDimension::TextureCubeArray => {
                format!("vec3<u32>({size}, textureNumLayers({texture}))")
            }
            ResourceDimension::Texture2D
            | ResourceDimension::Texture2DMs
            | ResourceDimension::TextureCube
            | ResourceDimension::Buffer
            | ResourceDimension::Texture1DArray
            | ResourceDimension::Texture2DMsArray
            | ResourceDimension::RawBuffer
            | ResourceDimension::StructuredBuffer => format!("vec3<u32>({size}, 0u)"),
        };
        self.line(&format!("{name} = vec4<u32>({dimensions}, {count});"));
        self.close();
        let mode = crate::shader::dxbc::fields::decode_resinfo_instruction_return_type(op.controls);
        if mode == 2 {
            name
        } else if mode == 1 {
            let axis = |lane: char, reciprocal| {
                if reciprocal {
                    format!("bitcast<f32>(dx_rcp(bitcast<u32>(f32({name}.{lane}))))")
                } else {
                    format!("f32({name}.{lane})")
                }
            };
            let x = axis('x', true);
            let y = axis('y', dimension != ResourceDimension::Texture1D);
            let z = axis('z', dimension == ResourceDimension::Texture3D);
            format!("bitcast<vec4<u32>>(vec4<f32>({x}, {y}, {z}, f32({name}.w)))")
        } else {
            format!("bitcast<vec4<u32>>(vec4<f32>({name}))")
        }
    }
}

fn storage_dimension(dimension: ResourceDimension) -> Option<&'static str> {
    match dimension {
        ResourceDimension::Texture1D => Some("1d"),
        ResourceDimension::Texture2D => Some("2d"),
        ResourceDimension::Texture2DArray => Some("2d_array"),
        ResourceDimension::Texture3D => Some("3d"),
        ResourceDimension::Buffer
        | ResourceDimension::Texture2DMs
        | ResourceDimension::TextureCube
        | ResourceDimension::Texture1DArray
        | ResourceDimension::Texture2DMsArray
        | ResourceDimension::TextureCubeArray
        | ResourceDimension::RawBuffer
        | ResourceDimension::StructuredBuffer => None,
    }
}

fn scalar_name(format: naga::StorageFormat) -> &'static str {
    use naga::common::wgsl::TryToWgsl;
    naga::Scalar::from(format).try_to_wgsl().unwrap()
}

fn scalar(ty: ResourceType) -> Result<&'static str, Error> {
    match ty {
        ResourceType::Float | ResourceType::Unorm | ResourceType::Snorm => Ok("f32"),
        ResourceType::Int => Ok("i32"),
        ResourceType::Uint => Ok("u32"),
        ResourceType::Mixed
        | ResourceType::Double
        | ResourceType::Continued
        | ResourceType::Unused => Err(Error::Invalid("typed resource component")),
    }
}

fn resource_name(register: Register, slot: u32) -> Result<String, Error> {
    let prefix = match register {
        Register::Sampler => "s",
        Register::Resource => "t",
        Register::ConstantBuffer => "b",
        Register::UnorderedAccessView => "u",
        Register::ThreadGroupSharedMemory => "g",
        other => return Err(Error::UnsupportedRegister(other)),
    };
    Ok(format!("{prefix}{slot}"))
}

fn binding(operand: &Operand) -> Result<(Register, u32), Error> {
    if matches!(operand.register(), Some((Register::Rasterizer, []))) {
        return Ok((Register::Rasterizer, 0));
    }
    if let Some((register, [slot])) = operand.register()
        && let Some(slot) = slot.direct()
    {
        return Ok((register, slot));
    }
    Err(Error::Invalid("dynamic resource binding"))
}

fn comparison_sample(block: &Block, slot: u32) -> bool {
    block.iter().any(|statement| match statement {
        Statement::Execute(op) => {
            matches!(
                op.opcode,
                Opcode::SampleC | Opcode::SampleCLz | Opcode::Gather4C | Opcode::Gather4PoC
            ) && op
                .sources
                .iter()
                .any(|arg| binding(arg) == Ok((Register::Resource, slot)))
        }
        Statement::If { accept, reject, .. } => {
            comparison_sample(accept, slot)
                || reject
                    .as_ref()
                    .is_some_and(|block| comparison_sample(block, slot))
        }
        Statement::Loop(block) => comparison_sample(block, slot),
        Statement::Switch { cases, .. } => {
            cases.iter().any(|case| comparison_sample(&case.body, slot))
        }
        Statement::Declare(_) | Statement::ImmediateConstants(_) | Statement::CustomData { .. } => {
            false
        }
    })
}

pub(super) const LOD_HELPERS: &str = "
fn dx_anisotropic_extent(a: f32, b: f32, c: f32, anisotropy: f32) -> f32 {
    let span = length(vec2<f32>(a - c, 2.0 * b));
    let minor = (a + c - span) * 0.5;
    let major = (a + c + span) * 0.5;
    return sqrt(max(minor, major / (anisotropy * anisotropy)));
}

fn dx_cube_gradient(coord: vec3<f32>, dcoord: vec3<f32>) -> vec2<f32> {
    var c = coord;
    var d = dcoord;
    let a = abs(coord);
    if a.x >= a.y && a.x >= a.z {
        c = coord.yzx;
        d = dcoord.yzx;
    } else if a.y >= a.z {
        c = coord.xzy;
        d = dcoord.xzy;
    }
    return (d.xy - c.xy * (d.z / c.z)) * (0.5 / abs(c.z));
}
";

use super::*;

impl Writer<'_> {
    pub(super) fn buffer_format(
        &self,
        register: Register,
        slot: u32,
    ) -> Result<naga::StorageFormat, Error> {
        if register == Register::UnorderedAccessView {
            self.storage_format(slot)
        } else {
            self.options
                .buffer_formats
                .iter()
                .find_map(|&(index, format)| (index == slot).then_some(format))
                .ok_or(Error::MissingTypedBufferFormat(slot))
        }
    }

    pub(super) fn typed_buffer_instruction(
        &mut self,
        op: &Instruction,
        args: &[String],
        name: &str,
        register: Register,
        slot: u32,
        dimension: ResourceDimension,
    ) -> Result<Option<String>, Error> {
        let format = self.buffer_format(register, slot)?;
        let mut layout = Layout::new(format)?;
        if register == Register::Resource && self.options.rgb_buffers.contains(&slot) {
            let alpha = layout.channels.pop().unwrap();
            if format != naga::StorageFormat::Bgra8Unorm {
                layout.bits -= alpha.width;
            }
        }
        let bits = layout.bits;
        let size = if bits < 32 {
            let count = self
                .options
                .buffer_elements
                .iter()
                .find_map(|&(file, index, count)| {
                    (file == register && index == slot).then_some(count)
                })
                .ok_or(Error::MissingTypedBufferElements(register, slot))?;
            format!("{count}u")
        } else {
            format!("dx_size_{name}() / {}u", bits / 32)
        };
        if op.opcode == Opcode::Bufinfo {
            return Ok(Some(format!("vec4<u32>({size})")));
        }
        let coordinate = &args[0];
        let (index, bounds) = if let Some([width, height, depth]) = self.storage_extent(slot)
            && register == Register::UnorderedAccessView
            && dimension != ResourceDimension::Buffer
        {
            match dimension {
                ResourceDimension::Texture1D => (
                    format!("{coordinate}.x"),
                    format!("{coordinate}.x < {width}u"),
                ),
                ResourceDimension::Texture1DArray | ResourceDimension::Texture2D => (
                    format!("({coordinate}.x + {width}u * {coordinate}.y)"),
                    format!("all({coordinate}.xy < vec2<u32>({width}u, {height}u))"),
                ),
                ResourceDimension::Texture2DArray | ResourceDimension::Texture3D => (
                    format!(
                        "({coordinate}.x + {width}u * ({coordinate}.y + {height}u * {coordinate}.z))"
                    ),
                    format!("all({coordinate}.xyz < vec3<u32>({width}u, {height}u, {depth}u))"),
                ),
                _ => return Err(Error::UnsupportedOpcode(op.opcode)),
            }
        } else {
            (
                format!("{coordinate}.x"),
                format!("{coordinate}.x < {size}"),
            )
        };
        if op.opcode.is_atomic() {
            return self.atomic_at(op, args, name, &index, &bounds);
        }
        let address = format!("typed_address_{}", self.serial);
        let result = format!("typed_result_{}", self.serial);
        let load = matches!(op.opcode, Opcode::Ld | Opcode::LdUavTyped);
        let one = if scalar_name(format) == "f32" {
            "0x3f800000u"
        } else {
            "1u"
        };
        if load {
            let missing_alpha = if layout.channels.len() < 4 && register == Register::Resource {
                one
            } else {
                "0u"
            };
            self.line(&format!(
                "var {result} = vec4<u32>(0u, 0u, 0u, {missing_alpha});"
            ));
        } else if op.opcode != Opcode::StoreUavTyped {
            return Err(Error::UnsupportedOpcode(op.opcode));
        }
        self.open(&format!("if {bounds}"));
        let shift = if bits < 32 {
            let per_word = 32 / bits;
            self.line(&format!("let {address} = {index} / {per_word}u;"));
            self.line(&format!(
                "let field_shift = ({index} % {per_word}u) * {bits}u;"
            ));
            "field_shift"
        } else {
            self.line(&format!("let {address} = {index} * {}u;", bits / 32));
            "0u"
        };
        if load {
            let mut values = ["0u".into(), "0u".into(), "0u".into(), one.into()];
            for (lane, channel) in layout.channels.iter().enumerate() {
                let (word, offset) = (channel.offset / 32, channel.offset % 32);
                let source = format!("dx_read_{name}({address} + {word}u)");
                let field = if channel.width == 32 {
                    source
                } else {
                    format!(
                        "(({source} >> ({shift} + {offset}u)) & {}u)",
                        mask(channel.width)
                    )
                };
                values[lane] = channel.decode(&field);
            }
            self.line(&format!("{result} = vec4<u32>({});", values.join(", ")));
        } else {
            let mut words = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
            for (channel, lane) in layout.channels.iter().zip(LANES.chars()) {
                let value = channel.encode(&format!("{}.{lane}", args[1]));
                words[(channel.offset / 32) as usize]
                    .push(format!("({value} << {}u)", channel.offset % 32));
            }
            for (word, components) in words.iter().filter(|c| !c.is_empty()).enumerate() {
                let value = components.join(" | ");
                if bits < 32 {
                    let mask = mask(bits);
                    // Two packed elements can be written by different invocations.
                    self.line(&format!("let address = &{name}[{address}];"));
                    self.open("loop");
                    self.line("let previous = atomicLoad(address);");
                    self.line(&format!("let packed = (previous & ~({mask}u << field_shift)) | (({value}) << field_shift);"));
                    self.line("if atomicCompareExchangeWeak(address, previous, packed).exchanged { break; }");
                    self.close();
                } else {
                    self.line(&format!("dx_write_{name}({address} + {word}u, {value});"));
                }
            }
        }
        self.close();
        Ok(load.then_some(result))
    }
}

#[derive(Clone, Copy)]
enum Encoding {
    Bits,
    Uint,
    Sint,
    Unorm,
    Snorm,
    Half,
    Ufloat,
}

struct Channel {
    offset: u32,
    width: u32,
    encoding: Encoding,
}

pub(super) struct Layout {
    pub(super) bits: u32,
    channels: Vec<Channel>,
}

impl Layout {
    pub(super) fn new(format: naga::StorageFormat) -> Result<Self, Error> {
        use Encoding::*;
        use naga::StorageFormat::*;
        let (widths, encoding): (&[u32], _) = match format {
            R8Unorm => (&[8], Unorm),
            R8Snorm => (&[8], Snorm),
            R8Uint => (&[8], Uint),
            R8Sint => (&[8], Sint),
            R16Uint => (&[16], Uint),
            R16Sint => (&[16], Sint),
            R16Float => (&[16], Half),
            R16Unorm => (&[16], Unorm),
            R16Snorm => (&[16], Snorm),
            Rg8Unorm => (&[8; 2], Unorm),
            Rg8Snorm => (&[8; 2], Snorm),
            Rg8Uint => (&[8; 2], Uint),
            Rg8Sint => (&[8; 2], Sint),
            R32Uint | R32Sint | R32Float => (&[32], Bits),
            Rg32Uint | Rg32Sint | Rg32Float => (&[32; 2], Bits),
            Rgba32Uint | Rgba32Sint | Rgba32Float => (&[32; 4], Bits),
            Rg16Uint => (&[16; 2], Uint),
            Rg16Sint => (&[16; 2], Sint),
            Rg16Float => (&[16; 2], Half),
            Rg16Unorm => (&[16; 2], Unorm),
            Rg16Snorm => (&[16; 2], Snorm),
            Rgba8Unorm | Bgra8Unorm => (&[8; 4], Unorm),
            Rgba8Snorm => (&[8; 4], Snorm),
            Rgba8Uint => (&[8; 4], Uint),
            Rgba8Sint => (&[8; 4], Sint),
            Rgb10a2Uint => (&[10, 10, 10, 2], Uint),
            Rgb10a2Unorm => (&[10, 10, 10, 2], Unorm),
            Rg11b10Ufloat => (&[11, 11, 10], Ufloat),
            Rgba16Uint => (&[16; 4], Uint),
            Rgba16Sint => (&[16; 4], Sint),
            Rgba16Float => (&[16; 4], Half),
            Rgba16Unorm => (&[16; 4], Unorm),
            Rgba16Snorm => (&[16; 4], Snorm),
            R64Uint => return Err(Error::Invalid("64-bit typed buffer view")),
        };
        let mut bits = 0;
        let mut channels: Vec<_> = widths
            .iter()
            .map(|&width| {
                let channel = Channel {
                    offset: bits,
                    width,
                    encoding,
                };
                bits += width;
                channel
            })
            .collect();
        if format == Bgra8Unorm {
            channels.swap(0, 2);
        }
        Ok(Self { bits, channels })
    }
}

impl Channel {
    fn decode(&self, field: &str) -> String {
        let mask = mask(self.width);
        let shift = 32 - self.width;
        let signed = format!("(bitcast<i32>({field} << {shift}u) >> {shift}u)");
        match self.encoding {
            Encoding::Bits | Encoding::Uint => field.to_owned(),
            Encoding::Sint => format!("bitcast<u32>({signed})"),
            Encoding::Unorm => format!("bitcast<u32>(f32({field}) / {mask}.0)"),
            Encoding::Snorm => format!("bitcast<u32>(max(-1.0, f32({signed}) / {}.0))", mask >> 1),
            Encoding::Half => format!("bitcast<u32>(unpack2x16float({field}).x)"),
            Encoding::Ufloat => format!(
                "bitcast<u32>(unpack2x16float({field} << {}u).x)",
                15 - self.width
            ),
        }
    }

    fn encode(&self, field: &str) -> String {
        let mask = mask(self.width);
        let max_signed = mask >> 1;
        let min_signed = max_signed + 1;
        let f32_value = format!("bitcast<f32>({field})");
        let round = |src| format!("bitcast<f32>(dx_round(bitcast<u32>({src}), 0u))");
        match self.encoding {
            Encoding::Bits => field.to_owned(),
            Encoding::Uint => format!("min({field}, {mask}u)"),
            Encoding::Sint => format!(
                "(bitcast<u32>(clamp(bitcast<i32>({field}), -{min_signed}i, {max_signed}i)) & {mask}u)"
            ),
            Encoding::Unorm => format!(
                "u32({})",
                round(format!("bitcast<f32>(dx_sat({field})) * {mask}.0"))
            ),
            Encoding::Snorm => format!(
                "(bitcast<u32>(i32({})) & {mask}u)",
                round(format!("clamp({f32_value}, -1.0, 1.0) * {max_signed}.0"))
            ),
            Encoding::Half => format!("pack2x16float(vec2<f32>({f32_value}, 0.0))"),
            Encoding::Ufloat => format!("dx_pack_ufloat({field}, {}u)", self.width - 5),
        }
    }
}

fn mask(width: u32) -> u32 {
    u32::MAX >> (32 - width)
}

#[cfg(feature = "gpu")]
pub(crate) fn clear_pattern(format: naga::StorageFormat, integer: bool) -> Result<String, Error> {
    let layout = Layout::new(format)?;
    let mut source = [super::super::alu::HELPERS, PACK_UFLOAT].concat();
    source.push_str(
        "fn clear_pattern(value: vec4<u32>) -> vec4<u32> {\n  var packed = vec4<u32>(0u);\n",
    );
    for (lane, channel) in layout.channels.iter().enumerate() {
        let field = format!("value[{lane}]");
        let bits = if integer {
            format!("({field} & {}u)", mask(channel.width))
        } else {
            if matches!(channel.encoding, Encoding::Uint | Encoding::Sint) {
                return Err(Error::Invalid("float clear of an integer UAV"));
            }
            if matches!(channel.encoding, Encoding::Snorm) {
                channel.encode(&format!("select({field}, 0u, dx_nan({field}))"))
            } else {
                channel.encode(&field)
            }
        };
        writeln!(
            source,
            "  packed[{}] |= {bits} << {}u;",
            channel.offset / 32,
            channel.offset % 32
        )
        .unwrap();
    }
    source.push_str("  return packed;\n}\n");
    Ok(source)
}

pub(super) const PACK_UFLOAT: &str = "
fn dx_pack_ufloat(bits: u32, mantissa_bits: u32) -> u32 {
    let exp = (bits >> 23u) & 255u;
    let frac = bits & 0x7fffffu;
    let infinity = 31u << mantissa_bits;
    if exp == 255u && frac != 0u { return infinity | (1u << (mantissa_bits - 1u)); }
    if bits >> 31u != 0u || exp < 112u - mantissa_bits { return 0u; }
    if exp > 142u { return infinity; }
    let shift = (23u - mantissa_bits) + (113u - min(exp, 113u));
    let base = select(frac, frac | 0x800000u, exp < 113u);
    let half = 1u << (shift - 1u);
    let rounded = (base + half - 1u + ((base >> shift) & 1u)) >> shift;
    let exponent = max(exp, 112u) - 112u;
    return (exponent << mantissa_bits) + rounded;
}
";

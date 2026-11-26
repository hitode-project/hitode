use super::*;
use passes::variant::{Interface, Lane, Storage, Varying};

pub(super) const RASTER_SLOT: u32 = 164;

pub const STATISTICS_SLOT: u32 = 160;

impl Writer<'_> {
    pub(super) fn varying_fields(&mut self, variant: &Interface) {
        if let Some(storage) = &variant.storage {
            self.line("@location(0) @interpolate(flat) dx_base: u32,");
            for (i, mode) in storage.modes.iter().enumerate() {
                self.line(&format!(
                    "@location({}){} dx_bary{i}: vec2<f32>,",
                    i + 1,
                    interpolation(*mode)
                ));
            }
        }
        for field in &variant.varyings {
            self.line(&format!(
                "@location({}){} dx_v{}: {},",
                field.location,
                interpolation(field.mode),
                field.location,
                field.ty()
            ));
        }
    }

    pub(super) fn clip_field(&mut self, variant: &Interface) {
        if variant.native_clip {
            self.line(&format!(
                "@builtin(clip_distances) dx_clip: array<f32, {}>,",
                variant.clip.len()
            ));
        }
    }

    pub(super) fn variant_inputs(&mut self, variant: &Interface) {
        if variant.count_fragments {
            self.line("let dx_previous = atomicAdd(&dx_statistics[0], 1u);");
            self.line("if dx_previous == 0xffffffffu { atomicAdd(&dx_statistics[1], 1u); }");
        }
        if let Some(storage) = &variant.storage {
            self.storage_inputs(storage);
        }
        if variant.discard_clip {
            let tests = variant
                .clip_locations
                .iter()
                .map(|location| format!("input.dx_v{location} < 0.0"))
                .collect::<Vec<_>>();
            self.line(&format!("if {} {{ discard; }}", tests.join(" || ")));
        }
        for field in &variant.varyings {
            for (component, lane) in field.lanes.iter().enumerate() {
                if let Some(lane) = lane {
                    let field_name = format!("input.dx_v{}", field.location);
                    let value = if field.lanes.len() == 1 {
                        field_name
                    } else {
                        format!("{field_name}.{}", LANES.as_bytes()[component] as char)
                    };
                    let value = if field.scalar() == "u32" {
                        value
                    } else {
                        format!("bitcast<u32>({value})")
                    };
                    let dst = access("v", *lane);
                    self.line(&format!("{dst} = {value};"));
                }
            }
        }
    }
    pub(super) fn storage_varying_binding(&mut self, storage: &Storage) {
        self.line(&format!("@group(3) @binding({RASTER_SLOT}) var<storage, read> dx_vertex_values: array<array<vec4<u32>, {}>>;", storage.registers + 1));
    }

    fn storage_inputs(&mut self, storage: &Storage) {
        for (i, _) in storage.modes.iter().enumerate() {
            self.line(&format!("let dx_f{i} = input.dx_bary{i};"));
        }
        for (source_field, target_field) in &storage.varyings {
            for (&source, &target) in source_field.lanes.iter().zip(&target_field.lanes) {
                let Some(target) = target else { continue };
                let read = |vertex| {
                    source.map_or_else(
                        || "0u".into(),
                        |lane| {
                            access(
                                &format!("dx_vertex_values[input.dx_base + {vertex}u]"),
                                lane,
                            )
                        },
                    )
                };
                let value = if source_field.mode == Interpolation::Flat {
                    read(0)
                } else {
                    let mode = storage.mode_index(source_field.mode);
                    let weight = [
                        format!("dx_f{mode}.x"),
                        format!("dx_f{mode}.y"),
                        format!("(1.0 - dx_f{mode}.x - dx_f{mode}.y)"),
                    ];
                    let terms: Vec<_> = (0..storage.vertices)
                        .map(|v| format!("bitcast<f32>({}) * {}", read(v), weight[v as usize]))
                        .collect();
                    format!("bitcast<u32>({})", terms.join(" + "))
                };
                self.line(&format!("{} = {value};", access("v", target)));
            }
        }
    }
}

pub(super) fn variant_outputs(variant: &Interface, register: &str) -> Vec<String> {
    let mut outputs: Vec<_> = variant
        .varyings
        .iter()
        .map(|field| {
            let values = field
                .lanes
                .iter()
                .map(|lane| scalar(register, *lane, field.scalar(), "0u"));
            if field.lanes.len() == 1 {
                values.into_iter().next().unwrap()
            } else {
                format!("{}({})", field.ty(), values.collect::<Vec<_>>().join(", "))
            }
        })
        .collect();
    if variant.native_clip {
        let distances: Vec<_> = variant
            .clip
            .iter()
            .map(|lane| scalar(register, *lane, "f32", "0x3f800000u"))
            .collect();
        outputs.push(format!(
            "array<f32, {}>({})",
            distances.len(),
            distances.join(", ")
        ));
    }
    outputs
}

fn access(register: &str, lane: Lane) -> String {
    format!(
        "{register}[{}].{}",
        lane.register,
        LANES.as_bytes()[lane.component] as char
    )
}

fn scalar(register: &str, lane: Option<Lane>, ty: &str, initial: &str) -> String {
    let raw = lane.map_or_else(|| initial.to_owned(), |lane| access(register, lane));
    if ty == "u32" {
        raw
    } else {
        format!("bitcast<{ty}>({raw})")
    }
}

impl Varying {
    fn scalar(&self) -> &'static str {
        match self.ty {
            ValueType::Float => "f32",
            ValueType::Int => "i32",
            _ => "u32",
        }
    }

    fn ty(&self) -> String {
        if self.lanes.len() == 1 {
            self.scalar().into()
        } else {
            format!("vec{}<{}>", self.lanes.len(), self.scalar())
        }
    }
}

fn interpolation(mode: Interpolation) -> &'static str {
    match mode {
        Interpolation::Undefined | Interpolation::Perspective => "",
        Interpolation::Flat => " @interpolate(flat)",
        Interpolation::PerspectiveCentroid => " @interpolate(perspective, centroid)",
        Interpolation::Linear => " @interpolate(linear)",
        Interpolation::LinearCentroid => " @interpolate(linear, centroid)",
        Interpolation::PerspectiveSample => " @interpolate(perspective, sample)",
        Interpolation::LinearSample => " @interpolate(linear, sample)",
    }
}

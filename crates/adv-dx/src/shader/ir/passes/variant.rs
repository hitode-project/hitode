//! Draw-specific interface and entry operations, after the semantic passes.

use super::*;
#[cfg(feature = "gpu")]
use crate::shader::variant::{self, Variant};

#[derive(Clone, Copy)]
pub(crate) struct Lane {
    pub register: u32,
    pub component: usize,
}

pub(crate) struct Varying {
    pub location: usize,
    pub ty: ValueType,
    pub mode: Interpolation,
    pub lanes: Vec<Option<Lane>>,
}

#[derive(Default)]
pub(crate) struct Interface {
    pub varyings: Vec<Varying>,
    pub clip: Vec<Option<Lane>>,
    pub clip_locations: Vec<usize>,
    pub native_clip: bool,
    pub storage: Option<Storage>,
    pub discard_clip: bool,
    pub count_fragments: bool,
    #[cfg(feature = "gpu")]
    pub pulled: bool,
}

pub(crate) struct Storage {
    pub vertices: u32,
    pub registers: u32,
    pub varyings: Vec<(Varying, Varying)>,
    pub modes: Vec<Interpolation>,
}

impl Storage {
    pub fn mode_index(&self, mode: Interpolation) -> usize {
        self.modes
            .iter()
            .position(|&current| current == mode)
            .unwrap()
    }
}

#[cfg(feature = "gpu")]
pub(crate) fn storage_varyings(
    interface: &mut Interface,
    stage: &Reflection,
    producer: &Reflection,
    fragment: &Reflection,
    vertices: u32,
    registers: u32,
) {
    let fields = pack(producer, fragment);
    let fragment_fields = pack(fragment, fragment);
    let modes = fields
        .iter()
        .map(|field| field.mode)
        .filter(|&mode| mode != Interpolation::Flat)
        .fold(Vec::new(), |mut modes, mode| {
            if !modes.contains(&mode) {
                modes.push(mode);
            }
            modes
        });
    let base = modes.len() + 1;
    let packed = pack(stage, fragment).len();
    interface.varyings.drain(..packed);
    for (i, field) in interface.varyings.iter_mut().enumerate() {
        field.location = base + i;
    }
    for (i, location) in interface.clip_locations.iter_mut().enumerate() {
        *location = base + i;
    }
    interface.storage = Some(Storage {
        vertices,
        registers,
        modes,
        varyings: fields.into_iter().zip(fragment_fields).collect(),
    });
}

#[cfg(feature = "gpu")]
pub(crate) fn apply(
    module: &mut Module,
    reflection: &Reflection,
    variant: &Variant,
    fragment: Option<&Reflection>,
) -> Result<Interface, Error> {
    let mut interface = Interface::default();
    if variant.pack() {
        if let Some(fragment) = if module.stage == Stage::Pixel {
            Some(reflection)
        } else {
            fragment
        } {
            interface.varyings = pack(reflection, fragment);
        }
        #[cfg(test)]
        verify(module)?;
    }
    if variant.clip_as_discard() || variant.clip_varyings() || variant.native_clip() {
        clip(&mut interface, reflection, variant);
        #[cfg(test)]
        verify(module)?;
    }
    if variant.early_tests() {
        early_tests(module);
        #[cfg(test)]
        verify(module)?;
    }
    if variant.count_fragments() {
        interface.count_fragments = true;
        #[cfg(test)]
        verify(module)?;
    }
    if variant.pulled() {
        interface.pulled = true;
        #[cfg(test)]
        verify(module)?;
    }
    Ok(interface)
}

#[cfg(feature = "gpu")]
fn pack(stage: &Reflection, fragment: &Reflection) -> Vec<Varying> {
    let mut fields = Vec::new();
    for input in &fragment.signatures.input {
        if input.system_value != SystemValue::None {
            continue;
        }
        let source = if stage.stage == Stage::Pixel {
            Some(input)
        } else {
            stage.signatures.output.iter().find(|output| {
                output.semantic.eq_ignore_ascii_case(&input.semantic)
                    && output.semantic_index == input.semantic_index
            })
        };
        let output_lanes = source
            .into_iter()
            .flat_map(lanes)
            .map(Some)
            .chain(std::iter::repeat(None));
        let mode = variant::interpolation(fragment, input.value_type, input.register);
        for (_, source) in lanes(input).zip(output_lanes) {
            push(&mut fields, input.value_type, mode, source);
        }
    }
    fields
}

#[cfg(feature = "gpu")]
fn push(fields: &mut Vec<Varying>, ty: ValueType, mode: Interpolation, lane: Option<Lane>) {
    if let Some(field) = fields
        .iter_mut()
        .find(|field| field.ty == ty && field.mode == mode && field.lanes.len() < 4)
    {
        field.lanes.push(lane);
    } else {
        fields.push(Varying {
            location: fields.len(),
            ty,
            mode,
            lanes: vec![lane],
        });
    }
}

#[cfg(feature = "gpu")]
fn clip(interface: &mut Interface, reflection: &Reflection, variant: &Variant) {
    let input = reflection.stage == Stage::Pixel;
    let elements = if input {
        &reflection.signatures.input
    } else {
        &reflection.signatures.output
    };
    interface
        .clip
        .resize(usize::from(variant.clip_components()), None);
    for element in elements
        .iter()
        .filter(|element| element.system_value == SystemValue::ClipDistance)
    {
        for (lane, value) in lanes(element).enumerate() {
            let index = element.semantic_index as usize * 4 + lane;
            if let Some(slot) = interface.clip.get_mut(index) {
                *slot = Some(value);
            }
        }
    }
    let as_varyings = variant.clip_as_discard() || variant.clip_varyings();
    interface.native_clip = variant.native_clip();
    if as_varyings {
        for lane in &interface.clip {
            interface.clip_locations.push(interface.varyings.len());
            interface.varyings.push(Varying {
                location: interface.varyings.len(),
                ty: ValueType::Float,
                mode: Interpolation::Perspective,
                lanes: vec![*lane],
            });
        }
        interface.discard_clip = input && variant.clip_as_discard();
    }
}

#[cfg(feature = "gpu")]
pub(crate) fn lanes(element: &SignatureElement) -> impl Iterator<Item = Lane> {
    (0..4)
        .filter(|&component| element.mask & (1 << component) != 0)
        .map(|component| Lane {
            register: element.register,
            component,
        })
}

#[cfg(feature = "gpu")]
fn early_tests(module: &mut Module) {
    let flags = crate::shader::dxbc::fields::D3D11_SB_GLOBAL_FLAG_FORCE_EARLY_DEPTH_STENCIL;
    if let Some(decl) = module
        .body
        .iter_mut()
        .find_map(|statement| match statement {
            Statement::Declare(decl) if decl.opcode == Opcode::DclGlobalFlags => Some(decl),
            _ => None,
        })
    {
        decl.controls |= flags;
    } else {
        module.body.insert(
            0,
            Statement::Declare(Declaration {
                opcode: Opcode::DclGlobalFlags,
                operand: None,
                controls: flags,
                data: Vec::new(),
                binding: None,
                interpolation: None,
            }),
        );
    }
}

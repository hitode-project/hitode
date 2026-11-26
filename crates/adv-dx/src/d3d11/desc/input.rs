//! Resolve per-stream offsets and classify input elements against the shader signature.
use super::Error;
use crate::d3d11::caps::{FormatOperation, Report, Requirement};
use crate::dxgi::formats::{FormatLayout, format_layout};
use crate::shader::{
    dxbc,
    ir::{Stage, SystemValue},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputRate {
    Vertex,
    Instance(u32),
}

pub struct InputElement {
    semantic: Vec<u8>,
    semantic_index: u32,
    pub format: &'static FormatLayout,
    pub slot: usize,
    pub offset: u32,
    pub rate: InputRate,
}

pub struct InputLayout(Vec<InputElement>);

impl InputLayout {
    pub fn new(
        entries: impl IntoIterator<Item = (Vec<u8>, adv_abi::types::D3D11_INPUT_ELEMENT_DESC)>,
        signature: &[u8],
        report: &Report,
    ) -> Result<Self, Error> {
        let module = dxbc::parse(signature).map_err(|_| Error::InvalidArgument)?;
        if module.stage != Stage::Vertex {
            return Err(Error::InvalidArgument);
        }
        let mut elements: Vec<InputElement> = Vec::new();
        let mut streams = [None; 32];
        for (semantic, raw) in entries {
            let slot = raw.InputSlot as usize;
            if slot >= report.level.limit(Requirement::IaSlots) as usize
                || elements.iter().any(|entry| {
                    entry.semantic_index == raw.SemanticIndex
                        && entry.semantic.eq_ignore_ascii_case(&semantic)
                })
            {
                return Err(Error::InvalidArgument);
            }
            let rate = match raw.InputSlotClass {
                0 if raw.InstanceDataStepRate == 0 => InputRate::Vertex,
                1 => InputRate::Instance(raw.InstanceDataStepRate),
                _ => return Err(Error::InvalidArgument),
            };
            let format = format_layout(raw.Format as u32)
                .filter(|format| report.supports(format.format, FormatOperation::VertexBuffer))
                .ok_or(Error::InvalidArgument)?;
            let bytes = u32::from(format.aligned_unit_bytes);
            let (previous_rate, end) = streams[slot].unwrap_or((rate, 0u32));
            if previous_rate != rate {
                return Err(Error::InvalidArgument);
            }
            let offset = if raw.AlignedByteOffset == u32::MAX {
                let alignment = (1u32 << bytes.trailing_zeros()).min(4);
                end.next_multiple_of(alignment)
            } else {
                raw.AlignedByteOffset
            };
            let end = offset
                .checked_add(bytes)
                .filter(|&end| end <= 2048)
                .ok_or(Error::InvalidArgument)?;
            streams[slot] = Some((rate, end));
            elements.push(InputElement {
                semantic,
                semantic_index: raw.SemanticIndex,
                format,
                slot,
                offset,
                rate,
            });
        }
        let layout = Self(elements);
        for input in &module.signatures.input {
            if input.system_value == SystemValue::None
                && layout
                    .element(input.semantic.as_bytes(), input.semantic_index)
                    .is_none()
            {
                return Err(Error::InvalidArgument);
            }
        }
        Ok(layout)
    }

    pub fn element(&self, semantic: &[u8], index: u32) -> Option<&InputElement> {
        self.0.iter().find(|element| {
            element.semantic_index == index && element.semantic.eq_ignore_ascii_case(semantic)
        })
    }
}

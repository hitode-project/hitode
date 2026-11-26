//! Operands and their index, literal and extension tokens.

use super::{Error, fields::*, take_word, take_words};

#[derive(Debug, PartialEq, Eq)]
pub struct Operand<'a> {
    pub kind: u32,
    pub components: u32,
    /// Component selection and unused token bits at their original positions.
    pub controls: u32,
    pub extensions: Vec<Extension>,
    pub indices: Vec<Index<'a>>,
    pub literals: &'a [u32],
}

#[derive(Debug, PartialEq, Eq)]
pub enum Index<'a> {
    Immediate32(u32),
    Immediate64([u32; 2]),
    Relative(Box<Operand<'a>>),
    Immediate32Relative(u32, Box<Operand<'a>>),
    Immediate64Relative([u32; 2], Box<Operand<'a>>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extension {
    pub kind: u32,
    pub controls: u32,
}

impl<'a> Operand<'a> {
    pub fn decode(words: &mut &'a [u32]) -> Result<Self, Error> {
        let token = take_word(words)?;
        let kind = decode_operand_type(token);
        let components = decode_operand_num_components(token);
        let dimensions = decode_operand_index_dimension(token);
        let mut controls = token
            & !(D3D10_SB_OPERAND_TYPE_MASK
                | D3D10_SB_OPERAND_NUM_COMPONENTS_MASK
                | D3D10_SB_OPERAND_INDEX_DIMENSION_MASK
                | D3D10_SB_OPERAND_EXTENDED_MASK);

        let mut extensions = Vec::new();
        let mut continued = decode_is_operand_extended(token) != 0;
        while continued {
            let ext = take_word(words)?;
            extensions.push(Extension {
                kind: decode_extended_operand_type(ext),
                controls: ext
                    & !(D3D10_SB_EXTENDED_OPERAND_TYPE_MASK
                        | D3D10_SB_OPERAND_DOUBLE_EXTENDED_MASK),
            });
            continued = decode_is_operand_double_extended(ext) != 0;
        }
        let mut indices = Vec::new();
        for dim in 0..dimensions {
            let index = match decode_operand_index_representation(dim, token) {
                D3D10_SB_OPERAND_INDEX_IMMEDIATE32 => Index::Immediate32(take_word(words)?),
                D3D10_SB_OPERAND_INDEX_IMMEDIATE64 => Index::Immediate64(pair(words)?),
                D3D10_SB_OPERAND_INDEX_RELATIVE => Index::Relative(Box::new(Self::decode(words)?)),
                D3D10_SB_OPERAND_INDEX_IMMEDIATE32_PLUS_RELATIVE => {
                    Index::Immediate32Relative(take_word(words)?, Box::new(Self::decode(words)?))
                }
                D3D10_SB_OPERAND_INDEX_IMMEDIATE64_PLUS_RELATIVE => {
                    Index::Immediate64Relative(pair(words)?, Box::new(Self::decode(words)?))
                }
                _ => return Err(Error::OperandIndex),
            };
            indices.push(index);
            controls &= !operand_index_representation_mask(dim);
        }
        let literals = match kind {
            D3D10_SB_OPERAND_TYPE_IMMEDIATE32 | D3D10_SB_OPERAND_TYPE_IMMEDIATE64 => {
                let words_per_scalar = if kind == D3D10_SB_OPERAND_TYPE_IMMEDIATE64 {
                    2
                } else {
                    1
                };
                let count = match components {
                    D3D10_SB_OPERAND_0_COMPONENT => 0,
                    D3D10_SB_OPERAND_1_COMPONENT => words_per_scalar,
                    // Four register lanes hold two 64-bit values or four 32-bit values.
                    D3D10_SB_OPERAND_4_COMPONENT => 4,
                    _ => return Err(Error::OperandComponents),
                };
                take_words(words, count)?
            }
            _ => &[],
        };
        Ok(Self {
            kind,
            components,
            controls,
            extensions,
            indices,
            literals,
        })
    }

    pub fn encode(&self, words: &mut Vec<u32>) {
        let head = words.len();
        words.push(
            encode_operand_type(self.kind)
                | encode_operand_num_components(self.components)
                | encode_operand_index_dimension(self.indices.len() as u32)
                | encode_operand_extended(u32::from(!self.extensions.is_empty()))
                | self.controls,
        );
        for (index, ext) in self.extensions.iter().enumerate() {
            words.push(
                encode_extended_operand_type(ext.kind)
                    | ext.controls
                    | encode_operand_double_extended(u32::from(index + 1 < self.extensions.len())),
            );
        }
        for (dimension, index) in self.indices.iter().enumerate() {
            let repr = match index {
                Index::Immediate32(value) => {
                    words.push(*value);
                    D3D10_SB_OPERAND_INDEX_IMMEDIATE32
                }
                Index::Immediate64(value) => {
                    words.extend(value);
                    D3D10_SB_OPERAND_INDEX_IMMEDIATE64
                }
                Index::Relative(relative) => {
                    relative.encode(words);
                    D3D10_SB_OPERAND_INDEX_RELATIVE
                }
                Index::Immediate32Relative(value, relative) => {
                    words.push(*value);
                    relative.encode(words);
                    D3D10_SB_OPERAND_INDEX_IMMEDIATE32_PLUS_RELATIVE
                }
                Index::Immediate64Relative(value, relative) => {
                    words.extend(value);
                    relative.encode(words);
                    D3D10_SB_OPERAND_INDEX_IMMEDIATE64_PLUS_RELATIVE
                }
            };
            words[head] |= encode_operand_index_representation(dimension as u32, repr);
        }
        words.extend_from_slice(self.literals);
    }
}

fn pair(words: &mut &[u32]) -> Result<[u32; 2], Error> {
    let pair = take_words(words, 2)?;
    Ok([pair[0], pair[1]])
}

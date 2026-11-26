//! Lossless token streams of SHDR and SHEX chunks in DXBC containers.
mod checksum;
mod container;
mod disassemble;
pub use disassemble::disassemble;
pub mod linkage;
mod lower;
pub use container::{Container, ParseError, parse};
pub mod fields;
pub mod operand;

use fields::*;
use operand::Operand;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Version {
    pub stage: u32,
    pub major: u32,
    pub minor: u32,
    pub reserved: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Truncated,
    ProgramLength,
    InstructionLength,
    OperandComponents,
    OperandIndex,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Program<'a> {
    pub version: Version,
    pub instructions: Vec<Instruction<'a>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Instruction<'a> {
    CustomData {
        class: u32,
        words: &'a [u32],
    },
    Operation {
        opcode: u32,
        /// Opcode-specific control bits, at their original positions.
        controls: u32,
        length_word: bool,
        extensions: Vec<OpcodeExtension>,
        body: Body<'a>,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum Body<'a> {
    Operands(Vec<Operand<'a>>),
    InterfaceCall {
        function: u32,
        interface: Operand<'a>,
    },
    Declaration {
        operand: Option<Operand<'a>>,
        /// Declaration-specific fields outside the opcode and operand tokens.
        words: &'a [u32],
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpcodeExtension {
    pub kind: u32,
    pub controls: u32,
}

impl OpcodeExtension {
    fn decode(token: u32) -> Self {
        Self {
            kind: decode_extended_opcode_type(token),
            controls: token & !(D3D10_SB_EXTENDED_OPCODE_TYPE_MASK | D3D10_SB_OPCODE_EXTENDED_MASK),
        }
    }

    fn encode(self, continued: bool) -> u32 {
        encode_extended_opcode_type(self.kind)
            | self.controls
            | encode_opcode_extended(u32::from(continued))
    }
}

impl<'a> Program<'a> {
    pub fn decode(mut words: &'a [u32]) -> Result<Self, Error> {
        let token = take_word(&mut words)?;
        let version = Version {
            stage: decode_tokenized_program_type(token),
            major: decode_tokenized_program_major_version(token),
            minor: decode_tokenized_program_minor_version(token),
            reserved: token
                & !(D3D10_SB_TOKENIZED_PROGRAM_TYPE_MASK
                    | D3D10_SB_TOKENIZED_PROGRAM_MAJOR_VERSION_MASK
                    | D3D10_SB_TOKENIZED_PROGRAM_MINOR_VERSION_MASK),
        };
        if decode_tokenized_program_length(take_word(&mut words)?) as usize != words.len() + 2 {
            return Err(Error::ProgramLength);
        }
        let mut instructions = Vec::new();
        while !words.is_empty() {
            instructions.push(Instruction::decode(&mut words)?);
        }
        Ok(Self {
            version,
            instructions,
        })
    }

    pub fn encode(&self) -> Vec<u32> {
        let Version {
            stage,
            major,
            minor,
            reserved,
        } = self.version;
        let mut words = vec![
            encode_tokenized_program_version_token(stage, major, minor) | reserved,
            0,
        ];
        for instruction in &self.instructions {
            instruction.encode(&mut words);
        }
        words[1] = encode_tokenized_program_length(words.len() as u32);
        words
    }
}

impl<'a> Instruction<'a> {
    fn decode(words: &mut &'a [u32]) -> Result<Self, Error> {
        let start = *words;
        let token = take_word(words)?;
        let opcode = decode_opcode_type(token);
        let custom = opcode == D3D10_SB_OPCODE_CUSTOMDATA;
        let extended = decode_is_opcode_extended(token) != 0;
        let length_word = extended
            && matches!(
                opcode,
                D3D11_SB_OPCODE_DCL_FUNCTION_BODY
                    | D3D11_SB_OPCODE_DCL_FUNCTION_TABLE
                    | D3D11_SB_OPCODE_DCL_INTERFACE
            );
        let length = if custom || length_word {
            take_word(words)?
        } else {
            decode_tokenized_instruction_length(token)
        } as usize;
        let header = start.len() - words.len();
        let mut payload = take_words(
            words,
            length.checked_sub(header).ok_or(Error::InstructionLength)?,
        )?;
        if custom {
            return Ok(Self::CustomData {
                class: decode_customdata_class(token),
                words: payload,
            });
        }
        let mut extensions = Vec::new();
        let mut continued = extended && !length_word;
        while continued {
            let ext = take_word(&mut payload)?;
            extensions.push(OpcodeExtension::decode(ext));
            continued = decode_is_opcode_extended(ext) != 0;
        }
        let body = if let Some(has_operand) = declaration_has_operand(opcode) {
            let operand = has_operand
                .then(|| Operand::decode(&mut payload))
                .transpose()?;
            Body::Declaration {
                operand,
                words: payload,
            }
        } else if opcode == D3D11_SB_OPCODE_INTERFACE_CALL {
            let function = take_word(&mut payload)?;
            let interface = Operand::decode(&mut payload)?;
            if !payload.is_empty() {
                return Err(Error::InstructionLength);
            }
            Body::InterfaceCall {
                function,
                interface,
            }
        } else {
            let mut operands = Vec::new();
            while !payload.is_empty() {
                operands.push(Operand::decode(&mut payload)?);
            }
            Body::Operands(operands)
        };
        let mut controls = token & !(D3D10_SB_OPCODE_TYPE_MASK | D3D10_SB_OPCODE_EXTENDED_MASK);
        if !length_word {
            controls &= !D3D10_SB_TOKENIZED_INSTRUCTION_LENGTH_MASK;
        }
        Ok(Self::Operation {
            opcode,
            controls,
            length_word,
            extensions,
            body,
        })
    }

    fn encode(&self, words: &mut Vec<u32>) {
        let start = words.len();
        match self {
            Self::CustomData { class, words: data } => {
                words.extend([encode_customdata_class(*class), 2 + data.len() as u32]);
                words.extend_from_slice(data);
            }
            Self::Operation {
                opcode,
                controls,
                length_word,
                extensions,
                body,
            } => {
                words.push(
                    encode_opcode_type(*opcode)
                        | controls
                        | encode_opcode_extended(u32::from(*length_word || !extensions.is_empty())),
                );
                if *length_word {
                    words.push(0);
                }
                for (index, extension) in extensions.iter().enumerate() {
                    words.push(extension.encode(index + 1 < extensions.len()));
                }
                match body {
                    Body::Operands(operands) => {
                        for operand in operands {
                            operand.encode(words);
                        }
                    }
                    Body::InterfaceCall {
                        function,
                        interface,
                    } => {
                        words.push(*function);
                        interface.encode(words);
                    }
                    Body::Declaration {
                        operand,
                        words: data,
                    } => {
                        if let Some(operand) = operand {
                            operand.encode(words);
                        }
                        words.extend_from_slice(data);
                    }
                }
                let length = (words.len() - start) as u32;
                if *length_word {
                    words[start + 1] = length;
                } else {
                    words[start] |= encode_tokenized_instruction_length(length);
                }
            }
        }
    }
}

fn take_words<'a>(words: &mut &'a [u32], count: usize) -> Result<&'a [u32], Error> {
    let (head, tail) = words.split_at_checked(count).ok_or(Error::Truncated)?;
    *words = tail;
    Ok(head)
}

fn take_word(words: &mut &[u32]) -> Result<u32, Error> {
    Ok(take_words(words, 1)?[0])
}

#[cfg(test)]
mod coverage;
#[cfg(all(test, feature = "gpu"))]
pub(crate) use coverage::pixel_uav_fixture;

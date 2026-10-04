//! The x86 projection of pinned, official Win32Metadata; no SDK or WinMD at runtime.
use std::sync::OnceLock;

use crate::Conv;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbiKind {
    Void,
    U32,
    I32,
    U64,
    I64,
    F32,
    F64,
    Pointer,
    Handle,
    AnsiString,
    WideString,
    Aggregate,
    Unsupported,
    /// An implementation may suspend or call the guest and return an Outcome later.
    Outcome,
}

impl AbiKind {
    pub const fn accepts(self, actual: Self) -> bool {
        self as u8 == actual as u8
            || matches!(
                (self, actual),
                (Self::U32, Self::I32) | (Self::I32, Self::U32)
            )
    }
    fn parse(s: &str) -> Self {
        match s {
            "void" => Self::Void,
            "u32" => Self::U32,
            "i32" => Self::I32,
            "u64" => Self::U64,
            "i64" => Self::I64,
            "f32" => Self::F32,
            "f64" => Self::F64,
            "ptr" => Self::Pointer,
            "handle" => Self::Handle,
            "str8" => Self::AnsiString,
            "str16" => Self::WideString,
            "aggregate" => Self::Aggregate,
            "unsupported" => Self::Unsupported,
            _ => panic!("invalid generated ABI kind {s}"),
        }
    }
}

#[derive(Debug)]
pub struct Parameter {
    pub name: &'static str,
    pub type_name: &'static str,
    pub kind: AbiKind,
    /// Number of x86 stack words, distinct from the number of parameters.
    pub words: u8,
}

#[derive(Debug)]
pub struct Signature {
    pub module: &'static str,
    pub name: &'static str,
    pub return_type: &'static str,
    pub return_kind: AbiKind,
    pub parameters: Vec<Parameter>,
    pub conv: Option<Conv>,
    /// Size of a data export's cell; data has no calling convention or parameters.
    pub data_size: Option<u32>,
    /// A COM signature includes the guest `this` pointer as its first parameter.
    pub com_slot: Option<u16>,
}

impl Signature {
    pub fn stack_words(&self) -> u16 {
        self.parameters.iter().map(|p| u16::from(p.words)).sum()
    }

    pub fn callable(&self) -> bool {
        self.conv.is_some()
            && !matches!(self.return_kind, AbiKind::Aggregate | AbiKind::Unsupported)
            && self
                .parameters
                .iter()
                .all(|p| p.kind != AbiKind::Unsupported && p.words > 0)
    }
}

fn params(text: &'static str) -> Vec<Parameter> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split('|')
        .map(|p| {
            let mut fields = p.split(':');
            let parameter = Parameter {
                name: fields.next().expect("parameter name"),
                type_name: fields.next().expect("parameter type"),
                kind: AbiKind::parse(fields.next().expect("parameter kind")),
                words: fields
                    .next()
                    .expect("stack words")
                    .parse()
                    .expect("numeric stack words"),
            };
            assert!(fields.next().is_none(), "extra parameter fields");
            parameter
        })
        .collect()
}

pub fn signatures() -> &'static [Signature] {
    static TABLE: OnceLock<Vec<Signature>> = OnceLock::new();
    TABLE.get_or_init(|| {
        include_str!("../../../data/abi/signatures.tsv")
            .lines()
            .filter(|line| !line.starts_with('#') && !line.is_empty())
            .map(|line| {
                let fields: Vec<_> = line.split('\t').collect();
                assert_eq!(fields.len(), 6, "invalid generated signature");
                let parameters = params(fields[5]);
                let words: u16 = parameters.iter().map(|p| u16::from(p.words)).sum();
                let conv = u8::try_from(words).ok().and_then(|argc| match fields[2] {
                    "stdcall" => Some(Conv::Stdcall { argc }),
                    "cdecl" => Some(Conv::Cdecl { argc }),
                    "variadic" => Some(Conv::CdeclVariadic { fixed: argc }),
                    _ => None,
                });
                Signature {
                    module: fields[0],
                    name: fields[1],
                    return_kind: AbiKind::parse(fields[3]),
                    return_type: fields[4],
                    parameters,
                    conv,
                    data_size: (fields[2] == "data").then(|| match fields[3] {
                        "ptr" | "u32" | "i32" => 4,
                        "u64" => 8,
                        _ => panic!("invalid generated data kind"),
                    }),
                    com_slot: None,
                }
            })
            .collect()
    })
}

pub fn lookup(module: &str, name: &str) -> Option<&'static Signature> {
    let module = module.to_ascii_lowercase();
    let table = signatures();
    table
        .binary_search_by(|s| (s.module, s.name).cmp(&(module.as_str(), name)))
        .ok()
        .map(|i| &table[i])
}

pub fn com_signatures() -> &'static [Signature] {
    static TABLE: OnceLock<Vec<Signature>> = OnceLock::new();
    TABLE.get_or_init(|| {
        include_str!("../../../data/abi/com.tsv")
            .lines()
            .filter(|line| !line.starts_with('#') && !line.is_empty())
            .map(|line| {
                let fields: Vec<_> = line.split('\t').collect();
                assert_eq!(fields.len(), 6);
                let parameters = params(fields[5]);
                let words: u16 = parameters.iter().map(|p| u16::from(p.words)).sum();
                Signature {
                    module: fields[0],
                    name: fields[2],
                    return_kind: AbiKind::parse(fields[3]),
                    return_type: fields[4],
                    parameters,
                    conv: u8::try_from(words).ok().map(|argc| Conv::Stdcall { argc }),
                    com_slot: Some(fields[1].parse().expect("COM slot")),
                    data_size: None,
                }
            })
            .collect()
    })
}

pub fn lookup_com(interface: &str, method: &str) -> Option<&'static Signature> {
    com_signatures()
        .iter()
        .find(|s| s.module == interface && s.name == method)
}

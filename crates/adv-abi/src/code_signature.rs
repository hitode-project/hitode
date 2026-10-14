//! Exact, external code fingerprints for reviewed library replacements.
//!
//! This module has no filesystem, loader, codec or API knowledge. The caller
//! supplies executable ranges and a checked guest reader. Only explicitly
//! declared absolute m32 operands are zeroed for SHA-256; every other byte,
//! including relative calls and branches, is covered. A match retains original
//! code/constant snapshots for the installer to revalidate before each call.
//!
//! Text schema v1 (one directive per line, `#` comments): `format 1`, `id NAME`,
//! `source TEXT`, `license TEXT`, `profile KERNEL`, `candidate cdecl-stack32|frame-ebp32`,
//! `entry ROLE`, `property NAME U32`, `function ROLE LENGTH SHA256 PROLOGUE_LEN`,
//! `constant ROLE F32_BITS_HEX ABS32_OPERAND_OFFSET...`, `call ROLE OFFSET ROLE`.
//! Integers are decimal or `0x` hexadecimal. Helper prologues use length zero.
//! Duplicate definitions, unknown directives and incomplete closures fail closed.
use iced_x86::{Decoder, DecoderOptions, FlowControl, Mnemonic, OpKind, Register};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

const MAX_TEXT: usize = 64 * 1024;
const MAX_FUNCTIONS: usize = 16;
const MAX_FUNCTION_LEN: usize = 16 * 1024;
const MAX_OPERANDS: usize = 128;
const MAX_CALLS: usize = 64;
const MAX_RANGES: usize = 512;
const MAX_SCAN_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CANDIDATES: usize = 4096;
const MAX_MATCHES: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub kernel: String,
    pub properties: BTreeMap<String, u32>,
}

#[derive(Clone, Debug)]
struct Constant {
    offset: usize,
    bits: u32,
}

#[derive(Clone, Debug)]
struct Call {
    offset: usize,
    target: String,
}

#[derive(Clone, Debug)]
struct Function {
    len: usize,
    digest: [u8; 32],
    prologue_len: usize,
    constants: Vec<Constant>,
    calls: Vec<Call>,
}

#[derive(Clone, Debug)]
pub struct Signature {
    pub id: String,
    pub source: String,
    pub license: String,
    pub profile: Profile,
    entry: String,
    frame_ebp: bool,
    functions: BTreeMap<String, Function>,
}

#[derive(Clone, Copy, Debug)]
pub struct ExecutableRange {
    pub start: u32,
    pub len: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionSnapshot {
    pub role: String,
    pub entry: u32,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConstantSnapshot {
    pub address: u32,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub entry: u32,
    pub prologue_len: usize,
    pub profile: Profile,
    pub functions: Vec<FunctionSnapshot>,
    pub constants: Vec<ConstantSnapshot>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Invalid(String),
    Limit(&'static str),
    Read {
        address: u32,
        len: usize,
        message: String,
    },
    NotFound,
    Ambiguous(String),
    Verification {
        role: String,
        message: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(f, "invalid code signature: {message}"),
            Self::Limit(which) => write!(f, "code signature {which} limit exceeded"),
            Self::Read {
                address,
                len,
                message,
            } => write!(f, "signature read {address:#x}+{len:#x}: {message}"),
            Self::NotFound => f.write_str("no exact library signature match"),
            Self::Ambiguous(message) => write!(f, "ambiguous code signature: {message}"),
            Self::Verification { role, message } => {
                write!(f, "signature closure {role}: {message}")
            }
        }
    }
}

impl std::error::Error for Error {}

fn number(value: &str) -> Result<u32, Error> {
    let parsed = if let Some(hex) = value.strip_prefix("0x") {
        u32::from_str_radix(hex, 16)
    } else {
        value.parse()
    };
    parsed.map_err(|_| Error::Invalid(format!("invalid integer {value}")))
}

fn name(value: &str) -> Result<String, Error> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(Error::Invalid(format!("invalid name {value}")));
    }
    Ok(value.to_owned())
}

fn digest(value: &str) -> Result<[u8; 32], Error> {
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Invalid("SHA256 must contain 64 hex digits".into()));
    }
    let mut result = [0; 32];
    for (index, byte) in result.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).unwrap();
    }
    Ok(result)
}

impl Signature {
    pub fn parse(text: &str) -> Result<Self, Error> {
        if text.len() > MAX_TEXT {
            return Err(Error::Limit("text size"));
        }
        let mut fields = BTreeMap::new();
        let mut properties = BTreeMap::new();
        let mut functions = BTreeMap::new();
        let mut constants = Vec::new();
        let mut calls = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let tokens: Vec<_> = line.split_whitespace().collect();
            let invalid = || Error::Invalid(format!("bad directive at line {}", index + 1));
            match tokens[0] {
                key @ ("format" | "id" | "source" | "license" | "profile" | "candidate"
                | "entry") => {
                    let value = line[tokens[0].len()..].trim();
                    if value.is_empty() || fields.insert(key.to_owned(), value.to_owned()).is_some()
                    {
                        return Err(invalid());
                    }
                }
                "property" if tokens.len() == 3 => {
                    let key = name(tokens[1])?;
                    if properties.len() >= 32
                        || properties.insert(key, number(tokens[2])?).is_some()
                    {
                        return Err(invalid());
                    }
                }
                "function" if tokens.len() == 5 => {
                    let role = name(tokens[1])?;
                    let function = Function {
                        len: number(tokens[2])? as usize,
                        digest: digest(tokens[3])?,
                        prologue_len: number(tokens[4])? as usize,
                        constants: Vec::new(),
                        calls: Vec::new(),
                    };
                    if functions.len() >= MAX_FUNCTIONS
                        || functions.insert(role, function).is_some()
                    {
                        return Err(invalid());
                    }
                }
                "constant" if tokens.len() >= 4 => {
                    let role = name(tokens[1])?;
                    if tokens[2].len() != 8 {
                        return Err(invalid());
                    }
                    let bits = u32::from_str_radix(tokens[2], 16).map_err(|_| invalid())?;
                    for offset in &tokens[3..] {
                        constants.push((
                            role.clone(),
                            Constant {
                                offset: number(offset)? as usize,
                                bits,
                            },
                        ));
                        if constants.len() > MAX_OPERANDS {
                            return Err(Error::Limit("absolute operands"));
                        }
                    }
                }
                "call" if tokens.len() == 4 => {
                    calls.push((
                        name(tokens[1])?,
                        Call {
                            offset: number(tokens[2])? as usize,
                            target: name(tokens[3])?,
                        },
                    ));
                    if calls.len() > MAX_CALLS {
                        return Err(Error::Limit("calls"));
                    }
                }
                _ => return Err(invalid()),
            }
        }
        let required = |key: &str| {
            fields
                .get(key)
                .cloned()
                .ok_or_else(|| Error::Invalid(format!("missing {key}")))
        };
        if required("format")? != "1"
            || !matches!(
                required("candidate")?.as_str(),
                "cdecl-stack32" | "frame-ebp32"
            )
        {
            return Err(Error::Invalid("unsupported format/candidate".into()));
        }
        let entry = name(&required("entry")?)?;
        let entry_function = functions
            .get(&entry)
            .ok_or_else(|| Error::Invalid("entry role not declared".into()))?;
        if !(5..=32).contains(&entry_function.prologue_len) {
            return Err(Error::Invalid(
                "entry prologue must contain 5..32 bytes".into(),
            ));
        }
        for (role, constant) in constants {
            functions
                .get_mut(&role)
                .ok_or_else(|| Error::Invalid(format!("unknown constant role {role}")))?
                .constants
                .push(constant);
        }
        for (role, call) in calls {
            if !functions.contains_key(&call.target) {
                return Err(Error::Invalid(format!(
                    "unknown call target {}",
                    call.target
                )));
            }
            functions
                .get_mut(&role)
                .ok_or_else(|| Error::Invalid(format!("unknown call role {role}")))?
                .calls
                .push(call);
        }
        for (role, function) in &mut functions {
            if function.len == 0
                || function.len > MAX_FUNCTION_LEN
                || function.prologue_len > function.len
            {
                return Err(Error::Invalid(format!(
                    "invalid function size/prologue {role}"
                )));
            }
            function.constants.sort_by_key(|c| c.offset);
            function.calls.sort_by_key(|c| c.offset);
            let mut occupied = BTreeSet::new();
            for constant in &function.constants {
                if constant
                    .offset
                    .checked_add(4)
                    .is_none_or(|end| end > function.len)
                    || (constant.offset..constant.offset + 4).any(|offset| !occupied.insert(offset))
                {
                    return Err(Error::Invalid(format!(
                        "overlapping/out-of-range constant {role}"
                    )));
                }
            }
            for call in &function.calls {
                if call
                    .offset
                    .checked_add(5)
                    .is_none_or(|end| end > function.len)
                    || (call.offset..call.offset + 5).any(|offset| !occupied.insert(offset))
                {
                    return Err(Error::Invalid(format!(
                        "overlapping/out-of-range call {role}"
                    )));
                }
            }
        }
        let mut reachable = BTreeSet::new();
        let mut pending = vec![entry.clone()];
        while let Some(role) = pending.pop() {
            if reachable.insert(role.clone()) {
                pending.extend(
                    functions[&role]
                        .calls
                        .iter()
                        .map(|call| call.target.clone()),
                );
            }
        }
        if reachable.len() != functions.len() {
            return Err(Error::Invalid("unreachable function declaration".into()));
        }
        Ok(Self {
            id: name(&required("id")?)?,
            source: required("source")?,
            license: required("license")?,
            profile: Profile {
                kernel: name(&required("profile")?)?,
                properties,
            },
            entry,
            frame_ebp: required("candidate")? == "frame-ebp32",
            functions,
        })
    }
}

fn ranges(ranges: &[ExecutableRange]) -> Result<Vec<(u64, u64)>, Error> {
    if ranges.is_empty() || ranges.len() > MAX_RANGES {
        return Err(Error::Limit("executable ranges"));
    }
    let mut intervals = Vec::with_capacity(ranges.len());
    let mut total = 0;
    for range in ranges {
        let start = u64::from(range.start);
        let end = start + u64::from(range.len);
        if range.len == 0 || end > 1u64 << 32 {
            return Err(Error::Invalid("invalid executable range".into()));
        }
        total += u64::from(range.len);
        if total > MAX_SCAN_BYTES {
            return Err(Error::Limit("scan bytes"));
        }
        intervals.push((start, end));
    }
    intervals.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::new();
    for (start, end) in intervals {
        if let Some(last) = merged.last_mut() {
            if start < last.1 {
                return Err(Error::Ambiguous("overlapping executable ranges".into()));
            }
            if start == last.1 {
                last.1 = end;
                continue;
            }
        }
        merged.push((start, end));
    }
    Ok(merged)
}

fn covers(ranges: &[(u64, u64)], address: u32, len: usize) -> bool {
    let start = u64::from(address);
    let end = start + len as u64;
    ranges.iter().any(|&(a, b)| a <= start && end <= b)
}

fn read_exact(
    read: &mut impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
    address: u32,
    len: usize,
) -> Result<Vec<u8>, Error> {
    if u64::from(address) + len as u64 > 1u64 << 32 {
        return Err(Error::Read {
            address,
            len,
            message: "address wraps".into(),
        });
    }
    let bytes = read(address, len).map_err(|message| Error::Read {
        address,
        len,
        message,
    })?;
    if bytes.len() != len {
        return Err(Error::Read {
            address,
            len,
            message: "reader returned a partial span".into(),
        });
    }
    Ok(bytes)
}

fn normalized(function: &Function, bytes: &[u8]) -> [u8; 32] {
    let mut normalized = bytes.to_vec();
    for constant in &function.constants {
        normalized[constant.offset..constant.offset + 4].fill(0);
    }
    Sha256::digest(normalized).into()
}

fn safe_prologue(bytes: &[u8], len: usize) -> bool {
    let Some(bytes) = bytes.get(..len) else {
        return false;
    };
    let mut decoder = Decoder::new(32, bytes, DecoderOptions::NONE);
    while decoder.can_decode() {
        let instruction = decoder.decode();
        if instruction.is_invalid()
            || instruction.flow_control() != FlowControl::Next
            || instruction.is_ip_rel_memory_operand()
        {
            return false;
        }
    }
    decoder.position() == len
}

/// Public x86/32-bit frame idioms, not stored library instruction prefixes.
/// The descriptor selects a stack-argument or saved-frame-pointer prologue.
/// Immediate/register/displacement values do not select a library version.
fn candidate(bytes: &[u8], prologue_len: usize, frame_ebp: bool) -> bool {
    if frame_ebp {
        // Public frame-pointer prologue: PUSH EBP; MOV EBP,ESP; SUB ESP,imm.
        if bytes.len() < prologue_len || !bytes.starts_with(&[0x55, 0x8b, 0xec]) {
            return false;
        }
        let mut decoder = Decoder::new(32, &bytes[3..prologue_len], DecoderOptions::NONE);
        let sub = decoder.decode();
        return sub.mnemonic() == Mnemonic::Sub
            && sub.op0_register() == Register::ESP
            && matches!(sub.op1_kind(), OpKind::Immediate32 | OpKind::Immediate8to32)
            && safe_prologue(bytes, prologue_len);
    }
    // 81/83 /5 register-direct ESP is the ISA encoding of SUB ESP, immediate.
    if bytes.len() < prologue_len
        || !matches!(bytes.first(), Some(0x81 | 0x83))
        || bytes.get(1) != Some(&0xec)
    {
        return false;
    }
    let mut decoder = Decoder::new(32, &bytes[..prologue_len], DecoderOptions::NONE);
    let sub = decoder.decode();
    let push = decoder.decode();
    let mov = decoder.decode();
    sub.mnemonic() == Mnemonic::Sub
        && sub.op0_register() == Register::ESP
        && matches!(sub.op1_kind(), OpKind::Immediate32 | OpKind::Immediate8to32)
        && push.mnemonic() == Mnemonic::Push
        && push.op0_kind() == OpKind::Register
        && matches!(
            push.op0_register(),
            Register::EBX | Register::EBP | Register::ESI | Register::EDI
        )
        && mov.mnemonic() == Mnemonic::Mov
        && mov.op0_register() == push.op0_register()
        && mov.op1_kind() == OpKind::Memory
        && mov.memory_base() == Register::ESP
        && mov.memory_index() == Register::None
        && safe_prologue(bytes, prologue_len)
}

fn operand_boundaries(function: &Function, address: u32, bytes: &[u8]) -> bool {
    let mut constants = BTreeSet::new();
    let mut calls = BTreeSet::new();
    let mut starts = BTreeSet::new();
    let mut targets = Vec::new();
    let mut decoder = Decoder::with_ip(32, bytes, u64::from(address), DecoderOptions::NONE);
    while decoder.can_decode() {
        let start = decoder.position();
        let instruction = decoder.decode();
        if instruction.is_invalid() {
            return false;
        }
        starts.insert(start);
        match instruction.flow_control() {
            FlowControl::Next => {}
            FlowControl::Return if instruction.mnemonic() == Mnemonic::Ret => {}
            FlowControl::Call => {
                if bytes[start] != 0xe8
                    || instruction.op0_kind() != OpKind::NearBranch32
                    || instruction.len() != 5
                {
                    return false;
                }
                calls.insert(start);
            }
            FlowControl::ConditionalBranch | FlowControl::UnconditionalBranch => {
                if instruction.op0_kind() != OpKind::NearBranch32 {
                    return false;
                }
                let target = u64::from(instruction.near_branch32());
                if target < u64::from(address) || target >= u64::from(address) + function.len as u64
                {
                    return false;
                }
                let target = (target - u64::from(address)) as usize;
                if target < function.prologue_len {
                    return false;
                }
                targets.push(target);
            }
            // No indirect/far calls or branches, interrupts, system transfers,
            // exceptions, transaction-control flow or far/interrupt returns.
            _ => return false,
        }
        let offsets = decoder.get_constant_offsets(&instruction);
        if instruction.memory_base() == Register::None
            && instruction.memory_index() == Register::None
            && instruction.segment_prefix() == Register::None
            && instruction.memory_size().size() == 4
            && offsets.displacement_size() == 4
            && (0..instruction.op_count()).any(|n| instruction.op_kind(n) == OpKind::Memory)
        {
            constants.insert(start + offsets.displacement_offset());
        }
    }
    targets.iter().all(|target| starts.contains(target))
        && function.constants.len() == constants.len()
        && function.calls.len() == calls.len()
        && function
            .constants
            .iter()
            .all(|c| constants.contains(&c.offset))
        && function.calls.iter().all(|c| calls.contains(&c.offset))
}

fn verify(
    signature: &Signature,
    entry: u32,
    ranges: &[(u64, u64)],
    read: &mut impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<Match, Error> {
    let mut pending = vec![(signature.entry.clone(), entry)];
    let mut resolved = BTreeMap::from([(signature.entry.clone(), entry)]);
    let mut visited = BTreeSet::new();
    let mut functions = Vec::new();
    let mut constants = BTreeMap::new();
    while let Some((role, address)) = pending.pop() {
        if !visited.insert(role.clone()) {
            continue;
        }
        let function = &signature.functions[&role];
        let fail = |message: &str| Error::Verification {
            role: role.clone(),
            message: message.into(),
        };
        if !covers(ranges, address, function.len) {
            return Err(fail("function is outside executable ranges"));
        }
        let bytes = read_exact(read, address, function.len)?;
        if normalized(function, &bytes) != function.digest {
            return Err(fail("full function SHA256 differs"));
        }
        if function.prologue_len != 0 && !safe_prologue(&bytes, function.prologue_len) {
            return Err(fail("unsafe/incomplete prologue"));
        }
        if !operand_boundaries(function, address, &bytes) {
            return Err(fail("declared operand/call is not an instruction field"));
        }
        for constant in &function.constants {
            let at = u32::from_le_bytes(
                bytes[constant.offset..constant.offset + 4]
                    .try_into()
                    .unwrap(),
            );
            let value = read_exact(read, at, 4)?;
            if value != constant.bits.to_le_bytes() {
                return Err(fail("absolute constant bits differ"));
            }
            if constants
                .insert(at, value.clone())
                .is_some_and(|old| old != value)
            {
                return Err(Error::Ambiguous("conflicting constant snapshots".into()));
            }
        }
        for call in &function.calls {
            let displacement =
                i32::from_le_bytes(bytes[call.offset + 1..call.offset + 5].try_into().unwrap());
            let target = i64::from(address) + call.offset as i64 + 5 + i64::from(displacement);
            let target = u32::try_from(target).map_err(|_| fail("relative call target wraps"))?;
            if let Some(previous) = resolved.insert(call.target.clone(), target)
                && previous != target
            {
                return Err(Error::Ambiguous(format!(
                    "role {} has multiple call targets",
                    call.target
                )));
            }
            pending.push((call.target.clone(), target));
        }
        functions.push(FunctionSnapshot {
            role,
            entry: address,
            bytes,
        });
    }
    if visited.len() != signature.functions.len() {
        return Err(Error::Invalid("incomplete verified closure".into()));
    }
    Ok(Match {
        entry,
        prologue_len: signature.functions[&signature.entry].prologue_len,
        profile: signature.profile.clone(),
        functions,
        constants: constants
            .into_iter()
            .map(|(address, bytes)| ConstantSnapshot { address, bytes })
            .collect(),
    })
}

/// Scan caller-verified executable mappings, with at most 64 MiB, 4096 frame
/// candidates and 16 independent complete library instances. No match, reads,
/// changed constants or an inconsistent closure are errors; a different main
/// digest is just a non-match. The caller must use a stable guest memory view.
pub fn scan(
    signature: &Signature,
    executable: &[ExecutableRange],
    read: &mut impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<Vec<Match>, Error> {
    let ranges = ranges(executable)?;
    let function = &signature.functions[&signature.entry];
    let mut matches = Vec::new();
    let mut candidates = 0;
    for &(start, end) in &ranges {
        let bytes = read_exact(read, start as u32, (end - start) as usize)?;
        if bytes.len() < function.len {
            continue;
        }
        for offset in 0..=bytes.len() - function.len {
            if !candidate(&bytes[offset..], function.prologue_len, signature.frame_ebp) {
                continue;
            }
            candidates += 1;
            if candidates > MAX_CANDIDATES {
                return Err(Error::Limit("frame candidates"));
            }
            let body = &bytes[offset..offset + function.len];
            if normalized(function, body) != function.digest {
                continue;
            }
            let matched = verify(signature, (start + offset as u64) as u32, &ranges, read)?;
            if matches.len() >= MAX_MATCHES {
                return Err(Error::Limit("library matches"));
            }
            if matches.iter().any(|m: &Match| {
                m.functions.iter().any(|f| {
                    matched
                        .functions
                        .iter()
                        .any(|g| f.entry == g.entry && (f.role != g.role || f.bytes != g.bytes))
                })
            }) {
                return Err(Error::Ambiguous(
                    "conflicting function roles/closures".into(),
                ));
            }
            matches.push(matched);
        }
    }
    if matches.is_empty() {
        return Err(Error::NotFound);
    }
    Ok(matches)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u32 = 0x1000;
    const CONSTANT: u32 = 0x4000;

    // Synthetic ISA/ABI fixture, authored here: stack allocation 16, saved ESI,
    // argument displacement 36, FLD absolute f32, one CALL and an epilogue.
    // It is not a copy of a library/game function or its fingerprint.
    fn main_code(constant: u32, target: u32) -> Vec<u8> {
        let mut bytes = vec![0x83, 0xec, 0x10, 0x56, 0x8b, 0x74, 0x24, 0x24, 0xd9, 0x05];
        bytes.extend(constant.to_le_bytes());
        bytes.push(0xe8);
        bytes.extend((target as i32 - (BASE + 19) as i32).to_le_bytes());
        bytes.extend([0x5e, 0x83, 0xc4, 0x10, 0xc3]);
        bytes
    }

    fn sha(bytes: &[u8], absolute: Option<usize>) -> String {
        let mut bytes = bytes.to_vec();
        if let Some(offset) = absolute {
            bytes[offset..offset + 4].fill(0);
        }
        format!("{:x}", Sha256::digest(bytes))
    }

    fn text(main: &[u8], absolute: usize, calls: &str) -> String {
        format!(
            "format 1\nid synthetic-cdecl-test\nsource synthetic ISA fixture\nlicense CC0 fixture\nprofile synthetic-kernel\ncandidate cdecl-stack32\nentry main\nproperty fixture 1\nfunction main {} {} 8\nfunction leaf 3 {} 0\nconstant main 3f000000 {}\n{}",
            main.len(),
            sha(main, Some(absolute)),
            sha(&[0xd9, 0xe8, 0xc3], None),
            absolute,
            calls
        )
    }

    struct Fixture {
        main: Vec<u8>,
        memory: Vec<u8>,
        text: String,
    }

    impl Fixture {
        fn new() -> Self {
            let main = main_code(CONSTANT, BASE + 0x80);
            let text = text(&main, 10, "call main 14 leaf\n");
            let mut memory = vec![0; 0x4000];
            memory[..main.len()].copy_from_slice(&main);
            memory[0x80..0x83].copy_from_slice(&[0xd9, 0xe8, 0xc3]);
            memory[0x3000..0x3004].copy_from_slice(&0.5f32.to_le_bytes());
            Self { main, memory, text }
        }

        fn read(&self, address: u32, len: usize) -> Result<Vec<u8>, String> {
            let start = address.checked_sub(BASE).ok_or("unmapped")? as usize;
            self.memory
                .get(start..start.saturating_add(len))
                .map(<[u8]>::to_vec)
                .ok_or_else(|| "unmapped".into())
        }

        fn scan(&self) -> Result<Vec<Match>, Error> {
            let signature = Signature::parse(&self.text)?;
            scan(
                &signature,
                &[ExecutableRange {
                    start: BASE,
                    len: 0x2000,
                }],
                &mut |at, len| self.read(at, len),
            )
        }
    }

    #[test]
    fn exact_closure_keeps_raw_snapshots_and_accepts_constant_relocation() {
        let mut fixture = Fixture::new();
        let matches = fixture.scan().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].entry, BASE);
        assert_eq!(matches[0].prologue_len, 8);
        assert_eq!(matches[0].functions[0].bytes, fixture.main);
        assert_eq!(matches[0].functions[1].entry, BASE + 0x80);
        assert_eq!(matches[0].profile.properties["fixture"], 1);
        assert_eq!(matches[0].constants[0].address, CONSTANT);
        fixture.memory[10..14].copy_from_slice(&(CONSTANT + 4).to_le_bytes());
        fixture.memory[0x3004..0x3008].copy_from_slice(&0.5f32.to_le_bytes());
        let matches = fixture.scan().unwrap();
        assert_eq!(matches[0].constants[0].address, CONSTANT + 4);
        assert_eq!(
            matches[0].functions[0].bytes[10..14],
            (CONSTANT + 4).to_le_bytes()
        );
    }

    // spec: DEV-AUDIO-4
    #[test]
    fn frame_pointer_candidate_still_requires_the_complete_closure() {
        let mut fixture = Fixture::new();
        fixture.main[..8].copy_from_slice(&[0x55, 0x8b, 0xec, 0x83, 0xec, 0x10, 0x90, 0x90]);
        fixture.memory[..fixture.main.len()].copy_from_slice(&fixture.main);
        fixture.text = text(&fixture.main, 10, "call main 14 leaf\n")
            .replace("candidate cdecl-stack32", "candidate frame-ebp32");
        assert_eq!(fixture.scan().unwrap().len(), 1);
        fixture.memory[0x81] = 0xee;
        assert!(matches!(fixture.scan(), Err(Error::Verification { .. })));
        fixture.memory[0x81] = 0xe8;
        fixture.memory[5] ^= 1;
        assert_eq!(fixture.scan(), Err(Error::NotFound));
    }

    #[test]
    fn non_operand_bytes_and_relative_calls_are_never_wildcards() {
        for offset in [2, 15] {
            let mut fixture = Fixture::new();
            fixture.memory[offset] ^= 1;
            assert_eq!(fixture.scan(), Err(Error::NotFound));
        }
    }

    #[test]
    fn changed_constant_missing_memory_and_callee_code_fail_closed() {
        let mut fixture = Fixture::new();
        fixture.memory[0x3000] ^= 1;
        assert!(matches!(fixture.scan(), Err(Error::Verification { .. })));
        let mut fixture = Fixture::new();
        fixture.memory[0x81] = 0xee; // FLDZ replaces the leaf's FLD1.
        assert!(matches!(fixture.scan(), Err(Error::Verification { .. })));
        let mut fixture = Fixture::new();
        fixture.memory[10..14].copy_from_slice(&0x9000u32.to_le_bytes());
        assert!(matches!(fixture.scan(), Err(Error::Read { .. })));
    }

    #[test]
    fn separately_rebased_complete_instances_are_independent_matches() {
        let mut fixture = Fixture::new();
        let mut rebased = fixture.main.clone();
        rebased[10..14].copy_from_slice(&(CONSTANT + 4).to_le_bytes());
        fixture.memory[0x1000..0x1000 + rebased.len()].copy_from_slice(&rebased);
        fixture.memory[0x1080..0x1083].copy_from_slice(&[0xd9, 0xe8, 0xc3]);
        fixture.memory[0x3004..0x3008].copy_from_slice(&0.5f32.to_le_bytes());
        let matches = fixture.scan().unwrap();
        assert_eq!(
            matches.iter().map(|m| m.entry).collect::<Vec<_>>(),
            [BASE, BASE + 0x1000]
        );
    }

    #[test]
    fn relative_target_ranges_and_conflicting_closure_roles_are_checked() {
        let mut fixture = Fixture::new();
        fixture.main = main_code(CONSTANT, 0x3800);
        fixture.memory[..fixture.main.len()].copy_from_slice(&fixture.main);
        fixture.text = text(&fixture.main, 10, "call main 14 leaf\n");
        assert!(matches!(fixture.scan(), Err(Error::Verification { .. })));
        let mut fixture = Fixture::new();
        let second_call = [0xe8, 0x90 - 24, 0, 0, 0];
        fixture.main.splice(19..19, second_call);
        fixture.memory[..fixture.main.len()].copy_from_slice(&fixture.main);
        fixture.memory[0x90..0x93].copy_from_slice(&[0xd9, 0xe8, 0xc3]);
        fixture.text = text(&fixture.main, 10, "call main 14 leaf\ncall main 19 leaf\n");
        assert!(matches!(fixture.scan(), Err(Error::Ambiguous(_))));
    }

    #[test]
    fn declarations_must_cover_real_absolute_operands_and_all_calls() {
        let mut fixture = Fixture::new();
        fixture.main[8..14].copy_from_slice(&[0xb8, 0, 0x40, 0, 0, 0x90]); // MOV EAX,imm32; NOP.
        fixture.memory[..fixture.main.len()].copy_from_slice(&fixture.main);
        fixture.text = text(&fixture.main, 9, "call main 14 leaf\n");
        assert!(matches!(fixture.scan(), Err(Error::Verification { .. })));
        let fixture = Fixture::new();
        let signature = fixture
            .text
            .lines()
            .filter(|line| !line.starts_with("function leaf") && !line.starts_with("call "))
            .collect::<Vec<_>>()
            .join("\n");
        let signature = Signature::parse(&signature).unwrap();
        assert!(matches!(
            scan(
                &signature,
                &[ExecutableRange {
                    start: BASE,
                    len: 0x2000
                }],
                &mut |at, len| fixture.read(at, len)
            ),
            Err(Error::Verification { .. })
        ));
    }

    #[test]
    fn exact_digest_does_not_bypass_detour_control_flow_safety() {
        for tail in [
            [0xff, 0xe0, 0x90, 0x90, 0xc3], // JMP EAX is outside a closed leaf.
            [0xeb, 0xff, 0x90, 0x90, 0xc3], // Branch enters an instruction byte.
            [0xeb, 0xee, 0x90, 0x90, 0xc3], // Branch enters displaced PUSH ESI.
            [0xeb, 0xeb, 0x90, 0x90, 0xc3], // Branch reenters installed entry.
            [0xcd, 0x03, 0x90, 0x90, 0xc3], // Interrupt flow is not a leaf.
            [0x90, 0x90, 0x90, 0x90, 0x0f], // Invalid truncated instruction.
        ] {
            let mut fixture = Fixture::new();
            fixture.main[19..24].copy_from_slice(&tail);
            fixture.memory[..fixture.main.len()].copy_from_slice(&fixture.main);
            fixture.text = text(&fixture.main, 10, "call main 14 leaf\n");
            assert!(matches!(fixture.scan(), Err(Error::Verification { .. })));
        }
        let mut fixture = Fixture::new();
        fixture.main[14..19].copy_from_slice(&[0x66, 0xe8, 0, 0, 0x90]);
        fixture.memory[..fixture.main.len()].copy_from_slice(&fixture.main);
        fixture.text = text(&fixture.main, 10, "call main 14 leaf\n");
        assert!(matches!(fixture.scan(), Err(Error::Verification { .. })));
    }

    #[test]
    fn invalid_metadata_overlap_and_range_wrap_are_rejected() {
        let fixture = Fixture::new();
        for suffix in [
            "\nprofile duplicate\n",
            "\nconstant main 3f000000 10\n",
            "\nunknown field\n",
        ] {
            assert!(matches!(
                Signature::parse(&(fixture.text.clone() + suffix)),
                Err(Error::Invalid(_))
            ));
        }
        let signature = Signature::parse(&fixture.text).unwrap();
        assert!(matches!(
            scan(
                &signature,
                &[ExecutableRange {
                    start: u32::MAX,
                    len: 2
                }],
                &mut |_, _| panic!("invalid ranges must not read")
            ),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            scan(
                &signature,
                &[
                    ExecutableRange {
                        start: BASE,
                        len: 256
                    },
                    ExecutableRange {
                        start: BASE + 128,
                        len: 256
                    }
                ],
                &mut |_, _| panic!("overlapping ranges must not read")
            ),
            Err(Error::Ambiguous(_))
        ));
        assert!(!safe_prologue(&[0xe8, 0, 0, 0, 0], 5));
        assert!(!safe_prologue(&[0x83, 0xec], 2));
    }

    #[test]
    fn shipped_fingerprint_has_only_the_reviewed_complete_closure() {
        // Runtime loading remains explicit; this include is a parser unit test.
        let signature =
            Signature::parse(include_str!("../../../data/codec/vorbis-mdct-x86.txt")).unwrap();
        assert_eq!(signature.functions.len(), 4);
        assert_eq!(
            signature
                .functions
                .values()
                .map(|f| f.constants.len())
                .sum::<usize>(),
            32
        );
        assert_eq!(
            signature
                .functions
                .values()
                .map(|f| f.calls.len())
                .sum::<usize>(),
            3
        );
        assert_eq!(signature.profile.kernel, "vorbis-mdct-backward-f32-cdecl32");
    }
}

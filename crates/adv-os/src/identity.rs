//! Identity of the single ordinary account in each isolated guest OS profile.
//! Renaming its display name does not change the account SID.
const USER_SUBAUTHORITIES: [u32; 5] = [21, 0, 0, 0, 1000];

pub fn current_user_sid() -> [u8; 28] {
    let mut sid = [0; 28];
    sid[0] = 1;
    sid[1] = USER_SUBAUTHORITIES.len() as u8;
    sid[7] = 5; // SECURITY_NT_AUTHORITY, big-endian six-byte identifier.
    for (index, value) in USER_SUBAUTHORITIES.iter().enumerate() {
        sid[8 + index * 4..12 + index * 4].copy_from_slice(&value.to_le_bytes());
    }
    sid
}
pub fn current_user_sid_string() -> String {
    sid_string(&current_user_sid()).expect("well-formed built-in account SID")
}

/// Standard SID text. Identifier authority is big endian; RIDs are little endian.
pub fn sid_string(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 8 || bytes[0] != 1 || bytes[1] > 15 || bytes.len() != 8 + 4 * bytes[1] as usize
    {
        return None;
    }
    let authority = bytes[2..8]
        .iter()
        .fold(0u64, |v, b| (v << 8) | u64::from(*b));
    let mut result = if authority <= u64::from(u32::MAX) {
        format!("S-1-{authority}")
    } else {
        format!("S-1-0x{authority:X}")
    };
    for rid in bytes[8..].chunks_exact(4) {
        result.push_str(&format!("-{}", u32::from_le_bytes(rid.try_into().ok()?)));
    }
    Some(result)
}

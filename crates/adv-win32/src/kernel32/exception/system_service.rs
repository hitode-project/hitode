//! Owned numeric direct-service directory for the virtual native x86 profile.
//! Export-name host thunks use a separately validated transport and do not
//! register numeric services. No host or Windows-version service IDs are copied.
//!
//! The two-table, 12-bit index decoding follows the ReactOS NDK/i386 dispatcher
//! reference. This is a virtual profile contract, not a claim that every real
//! Windows version or win32k failure convention is reproduced. The available
//! WOW64 host oracle cannot validate a native x86 kernel-service transition.

const INVALID_SYSTEM_SERVICE: u32 = 0xc000_001c;
const NOT_IMPLEMENTED: u32 = 0xc000_0002;

#[derive(Clone, Copy)]
struct Entry {
    index: u16,
}

// Both NT and GUI numeric service directories are explicitly empty. A named
// Nt* export being implemented does not assign it a version-dependent number.
const DIRECTORY: [&[Entry]; 2] = [&[], &[]];

#[derive(Debug, PartialEq, Eq)]
struct Number {
    table: usize,
    index: u16,
}
impl Number {
    fn decode(raw: u32) -> Self {
        Self {
            table: ((raw >> 12) & 1) as usize,
            index: (raw & 0xfff) as u16,
        }
    }
}

fn failure_in(raw: u32, directory: [&[Entry]; 2]) -> u32 {
    let number = Number::decode(raw);
    if directory[number.table]
        .iter()
        .any(|entry| entry.index == number.index)
    {
        // Registered but unavailable is distinct from an absent service. No
        // argument inspection or implementation dispatch is provided here.
        NOT_IMPLEMENTED
    } else {
        INVALID_SYSTEM_SERVICE
    }
}

pub(super) fn failure(raw: u32) -> u32 {
    failure_in(raw, DIRECTORY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_index_and_high_aliases_resolve_against_the_owned_directory() {
        for table in 0..2 {
            for index in [0, 7, 0x7ff, 0xfff] {
                let base = (table << 12) | index;
                for high in [0, 0x2000, 0x8000_0000, 0xffff_e000] {
                    let raw = base | high;
                    assert_eq!(
                        Number::decode(raw),
                        Number {
                            table: table as usize,
                            index: index as u16
                        }
                    );
                    assert_eq!(failure(raw), INVALID_SYSTEM_SERVICE);
                    let directory = [&[Entry { index: 7 }][..], &[][..]];
                    assert_eq!(
                        failure_in(raw, directory),
                        if table == 0 && index == 7 {
                            NOT_IMPLEMENTED
                        } else {
                            INVALID_SYSTEM_SERVICE
                        }
                    );
                }
            }
        }
    }
}

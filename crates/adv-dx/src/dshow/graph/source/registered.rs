//! Source filter choice from `HKCR\Media Type`, as quartz makes it: a file
//! extension entry decides the media type alone; otherwise the first entry
//! with a matching byte pattern decides the type and the source filter.

/// One tuple of a pattern: the bytes at `offset`, masked, equal `value`.
#[derive(Debug, PartialEq)]
pub(super) struct Tuple {
    pub offset: u64,
    pub mask: Vec<u8>,
    pub value: Vec<u8>,
}

/// Parse "offset, size, mask, value[, offset, size, mask, value…]". An empty
/// mask selects every bit; a malformed tuple never matches.
pub(super) fn parse(pattern: &str) -> Option<Vec<Tuple>> {
    let mut fields = pattern.split(',');
    let mut tuples = Vec::new();
    loop {
        let Some(offset) = fields.next() else {
            return Some(tuples);
        };
        let offset = leading_number(offset);
        let size = leading_number(fields.next()?) as usize;
        let mask = hex(fields.next()?, size, 0xff);
        let value = hex(fields.next()?, size, 0);
        tuples.push(Tuple {
            offset,
            mask,
            value,
        });
    }
}

/// wcstol: leading decimal digits after white space, zero without any.
fn leading_number(field: &str) -> u64 {
    let digits: String = field
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().unwrap_or(0)
}

/// Up to `size` bytes from the leading hex digits after any other characters.
/// Bytes past the digits keep `fill`; a lone last digit is a high nibble.
fn hex(field: &str, size: usize, fill: u8) -> Vec<u8> {
    let digits: Vec<u8> = field
        .chars()
        .skip_while(|c| !c.is_ascii_hexdigit())
        .map_while(|c| c.to_digit(16).map(|d| d as u8))
        .take(size * 2)
        .collect();
    let mut bytes = vec![fill; size];
    for (i, pair) in digits.chunks(2).enumerate() {
        bytes[i] = match pair {
            [high, low] => high << 4 | low,
            [high] => high << 4,
            _ => unreachable!(),
        };
    }
    bytes
}

/// Whether every tuple matches the file, given each tuple's bytes, if read.
pub(super) fn matches(
    tuples: &[Tuple],
    read: &mut dyn FnMut(u64, usize) -> Option<Vec<u8>>,
) -> bool {
    !tuples.is_empty()
        && tuples.iter().all(|tuple| {
            read(tuple.offset, tuple.mask.len()).is_some_and(|actual| {
                actual.len() == tuple.mask.len()
                    && actual
                        .iter()
                        .zip(&tuple.mask)
                        .zip(&tuple.value)
                        .all(|((a, m), v)| a & m == *v)
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(bytes: &[u8]) -> impl FnMut(u64, usize) -> Option<Vec<u8>> + '_ {
        |offset, length| {
            bytes
                .get(offset as usize..offset as usize + length)
                .map(<[u8]>::to_vec)
        }
    }

    // spec: MEDIA-GRAPH-14
    #[test]
    fn patterns_parse_and_match_as_quartz_reads_them() {
        let midi = b"MThd\0\0\0\x06";
        // An empty mask selects every bit.
        let tuples = parse("0,4,,4d546864").unwrap();
        assert_eq!(tuples[0].mask, [0xff; 4]);
        assert!(matches(&tuples, &mut file(midi)));
        // Every tuple must match; a mask clears bits before comparing.
        let both = parse("0, 4, , 4d546864, 4,4,ffff0000,00000000").unwrap();
        assert!(matches(&both, &mut file(midi)));
        let masked = parse("0,2,F0F0,4050").unwrap();
        assert!(matches(&masked, &mut file(midi)));
        assert!(!matches(&parse("0,4,,deadbeef").unwrap(), &mut file(midi)));
        // Reads past the end never match; a lone digit is a high nibble.
        assert!(!matches(&parse("6,4,,00000000").unwrap(), &mut file(midi)));
        assert_eq!(parse("0,2,f,4").unwrap()[0].mask, [0xf0, 0xff]);
        // A tuple without a value field is malformed.
        assert_eq!(parse("0,4,ffffffff"), None);
        assert_eq!(parse(""), None);
    }
}

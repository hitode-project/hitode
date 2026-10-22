//! Wine's debug channels: the table ntdll.dll reads at `PEB + PAGE`, built
//! from a WINEDEBUG-style specification as Wine's `dbg_init` builds it.
//!
//! The table holds the named channels sorted by name, each a flags byte and
//! a NUL-terminated name in 16 bytes, then an entry with no name whose flags
//! apply to every other channel.

const FIXME: u8 = 1 << 0;
const ERR: u8 = 1 << 1;
const CLASSES: [&str; 4] = ["fixme", "err", "warn", "trace"];
const ENTRY_SIZE: usize = 16;
const NAME_SIZE: usize = 15;

/// The channel table for `spec`, as the process named `app_name` sees it,
/// within `limit` bytes.
pub fn table(spec: &str, app_name: &str, limit: usize) -> Vec<u8> {
    let mut default = FIXME | ERR;
    let mut channels: Vec<(String, u8)> = Vec::new();
    for option in spec.split(',') {
        let option = match option.split_once(':') {
            Some((process, option)) if process.eq_ignore_ascii_case(app_name) => option,
            Some(_) => continue,
            None => option,
        };
        let split = option.find(['+', '-']).unwrap_or(0);
        let (class, rest) = option.split_at(split);
        let (mut set, mut clear) = (0u8, 0u8);
        if class.is_empty() {
            if rest.starts_with('-') {
                clear = !0;
            } else {
                set = !0;
            }
        } else {
            let Some(index) = CLASSES.iter().position(|&known| known == class) else {
                continue;
            };
            if rest.starts_with('+') {
                set = 1 << index;
            } else {
                clear = 1 << index;
            }
        }
        let name = rest.strip_prefix(['+', '-']).unwrap_or(rest);
        if name.is_empty() {
            continue;
        }
        if name == "all" {
            default = (default & !clear) | set;
            continue;
        }
        if name.len() >= NAME_SIZE {
            continue;
        }
        match channels.binary_search_by(|(known, _)| known.as_str().cmp(name)) {
            Ok(index) => {
                let flags = &mut channels[index].1;
                *flags = (*flags & !clear) | set;
            }
            Err(index) => channels.insert(index, (name.to_owned(), (default & !clear) | set)),
        }
    }

    channels.truncate(limit / ENTRY_SIZE - 1);
    let mut table = Vec::with_capacity((channels.len() + 1) * ENTRY_SIZE);
    for (name, flags) in channels
        .iter()
        .map(|(name, flags)| (name.as_str(), *flags))
        .chain([("", default)])
    {
        let mut entry = [0u8; ENTRY_SIZE];
        entry[0] = flags;
        entry[1..1 + name.len()].copy_from_slice(name.as_bytes());
        table.extend_from_slice(&entry);
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(table: &[u8]) -> Vec<(String, u8)> {
        table
            .chunks(ENTRY_SIZE)
            .map(|entry| {
                let name = entry[1..].split(|&b| b == 0).next().unwrap();
                (String::from_utf8(name.to_vec()).unwrap(), entry[0])
            })
            .collect()
    }

    #[test]
    fn options_set_channels_classes_and_the_default() {
        assert_eq!(
            entries(&table("", "app.exe", 4096)),
            [("".into(), FIXME | ERR)]
        );
        assert_eq!(
            entries(&table(
                "+loaddll,warn-heap,other.exe:+relay,-all",
                "app.exe",
                4096
            )),
            [
                ("heap".into(), FIXME | ERR),
                ("loaddll".into(), !0),
                ("".into(), 0),
            ]
        );
        assert_eq!(
            entries(&table("APP.EXE:trace+seh,bogus+x", "app.exe", 4096)),
            [
                ("seh".into(), FIXME | ERR | 1 << 3),
                ("".into(), FIXME | ERR)
            ]
        );
    }
}

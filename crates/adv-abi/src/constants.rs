//! Named enum/flag values projected from the same pinned metadata as signatures.
use std::sync::OnceLock;

#[derive(Debug)]
pub struct Constant {
    pub type_name: &'static str,
    pub name: &'static str,
    pub value: u64,
    pub flags: bool,
}

pub fn constants() -> &'static [Constant] {
    static TABLE: OnceLock<Vec<Constant>> = OnceLock::new();
    TABLE.get_or_init(|| {
        include_str!("../../../data/abi/constants.tsv")
            .lines()
            .filter(|line| !line.starts_with('#') && !line.is_empty())
            .map(|line| {
                let fields: Vec<_> = line.split('\t').collect();
                assert_eq!(fields.len(), 4);
                Constant {
                    type_name: fields[0],
                    name: fields[1],
                    value: u64::from_str_radix(fields[2], 16).expect("generated enum bits"),
                    flags: fields[3] == "flags",
                }
            })
            .collect()
    })
}

pub fn for_type(type_name: &str) -> &'static [Constant] {
    let table = constants();
    let start = table.partition_point(|c| c.type_name < type_name);
    let end = table.partition_point(|c| c.type_name <= type_name);
    &table[start..end]
}

pub fn named(type_name: &str, name: &str) -> Option<u64> {
    for_type(type_name)
        .iter()
        .find(|c| c.name == name)
        .map(|c| c.value)
}

pub fn format(type_name: &str, value: u64) -> Option<String> {
    let values = for_type(type_name);
    if let Some(c) = values.iter().find(|c| c.value == value) {
        return Some(c.name.to_owned());
    }
    if !values.first().is_some_and(|c| c.flags) {
        return None;
    }
    let mut remaining = value;
    let mut names = Vec::new();
    for constant in values {
        if constant.value.is_power_of_two() && remaining & constant.value != 0 {
            names.push(constant.name.to_owned());
            remaining &= !constant.value;
        }
    }
    if remaining != 0 {
        names.push(format!("{remaining:#x}"));
    }
    if names.is_empty() {
        None
    } else {
        Some(names.join("|"))
    }
}

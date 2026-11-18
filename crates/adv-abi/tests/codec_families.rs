use adv_abi::{code_signature::Signature, library_signature::Family};
use std::{collections::BTreeMap, fs, path::Path};

#[test]
fn committed_codec_families_parse_and_are_unique() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/codec");
    let mut families = BTreeMap::new();
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("txt") {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap();
        let mut records: Vec<_> = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect();
        if records.iter().any(|line| line.starts_with("format ")) {
            Signature::parse(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            records.retain(|line| {
                !matches!(
                    line.split_whitespace().next(),
                    Some("id" | "source" | "license")
                )
            });
        } else {
            Family::parse(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        }
        records.sort_unstable();
        if let Some(previous) = families.insert(records, path.clone()) {
            panic!(
                "duplicate codec families: {} and {}",
                previous.display(),
                path.display()
            );
        }
    }
    assert!(!families.is_empty(), "no committed codec families");
}

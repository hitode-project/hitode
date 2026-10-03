//! Cross-check against pefile on the local sample corpus (data/pe-fixtures.json, generated
//! by tools/analysis/pefixture.py). Samples are looked up with `adv-samples` (`BUILD/$exe` is
//! that build's game exe); missing ones are skipped, so this passes on machines without the games.

use adv_pe::{ExportTarget, Pe, ResId};
use serde_json::Value;

fn fnv(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// Compare a canonical string's digest; on mismatch, show the string for diffing with pefile.
fn assert_fnv(rel: &str, what: &str, canon: &str, want: &str) {
    if fnv(canon) != want {
        panic!(
            "{rel}: {what} differ from pefile; ours:
{canon}"
        );
    }
}

fn resid(r: &ResId) -> String {
    match r {
        ResId::Id(i) => i.to_string(),
        ResId::Name(n) => n.clone(),
    }
}

fn check(rel: &str, f: &Value, pe: &Pe) {
    let n = |k: &str| f[k].as_u64().unwrap() as u32;
    let s = |k: &str| f[k].as_str().unwrap().to_owned();
    assert_eq!(pe.image_base, n("image_base"), "{rel}");
    assert_eq!(pe.entry_rva, n("entry_rva"), "{rel}");
    assert_eq!(pe.size_of_image, n("size_of_image"), "{rel}");
    assert_eq!(pe.timestamp, n("timestamp"), "{rel}");
    let names: Vec<&str> = pe.sections.iter().map(|s| s.name.as_str()).collect();
    let want: Vec<&str> = f["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(names, want, "{rel}");

    let relocs = pe.relocations().unwrap();
    assert_eq!(relocs.len() as u32, n("relocs"), "{rel} relocs");
    let r: Vec<String> = relocs.iter().map(|r| format!("{r:x}")).collect();
    assert_fnv(rel, "relocs", &r.join(","), &s("relocs_fnv"));

    let imports = pe.imports().unwrap();
    assert_eq!(
        imports.iter().map(|m| m.imports.len()).sum::<usize>() as u32,
        n("imports"),
        "{rel}"
    );
    let canon: Vec<String> = imports
        .iter()
        .map(|m| {
            let v: Vec<String> = m
                .imports
                .iter()
                .map(|i| format!("{}@{:x}", i.name, i.iat_rva))
                .collect();
            format!("{}|{}", m.dll, v.join(","))
        })
        .collect();
    assert_fnv(rel, "imports", &canon.join(";"), &s("imports_fnv"));

    let delay = pe.delay_imports().unwrap();
    assert_eq!(
        delay.iter().map(|m| m.imports.len()).sum::<usize>() as u32,
        n("delay_imports"),
        "{rel}"
    );
    let canon: Vec<String> = delay
        .iter()
        .map(|m| {
            let v: Vec<String> = m
                .imports
                .iter()
                .map(|i| format!("{}@{:x}", i.name, i.iat_rva))
                .collect();
            format!("{}|{:x}|{}", m.dll, m.module_handle_rva, v.join(","))
        })
        .collect();
    assert_fnv(rel, "delay imports", &canon.join(";"), &s("delay_fnv"));

    let mut exports = pe.exports().unwrap();
    exports.sort_by_key(|e| e.ordinal);
    assert_eq!(exports.len() as u32, n("exports"), "{rel}");
    let canon: Vec<String> = exports
        .iter()
        .map(|e| {
            let t = match &e.target {
                ExportTarget::Rva(r) => format!("{r:x}"),
                ExportTarget::Forwarder(s) => format!("->{s}"),
            };
            format!("{}:{}:{t}", e.ordinal, e.name.as_deref().unwrap_or(""))
        })
        .collect();
    assert_fnv(rel, "exports", &canon.join(";"), &s("exports_fnv"));

    let leaves = pe
        .resources()
        .unwrap()
        .map(|r| r.leaves())
        .unwrap_or_default();
    assert_eq!(leaves.len() as u32, n("resources"), "{rel}");
    let canon: Vec<String> = leaves
        .iter()
        .map(|l| {
            format!(
                "{}/{}/{}:{:x}+{:x}",
                resid(&l.ty),
                resid(&l.name),
                l.lang,
                l.data.rva,
                l.data.size
            )
        })
        .collect();
    assert_fnv(rel, "resources", &canon.join(";"), &s("resources_fnv"));

    match (pe.tls().unwrap(), f["tls"].as_array()) {
        (None, None) => {}
        (Some(t), Some(w)) => {
            let w: Vec<u32> = w.iter().map(|v| v.as_u64().unwrap() as u32).collect();
            assert_eq!(
                [
                    t.raw_start_va,
                    t.raw_end_va,
                    t.index_va,
                    t.callbacks_va,
                    t.zero_fill
                ],
                w[..],
                "{rel}"
            );
        }
        _ => panic!("{rel}: TLS presence differs"),
    }
}

#[test]
fn matches_pefile_on_samples() {
    let fixtures: Value =
        serde_json::from_str(include_str!("../../../data/pe-fixtures.json")).unwrap();
    let mut checked = 0;
    for (rel, f) in fixtures.as_object().unwrap() {
        let (build, name) = rel.split_once('/').unwrap();
        let name = if name == "$exe" {
            adv_samples::exe(build)
        } else {
            Some(name.to_owned())
        };
        let Some(bytes) =
            name.and_then(|name| std::fs::read(adv_samples::file(build, &name)?).ok())
        else {
            continue;
        };
        let pe = Pe::parse(&bytes).unwrap_or_else(|e| panic!("{rel}: {e}"));
        check(rel, f, &pe);
        checked += 1;
    }
    eprintln!("checked {checked} sample images");
}

#[test]
fn rejects_non_pe() {
    assert!(Pe::parse(b"not a pe").is_err());
    assert!(Pe::parse(&[]).is_err());
}

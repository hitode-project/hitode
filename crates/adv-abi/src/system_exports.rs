//! Fixed Windows 10.0.26100.9549 x86 export existence and routing metadata.
//! This catalog does not declare signatures or promise implemented behavior.
use std::collections::BTreeMap;
use std::sync::OnceLock;

pub const CORE_MODULES: [&str; 3] = ["ntdll.dll", "kernel32.dll", "kernelbase.dll"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Code,
    Forwarder(&'static str),
    /// Native data bytes and their layout have not been supplied by a provider.
    DataUnsupported,
}

#[derive(Clone, Copy, Debug)]
pub struct Export {
    pub module: &'static str,
    pub name: &'static str,
    pub ordinal: Option<u16>,
    pub kind: Kind,
}

type Catalog = BTreeMap<&'static str, BTreeMap<&'static str, Export>>;

fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut result: Catalog = BTreeMap::new();
        for line in include_str!("../../../data/abi/system-exports.tsv")
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
        {
            let mut fields = line.split('\t');
            let module = fields.next().expect("system export module");
            let ordinal = fields
                .next()
                .expect("system export ordinal")
                .parse()
                .expect("u16 ordinal");
            let name = fields.next().expect("system export name");
            let kind = fields.next().expect("system export kind");
            let forwarder = fields.next().expect("system export forwarder");
            assert!(fields.next().is_none());
            let kind = match (kind, forwarder) {
                ("code", "") => Kind::Code,
                ("data", "") => Kind::DataUnsupported,
                ("forwarder", target) if target.contains('.') => Kind::Forwarder(target),
                _ => panic!("invalid fixed system export"),
            };
            let entry = Export {
                module,
                name,
                ordinal: (ordinal != 0).then_some(ordinal),
                kind,
            };
            assert!(
                result
                    .entry(module)
                    .or_default()
                    .insert(name, entry)
                    .is_none()
            );
        }
        result
    })
}

pub fn is_core_module(module: &str) -> bool {
    CORE_MODULES
        .iter()
        .any(|known| known.eq_ignore_ascii_case(module))
}

pub fn lookup(module: &str, name: &str) -> Option<&'static Export> {
    let module = module.to_ascii_lowercase();
    let name = ordinal_name(&module, name).unwrap_or(name);
    catalog().get(module.as_str())?.get(name)
}

pub fn is_core_export(module: &str, name: &str) -> bool {
    is_core_module(module) && lookup(module, name).is_some()
}

pub fn ordinal_name(module: &str, name: &str) -> Option<&'static str> {
    let ordinal: u16 = name.strip_prefix('#')?.parse().ok()?;
    exports(module).find_map(|export| (export.ordinal == Some(ordinal)).then_some(export.name))
}

pub fn exports(module: &str) -> impl Iterator<Item = &'static Export> {
    let module = module.to_ascii_lowercase();
    catalog()
        .get(module.as_str())
        .into_iter()
        .flat_map(|entries| entries.values())
}

pub fn has_module(module: &str) -> bool {
    exports(module).next().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_surface_and_forwarders_are_bounded_and_have_real_terminal_owners() {
        assert_eq!(exports("ntdll.dll").count(), 2523);
        assert_eq!(exports("kernel32.dll").count(), 1670);
        assert_eq!(exports("kernelbase.dll").count(), 2037);
        assert_eq!(
            lookup("KERNEL32.DLL", "HeapAlloc").unwrap().kind,
            Kind::Forwarder("NTDLL.RtlAllocateHeap")
        );
        assert_eq!(
            lookup("kernel32.dll", "EnterCriticalSection").unwrap().kind,
            Kind::Forwarder("NTDLL.RtlEnterCriticalSection")
        );
        for export in catalog().values().flat_map(|module| module.values()) {
            let mut current = export;
            let mut visited = std::collections::BTreeSet::new();
            while let Kind::Forwarder(target) = current.kind {
                assert!(
                    visited.insert((current.module, current.name)),
                    "forwarder cycle"
                );
                let (module, name) = target.rsplit_once('.').unwrap();
                let module = format!("{}.dll", module.to_ascii_lowercase());
                current = lookup(&module, name).expect("fixed forwarder target");
            }
            assert!(!matches!(current.kind, Kind::DataUnsupported) || visited.is_empty());
        }
    }
}

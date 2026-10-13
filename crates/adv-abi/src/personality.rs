//! Export classes of the Windows personality (PERS-EXPORT-2).
//! Structure keeps every declared name; discovery and calls follow the class.
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExportClass {
    /// A registered frontend or data provider owns the behavior.
    Implemented,
    /// Discoverable without a frontend: calls return the reviewed failure
    /// contract that native Windows can also return.
    Reviewed,
    /// Bound for static imports and hooks; discovery reports it absent.
    Hidden,
    /// Fixed data without a layout provider: neither bound nor discoverable.
    DataUnsupported,
}

impl ExportClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Implemented => "implemented",
            Self::Reviewed => "reviewed",
            Self::Hidden => "hidden",
            Self::DataUnsupported => "data-unsupported",
        }
    }
    pub fn discoverable(self) -> bool {
        matches!(self, Self::Implemented | Self::Reviewed)
    }
}

/// `data/abi/dynamic_exports.tsv` as (lowercased DLL, export) pairs.
pub fn reviewed_exports() -> &'static [(String, &'static str)] {
    static REVIEWED: OnceLock<Vec<(String, &'static str)>> = OnceLock::new();
    REVIEWED.get_or_init(|| {
        include_str!("../../../data/abi/dynamic_exports.tsv")
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| {
                let mut fields = line.split('\t');
                let module = fields.next().expect("reviewed export DLL");
                let name = fields.next().expect("reviewed export name");
                assert!(fields.next().is_none(), "reviewed export row: {line}");
                (module.to_ascii_lowercase(), name)
            })
            .collect()
    })
}

pub fn is_reviewed(module: &str, name: &str) -> bool {
    reviewed_exports()
        .iter()
        .any(|(dll, export)| dll.eq_ignore_ascii_case(module) && *export == name)
}

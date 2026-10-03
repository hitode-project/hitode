//! Named ordinal aliases projected from official x86 SDK import libraries.
use std::collections::BTreeMap;
use std::sync::OnceLock;

fn aliases() -> &'static BTreeMap<(&'static str, u16), &'static str> {
    static ALIASES: OnceLock<BTreeMap<(&'static str, u16), &'static str>> = OnceLock::new();
    ALIASES.get_or_init(|| {
        include_str!("../../../data/abi/import-ordinals.tsv")
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| {
                let mut fields = line.split('\t');
                let module = fields.next().expect("ordinal module");
                let ordinal = fields
                    .next()
                    .expect("ordinal number")
                    .parse()
                    .expect("u16 ordinal");
                let name = fields.next().expect("ordinal export");
                assert!(fields.next().is_none());
                ((module, ordinal), name)
            })
            .collect()
    })
}

pub fn lookup(module: &str, name: &str) -> Option<&'static str> {
    let ordinal = name.strip_prefix('#')?.parse().ok()?;
    aliases().get(&(module, ordinal)).copied()
}

/// Real export ordinals for the synthetic PE writer; never assign ordinals by sorting names.
pub fn exports(module: &str) -> impl Iterator<Item = (u16, &'static str)> + '_ {
    aliases()
        .iter()
        .filter_map(move |(&(dll, ordinal), &name)| (dll == module).then_some((ordinal, name)))
}

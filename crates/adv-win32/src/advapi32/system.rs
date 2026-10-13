//! Volatile system configuration derived from the emulated processor owner.
//! It shares the registry tree and handles; it never reads host hardware.
use super::store::*;
use std::collections::BTreeMap;

pub(super) fn install(registry: &mut VirtualRegistry, modified: u64) -> Result<(), u32> {
    let hklm = Key {
        root: 0x8000_0002,
        path: Vec::new(),
    };
    let path = "HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0";
    let units: Vec<_> = path.encode_utf16().collect();
    let (processor, parents) = child_key(&hklm, &units)?;
    let hardware = parents[0].clone();
    let mut keys = BTreeMap::new();
    let mut metadata = BTreeMap::new();
    for (key, part) in parents.iter().zip(units.split(|&unit| unit == 92)) {
        keys.insert(key.clone(), BTreeMap::new());
        metadata.insert(
            key.clone(),
            KeyMetadata {
                display_name: part.to_vec(),
                class: Vec::new(),
                modified,
            },
        );
    }
    let identity = adv_cpu::PROCESSOR_IDENTITY;
    let values = keys.get_mut(&processor).unwrap();
    let mhz = u32::try_from(adv_cpu::TSC_FREQ / 1_000_000).map_err(|_| INVALID_DATA)?;
    let mut insert = |name: &str, kind, bytes: Vec<u8>| {
        let display_name: Vec<_> = name.encode_utf16().collect();
        values.insert(
            fold(&display_name),
            Value {
                display_name,
                kind,
                bytes,
            },
        );
    };
    insert("~MHz", 4, mhz.to_le_bytes().to_vec());
    for (name, text) in [
        ("VendorIdentifier", identity.vendor.to_owned()),
        ("ProcessorNameString", identity.name.to_owned()),
        (
            "Identifier",
            format!(
                "x86 Family {} Model {} Stepping {}",
                identity.family(),
                identity.model(),
                identity.stepping()
            ),
        ),
    ] {
        insert(
            name,
            1,
            text.encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect(),
        );
    }
    // Persisted application data cannot override this runtime-owned hive.
    // Reserve the complete replacement before changing any tree or counters.
    let removed: Vec<_> = registry
        .keys
        .keys()
        .filter(|key| key.is_under(&hardware))
        .cloned()
        .collect();
    let value_counts = |values: &BTreeMap<Vec<u16>, Value>| {
        (
            values.len(),
            values.values().map(|v| v.bytes.len()).sum::<usize>(),
            values
                .iter()
                .map(|(name, v)| 16 + (name.len() + v.display_name.len()) * 2)
                .sum::<usize>(),
        )
    };
    let (mut total_values, mut total_bytes, mut metadata_bytes) = (
        registry.total_values,
        registry.total_bytes,
        registry.metadata_bytes,
    );
    for key in &removed {
        let (count, bytes, meta) = value_counts(&registry.keys[key]);
        total_values -= count;
        total_bytes -= bytes;
        metadata_bytes -= 12 + key.path.len() * 2 + registry.metadata[key].size() + meta;
    }
    for (key, values) in &keys {
        let (count, bytes, meta) = value_counts(values);
        total_values += count;
        total_bytes += bytes;
        metadata_bytes += 12 + key.path.len() * 2 + metadata[key].size() + meta;
    }
    if registry.keys.len() - removed.len() + keys.len() > MAX_KEYS
        || total_values > MAX_VALUES
        || total_bytes > MAX_TOTAL_BYTES
        || metadata_bytes > MAX_METADATA_BYTES
    {
        return Err(NO_MEMORY);
    }
    for key in removed {
        registry.keys.remove(&key);
        registry.metadata.remove(&key);
        registry.volatile_keys.remove(&key);
    }
    registry.volatile_keys.extend(keys.keys().cloned());
    registry.keys.extend(keys);
    registry.metadata.extend(metadata);
    registry.read_only_roots.insert(hardware);
    (
        registry.total_values,
        registry.total_bytes,
        registry.metadata_bytes,
    ) = (total_values, total_bytes, metadata_bytes);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exhausted_configuration_reservation_leaves_the_registry_unchanged() {
        let mut registry = VirtualRegistry::default();
        registry.total_values = MAX_VALUES;
        let before = (
            registry.keys.len(),
            registry.metadata_bytes,
            registry.total_bytes,
        );
        assert_eq!(install(&mut registry, 123), Err(NO_MEMORY));
        assert_eq!(
            before,
            (
                registry.keys.len(),
                registry.metadata_bytes,
                registry.total_bytes
            )
        );
        assert!(registry.read_only_roots.is_empty());
        assert!(registry.volatile_keys.is_empty());
        assert!(!registry.dirty);
    }
    #[test]
    // spec: WIN32-API-5
    fn reinstall_discards_saved_hardware_overrides_and_keeps_counters_consistent() {
        let mut registry = VirtualRegistry::default();
        install(&mut registry, 123).unwrap();
        let processor = registry
            .keys
            .keys()
            .find(|key| key.path.last() == Some(&48))
            .unwrap()
            .clone();
        let name: Vec<_> = "~MHZ".encode_utf16().collect();
        registry
            .keys
            .get_mut(&processor)
            .unwrap()
            .get_mut(&name)
            .unwrap()
            .bytes = 9000u32.to_le_bytes().to_vec();
        registry.volatile_keys.clear();
        registry.read_only_roots.clear();
        let before = (
            registry.total_values,
            registry.total_bytes,
            registry.metadata_bytes,
        );
        install(&mut registry, 456).unwrap();
        assert_eq!(
            registry.keys[&processor][&name].bytes,
            1000u32.to_le_bytes()
        );
        assert_eq!(
            before,
            (
                registry.total_values,
                registry.total_bytes,
                registry.metadata_bytes
            )
        );
        assert_eq!(registry.metadata[&processor].modified, 456);
        assert_eq!(registry.volatile_keys.len(), 5);
        assert!(registry.is_read_only(&processor));
        assert!(!registry.dirty);
    }
}

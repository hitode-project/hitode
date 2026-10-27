//! Host font files the player configures.
/// Read explicit host font files within the device's existing resource budget.
pub(crate) fn read_resources(paths: &[std::path::PathBuf]) -> Result<Vec<Vec<u8>>, String> {
    use std::io::Read;
    let mut remaining = adv_device::text::MAX_FONT_RESOURCE_BYTES;
    let mut files = Vec::new();
    for path in paths {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(remaining as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        remaining = remaining
            .checked_sub(bytes.len())
            .ok_or("font resources exceed the byte limit")?;
        files.push(bytes);
    }
    Ok(files)
}

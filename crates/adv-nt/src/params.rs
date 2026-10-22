//! The process parameters block (RTL_USER_PROCESS_PARAMETERS) of a new
//! process, laid out as Wine's `build_initial_params` does: the structure,
//! a MAX_PATH buffer for the current directory, the strings, and the
//! environment, in one private allocation that ntdll.dll later replaces and
//! frees.

/// What a process starts with.
pub struct Startup<'a> {
    /// The DOS path of the main image.
    pub image: &'a str,
    pub command_line: &'a str,
    /// The DOS path of the current directory, ending in a backslash.
    pub directory: &'a str,
    /// `NAME=value` pairs.
    pub environment: &'a [(&'a str, &'a str)],
    /// The LCID of the system and user locales.
    pub locale: u32,
    /// Wine debug channels, in WINEDEBUG syntax.
    pub debug: &'a str,
}

const SIZE: u32 = 0x2a4;
const MAX_PATH: u32 = 260;
const FLAG_NORMALIZED: u32 = 1;
const SW_SHOWNORMAL: u32 = 1;

const ALLOCATION_SIZE: usize = 0x00;
const LENGTH: usize = 0x04;
const FLAGS: usize = 0x08;
const CURRENT_DIRECTORY: usize = 0x24;
const IMAGE_PATH_NAME: usize = 0x38;
const COMMAND_LINE: usize = 0x40;
const ENVIRONMENT: usize = 0x48;
const SHOW_WINDOW: usize = 0x6c;
const WINDOW_TITLE: usize = 0x70;
const ENVIRONMENT_SIZE: usize = 0x290;
const PROCESS_GROUP_ID: usize = 0x29c;

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// The size of the block for `startup`.
pub fn size(startup: &Startup<'_>) -> u32 {
    let (strings, environment) = contents(startup);
    SIZE + 2 * (MAX_PATH + strings.len() as u32 + environment.len() as u32)
}

/// The strings after the current directory buffer, each with its NUL, and
/// the environment block.
fn contents(startup: &Startup<'_>) -> (Vec<u16>, Vec<u16>) {
    let mut strings = Vec::new();
    for text in [startup.image, startup.command_line, startup.image] {
        strings.extend(utf16(text));
        strings.push(0);
    }
    let mut environment = Vec::new();
    for (name, value) in startup.environment {
        environment.extend(utf16(&format!("{name}={value}")));
        environment.push(0);
    }
    environment.push(0);
    (strings, environment)
}

fn put(block: &mut [u8], offset: usize, value: u32) {
    block[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_text(block: &mut [u8], offset: usize, text: &[u16]) {
    for (index, unit) in text.iter().enumerate() {
        let at = offset + 2 * index;
        block[at..at + 2].copy_from_slice(&unit.to_le_bytes());
    }
}

/// Stores a UNICODE_STRING at `field` for `text` placed at `offset`.
fn put_string(block: &mut [u8], base: u32, field: usize, offset: u32, text: &[u16], maximum: u32) {
    put(block, field, (2 * text.len() as u32) | (maximum << 16));
    put(block, field + 4, base + offset);
    put_text(block, offset as usize, text);
}

/// The block for `startup`, to be placed at `base`.
pub fn build(startup: &Startup<'_>, base: u32, process_id: u32) -> Vec<u8> {
    let size = size(startup);
    let mut block = vec![0u8; size as usize];
    put(&mut block, ALLOCATION_SIZE, size);
    put(&mut block, LENGTH, size);
    put(&mut block, FLAGS, FLAG_NORMALIZED);
    put(&mut block, SHOW_WINDOW, SW_SHOWNORMAL);
    put(&mut block, PROCESS_GROUP_ID, process_id);

    let directory = utf16(startup.directory);
    put_string(
        &mut block,
        base,
        CURRENT_DIRECTORY,
        SIZE,
        &directory,
        2 * MAX_PATH,
    );
    let mut cursor = SIZE + 2 * MAX_PATH;
    for (field, text) in [
        (IMAGE_PATH_NAME, startup.image),
        (COMMAND_LINE, startup.command_line),
        (WINDOW_TITLE, startup.image),
    ] {
        let text = utf16(text);
        let maximum = 2 * (text.len() as u32 + 1);
        put_string(&mut block, base, field, cursor, &text, maximum);
        cursor += maximum;
    }
    let (_, environment) = contents(startup);
    put(&mut block, ENVIRONMENT, base + cursor);
    put(&mut block, ENVIRONMENT_SIZE, 2 * environment.len() as u32);
    put_text(&mut block, cursor as usize, &environment);
    block
}

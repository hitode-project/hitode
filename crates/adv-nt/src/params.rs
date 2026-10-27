//! The process parameters block (RTL_USER_PROCESS_PARAMETERS) of a new
//! process, laid out as Wine's `build_initial_params` does: the structure,
//! a MAX_PATH buffer for the current directory, the strings, and the
//! environment, in one private allocation that ntdll.dll later replaces and
//! frees.

use std::os::fd::RawFd;

/// What a process starts with.
pub struct Startup<'a> {
    /// The DOS path of the main image.
    pub image: &'a str,
    pub command_line: &'a str,
    /// The DOS path of the current directory, ending in a backslash.
    pub directory: &'a str,
    /// `NAME=value` pairs, before the ones Wine adds
    /// ([`crate::environment`]).
    pub environment: &'a [(&'a str, &'a str)],
    /// The LCID of the system and user locales.
    pub locale: u32,
    /// Wine debug channels, in WINEDEBUG syntax.
    pub debug: &'a str,
    /// Host descriptors for the standard handles; none leaves a handle 0.
    pub stdin: Option<RawFd>,
    pub stdout: Option<RawFd>,
    pub stderr: Option<RawFd>,
    /// The display driver win32u loads, which explorer would choose in
    /// Wine: `null` for windows that show nowhere.
    pub graphics_driver: &'a str,
    /// Wine's font `Path`: the directories win32u loads fonts from besides
    /// the Windows one, `;`-separated.
    pub fonts: &'a str,
}

const SIZE: u32 = 0x2a4;
const MAX_PATH: u32 = 260;
const FLAG_NORMALIZED: u32 = 1;
const SW_SHOWNORMAL: u32 = 1;

const ALLOCATION_SIZE: usize = 0x00;
const LENGTH: usize = 0x04;
const FLAGS: usize = 0x08;
const STD_INPUT: usize = 0x18;
const STD_OUTPUT: usize = 0x1c;
const STD_ERROR: usize = 0x20;
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

/// The size of the block for `startup` with the environment block
/// `environment`.
pub fn size(startup: &Startup<'_>, environment: &[u16]) -> u32 {
    SIZE + 2 * (MAX_PATH + strings(startup).len() as u32 + environment.len() as u32)
}

/// The strings after the current directory buffer, each with its NUL.
fn strings(startup: &Startup<'_>) -> Vec<u16> {
    let mut strings = Vec::new();
    for text in [startup.image, startup.command_line, startup.image] {
        strings.extend(utf16(text));
        strings.push(0);
    }
    strings
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
pub fn build(
    startup: &Startup<'_>,
    environment: &[u16],
    base: u32,
    process_id: u32,
    handles: [u32; 3],
) -> Vec<u8> {
    let size = size(startup, environment);
    let mut block = vec![0u8; size as usize];
    put(&mut block, ALLOCATION_SIZE, size);
    put(&mut block, LENGTH, size);
    put(&mut block, FLAGS, FLAG_NORMALIZED);
    put(&mut block, SHOW_WINDOW, SW_SHOWNORMAL);
    put(&mut block, PROCESS_GROUP_ID, process_id);
    for (field, handle) in [STD_INPUT, STD_OUTPUT, STD_ERROR].into_iter().zip(handles) {
        put(&mut block, field, handle);
    }

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
    put(&mut block, ENVIRONMENT, base + cursor);
    put(&mut block, ENVIRONMENT_SIZE, 2 * environment.len() as u32);
    put_text(&mut block, cursor as usize, environment);
    block
}

/// Wine's startup_info_data followed by its strings, and the environment block.
pub(crate) fn serialize_startup(
    env: &mut adv_machine::Env<'_>,
    parameters: u32,
    image: &str,
) -> Result<(Vec<u8>, Vec<u8>), crate::status::Error> {
    use crate::{object::read_u32, status};
    let mut info = vec![0; std::mem::size_of::<crate::protocol::StartupInfoData>()];
    for (to, from) in [
        (0, 0x0c),
        (4, 0x14),
        (8, 0x10),
        (12, 0x18),
        (16, 0x1c),
        (20, 0x20),
        (24, 0x4c),
        (28, 0x50),
        (32, 0x54),
        (36, 0x58),
        (40, 0x5c),
        (44, 0x60),
        (48, 0x64),
        (52, 0x68),
        (56, 0x6c),
        (60, 0x29c),
    ] {
        put(&mut info, to, read_u32(env, parameters + from)?);
    }
    let normalized = read_u32(env, parameters + 8)? & 1 != 0;
    for (index, field) in [0x24, 0x30, 0x38, 0x40, 0x70, 0x78, 0x80, 0x88]
        .into_iter()
        .enumerate()
    {
        let bytes = if field == 0x38 {
            image
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>()
        } else {
            let len = read_u32(env, parameters + field)? & 0xffff;
            let mut pointer = read_u32(env, parameters + field + 4)?;
            if !normalized {
                pointer += parameters;
            }
            let mut bytes = vec![0; len as usize];
            if len != 0 {
                env.cpu
                    .read(pointer, &mut bytes)
                    .map_err(|_| status::ACCESS_VIOLATION)?;
            }
            bytes
        };
        put(&mut info, 64 + 4 * index, bytes.len() as u32);
        info.extend(bytes);
    }
    if !info.len().is_multiple_of(2) {
        info.push(0);
    }
    let pointer = read_u32(env, parameters + 0x48)?;
    let mut environment = Vec::new();
    if pointer != 0 {
        let mut previous = false;
        for at in (0..2 * 1024 * 1024).step_by(2) {
            let mut unit = [0; 2];
            env.cpu
                .read(pointer + at, &mut unit)
                .map_err(|_| status::ACCESS_VIOLATION)?;
            environment.extend(unit);
            if unit == [0; 2] && previous {
                break;
            }
            previous = unit == [0; 2];
        }
    } else {
        environment.extend([0; 4]);
    }
    Ok((info, environment))
}

pub(crate) fn child_size(bytes: &[u8]) -> u32 {
    SIZE + 520 + bytes.len() as u32 + 32
}

/// Reconstruct normalized process parameters from get_startup_info, as init_startup_info does.
pub(crate) fn child_block(
    bytes: &[u8],
    info_size: u32,
    base: u32,
) -> Result<Vec<u8>, crate::process::Error> {
    let get = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let mut block = vec![0; child_size(bytes) as usize];
    let len = block.len() as u32;
    put(&mut block, 0, len);
    put(&mut block, 4, len);
    put(&mut block, 8, 1);
    for (from, to) in [
        (0, 0x0c),
        (4, 0x14),
        (8, 0x10),
        (12, 0x18),
        (16, 0x1c),
        (20, 0x20),
        (24, 0x4c),
        (28, 0x50),
        (32, 0x54),
        (36, 0x58),
        (40, 0x5c),
        (44, 0x60),
        (48, 0x64),
        (52, 0x68),
        (56, 0x6c),
        (60, 0x29c),
    ] {
        put(&mut block, to, get(from));
    }
    let mut source = 96;
    let mut target = SIZE as usize;
    for (index, field) in [0x24, 0x30, 0x38, 0x40, 0x70, 0x78, 0x80, 0x88]
        .into_iter()
        .enumerate()
    {
        let len = get(64 + 4 * index) as usize;
        if field == 0x30 && len == 0 {
            continue;
        }
        let mut text = &bytes[source..source + len];
        if field == 0x38 && text.starts_with(&[92, 0, 63, 0, 63, 0, 92, 0]) {
            text = &text[8..];
        }
        let max = if index == 0 { 520 } else { text.len() + 2 };
        put(&mut block, field, text.len() as u32 | ((max as u32) << 16));
        put(&mut block, field + 4, base + target as u32);
        block[target..target + text.len()].copy_from_slice(text);
        target += max;
        source += len;
    }
    let environment = &bytes[info_size as usize..];
    put(&mut block, ENVIRONMENT, base + target as u32);
    put(&mut block, ENVIRONMENT_SIZE, environment.len() as u32);
    block[target..target + environment.len()].copy_from_slice(environment);
    Ok(block)
}

//! The environment a process without a parent starts with, as Wine's
//! ntdll.so builds it (`dlls/ntdll/unix/env.c`): the variables it is given,
//! the ones Wine adds for each process, then the ones the registry sets.

use adv_machine::Env;

use crate::protocol::{EnumKeyValueRequest, GetTokenSidRequest};
use crate::registry;
use crate::server;
use crate::status::{self, Error};

/// The user of every prefix, whose profile the prefix template holds.
pub const USER_NAME: &str = "Player";
/// The computer every prefix runs on, as `gethostname` names it.
pub const COMPUTER_NAME: &str = "HITODE";

/// `GetCurrentThreadEffectiveToken()`.
const EFFECTIVE_TOKEN: u32 = 0xffff_fffa;
const TOKEN_USER: u32 = 1;
const KEY_VALUE_FULL: i32 = 1;
const REG_EXPAND_SZ: u32 = 2;
/// Wine reads values into a buffer of 1024 characters.
const VALUE_LIMIT: usize = 2048;

const SYSTEM_ENVIRONMENT: &str =
    r"\Registry\Machine\System\CurrentControlSet\Control\Session Manager\Environment";
const PROFILE_LIST: &str =
    r"\Registry\Machine\Software\Microsoft\Windows NT\CurrentVersion\ProfileList";
const CURRENT_VERSION: &str = r"\Registry\Machine\Software\Microsoft\Windows\CurrentVersion";
const ACTIVE_COMPUTER_NAME: &str =
    r"\Registry\Machine\System\CurrentControlSet\Control\ComputerName\ActiveComputerName";

/// `NAME=value` strings.
pub struct Environment(Vec<Vec<u16>>);

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// Names compare as `wcsnicmp` does, ignoring ASCII case.
fn same(a: &[u16], b: &[u16]) -> bool {
    let lower = |unit: u16| match unit {
        0x41..=0x5a => unit + 0x20,
        _ => unit,
    };
    a.len() == b.len() && a.iter().zip(b).all(|(&a, &b)| lower(a) == lower(b))
}

fn same_name(entry: &[u16], name: &[u16]) -> bool {
    entry.len() > name.len()
        && entry[name.len()] == u16::from(b'=')
        && same(&entry[..name.len()], name)
}

impl Environment {
    pub fn new(pairs: &[(&str, &str)]) -> Environment {
        Environment(
            pairs
                .iter()
                .map(|(name, value)| utf16(&format!("{name}={value}")))
                .collect(),
        )
    }

    fn get(&self, name: &[u16]) -> Option<&[u16]> {
        self.0
            .iter()
            .find(|entry| same_name(entry, name))
            .map(|entry| &entry[name.len() + 1..])
    }

    /// Replaces `name`, or removes it when `value` is `None`.
    fn set(&mut self, name: &[u16], value: Option<&[u16]>) {
        self.0.retain(|entry| !same_name(entry, name));
        if let Some(value) = value {
            let mut entry = name.to_vec();
            entry.push(u16::from(b'='));
            entry.extend_from_slice(value);
            self.0.push(entry);
        }
    }

    /// `%NAME%` replaced by its value; unknown and unfinished names stay.
    fn expand(&self, mut source: &[u16]) -> Vec<u16> {
        let percent = u16::from(b'%');
        let mut expanded = Vec::new();
        while !source.is_empty() {
            if source[0] != percent {
                let length = source
                    .iter()
                    .position(|&unit| unit == percent)
                    .unwrap_or(source.len());
                expanded.extend_from_slice(&source[..length]);
                source = &source[length..];
                continue;
            }
            match source[1..].iter().position(|&unit| unit == percent) {
                Some(length) => {
                    match self.get(&source[1..1 + length]) {
                        Some(value) => expanded.extend_from_slice(value),
                        None => expanded.extend_from_slice(&source[..length + 2]),
                    }
                    source = &source[length + 2..];
                }
                None => {
                    expanded.extend_from_slice(source);
                    source = &[];
                }
            }
        }
        expanded
    }

    /// The environment block: each string with its NUL, then a NUL.
    pub fn block(&self) -> Vec<u16> {
        let mut block: Vec<u16> = self
            .0
            .iter()
            .flat_map(|entry| entry.iter().copied().chain([0]))
            .collect();
        block.push(0);
        block
    }

    /// `add_dynamic_environment`: what can differ between processes. A
    /// session has no Unix home, configuration or loader to name.
    pub fn add_dynamic(&mut self) {
        for (name, value) in [
            ("WINEUSERNAME", USER_NAME),
            ("SystemDrive", "C:"),
            ("SystemRoot", r"C:\windows"),
        ] {
            self.set(&utf16(name), Some(&utf16(value)));
        }
    }

    /// `add_registry_environment`: the variables the registry sets, for a
    /// process whose first thread runs `env`.
    pub fn add_registry(&mut self, env: &mut Env<'_>) -> Result<(), Error> {
        let user = user_key(env)?;
        for path in [
            SYSTEM_ENVIRONMENT.to_owned(),
            format!(r"{user}\Environment"),
            format!(r"{user}\Volatile Environment"),
        ] {
            if let Some(key) = open(env, &path)? {
                let added = self.add_variables(env, key);
                server::close(env, key)?;
                added?;
            }
        }
        self.add_values(
            env,
            PROFILE_LIST,
            &[
                ("ProgramData", &["ALLUSERSPROFILE", "ProgramData"]),
                ("Public", &["PUBLIC"]),
            ],
        )?;
        // An i386 prefix has no "(x86)" directories.
        self.add_values(
            env,
            CURRENT_VERSION,
            &[
                ("ProgramFilesDir", &["ProgramFiles"]),
                ("CommonFilesDir", &["CommonProgramFiles"]),
            ],
        )?;
        self.add_values(
            env,
            ACTIVE_COMPUTER_NAME,
            &[("ComputerName", &["COMPUTERNAME"])],
        )
    }

    /// `add_registry_variables`: every value of `key`, `PATH` appended to.
    fn add_variables(&mut self, env: &mut Env<'_>, key: u32) -> Result<(), Error> {
        let path = utf16("PATH");
        for index in 0.. {
            let mut request = EnumKeyValueRequest {
                hkey: key,
                index,
                info_class: KEY_VALUE_FULL,
                ..Default::default()
            };
            let mut data = vec![0u8; VALUE_LIMIT + 2 * 1024];
            let (reply, size) = server::request(env, &mut request, &[], &mut data)?;
            if reply.header.error != status::SUCCESS {
                break;
            }
            let units: Vec<u16> = data[..size]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&pair| u16::from_le_bytes(pair))
                .collect();
            let (name, value) = units.split_at((reply.namelen as usize / 2).min(units.len()));
            let value = trim(value);
            if value.is_empty() {
                continue;
            }
            let mut value = match reply.r#type as u32 {
                REG_EXPAND_SZ => self.expand(value),
                _ => value.to_vec(),
            };
            if same(name, &path)
                && let Some(old) = self.get(&path)
            {
                let mut joined = old.to_vec();
                joined.push(u16::from(b';'));
                joined.append(&mut value);
                value = joined;
            }
            self.set(name, Some(&value));
        }
        Ok(())
    }

    /// Each value of the key at `path` sets the variables it is listed with.
    fn add_values(
        &mut self,
        env: &mut Env<'_>,
        path: &str,
        values: &[(&str, &[&str])],
    ) -> Result<(), Error> {
        let Some(key) = open(env, path)? else {
            return Ok(());
        };
        let mut result = Ok(());
        for &(value, names) in values {
            match registry::value(env, key, value, VALUE_LIMIT) {
                Ok(Some((kind, data))) => {
                    let units: Vec<u16> = data
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|&pair| u16::from_le_bytes(pair))
                        .collect();
                    let units = trim(&units);
                    let value = match kind {
                        REG_EXPAND_SZ => self.expand(units),
                        _ => units.to_vec(),
                    };
                    for name in names {
                        self.set(&utf16(name), Some(&value));
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    result = Err(error);
                    break;
                }
            }
        }
        server::close(env, key)?;
        result
    }
}

/// A string value's characters, up to its first NUL.
fn trim(units: &[u16]) -> &[u16] {
    &units[..units
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(units.len())]
}

fn open(env: &mut Env<'_>, path: &str) -> Result<Option<u32>, Error> {
    match registry::open_for_reading(env, 0, path) {
        Ok(key) => Ok(Some(key)),
        Err(Error::Status(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

/// `\Registry\User\<sid>` of the thread's user, as `open_hkcu_key` names it.
fn user_key(env: &mut Env<'_>) -> Result<String, Error> {
    let mut request = GetTokenSidRequest {
        handle: EFFECTIVE_TOKEN,
        which_sid: TOKEN_USER,
        ..Default::default()
    };
    let mut sid = [0u8; 68];
    let (reply, size) = server::request(env, &mut request, &[], &mut sid)?;
    status::check(reply.header.error)?;
    let sid = &sid[..size];
    if sid.len() < 8 {
        return Err(Error::Unsupported(
            "a user SID without its authority".into(),
        ));
    }
    let authority = sid[2..8]
        .iter()
        .fold(0u64, |value, &byte| value << 8 | u64::from(byte));
    let mut name = format!(r"\Registry\User\S-{}-{}", sid[0], authority as u32);
    for sub in sid[8..].as_chunks::<4>().0.iter().take(usize::from(sid[1])) {
        name.push_str(&format!("-{}", u32::from_le_bytes(*sub)));
    }
    Ok(name)
}

//! A pinned, isolated font inventory and explicit ordered replacement rules.
use super::*;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

pub(super) struct Loaded {
    pub paths: BTreeMap<ID, PathBuf>,
    rules: BTreeMap<Selector, Vec<ID>>,
    pub sha256: String,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Selector {
    Face(String),
    Default(DefaultFont),
}

pub(super) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

impl Loaded {
    pub(super) fn load(path: &Path, db: &mut Database) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
        if bytes.len() > 1024 * 1024 {
            return Err("font profile exceeds 1 MiB".into());
        }
        let document: Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        let root = object(&document, &["version", "faces", "rules"])?;
        if root.get("version").and_then(Value::as_u64) != Some(1) {
            return Err("font profile version must be 1".into());
        }
        let faces = array(root, "faces")?;
        let rules = array(root, "rules")?;
        if faces.len() > 128 || rules.len() > 256 {
            return Err("font profile face/rule budget exceeded".into());
        }
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let mut files: BTreeMap<PathBuf, (String, Vec<ID>)> = BTreeMap::new();
        let mut ids = BTreeMap::new();
        let mut paths = BTreeMap::new();
        let mut total_bytes = 0usize;
        for value in faces {
            let face = object(value, &["id", "path", "sha256", "index", "family"])?;
            let id = string(face, "id")?;
            if ids.contains_key(id) {
                return Err(format!("duplicate font profile face id {id:?}"));
            }
            let path = parent.join(string(face, "path")?);
            let expected = string(face, "sha256")?;
            if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(format!("invalid font SHA-256 for {id:?}"));
            }
            let index = face
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| format!("invalid collection index for {id:?}"))?;
            let family = string(face, "family")?;
            if !files.contains_key(&path) {
                let length = std::fs::metadata(&path)
                    .map_err(|error| format!("{}: {error}", path.display()))?
                    .len();
                if length > MAX_SYSTEM_FONT_BYTES as u64 {
                    return Err(format!(
                        "font profile file exceeds data limit: {}",
                        path.display()
                    ));
                }
                let data =
                    std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
                total_bytes = total_bytes.saturating_add(data.len());
                if data.len() > MAX_SYSTEM_FONT_BYTES || total_bytes > 128 * 1024 * 1024 {
                    return Err("font profile data budget exceeded".into());
                }
                let digest = hash(&data);
                if !digest.eq_ignore_ascii_case(expected) {
                    return Err(format!(
                        "font profile SHA-256 mismatch for {}",
                        path.display()
                    ));
                }
                // Keep exactly the verified bytes, rather than reopen a file
                // after checking it. Unlisted TTC faces are retired below.
                let loaded = db.load_font_source(Source::Binary(Arc::new(data)));
                files.insert(path.clone(), (digest, loaded.into_iter().collect()));
            }
            let (digest, loaded) = &files[&path];
            if !digest.eq_ignore_ascii_case(expected) {
                return Err(format!("conflicting font SHA-256 for {}", path.display()));
            }
            let selected = loaded
                .iter()
                .copied()
                .find(|&id| db.face(id).is_some_and(|info| info.index == index))
                .ok_or_else(|| {
                    format!("font profile collection index {index} missing for {id:?}")
                })?;
            let info = db.face(selected).unwrap();
            if !info.families.iter().any(|name| name.0 == family) {
                return Err(format!(
                    "font profile family {family:?} missing at index {index}"
                ));
            }
            let valid = db.with_face_data(selected, |data, index| {
                FontRef::from_index(data, index).is_ok_and(|font| {
                    font.head().is_ok_and(|head| head.units_per_em() != 0)
                        && font.maxp().is_ok()
                        && font.hmtx().is_ok()
                })
            });
            if valid != Some(true) {
                return Err(format!("invalid OpenType font profile face {id:?}"));
            }
            ids.insert(id.to_owned(), selected);
            paths.insert(selected, path);
        }
        let retained: BTreeSet<_> = ids.values().copied().collect();
        for (_, loaded) in files.values() {
            for &id in loaded {
                if !retained.contains(&id) {
                    db.remove_face(id);
                }
            }
        }
        let mut replacements = BTreeMap::new();
        for value in rules {
            let rule = object(value, &["face", "default", "faces"])?;
            let selector = match (rule.get("face"), rule.get("default")) {
                (Some(_), None) => Selector::Face(string(rule, "face")?.to_lowercase()),
                (None, Some(_)) => Selector::Default(match string(rule, "default")? {
                    "console" => DefaultFont::Console,
                    "fixed" => DefaultFont::Fixed,
                    "variable" => DefaultFont::Variable,
                    "system" => DefaultFont::System,
                    "device" => DefaultFont::Device,
                    "system_fixed" => DefaultFont::SystemFixed,
                    "interface" => DefaultFont::Interface,
                    other => {
                        return Err(format!("invalid font profile default selector {other:?}"));
                    }
                }),
                _ => {
                    return Err(
                        "font profile rule requires exactly one face/default selector".into(),
                    );
                }
            };
            let names = array(rule, "faces")?;
            if names.is_empty() || names.len() > 128 {
                return Err("font profile rule needs 1..128 ordered faces".into());
            }
            let mut candidates = Vec::new();
            for name in names {
                let name = name
                    .as_str()
                    .ok_or("font profile face id must be a string")?;
                let &id = ids
                    .get(name)
                    .ok_or_else(|| format!("font profile face id {name:?} is not declared"))?;
                if candidates.contains(&id) {
                    return Err(format!("duplicate face in font profile rule: {name:?}"));
                }
                candidates.push(id);
            }
            if replacements.insert(selector, candidates).is_some() {
                return Err("duplicate font profile rule selector".into());
            }
        }
        Ok(Self {
            paths,
            rules: replacements,
            sha256: hash(&bytes),
        })
    }

    pub(super) fn candidates(&self, request: &FontRequest) -> Option<&[ID]> {
        let selector = match request.default {
            Some(default) => Selector::Default(default),
            None => Selector::Face(request.face.to_lowercase()),
        };
        self.rules.get(&selector).map(Vec::as_slice)
    }
}

fn object<'a>(value: &'a Value, allowed: &[&str]) -> Result<&'a Map<String, Value>, String> {
    let object = value
        .as_object()
        .ok_or("font profile entry must be an object")?;
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(format!("unknown font profile field {field:?}"));
    }
    Ok(object)
}
fn string<'a>(object: &'a Map<String, Value>, field: &str) -> Result<&'a str, String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && !value.contains('\0'))
        .ok_or_else(|| format!("font profile {field:?} must be a nonempty string"))
}
fn array<'a>(object: &'a Map<String, Value>, field: &str) -> Result<&'a [Value], String> {
    object
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| format!("font profile {field:?} must be an array"))
}

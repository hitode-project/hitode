//! Portable OpenType adapter. Discovery, parsing/hinting and rasterization are
//! delegated to fontdb, skrifa and zeno respectively. No guest ABI is stored here.
use super::*;
use fontdb::{Database, FaceInfo, ID, Source, Style};
use skrifa::{
    FontRef, GlyphId, MetadataProvider,
    instance::{Location, LocationRef, Size},
    outline::{DrawSettings, HintingInstance, OutlinePen, Target},
    raw::TableProvider,
};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};
use zeno::{Command, Vector};

mod discovery;
mod enumeration;
mod profile;
mod replacements;

// System TTCs such as PingFang exceed the guest font-resource budget.
const MAX_SYSTEM_FONT_BYTES: usize = 128 * 1024 * 1024;

type FontBytes = Arc<dyn AsRef<[u8]> + Send + Sync>;
#[derive(Default)]
struct FaceStore {
    faces: BTreeMap<ID, Arc<Face>>,
    files: BTreeMap<PathBuf, FontBytes>,
    digests: BTreeMap<usize, (FontBytes, String)>,
    file_bytes: usize,
}
struct Resource {
    faces: Vec<ID>,
    bytes: usize,
    enumerable: bool,
}
#[derive(Default)]
pub(super) struct Backend {
    database: OnceLock<Result<Discovery, String>>,
    directories: Option<Vec<PathBuf>>,
    profile_path: Option<PathBuf>,
    resources: BTreeMap<FontResourceId, Resource>,
    /// BCP 47 tag of the guest UI language, which names enumerated families.
    pub(super) language: Option<&'static str>,
    next_resource: u64,
    resource_bytes: usize,
    store: Arc<Mutex<FaceStore>>,
}
/// Installed fonts are scanned once per process; a backend's resources copy
/// the shared database on first change.
static SYSTEM_FONTS: OnceLock<Arc<Database>> = OnceLock::new();

fn system_fonts() -> Arc<Database> {
    SYSTEM_FONTS
        .get_or_init(|| {
            let mut db = Database::new();
            #[cfg(target_os = "macos")]
            discovery::load_system_fonts(&mut db);
            #[cfg(not(target_os = "macos"))]
            db.load_system_fonts();
            // fontdb's system discovery intentionally excludes Android.
            #[cfg(target_os = "android")]
            for directory in ["/system/fonts", "/product/fonts", "/vendor/fonts"] {
                db.load_fonts_dir(directory);
            }
            if let Some(directories) = std::env::var_os("ADV_FONT_DIR") {
                for directory in std::env::split_paths(&directories) {
                    db.load_fonts_dir(directory);
                }
            }
            Arc::new(db)
        })
        .clone()
}

pub(super) fn system_font_families() -> Vec<String> {
    let names: BTreeSet<_> = system_fonts()
        .faces()
        .flat_map(|face| face.families.iter().map(|(name, _)| name.clone()))
        .filter(|name| listed(name))
        .collect();
    names.into_iter().collect()
}

/// Scan installed fonts on a background thread so the first text call does
/// not wait for it. A pinned font profile replaces the scan.
pub(super) fn prefetch_system_fonts() {
    if std::env::var_os("ADV_FONT_PROFILE").is_none() && SYSTEM_FONTS.get().is_none() {
        let _ = std::thread::Builder::new()
            .name("font-discovery".into())
            .spawn(|| drop(system_fonts()));
    }
}
struct Discovery {
    database: Arc<Database>,
    profile: Option<Arc<profile::Loaded>>,
}
impl Backend {
    pub(super) fn with_directories(directories: &[PathBuf]) -> Self {
        Self {
            directories: Some(directories.to_vec()),
            ..Self::default()
        }
    }
    pub(super) fn with_profile(path: &std::path::Path) -> Result<Self, TextError> {
        let backend = Self {
            profile_path: Some(path.to_owned()),
            ..Self::default()
        };
        backend.discovery()?;
        Ok(backend)
    }
    fn discovery(&self) -> Result<&Discovery, TextError> {
        self.database
            .get_or_init(|| {
                let path = self.profile_path.clone().or_else(|| {
                    self.directories
                        .is_none()
                        .then(|| std::env::var_os("ADV_FONT_PROFILE").map(PathBuf::from))
                        .flatten()
                });
                let mut db = Database::new();
                let profile = if let Some(path) = path {
                    Some(Arc::new(profile::Loaded::load(&path, &mut db).map_err(
                        |error| format!("font profile {}: {error}", path.display()),
                    )?))
                } else if let Some(directories) = &self.directories {
                    for directory in directories {
                        db.load_fonts_dir(directory);
                    }
                    None
                } else {
                    return Ok(Discovery {
                        database: system_fonts(),
                        profile: None,
                    });
                };
                Ok(Discovery {
                    database: Arc::new(db),
                    profile,
                })
            })
            .as_ref()
            .map_err(|error| TextError::Font(error.clone()))
    }
    pub(super) fn resource_stats(&self) -> (usize, usize) {
        (self.resources.len(), self.resource_bytes)
    }
    pub(super) fn add_resource(
        &mut self,
        bytes: &[u8],
        enumerable: bool,
    ) -> Result<(FontResourceId, u32), TextError> {
        if bytes.is_empty()
            || self.resources.len() >= MAX_FONT_RESOURCES
            || bytes.len() > MAX_FONT_RESOURCE_BYTES.saturating_sub(self.resource_bytes)
        {
            return Err(TextError::Font("font resource budget exceeded".into()));
        }
        let next = self
            .next_resource
            .checked_add(1)
            .ok_or_else(|| TextError::Font("font resource identity exhausted".into()))?;
        // Parse before changing the live database. fontdb owns the input and
        // enumerates TTC faces; skrifa validates every accepted face's tables.
        let source = Source::Binary(Arc::new(bytes.to_vec()));
        let mut probe = Database::new();
        let probed = probe.load_font_source(source.clone());
        if probed.is_empty()
            || probed.iter().any(|&id| {
                probe.with_face_data(id, |data, index| {
                    FontRef::from_index(data, index).is_ok_and(|font| {
                        font.head().is_ok_and(|head| head.units_per_em() != 0)
                            && font.maxp().is_ok()
                            && font.hmtx().is_ok()
                    })
                }) != Some(true)
            })
        {
            return Err(TextError::Font("invalid OpenType font resource".into()));
        }
        self.discovery()?;
        let db = Arc::make_mut(&mut self.database.get_mut().unwrap().as_mut().unwrap().database);
        let faces: Vec<_> = db.load_font_source(source).into_iter().collect();
        let count = faces.len() as u32;
        let id = FontResourceId(next);
        self.resources.insert(
            id,
            Resource {
                faces,
                bytes: bytes.len(),
                enumerable,
            },
        );
        self.next_resource = next;
        self.resource_bytes += bytes.len();
        self.store = Arc::default();
        Ok((id, count))
    }
    pub(super) fn remove_resource(&mut self, id: FontResourceId) -> bool {
        let Some(resource) = self.resources.remove(&id) else {
            return false;
        };
        let db = Arc::make_mut(&mut self.database.get_mut().unwrap().as_mut().unwrap().database);
        for face in resource.faces {
            db.remove_face(face);
        }
        self.resource_bytes -= resource.bytes;
        self.store = Arc::default();
        true
    }
    pub(super) fn font(&self, request: &FontRequest) -> Result<Font, TextError> {
        validate_font(request)?;
        let discovery = self.discovery()?;
        let db = Arc::clone(&discovery.database);
        let profile = discovery.profile.clone();
        let resources: BTreeSet<_> = self
            .resources
            .values()
            .flat_map(|resource| resource.faces.iter().copied())
            .collect();
        if let Some(profile) = &profile {
            let mut guests: Vec<_> = db
                .faces()
                .filter(|face| {
                    resources.contains(&face.id)
                        && request.default.is_none()
                        && family_matches(&db, face, &request.face)
                })
                .collect();
            guests.sort_by_key(|face| {
                (
                    usize::from((face.style != Style::Normal) != request.italic),
                    (i32::from(face.weight.0) - normalized_weight(request)).unsigned_abs(),
                    face.index,
                )
            });
            let mut candidates: Vec<_> = guests.into_iter().map(|face| face.id).collect();
            if let Some(replacements) = profile.candidates(request) {
                candidates.extend_from_slice(replacements);
            }
            let primary = candidates.first().copied().ok_or_else(|| TextError::Font(format!(
                "strict font profile has no guest font or replacement rule for face={:?} default={:?}",
                request.face, request.default
            )))?;
            let face = load_face(&db, &self.store, primary, Some(profile))?;
            return Font::new(
                request,
                db,
                Arc::clone(&self.store),
                candidates,
                face,
                Some(profile.clone()),
            );
        }
        let names = families(request);
        let mut candidates: Vec<_> = db.faces().collect();
        let weight = normalized_weight(request);
        candidates.sort_by_cached_key(|face| {
            let rank = names
                .iter()
                .position(|name| family_matches(&db, face, name))
                .unwrap_or(names.len() + usize::from(!face.monospaced && fixed(request)));
            (
                rank,
                !resources.contains(&face.id),
                usize::from((face.style != Style::Normal) != request.italic),
                (i32::from(face.weight.0) - weight).unsigned_abs(),
            )
        });
        let candidates: Vec<_> = candidates.into_iter().map(|face| face.id).collect();
        let primary = candidates.first().copied().ok_or_else(|| {
            TextError::Font("no OpenType fonts found; install fonts or set ADV_FONT_DIR".into())
        })?;
        let face = load_face(&db, &self.store, primary, None)?;
        Font::new(request, db, Arc::clone(&self.store), candidates, face, None)
    }
    pub(super) fn enumerate(
        &self,
        face: Option<&str>,
        mode: FontEnumeration,
    ) -> Result<Vec<EnumeratedFont>, TextError> {
        if face.is_some_and(|name| name.contains('\0') || name.encode_utf16().count() > 32767) {
            return Err(TextError::Font("invalid enumeration face name".into()));
        }
        let db = &self.discovery()?.database;
        if db.faces().next().is_none() {
            return Err(TextError::Font("no OpenType fonts found".into()));
        }
        let mut output = Vec::new();
        // A full enumeration reports each family once, preferring its regular
        // style, as GDI does; extended enumeration does so per character set.
        let mut seen = BTreeMap::new();
        let hidden: BTreeSet<_> = self
            .resources
            .values()
            .filter(|resource| !resource.enumerable)
            .flat_map(|resource| resource.faces.iter().copied())
            .collect();
        let named = face.filter(|name| !name.is_empty());
        let present = |name: &str| db.faces().any(|info| family_matches(db, info, name));
        // A family the host lacks enumerates as the face that draws it, under
        // the requested name, so a presence check agrees with drawing.
        let substitute = |name: &str| {
            let mut request = enumeration_request(db.faces().next()?, name.to_owned());
            (request.weight, request.italic) = (400, false);
            (request.pitch, request.family) = (Pitch::Default, Family::Default);
            if let FontEnumeration::FamiliesEx(encoding) = mode {
                request.encoding = encoding;
            }
            let id = self.font(&request).ok()?.primary.info.id;
            Some((db.face(id)?, Some(name.to_owned())))
        };
        let faces: Vec<_> = match named {
            Some(name) if !present(name) => substitute(name).into_iter().collect(),
            Some(name) => db
                .faces()
                .filter(|info| family_matches(db, info, name))
                .map(|info| (info, None))
                .collect(),
            // Windows always ships its standard families, so a full enumeration
            // lists the missing ones the same way.
            None => db
                .faces()
                .map(|info| (info, None))
                .chain(
                    WINDOWS_FAMILIES
                        .iter()
                        .filter(|family| !family.names().any(present))
                        .filter_map(|family| substitute(family.name(self.language))),
                )
                .collect(),
        };
        for (info, forced) in faces
            .into_iter()
            .filter(|(info, _)| !hidden.contains(&info.id))
        {
            if output.len() >= 8192 {
                return Err(TextError::Font("font enumeration budget exceeded".into()));
            }
            let data = self
                .store
                .lock()
                .map_err(|_| TextError::Font("font cache lock poisoned".into()))
                .and_then(|mut store| source_bytes(&mut store, &info.source));
            let result = data
                .ok()
                .map(|data| -> Result<_, TextError> {
                    let (bytes, index) = (data.as_ref().as_ref(), info.index);
                    let font = FontRef::from_index(bytes, index).map_err(font_error)?;
                    // Report the legacy family GDI uses: the requested spelling
                    // when it names one, else the guest UI language, else English.
                    let requested = forced.clone().or_else(|| {
                        let name = named?;
                        legacy_families(&font)
                            .find(|family| family.to_lowercase() == name.to_lowercase())
                    });
                    let localized = self.language.and_then(|tag| {
                        font.localized_strings(skrifa::string::StringId::FAMILY_NAME)
                            .find(|name| name.language() == Some(tag))
                            .map(|name| name.to_string())
                    });
                    let name = requested
                        .or(localized)
                        .or_else(|| {
                            font.localized_strings(skrifa::string::StringId::FAMILY_NAME)
                                .english_or_first()
                                .map(|name| name.to_string())
                        })
                        .or_else(|| info.families.first().map(|family| family.0.clone()))
                        .unwrap_or_default();
                    let request = enumeration_request(info, name);
                    let signature = matches!(mode, FontEnumeration::FamiliesEx(_)).then(|| {
                        font.os2()
                            .ok()
                            .map(|os2| FontSignature {
                                unicode: [
                                    os2.ul_unicode_range_1(),
                                    os2.ul_unicode_range_2(),
                                    os2.ul_unicode_range_3(),
                                    os2.ul_unicode_range_4(),
                                ],
                                codepages: [
                                    os2.ul_code_page_range_1().unwrap_or(0),
                                    os2.ul_code_page_range_2().unwrap_or(0),
                                ],
                            })
                            .unwrap_or_default()
                    });
                    let (em, xscale, metrics) = font_metrics(&font, &request)?;
                    let _ = (em, xscale);
                    let unscaled = font.metrics(Size::unscaled(), LocationRef::default());
                    let (asc, desc) = cell_units(&font);
                    let charmap = font.charmap();
                    let first = charmap
                        .mappings()
                        .next()
                        .map(|(c, _)| c.min(65535) as u16)
                        .unwrap_or(0);
                    let last = charmap
                        .mappings()
                        .last()
                        .map(|(c, _)| c.min(65535) as u16)
                        .unwrap_or(0);
                    let string = |id| {
                        font.localized_strings(id)
                            .english_or_first()
                            .map(|s| s.to_string())
                            .unwrap_or_default()
                    };
                    Ok(EnumeratedFont {
                        font: request,
                        metrics,
                        characters: [first, last, b'?' as u16, b' ' as u16],
                        kind: FontKind::TrueType,
                        device: false,
                        full_name: string(skrifa::string::StringId::FULL_NAME),
                        style: string(skrifa::string::StringId::SUBFAMILY_NAME),
                        script: String::new(),
                        signature,
                        design: (mode != FontEnumeration::Faces).then_some(FontDesign {
                            italic: info.style != Style::Normal,
                            bold: info.weight.0 >= 700,
                            regular: info.style == Style::Normal && info.weight.0 < 700,
                            nonnegative_spacing: true,
                            postscript_outline: font.cff().is_ok() || font.cff2().is_ok(),
                            truetype_outline: font.glyf().is_ok(),
                            multiple_master: !font.axes().is_empty(),
                            type1: false,
                            digital_signature: font.dsig().is_ok(),
                            em_units: u32::from(unscaled.units_per_em),
                            cell_height: (asc + desc).round() as u32,
                            average_width: unscaled.average_width.unwrap_or(0.0).max(0.0).round()
                                as u32,
                        }),
                        substitute: forced.is_some(),
                    })
                })
                .ok_or_else(|| TextError::Font("font discovery file became unreadable".into()));
            // Windows leaves out fonts it cannot use; one bad file must not
            // end enumeration of the rest.
            let Ok(Ok(result)) = result else {
                continue;
            };
            if named.is_none() && !listed(&result.font.face) {
                continue;
            }
            let fonts = match mode {
                FontEnumeration::FamiliesEx(encoding) => enumeration::repertoires(result, encoding),
                _ => vec![result],
            };
            for font in fonts {
                if named.is_none() {
                    let key = (font.font.face.to_lowercase(), font.font.encoding);
                    let regular = font.design.as_ref().is_none_or(|design| design.regular);
                    if let Some(&at) = seen.get(&key) {
                        let current: &EnumeratedFont = &output[at];
                        if regular && !current.design.as_ref().is_none_or(|d| d.regular) {
                            output[at] = font;
                        }
                        continue;
                    }
                    seen.insert(key, output.len());
                }
                if output.len() >= 8192 {
                    return Err(TextError::Font("font enumeration budget exceeded".into()));
                }
                output.push(font);
            }
        }
        Ok(output)
    }
}

/// A family every Windows installation ships, with its name in the UI
/// language that localizes it.
struct WindowsFamily(&'static str, Option<(&'static str, &'static str)>);
impl WindowsFamily {
    fn name(&self, language: Option<&str>) -> &'static str {
        match self.1 {
            Some((tag, local)) if language == Some(tag) => local,
            _ => self.0,
        }
    }
    fn names(&self) -> impl Iterator<Item = &'static str> {
        let legacy = match self.0 {
            "MS Gothic" => Some("MS ゴシック"),
            "MS PGothic" => Some("MS Pゴシック"),
            "MS Mincho" => Some("MS 明朝"),
            "MS PMincho" => Some("MS P明朝"),
            _ => None,
        };
        std::iter::once(self.0)
            .chain(self.1.map(|(_, local)| local))
            .chain(legacy)
    }
}
const WINDOWS_FAMILIES: &[WindowsFamily] = &[
    WindowsFamily("Arial", None),
    WindowsFamily("Courier New", None),
    WindowsFamily("Times New Roman", None),
    WindowsFamily("Tahoma", None),
    WindowsFamily("Verdana", None),
    WindowsFamily("Microsoft Sans Serif", None),
    WindowsFamily("Segoe UI", None),
    WindowsFamily("MS Gothic", Some(("ja-JP", "ＭＳ ゴシック"))),
    WindowsFamily("MS PGothic", Some(("ja-JP", "ＭＳ Ｐゴシック"))),
    WindowsFamily("MS UI Gothic", None),
    WindowsFamily("MS Mincho", Some(("ja-JP", "ＭＳ 明朝"))),
    WindowsFamily("MS PMincho", Some(("ja-JP", "ＭＳ Ｐ明朝"))),
    WindowsFamily("Meiryo", Some(("ja-JP", "メイリオ"))),
    WindowsFamily("Meiryo UI", None),
    WindowsFamily("Yu Gothic", Some(("ja-JP", "游ゴシック"))),
    WindowsFamily("Yu Mincho", Some(("ja-JP", "游明朝"))),
    WindowsFamily("SimSun", Some(("zh-Hans", "宋体"))),
    WindowsFamily("NSimSun", Some(("zh-Hans", "新宋体"))),
    WindowsFamily("SimHei", Some(("zh-Hans", "黑体"))),
    WindowsFamily("Microsoft YaHei", Some(("zh-Hans", "微软雅黑"))),
    WindowsFamily("MingLiU", Some(("zh-Hant", "細明體"))),
    WindowsFamily("PMingLiU", Some(("zh-Hant", "新細明體"))),
    WindowsFamily("Gulim", Some(("ko-KR", "굴림"))),
    WindowsFamily("Batang", Some(("ko-KR", "바탕"))),
];

/// Whether a full enumeration lists a family. A leading period marks a
/// host-private family that host font menus also leave out.
fn listed(family: &str) -> bool {
    !family.is_empty() && !family.starts_with('.')
}

struct Face {
    data: FontBytes,
    index: u32,
    info: FaceInfo,
    source: String,
    sha256: String,
}
impl Face {
    fn font(&self) -> Result<FontRef<'_>, TextError> {
        FontRef::from_index(self.data.as_ref().as_ref(), self.index).map_err(font_error)
    }
}
fn font_error(error: impl std::fmt::Display) -> TextError {
    TextError::Font(format!("OpenType font: {error}"))
}
fn diagnostic(
    request: &FontRequest,
    face: &Face,
    role: &str,
    em: f32,
    cell: i32,
    profile: Option<&profile::Loaded>,
) {
    if std::env::var_os("ADV_TEXT_DIAGNOSTICS").is_none() {
        return;
    }
    let family = face
        .info
        .families
        .first()
        .map(|name| name.0.as_str())
        .unwrap_or("");
    let source = &face.source;
    let sha256 = &face.sha256;
    let profile_sha256 = profile
        .map(|profile| profile.sha256.as_str())
        .unwrap_or("uncontrolled");
    let font_bytes = face.data.as_ref().as_ref().len();
    eprintln!(
        "adv-text {role}: requested={:?} default={:?} resolved={family:?} source={source:?} face_index={} sha256={sha256} font_bytes={font_bytes} height={} width={} weight={} italic={} em={em:.3} cell={cell} backend=portable/skrifa+zeno profile_sha256={profile_sha256}",
        request.face,
        request.default,
        face.index,
        request.height,
        request.width,
        request.weight,
        request.italic
    );
}
/// A face source's bytes. Files come through the store's bounded cache, so a
/// collection's faces and repeated lookups read the file once.
fn source_bytes(store: &mut FaceStore, source: &Source) -> Result<FontBytes, TextError> {
    let path = match source {
        Source::Binary(bytes) => return Ok(Arc::clone(bytes)),
        Source::File(path) => path,
    };
    if let Some(bytes) = store.files.get(path) {
        return Ok(Arc::clone(bytes));
    }
    let length = std::fs::metadata(path).map_err(font_error)?.len();
    if length > MAX_SYSTEM_FONT_BYTES as u64 {
        return Err(TextError::Font("system font exceeds data limit".into()));
    }
    let bytes: FontBytes = Arc::new(std::fs::read(path).map_err(font_error)?);
    let length = bytes.as_ref().as_ref().len();
    if length > MAX_SYSTEM_FONT_BYTES {
        return Err(TextError::Font("system font exceeds data limit".into()));
    }
    // Keep a bounded number of shared collection files. Font owners
    // independently retain data after this lookup cache is cleared.
    if store.files.len() >= 32 || store.file_bytes + length > MAX_SYSTEM_FONT_BYTES {
        store.faces.clear();
        store.files.clear();
        store.digests.clear();
        store.file_bytes = 0;
    }
    store.file_bytes += length;
    store.files.insert(path.clone(), Arc::clone(&bytes));
    Ok(bytes)
}

fn load_face(
    db: &Database,
    store: &Mutex<FaceStore>,
    id: ID,
    profile: Option<&profile::Loaded>,
) -> Result<Arc<Face>, TextError> {
    let mut store = store
        .lock()
        .map_err(|_| TextError::Font("font cache lock poisoned".into()))?;
    if let Some(face) = store.faces.get(&id) {
        return Ok(Arc::clone(face));
    }
    let info = db
        .face(id)
        .ok_or_else(|| TextError::Font("font face no longer registered".into()))?
        .clone();
    let data = source_bytes(&mut store, &info.source)?;
    let source = match &info.source {
        Source::Binary(_) => profile
            .and_then(|profile| profile.paths.get(&id))
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "guest resource".to_owned()),
        Source::File(path) => path.display().to_string(),
    };
    // A TTC's faces share one buffer; hash the immutable bytes once. Retain
    // its Arc with the digest so an allocation address cannot be reused here.
    let identity = Arc::as_ptr(&data) as *const () as usize;
    let sha256 = if let Some((_, digest)) = store.digests.get(&identity) {
        digest.clone()
    } else {
        let digest = profile::hash(data.as_ref().as_ref());
        if store.digests.len() >= 96 {
            store.digests.clear();
        }
        store
            .digests
            .insert(identity, (Arc::clone(&data), digest.clone()));
        digest
    };
    let face = Arc::new(Face {
        data,
        index: info.index,
        info,
        source,
        sha256,
    });
    face.font()?;
    if store.faces.len() >= 128 {
        store.faces.clear();
    }
    store.faces.insert(id, Arc::clone(&face));
    Ok(face)
}

fn validate_font(request: &FontRequest) -> Result<(), TextError> {
    if request.height.unsigned_abs() > 16384 || request.width.unsigned_abs() > 16384 {
        return Err(TextError::Font("invalid font dimensions".into()));
    }
    if request.baseline_angle != 0 || request.glyph_angle != 0 || request.face.starts_with('@') {
        return Err(TextError::Font(
            "portable rotated or vertical fonts are unsupported".into(),
        ));
    }
    if matches!(
        request.precision,
        Precision::Raster | Precision::Device | Precision::Reserved(_)
    ) {
        return Err(TextError::Font(
            "portable raster/device font precision is unsupported".into(),
        ));
    }
    Ok(())
}
fn family_matches(db: &Database, face: &FaceInfo, name: &str) -> bool {
    let name = name.to_lowercase();
    face.families
        .iter()
        .any(|family| family.0.to_lowercase() == name)
        || face.post_script_name.to_lowercase() == name
        // fontdb keeps only the typographic family when one exists; GDI names
        // faces by their legacy family. Only faces whose typographic family
        // prefixes the request are read again.
        || (face
            .families
            .iter()
            .any(|family| !family.0.is_empty() && name.starts_with(&family.0.to_lowercase()))
            && db
                .with_face_data(face.id, |data, index| {
                    FontRef::from_index(data, index).is_ok_and(|font| {
                        legacy_families(&font).any(|family| family.to_lowercase() == name)
                    })
                })
                .unwrap_or(false))
}
/// The name ID 1 family in every language the font provides.
fn legacy_families<'a>(font: &FontRef<'a>) -> impl Iterator<Item = String> + 'a {
    font.localized_strings(skrifa::string::StringId::FAMILY_NAME)
        .map(|name| name.to_string())
}
fn fixed(request: &FontRequest) -> bool {
    matches!(request.pitch, Pitch::Fixed)
        || matches!(request.family, Family::Monospace)
        || matches!(
            request.default,
            Some(DefaultFont::Console | DefaultFont::Fixed | DefaultFont::SystemFixed)
        )
        || matches!(
            request.face.to_lowercase().as_str(),
            "ms gothic" | "ｍｓ ゴシック" | "ms mincho" | "ｍｓ 明朝"
        )
}
fn normalized_weight(request: &FontRequest) -> i32 {
    if request.weight == 0 {
        400
    } else {
        request.weight.clamp(1, 1000)
    }
}
fn macos_cjk_families(region: &str) -> &'static [&'static str] {
    match region {
        "SC" => &[
            "PingFang SC",
            "Hiragino Sans GB",
            "Heiti SC",
            "Songti SC",
            "STHeiti",
        ],
        "TC" => &["PingFang TC", "Songti TC"],
        "KR" => &["Apple SD Gothic Neo", "AppleMyungjo"],
        _ => &[],
    }
}

fn families(request: &FontRequest) -> Vec<String> {
    let name = request.face.to_lowercase();
    let named_sans = matches!(
        name.as_str(),
        "simhei"
            | "黑体"
            | "黑體"
            | "微软雅黑"
            | "微軟正黑體"
            | "dotum"
            | "gulim"
            | "malgun gothic"
            | "돋움"
            | "굴림"
            | "ms gothic"
            | "ms pgothic"
            | "ｍｓ ゴシック"
            | "ｍｓ ｐゴシック"
    );
    let serif = !named_sans
        && (request.family == Family::Serif
            || name.contains("mincho")
            || name.contains("明朝")
            || name.contains("times")
            || (name.contains("serif") && !name.contains("sans"))
            || matches!(
                name.as_str(),
                "simsun"
                    | "nsimsun"
                    | "simsun-extb"
                    | "宋体"
                    | "新宋体"
                    | "宋體"
                    | "mingliu"
                    | "pmingliu"
                    | "細明體"
                    | "新細明體"
                    | "batang"
                    | "바탕"
            ));
    // The repertoire chooses the regional CJK glyph forms before name-based
    // heuristics. Explicit installed/guest family names remain the first choice.
    let region = match request.encoding {
        Encoding::Japanese => Some("JP"),
        Encoding::SimplifiedChinese => Some("SC"),
        Encoding::TraditionalChinese => Some("TC"),
        Encoding::Korean | Encoding::Johab => Some("KR"),
        _ if matches!(
            name.as_str(),
            "simsun"
                | "nsimsun"
                | "simsun-extb"
                | "simhei"
                | "宋体"
                | "新宋体"
                | "黑体"
                | "微软雅黑"
        ) =>
        {
            Some("SC")
        }
        _ if matches!(
            name.as_str(),
            "mingliu" | "pmingliu" | "細明體" | "新細明體" | "微軟正黑體"
        ) =>
        {
            Some("TC")
        }
        _ if matches!(
            name.as_str(),
            "batang" | "dotum" | "gulim" | "malgun gothic" | "바탕" | "돋움" | "굴림"
        ) =>
        {
            Some("KR")
        }
        _ if name.contains("gothic")
            || name.contains("mincho")
            || name.contains("ゴシック")
            || name.contains("明朝") =>
        {
            Some("JP")
        }
        _ => None,
    };
    let japanese = region == Some("JP");
    let mut names = Vec::new();
    if request.default.is_none() && !request.face.is_empty() {
        names.push(request.face.clone());
    }
    if let Some(region) = region.filter(|&region| region != "JP") {
        if !serif && fixed(request) {
            names.push(format!("Noto Sans Mono CJK {region}"));
        }
        names.push(format!(
            "Noto {} CJK {region}",
            if serif { "Serif" } else { "Sans" }
        ));
        if serif {
            names.push(format!("Noto Sans CJK {region}"));
        }
        names.push(format!(
            "Noto {} {region}",
            if serif { "Serif" } else { "Sans" }
        ));
        names.extend(
            macos_cjk_families(region)
                .iter()
                .map(|name| (*name).to_owned()),
        );
    }
    if japanese {
        names.extend(
            if serif {
                &[
                    "Noto Serif CJK JP",
                    "Noto Serif JP",
                    "Hiragino Mincho ProN",
                    "IPAMincho",
                    "TakaoMincho",
                    "Noto Sans CJK JP",
                    "Droid Serif",
                ][..]
            } else if fixed(request) {
                &[
                    "Noto Sans Mono CJK JP",
                    "Noto Sans CJK JP",
                    "Noto Sans JP",
                    "Hiragino Kaku Gothic ProN",
                    "IPAGothic",
                    "TakaoGothic",
                    "Droid Sans Japanese",
                    "Droid Sans Fallback",
                ][..]
            } else {
                &[
                    "Noto Sans CJK JP",
                    "Noto Sans JP",
                    "Hiragino Kaku Gothic ProN",
                    "IPAGothic",
                    "TakaoGothic",
                    "Droid Sans Japanese",
                    "Droid Sans Fallback",
                ][..]
            }
            .iter()
            .map(|s| (*s).to_owned()),
        );
    }
    names.extend(
        if fixed(request) && !japanese {
            &[
                "Noto Sans Mono",
                "DejaVu Sans Mono",
                "Liberation Mono",
                "Menlo",
                "Courier New",
            ][..]
        } else if serif {
            &[
                "Noto Serif",
                "Liberation Serif",
                "DejaVu Serif",
                "Times New Roman",
                "Times",
            ][..]
        } else {
            &[
                "Roboto",
                "Noto Sans",
                "Liberation Sans",
                "DejaVu Sans",
                "Helvetica",
                "Arial",
            ][..]
        }
        .iter()
        .map(|s| (*s).to_owned()),
    );
    // Keep same-region coverage first even if the intended family is missing.
    let regions = match region {
        Some("SC") => ["SC", "TC", "JP", "KR"],
        Some("TC") => ["TC", "SC", "JP", "KR"],
        Some("KR") => ["KR", "JP", "SC", "TC"],
        _ => ["JP", "TC", "SC", "KR"],
    };
    for region in regions {
        if serif {
            names.push(format!("Noto Serif CJK {region}"));
        }
        names.push(format!("Noto Sans CJK {region}"));
        names.extend(
            macos_cjk_families(region)
                .iter()
                .map(|name| (*name).to_owned()),
        );
    }
    names.push("Droid Sans Fallback".to_owned());
    names
}
fn location(font: &FontRef<'_>, request: &FontRequest) -> Location {
    font.axes().location([
        (
            "wght",
            if request.weight == 0 {
                400.0
            } else {
                request.weight as f32
            },
        ),
        ("ital", u8::from(request.italic) as f32),
        ("slnt", if request.italic { -12.0 } else { 0.0 }),
    ])
}
fn cell_units(font: &FontRef<'_>) -> (f32, f32) {
    if let Ok(os2) = font.os2() {
        let (asc, desc) = (os2.us_win_ascent() as f32, os2.us_win_descent() as f32);
        if asc + desc > 0.0 {
            return (asc, desc);
        }
    }
    let m = font.metrics(Size::unscaled(), LocationRef::default());
    (m.ascent.max(0.0), (-m.descent).max(0.0))
}
fn font_metrics(
    font: &FontRef<'_>,
    request: &FontRequest,
) -> Result<(f32, f32, TextMetrics), TextError> {
    let loc = location(font, request);
    let units = font.metrics(Size::unscaled(), &loc);
    let (asc, desc) = cell_units(font);
    if units.units_per_em == 0 || asc + desc <= 0.0 {
        return Err(TextError::Font("font has invalid em/cell metrics".into()));
    }
    let em = if request.height < 0 {
        -request.height as f32
    } else if request.height > 0 {
        request.height as f32 * f32::from(units.units_per_em) / (asc + desc)
    } else {
        16.0
    };
    let scale = em / f32::from(units.units_per_em);
    let m = font.metrics(Size::new(em), &loc);
    let average = if fixed(request) {
        em * 0.5
    } else {
        m.average_width.unwrap_or(em * 0.5).abs().max(1.0)
    };
    let xscale = if request.width != 0 {
        request.width.unsigned_abs() as f32 / average
    } else {
        1.0
    };
    let height = if request.height > 0 {
        request.height
    } else {
        ((asc + desc) * scale).round() as i32
    };
    let ascent = (asc * scale).round() as i32;
    let metrics = TextMetrics {
        height,
        ascent,
        descent: height - ascent,
        internal_leading: (height - em.round() as i32).max(0),
        external_leading: m.leading.max(0.0).round() as i32,
        average_width: (average * xscale).round().max(1.0) as i32,
        maximum_width: (m.max_width.unwrap_or(em).max(em) * xscale).ceil() as i32,
        weight: if request.weight == 0 {
            400
        } else {
            request.weight
        },
        overhang: 0,
        aspect_x: 96,
        aspect_y: 96,
        first_character: 0,
        last_character: 255,
        default_character: b'?',
        break_character: b' ',
        italic: request.italic,
        underline: request.underline,
        strikeout: request.strikeout,
        variable_pitch: !fixed(request) && !m.is_monospace,
        vector: true,
        outline: true,
        device: false,
        family: request.family,
        encoding: request.encoding,
    };
    Ok((em, xscale, metrics))
}
fn enumeration_request(info: &FaceInfo, face: String) -> FontRequest {
    FontRequest {
        default: None,
        face,
        height: -16,
        width: 0,
        baseline_angle: 0,
        glyph_angle: 0,
        weight: i32::from(info.weight.0),
        italic: info.style != Style::Normal,
        underline: false,
        strikeout: false,
        encoding: Encoding::Automatic,
        precision: Precision::TrueType,
        clipping: Clipping {
            precision: ClipPrecision::Default,
            transform_rotation: false,
            force_outline: false,
            disable_association: false,
            embedded: false,
        },
        quality: Quality::Antialiased,
        pitch: if info.monospaced {
            Pitch::Fixed
        } else {
            Pitch::Variable
        },
        family: if info.monospaced {
            Family::Monospace
        } else {
            Family::Default
        },
    }
}

pub(super) struct Font {
    request: FontRequest,
    database: Arc<Database>,
    store: Arc<Mutex<FaceStore>>,
    candidates: Vec<ID>,
    primary: Arc<Face>,
    profile: Option<Arc<profile::Loaded>>,
    resolved: Mutex<BTreeMap<u32, (Arc<Face>, GlyphId)>>,
    hints: Mutex<BTreeMap<(ID, bool), HintingInstance>>,
    em: f32,
    xscale: f32,
    metrics: TextMetrics,
}
impl Font {
    fn new(
        request: &FontRequest,
        database: Arc<Database>,
        store: Arc<Mutex<FaceStore>>,
        candidates: Vec<ID>,
        primary: Arc<Face>,
        profile: Option<Arc<profile::Loaded>>,
    ) -> Result<Self, TextError> {
        let (em, xscale, metrics) = font_metrics(&primary.font()?, request)?;
        diagnostic(
            request,
            &primary,
            "match",
            em,
            metrics.height,
            profile.as_deref(),
        );
        Ok(Self {
            request: request.clone(),
            database,
            store,
            candidates,
            primary,
            profile,
            resolved: Mutex::default(),
            hints: Mutex::default(),
            em,
            xscale,
            metrics,
        })
    }
    pub(super) fn metrics(&self) -> TextMetrics {
        self.metrics.clone()
    }
    /// GetTextMetricsW's UTF-16 first, last, default and break characters of
    /// the primary face, by the rules native GDI shows (recorded in Wine's
    /// gdi32 font tests). First and last span the Unicode cmap below U+FFFF;
    /// break follows OS/2 usFirstCharIndex and default is the code before it.
    /// Symbol cmaps report the F000 private-use page.
    pub(super) fn unicode_metrics(&self) -> Result<(TextMetrics, [u16; 4]), TextError> {
        let font = self.primary.font()?;
        let charmap = font.charmap();
        let mut codes = charmap
            .mappings()
            .map(|(code, _)| code)
            .filter(|&code| code <= 0xfffe);
        let first = codes
            .next()
            .ok_or_else(|| TextError::Font("font has no Unicode character map".into()))?;
        let last = codes.last().unwrap_or(first);
        let characters = if charmap.is_symbol() || (0xf000..0xf100).contains(&first) {
            [0, 0xf0ff, 0x1f, 0x20]
        } else {
            let os2_first = font.os2().map_or(0x20, |os2| os2.us_first_char_index());
            let break_character = match os2_first {
                0 | 1 => os2_first + 2,
                0x100.. => 0x20,
                code => code,
            };
            [
                first as u16,
                last as u16,
                break_character - 1,
                break_character,
            ]
        };
        Ok((self.metrics(), characters))
    }
    /// GDI's outline metrics of the primary face: typographic values from
    /// OS/2, Mac values from hhea, the box and minimum size from head and
    /// the underline and italic angle from post, scaled to this font's em
    /// as GDI scales them. Names are family, full (ID 4), subfamily (ID 2)
    /// and unique (ID 3).
    pub(super) fn outline_metrics(&self) -> Result<OutlineMetrics, TextError> {
        let font = self.primary.font()?;
        let (os2, hhea, head) = (
            font.os2().map_err(font_error)?,
            font.hhea().map_err(font_error)?,
            font.head().map_err(font_error)?,
        );
        let units = f32::from(head.units_per_em());
        if units == 0.0 {
            return Err(TextError::Font("font has no em square".into()));
        }
        let y = |value: i16| (f32::from(value) * self.em / units).round() as i32;
        let x = |value: i16| (f32::from(value) * self.em * self.xscale / units).round() as i32;
        let (italic_angle, underline_size, underline_position) = match font.post() {
            Ok(post) => (
                (post.italic_angle().to_f32() * 10.0).round() as i32,
                y(post.underline_thickness().to_i16()),
                y(post.underline_position().to_i16()),
            ),
            Err(_) => (0, y(os2.y_strikeout_size()), -y(os2.y_strikeout_size())),
        };
        let mut panose = [0; 10];
        let bytes = os2.panose_10();
        panose[..bytes.len().min(10)].copy_from_slice(&bytes[..bytes.len().min(10)]);
        // Simulated italic and bold, and underline and strikeout, show in
        // the selection flags; only the embedding bits of fsType remain.
        let mut selection = u32::from(os2.fs_selection().bits());
        if self.request.italic {
            selection |= 1;
        }
        if self.request.underline {
            selection |= 1 << 1;
        }
        if self.request.strikeout {
            selection |= 1 << 4;
        }
        if self.request.weight >= 600 {
            selection |= 1 << 5;
        }
        let name = |id| {
            font.localized_strings(id)
                .english_or_first()
                .map(|name| name.to_string())
                .unwrap_or_default()
        };
        let family = self
            .primary
            .info
            .families
            .first()
            .map(|name| name.0.clone())
            .unwrap_or_else(|| name(skrifa::string::StringId::FAMILY_NAME));
        let point = |px: i16, py: i16| Point { x: x(px), y: y(py) };
        Ok(OutlineMetrics {
            text: self.metrics.clone(),
            panose,
            selection,
            embedding: u32::from(os2.fs_type() & 0x30e),
            caret_rise: i32::from(hhea.caret_slope_rise()),
            caret_run: i32::from(hhea.caret_slope_run()),
            italic_angle,
            em_units: u32::from(head.units_per_em()),
            ascent: y(os2.s_typo_ascender()),
            descent: y(os2.s_typo_descender()),
            line_gap: y(os2.s_typo_line_gap()).max(0) as u32,
            cap_height: y(os2.s_cap_height().unwrap_or(0)).max(0) as u32,
            x_height: y(os2.sx_height().unwrap_or(0)).max(0) as u32,
            bounds: TextBox {
                left: x(head.x_min()),
                top: y(head.y_max()),
                right: x(head.x_max()),
                bottom: y(head.y_min()),
            },
            mac_ascent: y(hhea.ascender().to_i16()),
            mac_descent: y(hhea.descender().to_i16()),
            mac_line_gap: y(hhea.line_gap().to_i16()).max(0) as u32,
            minimum_ppem: u32::from(head.lowest_rec_ppem()),
            subscript_size: point(os2.y_subscript_x_size(), os2.y_subscript_y_size()),
            subscript_offset: point(os2.y_subscript_x_offset(), os2.y_subscript_y_offset()),
            superscript_size: point(os2.y_superscript_x_size(), os2.y_superscript_y_size()),
            superscript_offset: point(os2.y_superscript_x_offset(), os2.y_superscript_y_offset()),
            strikeout_size: y(os2.y_strikeout_size()).max(0) as u32,
            strikeout_position: y(os2.y_strikeout_position()),
            underline_size,
            underline_position,
            names: [
                family,
                name(skrifa::string::StringId::FULL_NAME),
                name(skrifa::string::StringId::SUBFAMILY_NAME),
                name(skrifa::string::StringId::UNIQUE_ID),
            ],
        })
    }
    pub(super) fn logical_font(&self) -> FontRequest {
        let mut logical = self.request.clone();
        if logical.default.is_some() {
            // Describe the already selected default face and cell size. Font
            // discovery/profile selection has one owner in this provider.
            logical.default = None;
            logical.face = self
                .primary
                .info
                .families
                .first()
                .map(|name| name.0.clone())
                .unwrap_or_default();
            logical.height = self.metrics.height;
            logical.weight = self.metrics.weight;
            logical.pitch = if self.primary.info.monospaced {
                Pitch::Fixed
            } else {
                Pitch::Variable
            };
            logical.family = self.metrics.family;
        }
        logical
    }
    pub(super) fn resolved_character(&self, code: u32) -> Result<FontResolution, TextError> {
        let (face, glyph) = self.resolve(code)?;
        let mapped = face
            .font()?
            .charmap()
            .map(code)
            .filter(|glyph| glyph.to_u32() != 0);
        Ok(FontResolution {
            family: face
                .info
                .families
                .first()
                .map(|name| name.0.clone())
                .unwrap_or_default(),
            source: face.source.clone(),
            face_index: face.index,
            sha256: face.sha256.clone(),
            font_bytes: face.data.as_ref().as_ref().len(),
            glyph_index: glyph.to_u32(),
            default_glyph: code == 0 || mapped != Some(glyph),
            em_pixels: self.em,
            cell_pixels: self.metrics.height,
            horizontal_scale: self.xscale,
            profile_sha256: self.profile.as_ref().map(|profile| profile.sha256.clone()),
            backend: "portable/skrifa+zeno",
        })
    }
    fn default_glyph(&self) -> Result<(Arc<Face>, GlyphId), TextError> {
        let font = self.primary.font()?;
        let glyph = font
            .os2()
            .ok()
            .and_then(|os2| os2.us_default_char())
            .filter(|&code| code != 0)
            .and_then(|code| font.charmap().map(u32::from(code)))
            .filter(|glyph| glyph.to_u32() != 0)
            .unwrap_or(GlyphId::new(0));
        Ok((Arc::clone(&self.primary), glyph))
    }
    fn resolve(&self, code: u32) -> Result<(Arc<Face>, GlyphId), TextError> {
        // Counted runs retain NUL, including the selected face's default
        // glyph advance and outline rather than terminating the run.
        if code == 0 {
            return self.default_glyph();
        }
        let mut resolved = self
            .resolved
            .lock()
            .map_err(|_| TextError::Glyph("font resolution lock poisoned".into()))?;
        if let Some(found) = resolved.get(&code) {
            return Ok(found.clone());
        }
        if char::from_u32(code).is_none() {
            return Err(TextError::Glyph("invalid Unicode scalar".into()));
        }
        let mut found = None;
        for &id in &self.candidates {
            let face = load_face(&self.database, &self.store, id, self.profile.as_deref())?;
            if let Some(glyph) = face
                .font()?
                .charmap()
                .map(code)
                .filter(|glyph| glyph.to_u32() != 0)
            {
                found = Some((face, glyph));
                break;
            }
            if self.request.clipping.disable_association {
                break;
            }
        }
        // Missing coverage is a supported default-glyph result, as in GDI.
        // Unsupported font formats and malformed scalar inputs remain errors.
        let found = match found {
            Some(found) => found,
            None => {
                if std::env::var_os("ADV_TEXT_DIAGNOSTICS").is_some() {
                    eprintln!(
                        "adv-text missing coverage: requested={:?} code=U+{code:04X} selected_default_glyph=true",
                        self.request.face
                    );
                }
                self.default_glyph()?
            }
        };
        if found.0.info.id != self.primary.info.id
            && !resolved
                .values()
                .any(|(face, _)| face.info.id == found.0.info.id)
        {
            diagnostic(
                &self.request,
                &found.0,
                "fallback",
                self.em,
                self.metrics.height,
                self.profile.as_deref(),
            );
        }
        if resolved.len() >= 4096 {
            resolved.clear();
        }
        resolved.insert(code, found.clone());
        Ok(found)
    }
    fn advance(&self, face: &Face, glyph: GlyphId, code: Option<u32>) -> Result<f32, TextError> {
        let font = face.font()?;
        if fixed(&self.request)
            && let Some(code) = code
            && code != 0
            && font.charmap().map(code) == Some(glyph)
        {
            let full = matches!(code, 0x1100..=0x11ff | 0x2e80..=0xa4cf | 0xac00..=0xd7af | 0xf900..=0xfaff | 0xfe10..=0xfe6f | 0xff01..=0xff60 | 0xffe0..=0xffe6 | 0x20000..=0x3ffff);
            return Ok((self.em * if full { 1.0 } else { 0.5 } * self.xscale).round());
        }
        let loc = location(&font, &self.request);
        font.glyph_metrics(Size::new(self.em), &loc)
            .advance_width(glyph)
            .map(|n| (n * self.xscale).round())
            .ok_or_else(|| TextError::Glyph("invalid glyph advance".into()))
    }
    pub(super) fn run(&self, input: TextInput<'_>) -> Result<Vec<u32>, TextError> {
        let decoded = decode(input, self.request.encoding)?;
        // These scripts require shaping/bidi. Report that capability boundary
        // before any paint writes rather than silently drawing isolated forms.
        if decoded.chars().any(|c| matches!(u32::from(c), 0x0300..=0x036f | 0x0590..=0x0eff | 0x200c..=0x200f | 0x202a..=0x202e | 0x2066..=0x2069 | 0xfe00..=0xfe0f | 0xe0100..=0xe01ef)) {
            return Err(TextError::Font("portable text shaping/bidi is unsupported for this run".into()));
        }
        Ok(decoded.chars().map(u32::from).collect())
    }
    pub(super) fn extent(&self, input: TextInput<'_>) -> Result<TextExtent, TextError> {
        let run = self.run(input)?;
        if run.is_empty() {
            return Ok(TextExtent::default());
        }
        let mut width = 0i64;
        for code in run {
            let (face, glyph) = self.resolve(code)?;
            width += self.advance(&face, glyph, Some(code))? as i64;
        }
        Ok(TextExtent {
            width: i32::try_from(width)
                .map_err(|_| TextError::Font("text extent overflow".into()))?,
            height: self.metrics.height,
        })
    }
    pub(super) fn unicode_ranges(&self) -> Result<FontCoverage, TextError> {
        let font = self.primary.font()?;
        let codes: BTreeSet<_> = font
            .charmap()
            .mappings()
            .filter(|&(code, glyph)| code <= 65535 && glyph.to_u32() != 0)
            .map(|(code, _)| code as u16)
            .collect();
        let mut ranges: Vec<(u16, u16)> = Vec::new();
        for code in codes {
            if let Some((first, count)) = ranges.last_mut()
                && *count != u16::MAX
                && u32::from(*first) + u32::from(*count) == u32::from(code)
            {
                *count += 1;
            } else {
                ranges.push((code, 1));
            }
        }
        Ok(FontCoverage {
            flags: 0,
            glyphs: ranges.iter().map(|&(_, count)| u32::from(count)).sum(),
            ranges,
        })
    }

    fn spacing_index(&self, index: u32) -> Result<GlyphId, TextError> {
        let font = self.primary.font()?;
        let loc = location(&font, &self.request);
        let glyph = GlyphId::new(index);
        Ok(
            if font
                .glyph_metrics(Size::new(self.em), &loc)
                .advance_width(glyph)
                .is_some()
            {
                glyph
            } else {
                GlyphId::new(0)
            },
        )
    }

    pub(super) fn glyph_widths(
        &self,
        first: u32,
        count: u32,
        indices: Option<&[u16]>,
    ) -> Result<Vec<i32>, TextError> {
        (0..count)
            .map(|at| {
                let index = indices.map_or_else(
                    || first.saturating_add(at),
                    |indices| u32::from(indices[at as usize]),
                );
                Ok(self.advance(&self.primary, self.spacing_index(index)?, None)? as i32)
            })
            .collect()
    }

    pub(super) fn glyph_abc_widths(
        &self,
        first: u32,
        count: u32,
        indices: Option<&[u16]>,
    ) -> Result<Vec<AbcWidths>, TextError> {
        let characters: Vec<_> = (0..count)
            .map(|at| {
                Character::GlyphIndex(indices.map_or_else(
                    || first.saturating_add(at),
                    |indices| u32::from(indices[at as usize]),
                ))
            })
            .collect();
        self.character_abc_widths(&characters)
    }

    pub(super) fn character_abc_widths(
        &self,
        characters: &[Character],
    ) -> Result<Vec<AbcWidths>, TextError> {
        if !self.metrics.outline {
            return Err(TextError::Font("ABC query requires a scalable font".into()));
        }
        characters
            .iter()
            .map(|&character| {
                let character = match character {
                    Character::GlyphIndex(index) => {
                        Character::GlyphIndex(self.spacing_index(index)?.to_u32())
                    }
                    character => character,
                };
                let glyph = self.glyph(GlyphRequest {
                    character,
                    format: GlyphFormat::Metrics,
                    hinted: true,
                    transform: Transform::IDENTITY,
                })?;
                let metrics = glyph.metrics;
                Ok(AbcWidths {
                    left: metrics.origin_x,
                    ink: metrics.width,
                    right: i32::from(metrics.advance_x) - metrics.origin_x - metrics.width as i32,
                })
            })
            .collect()
    }

    pub(super) fn glyph(&self, request: GlyphRequest) -> Result<Glyph, TextError> {
        let (face, glyph, code) = match request.character {
            Character::GlyphIndex(index) => (Arc::clone(&self.primary), GlyphId::new(index), None),
            Character::Unicode(code) => {
                let (face, glyph) = self.resolve(code)?;
                (face, glyph, Some(code))
            }
            Character::Encoded(code) => {
                // GDI ignores the high word of an ANSI glyph selector. SBCS
                // repertoires also ignore the high byte; DBCS uses it only
                // when it is a lead byte in the selected repertoire.
                let bytes = (code as u16).to_be_bytes();
                let lead = match self.request.encoding {
                    Encoding::Japanese => matches!(bytes[0], 0x81..=0x9f | 0xe0..=0xfc),
                    Encoding::Korean
                    | Encoding::SimplifiedChinese
                    | Encoding::TraditionalChinese => matches!(bytes[0], 0x81..=0xfe),
                    _ => false,
                };
                let bytes = if lead { &bytes[..] } else { &bytes[1..] };
                // GDI's ANSI glyph conversion substitutes '?' for an invalid
                // packed DBCS sequence. Counted run decoding stays strict.
                let (decoded, errors) =
                    codec(self.request.encoding)?.decode_without_bom_handling(bytes);
                let decoded = if errors { "?" } else { decoded.as_ref() };
                let mut chars = decoded.chars();
                let code = chars
                    .next()
                    .filter(|_| chars.next().is_none())
                    .map(u32::from)
                    .ok_or_else(|| {
                        TextError::Glyph("encoded glyph does not decode to one scalar".into())
                    })?;
                let (face, glyph) = self.resolve(code)?;
                (face, glyph, Some(code))
            }
        };
        let font = face.font()?;
        let loc = location(&font, &self.request);
        let outlines = font.outline_glyphs();
        let outline = outlines.get(glyph).ok_or_else(|| {
            TextError::Glyph(
                "glyph has no supported scalable outline (embedded bitmap/color fonts unsupported)"
                    .into(),
            )
        })?;
        let italic = self.request.italic
            && face.info.style == Style::Normal
            && font.axes().get_by_tag(skrifa::Tag::new(b"ital")).is_none()
            && font.axes().get_by_tag(skrifa::Tag::new(b"slnt")).is_none();
        let mut pen = Pen {
            commands: Vec::new(),
            transform: request.transform,
            xscale: self.xscale,
            italic,
        };
        if request.hinted {
            let mono = request.format == GlyphFormat::Monochrome
                || self.request.quality == Quality::Monochrome;
            let mut hints = self
                .hints
                .lock()
                .map_err(|_| TextError::Glyph("font hinter lock poisoned".into()))?;
            if let std::collections::btree_map::Entry::Vacant(entry) =
                hints.entry((face.info.id, mono))
            {
                let hint = HintingInstance::new(
                    &outlines,
                    Size::new(self.em),
                    &loc,
                    if mono {
                        Target::Mono
                    } else {
                        Target::Smooth {
                            mode: Default::default(),
                            symmetric_rendering: false,
                            preserve_linear_metrics: true,
                        }
                    },
                )
                .map_err(|e| TextError::Glyph(format!("font hinting: {e}")))?;
                entry.insert(hint);
            }
            outline
                .draw(
                    DrawSettings::hinted(&hints[&(face.info.id, mono)], false),
                    &mut pen,
                )
                .map_err(|e| TextError::Glyph(format!("glyph outline: {e}")))?;
        } else {
            outline
                .draw(DrawSettings::unhinted(Size::new(self.em), &loc), &mut pen)
                .map_err(|e| TextError::Glyph(format!("glyph outline: {e}")))?;
        }
        let advance = self.advance(&face, glyph, code)?;
        let (advance_x, advance_y) = transform_point(request.transform, advance, 0.0);
        let advance = |n: f32| {
            i16::try_from(n.round() as i32)
                .map_err(|_| TextError::Glyph("glyph advance overflow".into()))
        };
        let bounds = zeno::bounds(&pen.commands, zeno::Fill::NonZero, None);
        let (left, bottom, right, top) = if pen.commands.is_empty() {
            (0.0, 0.0, 0.0, 0.0)
        } else {
            (
                bounds.min.x.floor(),
                bounds.min.y.floor(),
                bounds.max.x.ceil(),
                bounds.max.y.ceil(),
            )
        };
        if [left, bottom, right, top]
            .iter()
            .any(|n| !n.is_finite() || n.abs() > 32767.0)
        {
            return Err(TextError::Glyph("transformed glyph bounds overflow".into()));
        }
        let metrics = GlyphMetrics {
            width: (right - left) as u32,
            height: (top - bottom) as u32,
            origin_x: left as i32,
            origin_y: top as i32,
            advance_x: advance(advance_x)?,
            // Outline coordinates point upwards; cell advances use the
            // top-down pixel coordinate system of the text API.
            advance_y: advance(-advance_y)?,
        };
        let (data, required_bytes) = match request.format {
            // GGO_METRICS has a positive success result even for spaces;
            // Microsoft's API does not define that native result as a bitmap
            // allocation size. The portable result names the 20-byte metrics
            // record, without publishing any glyph payload.
            GlyphFormat::Metrics => (GlyphData::None, 20),
            GlyphFormat::QuadraticOutline | GlyphFormat::CubicOutline => {
                let contours = contours(&pen.commands, request.format)?;
                let bytes = contours
                    .iter()
                    .try_fold(0u32, |sum, contour| {
                        contour
                            .curves
                            .iter()
                            .try_fold(sum.checked_add(16)?, |sum, curve| {
                                let n = match curve {
                                    Curve::Line(p) | Curve::Quadratic(p) | Curve::Cubic(p) => {
                                        p.len()
                                    }
                                };
                                sum.checked_add(4 + u32::try_from(n).ok()?.checked_mul(8)?)
                            })
                    })
                    .ok_or_else(|| TextError::Glyph("glyph outline size overflow".into()))?;
                (GlyphData::Outline(contours), bytes)
            }
            format => {
                let width = metrics.width as usize;
                let height = metrics.height as usize;
                let row = if format == GlyphFormat::Monochrome {
                    width.div_ceil(8)
                } else {
                    width
                };
                let pitch = (row + 3) & !3;
                let size = pitch.checked_mul(height).ok_or_else(|| {
                    TextError::Glyph("glyph bitmap exceeds allocation limit".into())
                })?;
                let coverage_size = width
                    .checked_mul(height)
                    .filter(|&n| {
                        n.checked_add(size)
                            .is_some_and(|total| total <= 16 * 1024 * 1024)
                    })
                    .ok_or_else(|| {
                        TextError::Glyph("glyph coverage and bitmap exceed allocation limit".into())
                    })?;
                let mut bitmap = vec![0; size];
                if width != 0 && height != 0 {
                    let path: Vec<_> = pen
                        .commands
                        .iter()
                        .map(|command| command.transform(&zeno::Transform::scale(1.0, -1.0)))
                        .collect();
                    let mut coverage = vec![0; coverage_size];
                    zeno::Mask::new(&path)
                        .size(metrics.width, metrics.height)
                        .offset([-left, top])
                        .render_into(&mut coverage, None);
                    let levels = match format {
                        GlyphFormat::Gray2 => 4,
                        GlyphFormat::Gray4 => 16,
                        _ => 64,
                    };
                    for y in 0..height {
                        for x in 0..width {
                            let value = coverage[y * width + x];
                            if format == GlyphFormat::Monochrome {
                                if value >= 128 {
                                    bitmap[y * pitch + x / 8] |= 0x80 >> (x % 8);
                                }
                            } else {
                                bitmap[y * pitch + x] =
                                    ((u32::from(value) * levels + 127) / 255) as u8;
                            }
                        }
                    }
                }
                (GlyphData::Bitmap(bitmap), size as u32)
            }
        };
        Ok(Glyph {
            metrics,
            required_bytes,
            data,
        })
    }
}

fn decode(input: TextInput<'_>, encoding: Encoding) -> Result<String, TextError> {
    match input {
        TextInput::Unicode(units) => String::from_utf16(units)
            .map_err(|_| TextError::Font("invalid counted UTF-16 text".into())),
        TextInput::Encoded(bytes) => {
            let (decoded, errors) = codec(encoding)?.decode_without_bom_handling(bytes);
            if errors {
                return Err(TextError::Font("invalid counted encoded text".into()));
            }
            Ok(decoded.into_owned())
        }
    }
}
fn codec(encoding: Encoding) -> Result<&'static encoding_rs::Encoding, TextError> {
    use encoding_rs::{
        BIG5, EUC_KR, GBK, MACINTOSH, SHIFT_JIS, WINDOWS_874, WINDOWS_1250, WINDOWS_1251,
        WINDOWS_1252, WINDOWS_1253, WINDOWS_1254, WINDOWS_1255, WINDOWS_1256, WINDOWS_1257,
        WINDOWS_1258,
    };
    Ok(match encoding {
        Encoding::Latin | Encoding::Automatic => WINDOWS_1252,
        Encoding::Japanese => SHIFT_JIS,
        Encoding::Korean => EUC_KR,
        Encoding::SimplifiedChinese => GBK,
        Encoding::TraditionalChinese => BIG5,
        Encoding::Greek => WINDOWS_1253,
        Encoding::Turkish => WINDOWS_1254,
        Encoding::Vietnamese => WINDOWS_1258,
        Encoding::Hebrew => WINDOWS_1255,
        Encoding::Arabic => WINDOWS_1256,
        Encoding::Baltic => WINDOWS_1257,
        Encoding::Cyrillic => WINDOWS_1251,
        Encoding::Thai => WINDOWS_874,
        Encoding::EasternEuropean => WINDOWS_1250,
        Encoding::MacRoman => MACINTOSH,
        Encoding::Symbol | Encoding::Johab | Encoding::Oem | Encoding::Reserved(_) => {
            return Err(TextError::Font(
                "portable encoded repertoire is unsupported; supply decoded Unicode".into(),
            ));
        }
    })
}
fn transform_point(matrix: Transform, x: f32, y: f32) -> (f32, f32) {
    (
        (matrix.xx as f32 * x + matrix.yx as f32 * y) / 65536.0,
        (matrix.xy as f32 * x + matrix.yy as f32 * y) / 65536.0,
    )
}
struct Pen {
    commands: Vec<Command>,
    transform: Transform,
    xscale: f32,
    italic: bool,
}
impl Pen {
    fn point(&self, x: f32, y: f32) -> Vector {
        let (x, y) = transform_point(
            self.transform,
            (x + if self.italic { y * 0.21256 } else { 0.0 }) * self.xscale,
            y,
        );
        Vector::new(x, y)
    }
}
impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.commands.push(Command::MoveTo(self.point(x, y)));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.commands.push(Command::LineTo(self.point(x, y)));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.commands
            .push(Command::QuadTo(self.point(x1, y1), self.point(x, y)));
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.commands.push(Command::CurveTo(
            self.point(x1, y1),
            self.point(x2, y2),
            self.point(x, y),
        ));
    }
    fn close(&mut self) {
        self.commands.push(Command::Close);
    }
}
fn contours(commands: &[Command], format: GlyphFormat) -> Result<Vec<Contour>, TextError> {
    let point = |p: Vector| Point {
        x: (p.x * 65536.0).round() as i32,
        y: (p.y * 65536.0).round() as i32,
    };
    let mut contours: Vec<Contour> = Vec::new();
    let mut current = Vector::ZERO;
    for &command in commands {
        let curve = match command {
            Command::MoveTo(p) => {
                contours.push(Contour {
                    start: point(p),
                    curves: Vec::new(),
                });
                current = p;
                continue;
            }
            Command::LineTo(p) => {
                current = p;
                Curve::Line(vec![point(p)])
            }
            Command::QuadTo(control, p) => {
                let curve = if format == GlyphFormat::CubicOutline {
                    Curve::Cubic(vec![
                        point(current + (control - current) * (2.0 / 3.0)),
                        point(p + (control - p) * (2.0 / 3.0)),
                        point(p),
                    ])
                } else {
                    Curve::Quadratic(vec![point(control), point(p)])
                };
                current = p;
                curve
            }
            Command::CurveTo(a, b, p) => {
                if format == GlyphFormat::QuadraticOutline {
                    return Err(TextError::Glyph(
                        "cubic font cannot provide an exact quadratic outline".into(),
                    ));
                }
                current = p;
                Curve::Cubic(vec![point(a), point(b), point(p)])
            }
            Command::Close => {
                if let Some(contour) = contours.last() {
                    current = Vector::new(
                        contour.start.x as f32 / 65536.0,
                        contour.start.y as f32 / 65536.0,
                    );
                }
                continue;
            }
        };
        contours
            .last_mut()
            .ok_or_else(|| TextError::Glyph("invalid outline path".into()))?
            .curves
            .push(curve);
    }
    Ok(contours)
}

pub(super) fn paint(
    text: &mut Text,
    request: &FontRequest,
    paint: Paint<'_>,
    mut pixels: Pixels<'_>,
) -> Result<(), TextError> {
    let font = text.font(request)?;
    let run = font.run(paint.input)?;
    let metrics = font.metrics();
    let size = font.extent(paint.input)?;
    let format = if font.request.quality == Quality::Monochrome {
        GlyphFormat::Monochrome
    } else {
        GlyphFormat::Gray8
    };
    // Prepare every glyph before touching pixel storage so unsupported fonts or
    // characters cannot turn a failed call into a partially drawn run.
    let mut glyphs = Vec::with_capacity(run.len());
    let mut unique: BTreeMap<u32, Arc<Glyph>> = BTreeMap::new();
    let mut prepared_bytes = 0usize;
    for code in run {
        let glyph = if let Some(glyph) = unique.get(&code) {
            Arc::clone(glyph)
        } else {
            let glyph = text.glyph(
                request,
                GlyphRequest {
                    character: Character::Unicode(code),
                    format,
                    hinted: true,
                    transform: Transform::IDENTITY,
                },
            )?;
            prepared_bytes = prepared_bytes
                .checked_add(glyph.data.cache_bytes())
                .filter(|&n| n <= 32 * 1024 * 1024)
                .ok_or_else(|| {
                    TextError::Glyph("text run glyph storage exceeds allocation limit".into())
                })?;
            unique.insert(code, Arc::clone(&glyph));
            glyph
        };
        glyphs.push(glyph);
    }
    let x = i64::from(paint.origin.x)
        - match paint.horizontal {
            Horizontal::Left => 0,
            Horizontal::Center => i64::from(size.width) / 2,
            Horizontal::Right => i64::from(size.width),
        };
    let top = i64::from(paint.origin.y)
        - match paint.vertical {
            Vertical::Top => 0,
            Vertical::Baseline => i64::from(metrics.ascent),
            Vertical::Bottom => i64::from(metrics.height),
        };
    let baseline = top + i64::from(metrics.ascent);
    let rect = |left: i64, top: i64, right: i64, bottom: i64| (left, top, right, bottom);
    let fill = |pixels: &mut Pixels<'_>, r: (i64, i64, i64, i64), color: [u8; 3]| {
        let r = clipped(pixels, paint.clip, r);
        for py in r.1..r.3 {
            for px in r.0..r.2 {
                blend(pixels, px, py, color, 255);
            }
        }
    };
    if let Some(r) = paint.fill {
        fill(
            &mut pixels,
            rect(
                i64::from(r.left),
                i64::from(r.top),
                i64::from(r.right),
                i64::from(r.bottom),
            ),
            paint.background,
        );
    }
    if paint.opaque_background {
        fill(
            &mut pixels,
            rect(
                x,
                top,
                x + i64::from(size.width),
                top + i64::from(metrics.height),
            ),
            paint.background,
        );
    }
    let mut cursor = x;
    for glyph in glyphs {
        let GlyphData::Bitmap(bytes) = &glyph.data else {
            return Err(TextError::Glyph("paint glyph is not a bitmap".into()));
        };
        let width = glyph.metrics.width as usize;
        let row = if format == GlyphFormat::Monochrome {
            width.div_ceil(8)
        } else {
            width
        };
        let pitch = (row + 3) & !3;
        let left = cursor + i64::from(glyph.metrics.origin_x);
        let top = baseline - i64::from(glyph.metrics.origin_y);
        let r = clipped(
            &pixels,
            paint.clip,
            rect(
                left,
                top,
                left + i64::from(glyph.metrics.width),
                top + i64::from(glyph.metrics.height),
            ),
        );
        for py in r.1..r.3 {
            for px in r.0..r.2 {
                let gx = (px - left) as usize;
                let gy = (py - top) as usize;
                let coverage = if format == GlyphFormat::Monochrome {
                    if bytes[gy * pitch + gx / 8] & (0x80 >> (gx % 8)) != 0 {
                        255
                    } else {
                        0
                    }
                } else {
                    (u32::from(bytes[gy * pitch + gx]) * 255 + 32) / 64
                };
                if coverage != 0 {
                    blend(&mut pixels, px, py, paint.foreground, coverage);
                }
            }
        }
        cursor += i64::from(glyph.metrics.advance_x);
    }
    let thickness = (metrics.height / 16).max(1) as i64;
    if request.underline {
        fill(
            &mut pixels,
            rect(x, baseline + thickness, cursor, baseline + thickness * 2),
            paint.foreground,
        );
    }
    if request.strikeout {
        fill(
            &mut pixels,
            rect(
                x,
                baseline - i64::from(metrics.ascent) / 3,
                cursor,
                baseline - i64::from(metrics.ascent) / 3 + thickness,
            ),
            paint.foreground,
        );
    }
    Ok(())
}
fn clipped(
    pixels: &Pixels<'_>,
    clip: Option<TextBox>,
    rect: (i64, i64, i64, i64),
) -> (i64, i64, i64, i64) {
    let bounds = pixels.bounds;
    let mut r = (
        rect.0.max(i64::from(bounds.x)),
        rect.1.max(i64::from(bounds.y)),
        rect.2.min(i64::from(bounds.x + bounds.w)),
        rect.3.min(i64::from(bounds.y + bounds.h)),
    );
    if let Some(c) = clip {
        r = (
            r.0.max(i64::from(c.left)),
            r.1.max(i64::from(c.top)),
            r.2.min(i64::from(c.right)),
            r.3.min(i64::from(c.bottom)),
        );
    }
    r
}
fn blend(pixels: &mut Pixels<'_>, x: i64, y: i64, color: [u8; 3], coverage: u32) {
    let bytes = pixels.format.bytes() as usize;
    let at = (y - i64::from(pixels.bounds.y)) as usize * pixels.pitch as usize
        + (x - i64::from(pixels.bounds.x)) as usize * bytes;
    let target = &mut pixels.bytes[at..at + bytes];
    let mut pixel = pixels.format.decode(target);
    for (slot, source) in pixel[..3].iter_mut().zip(color.into_iter().rev()) {
        *slot = ((u32::from(*slot) * (255 - coverage) + u32::from(source) * coverage + 127) / 255)
            as u8;
    }
    let encoded = pixels.format.encode(pixel);
    target.copy_from_slice(&encoded[..bytes]);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // spec: DEV-FONT-5
    fn private_and_unnamed_families_are_not_listed() {
        assert!(listed("PingFang SC"));
        assert!(listed("宋体"));
        assert!(!listed(".SF NS"));
        assert!(!listed(""));
    }
    fn font(face: &str) -> FontRequest {
        FontRequest {
            default: None,
            face: face.into(),
            height: -40,
            width: 0,
            baseline_angle: 0,
            glyph_angle: 0,
            weight: 400,
            italic: false,
            underline: false,
            strikeout: false,
            encoding: Encoding::Latin,
            precision: Precision::Default,
            clipping: Clipping {
                precision: ClipPrecision::Default,
                transform_rotation: false,
                force_outline: false,
                disable_association: false,
                embedded: false,
            },
            quality: Quality::Antialiased,
            pitch: Pitch::Default,
            family: Family::Default,
        }
    }

    #[test]
    fn regional_macos_families_follow_noto_and_preserve_requested_family_priority() {
        for (encoding, region, expected) in [
            (
                Encoding::SimplifiedChinese,
                "SC",
                &[
                    "PingFang SC",
                    "Hiragino Sans GB",
                    "Heiti SC",
                    "Songti SC",
                    "STHeiti",
                ][..],
            ),
            (
                Encoding::TraditionalChinese,
                "TC",
                &["PingFang TC", "Songti TC"][..],
            ),
            (
                Encoding::Korean,
                "KR",
                &["Apple SD Gothic Neo", "AppleMyungjo"][..],
            ),
            (
                Encoding::Johab,
                "KR",
                &["Apple SD Gothic Neo", "AppleMyungjo"][..],
            ),
        ] {
            for family in [Family::SansSerif, Family::Serif] {
                let mut request = font("Guest Family");
                request.encoding = encoding;
                request.family = family;
                let names = families(&request);
                assert_eq!(names[0], "Guest Family");
                let start = names.iter().position(|name| name == expected[0]).unwrap();
                assert_eq!(&names[start..start + expected.len()], expected);
                let noto = format!(
                    "Noto {} {region}",
                    if family == Family::Serif {
                        "Serif"
                    } else {
                        "Sans"
                    }
                );
                assert_eq!(names[start - 1], noto);
                assert!(
                    start
                        < names
                            .iter()
                            .position(|name| name == "Times New Roman" || name == "Arial")
                            .unwrap()
                );
            }
        }
    }
}

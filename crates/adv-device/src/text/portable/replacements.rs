//! Share installed files and the portable matcher's family order with renderers
//! that load fonts themselves. Metadata reads do not load glyph tables.
use super::*;

struct Candidate {
    info: FaceInfo,
    names: Vec<String>,
    codepages: u32,
    truetype: bool,
}
impl Candidate {
    fn matches(&self, name: &str) -> bool {
        let name = name.to_lowercase();
        self.names
            .iter()
            .any(|actual| actual.to_lowercase() == name)
            || self
                .info
                .families
                .iter()
                .any(|family| family.0.to_lowercase() == name)
            || self.info.post_script_name.to_lowercase() == name
    }
}

fn repertoire(family: &WindowsFamily) -> Encoding {
    match family.1.map(|(language, _)| language) {
        Some("ja-JP") => Encoding::Japanese,
        Some("zh-Hans") => Encoding::SimplifiedChinese,
        Some("zh-Hant") => Encoding::TraditionalChinese,
        Some("ko-KR") => Encoding::Korean,
        _ => match family.0 {
            "MS UI Gothic" | "Meiryo UI" => Encoding::Japanese,
            _ => Encoding::Latin,
        },
    }
}

fn codepage(encoding: Encoding) -> u32 {
    1 << match encoding {
        Encoding::Japanese => 17,
        Encoding::SimplifiedChinese => 18,
        Encoding::Korean => 19,
        Encoding::TraditionalChinese => 20,
        _ => 0,
    }
}

fn replacements(candidates: &[Candidate]) -> Vec<(Vec<String>, Vec<String>)> {
    let mut output = Vec::new();
    for family in WINDOWS_FAMILIES {
        if family
            .names()
            .any(|name| candidates.iter().any(|face| face.matches(name)))
        {
            continue;
        }
        let encoding = repertoire(family);
        let Some(first) = candidates.first() else {
            break;
        };
        let mut request = enumeration_request(&first.info, family.0.into());
        request.encoding = encoding;
        request.weight = 400;
        request.italic = false;
        request.pitch = Pitch::Default;
        request.family = Family::Default;
        let names = families(&request);
        let best = candidates
            .iter()
            .filter(|candidate| candidate.codepages & codepage(encoding) != 0)
            .min_by_key(|candidate| {
                (
                    !candidate.truetype,
                    names
                        .iter()
                        .position(|name| candidate.matches(name))
                        .unwrap_or(names.len()),
                    !candidate.info.monospaced && fixed(&request),
                    candidate.info.style != Style::Normal,
                    candidate.info.weight.0.abs_diff(400),
                    candidate.names.first(),
                )
            });
        if let Some(best) = best {
            output.push((
                family.names().map(str::to_owned).collect(),
                best.names.clone(),
            ));
        }
    }
    output
}

impl Backend {
    pub(in crate::text) fn replacement_fonts(&self) -> Result<FontReplacements, TextError> {
        let db = &self.discovery()?.database;
        let mut files = BTreeSet::new();
        let mut candidates = Vec::new();
        for info in db.faces() {
            let path = match &info.source {
                Source::File(path) => path,
                _ => continue,
            };
            let Ok((data, truetype)) = discovery::face_metadata(path, info.index) else {
                continue;
            };
            let Ok(font) = FontRef::new(&data) else {
                continue;
            };
            let names: Vec<_> = legacy_families(&font)
                .filter(|name| listed(name))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            if names.is_empty() {
                continue;
            }
            files.insert(path.clone());
            candidates.push(Candidate {
                info: info.clone(),
                names,
                codepages: font
                    .os2()
                    .ok()
                    .and_then(|os2| os2.ul_code_page_range_1())
                    .unwrap_or(0),
                truetype,
            });
        }
        Ok(FontReplacements {
            files: files.into_iter().collect(),
            families: replacements(&candidates),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: DEV-FONT-1, WINE-SES-5
    #[test]
    fn simplified_chinese_replacements_prefer_heiti_to_songti() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/font-resource.ttf");
        let mut db = Database::new();
        db.load_font_file(path).unwrap();
        let info = db.faces().next().unwrap();
        let faces: Vec<_> = ["Songti SC", "Heiti SC"]
            .into_iter()
            .map(|name| Candidate {
                info: info.clone(),
                names: vec![name.into()],
                codepages: codepage(Encoding::SimplifiedChinese),
                truetype: true,
            })
            .collect();
        let aliases = replacements(&faces);
        for name in ["SimSun", "NSimSun", "SimHei", "Microsoft YaHei"] {
            let (_, targets) = aliases
                .iter()
                .find(|(names, _)| names.iter().any(|alias| alias == name))
                .unwrap();
            assert_eq!(targets, &["Heiti SC"]);
        }
    }

    // spec: DEV-FONT-1, WINE-SES-5
    #[test]
    fn replacements_require_repertoire_and_prefer_actual_truetype() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/font-resource.ttf");
        let mut db = Database::new();
        db.load_font_file(path).unwrap();
        let info = db.faces().next().unwrap().clone();
        let candidate = |name: &str, truetype, encoding| Candidate {
            info: info.clone(),
            names: vec![name.into()],
            codepages: codepage(encoding),
            truetype,
        };
        let mut faces = vec![
            candidate("Japanese CFF", false, Encoding::Japanese),
            candidate("Latin TrueType", true, Encoding::Latin),
            candidate("Japanese TrueType", true, Encoding::Japanese),
        ];
        let target = |faces: &[Candidate]| {
            replacements(faces)
                .into_iter()
                .find(|(names, _)| names.iter().any(|name| name == "MS Gothic"))
                .map(|(_, targets)| targets)
        };
        assert_eq!(target(&faces), Some(vec!["Japanese TrueType".into()]));
        faces.pop();
        assert_eq!(target(&faces), Some(vec!["Japanese CFF".into()]));
        faces.remove(0);
        assert_eq!(target(&faces), None);
        faces.push(candidate("MS Gothic", false, Encoding::Japanese));
        assert_eq!(target(&faces), None, "installed families take precedence");
    }
}

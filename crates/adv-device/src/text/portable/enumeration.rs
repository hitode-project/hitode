//! Charset discovery follows the font's OS/2 code page flags, not its name.
use super::*;

/// OpenType OS/2 ulCodePageRange1 bits in the order Windows enumerates a
/// face's charsets after the ANSI code page's own. Windows never enumerates
/// the Macintosh or OEM bits. Script names are neutral English labels;
/// unlike native Windows this backend does not localize them to the host UI.
const REPERTOIRES: [(u32, Encoding, &str); 16] = [
    (0, Encoding::Latin, "Western"),
    (17, Encoding::Japanese, "Japanese"),
    (19, Encoding::Korean, "Korean"),
    (21, Encoding::Johab, "Korean Johab"),
    (18, Encoding::SimplifiedChinese, "Chinese GB2312"),
    (20, Encoding::TraditionalChinese, "Chinese Big5"),
    (5, Encoding::Hebrew, "Hebrew"),
    (6, Encoding::Arabic, "Arabic"),
    (3, Encoding::Greek, "Greek"),
    (4, Encoding::Turkish, "Turkish"),
    (7, Encoding::Baltic, "Baltic"),
    (1, Encoding::EasternEuropean, "Central European"),
    (2, Encoding::Cyrillic, "Cyrillic"),
    (16, Encoding::Thai, "Thai"),
    (8, Encoding::Vietnamese, "Vietnamese"),
    (31, Encoding::Symbol, "Symbol"),
];

/// Whether a face's OS/2 code page flags declare `encoding`.
pub(super) fn declares(codepages: u32, encoding: Encoding) -> bool {
    REPERTOIRES
        .iter()
        .any(|&(bit, known, _)| known == encoding && codepages & (1 << bit) != 0)
}

/// The charsets a face enumerates for `query`; DEFAULT_CHARSET lists the
/// ANSI code page's charset first, as Windows does.
pub(super) fn repertoires(
    font: EnumeratedFont,
    query: Encoding,
    ansi: Encoding,
) -> Vec<EnumeratedFont> {
    let signature = font.signature.unwrap_or_default();
    let repertoires = REPERTOIRES
        .iter()
        .filter(|&&(_, encoding, _)| encoding == ansi)
        .chain(
            REPERTOIRES
                .iter()
                .filter(|&&(_, encoding, _)| encoding != ansi),
        )
        .copied();
    let mut output = Vec::new();
    for (bit, encoding, script) in repertoires {
        if signature.codepages[0] & (1 << bit) == 0
            || (query != Encoding::Automatic && query != encoding)
        {
            continue;
        }
        let mut entry = font.clone();
        entry.font.encoding = encoding;
        entry.metrics.encoding = encoding;
        entry.script = script.into();
        output.push(entry);
    }
    // Old OS/2 tables have no code page declaration. Default discovery can
    // still expose the actual Unicode font, without claiming a specific DBCS
    // repertoire which has not been declared by its author.
    if output.is_empty() && query == Encoding::Automatic && signature.codepages[0] == 0 {
        output.push(font);
    }
    output
}

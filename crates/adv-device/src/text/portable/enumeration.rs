//! Charset discovery follows the font's OS/2 code page flags, not its name.
use super::*;

pub(super) fn repertoires(font: EnumeratedFont, query: Encoding) -> Vec<EnumeratedFont> {
    let signature = font.signature.unwrap_or_default();
    // OpenType OS/2 ulCodePageRange1. Script names are neutral English labels;
    // unlike native Windows this backend does not localize them to the host UI.
    let repertoires = [
        (0, Encoding::Latin, "Western"),
        (1, Encoding::EasternEuropean, "Central European"),
        (2, Encoding::Cyrillic, "Cyrillic"),
        (3, Encoding::Greek, "Greek"),
        (4, Encoding::Turkish, "Turkish"),
        (5, Encoding::Hebrew, "Hebrew"),
        (6, Encoding::Arabic, "Arabic"),
        (7, Encoding::Baltic, "Baltic"),
        (8, Encoding::Vietnamese, "Vietnamese"),
        (16, Encoding::Thai, "Thai"),
        (17, Encoding::Japanese, "Japanese"),
        (18, Encoding::SimplifiedChinese, "Chinese GB2312"),
        (19, Encoding::Korean, "Korean"),
        (20, Encoding::TraditionalChinese, "Chinese Big5"),
        (21, Encoding::Johab, "Korean Johab"),
        (29, Encoding::MacRoman, "Macintosh"),
        (30, Encoding::Oem, "OEM"),
        (31, Encoding::Symbol, "Symbol"),
    ];
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

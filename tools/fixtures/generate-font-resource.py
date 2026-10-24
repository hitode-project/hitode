"""Generate tiny, entirely authored font-resource fixtures using fontTools.

No installed or third-party font data is used. fontTools is a developer tool,
not a runtime/build dependency. Geometry, names, metrics and maps are below.
"""
from pathlib import Path
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib import TTCollection

OUTPUT = Path(__file__).resolve().parents[2] / "crates/adv-device/tests/fixtures"


def font(family, advance):
    builder = FontBuilder(1000, isTTF=True)
    order = [".notdef", "space", "A", "B"]
    builder.setupGlyphOrder(order)
    builder.setupCharacterMap({32: "space", 65: "A", 66: "B"})
    shapes = {
        ".notdef": [(50, 0), (450, 0), (450, 700), (50, 700)],
        "space": [],
        "A": [(100, 0), (400, 700), (700, 0)],
        "B": [(100, 0), (350, 0), (350, 700), (100, 700)],
    }
    glyphs = {}
    for name, points in shapes.items():
        pen = TTGlyphPen(None)
        if points:
            pen.moveTo(points[0])
            for point in points[1:]:
                pen.lineTo(point)
            pen.closePath()
        glyphs[name] = pen.glyph()
    builder.setupGlyf(glyphs)
    builder.setupHorizontalMetrics({".notdef": (600, 50), "space": (300, 0),
                                    "A": (800, 100), "B": (advance, 100)})
    builder.setupHorizontalHeader(ascent=800, descent=-200)
    builder.setupNameTable({"familyName": family, "styleName": "Regular",
                           "uniqueFontIdentifier": family + "-authored-20261004",
                           "fullName": family + " Regular",
                           "psName": family.replace(" ", "") + "-Regular",
                           "version": "Version 1.0"})
    builder.setupOS2(sTypoAscender=800, sTypoDescender=-200,
                    usWinAscent=800, usWinDescent=200, fsType=0)
    builder.setupPost()
    builder.setupMaxp()
    # Fixed timestamps make fixture bytes reproducible.
    builder.font["head"].created = builder.font["head"].modified = 3863462400
    builder.font.recalcTimestamp = False
    return builder.font


if __name__ == "__main__":
    OUTPUT.mkdir(parents=True, exist_ok=True)
    single = font("ADV Resource Fixture", 500)
    single.save(OUTPUT / "font-resource.ttf", reorderTables=False)
    collection = TTCollection()
    collection.fonts = [font("ADV Collection One", 500), font("ADV Collection Two", 900)]
    collection.save(OUTPUT / "font-resource.ttc")
    japanese = font("ADV Japanese Fixture", 500)
    for table in japanese["cmap"].tables:
        if table.isUnicode():
            table.cmap[0x3042] = "A"
    japanese["OS/2"].ulCodePageRange1 = (1 << 17) | 1
    japanese["name"].setName("ADV 日本語フィクスチャ", 1, 3, 1, 0x411)
    japanese.save(OUTPUT / "font-resource-japanese.ttf", reorderTables=False)

"""Author the CFF Japanese font fixture: 'あ' maps to a ring of cubic arcs.

Run: python3 tools/fixtures/cff_font.py
Writes crates/adv-device/tests/fixtures/font-resource-japanese-cff.otf.
"""
from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.t2CharStringPen import T2CharStringPen

FAMILY = "ADV Japanese CFF Fixture"
OUTPUT = (Path(__file__).resolve().parents[2]
          / "crates/adv-device/tests/fixtures/font-resource-japanese-cff.otf")
# Cubic arcs approximating a circle: 0.5523 of the radius per control.
K = 0.5523


def circle(pen, cx, cy, r, clockwise):
    k = round(r * K)
    if clockwise:
        pen.moveTo((cx, cy + r))
        pen.curveTo((cx + k, cy + r), (cx + r, cy + k), (cx + r, cy))
        pen.curveTo((cx + r, cy - k), (cx + k, cy - r), (cx, cy - r))
        pen.curveTo((cx - k, cy - r), (cx - r, cy - k), (cx - r, cy))
        pen.curveTo((cx - r, cy + k), (cx - k, cy + r), (cx, cy + r))
    else:
        pen.moveTo((cx, cy + r))
        pen.curveTo((cx - k, cy + r), (cx - r, cy + k), (cx - r, cy))
        pen.curveTo((cx - r, cy - k), (cx - k, cy - r), (cx, cy - r))
        pen.curveTo((cx + k, cy - r), (cx + r, cy - k), (cx + r, cy))
        pen.curveTo((cx + r, cy + k), (cx + k, cy + r), (cx, cy + r))
    pen.closePath()


def main():
    builder = FontBuilder(1000, isTTF=False)
    builder.setupGlyphOrder([".notdef", "space", "ring"])
    builder.setupCharacterMap({0x20: "space", 0x3042: "ring"})
    blank = T2CharStringPen(500, None)
    ring = T2CharStringPen(1000, None)
    circle(ring, 500, 380, 420, False)
    circle(ring, 500, 380, 220, True)
    builder.setupCFF(FAMILY.replace(" ", "") + "-Regular", {"FullName": FAMILY},
                     {".notdef": blank.getCharString(), "space": blank.getCharString(),
                      "ring": ring.getCharString()}, {})
    builder.setupHorizontalMetrics({".notdef": (500, 0), "space": (500, 0),
                                    "ring": (1000, 80)})
    builder.setupHorizontalHeader(ascent=880, descent=-120)
    builder.setupNameTable({"familyName": FAMILY, "styleName": "Regular",
                            "uniqueFontIdentifier": FAMILY + "-authored-20261029",
                            "psName": FAMILY.replace(" ", "") + "-Regular"})
    # Latin 1 and JIS/Japan code pages, as the TrueType fixture declares.
    builder.setupOS2(sTypoAscender=880, sTypoDescender=-120, usWinAscent=880,
                     usWinDescent=120, ulCodePageRange1=0x20001)
    builder.setupPost()
    builder.save(OUTPUT)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Draws the application icon and rasterises every platform's icon set.

  python3 apps/adv-launcher/scripts/icons.py

The icon is a tilted wooden star with rounded points, cut from a board (the
side shows its thickness) on a night-blue tile with two faint light orbs. The
drawing lives here so every variant comes from the same geometry; the SVGs it
writes to src-tauri/icons/source/ are the editable sources, and the rasters
next to them are produced by `tauri icon` (pinned in package.json). Run
`pnpm install --frozen-lockfile` in apps/adv-launcher once first.

Variants:
  icon.svg        desktop tile with the macOS margin, 48 px and up
  icon-small.svg  the same tile with a plain star and two broad grain bands,
                  for 16-32 px where fine grain only looks dirty
  ios.svg         full-bleed and opaque; iOS applies its own mask
  android-*.svg   adaptive icon layers: background, foreground inside the
                  66/108 safe circle, and the themed-icon monochrome mask

icon.ico and icon.icns are assembled here so their small entries use the
small variant. The iOS set is written to icons/ios/ but not committed: its
AppIcon file names carry an "at 2x" suffix that the privacy scan reads as an
e-mail address, and it is regenerated whenever the iOS project is set up.
"""
from __future__ import annotations

import json
import math
import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

APP = Path(__file__).resolve().parent.parent
ICONS = APP / "src-tauri" / "icons"
SOURCE = ICONS / "source"

# Each point of the star differs a little in length and angle, so it reads as
# cut by hand rather than drawn: (radius scale, angle offset in degrees) for
# the ten outline vertices, tips and valleys alternating.
JITTER = [(1.00, 0), (1.03, 1), (0.95, -2), (0.98, 2), (1.02, 3),
          (1.04, -1), (0.97, -3), (0.96, 2), (1.00, 2), (1.02, -1)]
INNER = 0.6  # valley radius / tip radius
TILT = 12  # degrees, clockwise
DEPTH = 0.13  # board thickness / tip radius
TIP_ROUND = 0.22  # corner radius at the tips / tip radius
VALLEY_ROUND = 0.10

NIGHT_TOP, NIGHT_BOTTOM = "#2b3c66", "#141b30"
ORBS = [(245, 250, 40), (790, 812, 30)]  # on the 1024 canvas
TILE = (100, 824, 185)  # macOS grid: offset, size, corner radius


def outline(radius: float, tip_round: float, valley_round: float) -> str:
    """The star's outline centred on the origin, corners rounded with quadratics."""
    pts = []
    for i, (scale, offset) in enumerate(JITTER):
        angle = math.radians(-90 + TILT + i * 36 + offset)
        r = radius * (1 if i % 2 == 0 else INNER) * scale
        pts.append((r * math.cos(angle), r * math.sin(angle)))

    def toward(a: tuple[float, float], b: tuple[float, float], dist: float) -> tuple[float, float]:
        dx, dy = b[0] - a[0], b[1] - a[1]
        length = math.hypot(dx, dy)
        return a[0] + dx * dist / length, a[1] + dy * dist / length

    path = ""
    for i, p in enumerate(pts):
        cut = radius * (tip_round if i % 2 == 0 else valley_round)
        start, end = toward(p, pts[i - 1], cut), toward(p, pts[(i + 1) % 10], cut)
        path += f"{'M' if i == 0 else ' L'}{start[0]:.1f},{start[1]:.1f}"
        path += f" Q{p[0]:.1f},{p[1]:.1f} {end[0]:.1f},{end[1]:.1f}"
    return path + " Z"


def grain(radius: float) -> str:
    """Board grain across the star: wavy lines, every third one darker and wider."""
    lines = []
    y0, i = -radius * 1.4, 0
    while y0 < radius * 1.4:
        pts = []
        for j in range(61):
            x = -radius * 1.4 + j * radius * 2.8 / 60
            wobble = radius * (0.012 * math.sin(x / radius * 5 + i * 1.7)
                               + 0.006 * math.sin(x / radius * 13 + i))
            pts.append(f"{x:.1f},{y0 + wobble:.1f}")
        heavy = i % 3 == 0
        opacity = (0.3 if heavy else 0.16) * (0.75 + 0.25 * math.sin(i * 2.3))
        width = radius * (0.016 if heavy else 0.008)
        lines.append(f'<path d="M{" L".join(pts)}" fill="none" stroke="#8a4f22" '
                     f'stroke-opacity="{opacity:.2f}" stroke-width="{width:.1f}"/>')
        y0 += radius * 0.075 * (0.8 + 0.4 * abs(math.sin(i * 1.3)))
        i += 1
    return "".join(lines)


def bands(radius: float) -> str:
    """Two broad grain bands: what is left of the grain at 16-32 px."""
    return "".join(
        f'<path d="M{-radius * 1.3:.1f},{y:.1f} C{-radius * 0.4:.1f},{y - radius * 0.12:.1f} '
        f'{radius * 0.3:.1f},{y + radius * 0.14:.1f} {radius * 1.3:.1f},{y:.1f}" fill="none" '
        f'stroke="#a86a33" stroke-opacity=".55" stroke-width="{radius * 0.07:.1f}"/>'
        for y in (-radius * 0.18, radius * 0.3))


def star(cx: float, cy: float, radius: float, small: bool = False,
         parts: tuple[str, ...] = ("side", "face", "detail")) -> str:
    """The wooden star at (cx, cy); `parts` picks layers for the adaptive icon."""
    rounding = 1.1 if small else 1.0
    d = outline(radius, TIP_ROUND * rounding, VALLEY_ROUND * rounding)
    thickness = radius * DEPTH
    out = [f'<g transform="translate({cx},{cy})">']
    if "side" in parts:
        # The board's edge: the outline stacked down and slightly right.
        side = "".join(f'<path d="{d}" transform="translate({thickness * 0.02 * k:.1f},'
                       f'{thickness * 0.1 * k:.1f})"/>' for k in range(11))
        shadow = (f'<filter id="drop" x="-30%" y="-30%" width="160%" height="160%">'
                  f'<feDropShadow dx="{radius * 0.017:.1f}" dy="{radius * 0.062:.1f}" '
                  f'stdDeviation="{radius * 0.056:.1f}" flood-color="#000" flood-opacity=".35"/></filter>')
        out.append(f'<defs>{shadow}</defs><g filter="url(#drop)" fill="#97602f">{side}</g>')
    if "face" in parts:
        out.append('<defs><linearGradient id="face" x1="0" y1="0" x2="1" y2="1">'
                   '<stop offset="0" stop-color="#f4d7a2"/><stop offset=".55" stop-color="#e4b677"/>'
                   '<stop offset="1" stop-color="#d09a5a"/></linearGradient>'
                   f'<clipPath id="cut"><path d="{d}"/></clipPath></defs>'
                   f'<path d="{d}" fill="url(#face)"/>')
    if "detail" in parts:
        if small:
            out.append(f'<g clip-path="url(#cut)"><g transform="rotate(-22)">{bands(radius)}</g></g>'
                       f'<path d="{d}" fill="none" stroke="#6e4220" stroke-opacity=".5" '
                       f'stroke-width="{radius * 0.03:.1f}"/>')
        else:
            # Grain, then the chamfered top edge catching the light.
            out.append(f'<g clip-path="url(#cut)"><g transform="rotate(-22)">{grain(radius)}</g>'
                       f'<path d="{d}" fill="none" stroke="#fff4df" stroke-opacity=".55" '
                       f'stroke-width="{radius * 0.03:.1f}" '
                       f'transform="translate({radius * 0.01:.1f},{radius * 0.014:.1f})"/></g>'
                       f'<path d="{d}" fill="none" stroke="#6e4220" stroke-opacity=".4" '
                       f'stroke-width="{radius * 0.012:.1f}"/>')
    if parts == ("mask",):
        out.append(f'<path d="{d}" fill="#000"/>')
    out.append("</g>")
    return "".join(out)


def night(x: float, y: float, size: float, rounded: float) -> str:
    return ('<defs><linearGradient id="night" x1="0" y1="0" x2="0" y2="1">'
            f'<stop offset="0" stop-color="{NIGHT_TOP}"/><stop offset="1" stop-color="{NIGHT_BOTTOM}"/>'
            '</linearGradient></defs>'
            f'<rect x="{x}" y="{y}" width="{size}" height="{size}" rx="{rounded}" fill="url(#night)"/>')


def orbs(scale: float = 1.0, origin: float = 0.0) -> str:
    glow = ('<defs><radialGradient id="orb"><stop offset="0" stop-color="#fffbe8" stop-opacity=".7"/>'
            '<stop offset=".35" stop-color="#ffe9a8" stop-opacity=".3"/>'
            '<stop offset="1" stop-color="#ffe9a8" stop-opacity="0"/></radialGradient></defs>')
    return glow + "".join(
        f'<circle cx="{origin + x * scale:.1f}" cy="{origin + y * scale:.1f}" r="{r * scale:.1f}" '
        f'fill="url(#orb)"/>' for x, y, r in ORBS)


def svg(*body: str) -> str:
    return ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">'
            + "".join(body) + "</svg>\n")


def tile_shadow() -> str:
    offset, size, rounded = TILE
    return ('<defs><filter id="tile" x="-10%" y="-10%" width="120%" height="120%">'
            '<feDropShadow dx="0" dy="10" stdDeviation="12" flood-color="#000" flood-opacity=".28"/>'
            f'</filter></defs><rect x="{offset}" y="{offset}" width="{size}" height="{size}" '
            f'rx="{rounded}" fill="{NIGHT_BOTTOM}" filter="url(#tile)"/>')


def sources() -> dict[str, str]:
    offset, size, rounded = TILE
    # Inside the Android foreground's 66/108 safe circle with the tilt and
    # the board's edge included.
    safe = 1024 * 66 / 108 / 2
    return {
        "icon.svg": svg(tile_shadow(), night(offset, offset, size, rounded), orbs(),
                        star(512, 505, 355)),
        "icon-small.svg": svg(night(offset, offset, size, rounded), star(512, 500, 390, small=True)),
        "ios.svg": svg(night(0, 0, 1024, 0), orbs(1.2, -102), star(512, 505, 430)),
        "android-background.svg": svg(night(0, 0, 1024, 0), orbs(1.2, -102)),
        "android-foreground.svg": svg(star(512, 500, safe * 0.86)),
        "android-monochrome.svg": svg(star(512, 500, safe * 0.86, parts=("mask",))),
    }


def tauri(*args: str) -> None:
    pnpm = shutil.which("pnpm")
    if pnpm is None or not (APP / "node_modules").is_dir():
        sys.exit("needs pnpm and `pnpm install --frozen-lockfile` in apps/adv-launcher")
    subprocess.run([pnpm, "--dir", str(APP), "exec", "tauri", "icon", *args], check=True,
                   stdout=subprocess.DEVNULL)


def rasterise(source: Path, out: Path, sizes: list[int]) -> dict[int, bytes]:
    tauri(str(source), "-o", str(out), *[a for s in sizes for a in ("-p", str(s))])
    return {s: (out / f"{s}x{s}.png").read_bytes() for s in sizes}


def ico(images: dict[int, bytes]) -> bytes:
    """A Windows .ico with PNG-compressed entries (Vista and later)."""
    head = struct.pack("<HHH", 0, 1, len(images))
    offset = len(head) + 16 * len(images)
    entries, data = b"", b""
    for size, png in sorted(images.items()):
        entries += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(png),
                               offset + len(data))
        data += png
    return head + entries + data


# icns element types holding PNG data, by pixel size (Retina entries reuse the
# double-size image: ic11 is 16@2x, ic12 32@2x, ic13 128@2x, ic14 256@2x).
ICNS = [("icp4", 16), ("icp5", 32), ("ic11", 32), ("ic12", 64), ("ic07", 128),
        ("ic13", 256), ("ic08", 256), ("ic14", 512), ("ic09", 512), ("ic10", 1024)]


def icns(images: dict[int, bytes]) -> bytes:
    body = b"".join(kind.encode() + struct.pack(">I", 8 + len(images[size])) + images[size]
                    for kind, size in ICNS)
    return b"icns" + struct.pack(">I", 8 + len(body)) + body


def main() -> None:
    SOURCE.mkdir(parents=True, exist_ok=True)
    for name, text in sources().items():
        (SOURCE / name).write_text(text, encoding="utf-8")

    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        small = rasterise(SOURCE / "icon-small.svg", work / "small", [16, 24, 32])
        full = rasterise(SOURCE / "icon.svg", work / "full", [48, 64, 128, 256, 512, 1024])
        images = small | full

        (ICONS / "icon.ico").write_bytes(ico({s: images[s] for s in (16, 24, 32, 48, 64, 256)}))
        (ICONS / "icon.icns").write_bytes(icns(images))
        for size, name in ((32, "32x32.png"), (128, "128x128.png"), (256, "256x256.png"),
                           (512, "icon.png")):
            (ICONS / name).write_bytes(images[size])

        # Mobile sets: tauri lays out the Android mipmaps and the iOS AppIcon
        # names; the desktop files it also writes are discarded.
        manifest = work / "mobile.json"
        manifest.write_text(json.dumps({
            "default": str(SOURCE / "ios.svg"),
            "bg_color": NIGHT_BOTTOM,
            "android_bg": str(SOURCE / "android-background.svg"),
            "android_fg": str(SOURCE / "android-foreground.svg"),
            "android_fg_scale": 100,
            "android_monochrome": str(SOURCE / "android-monochrome.svg"),
        }))
        tauri(str(manifest), "-o", str(work / "mobile"))
        for platform in ("android", "ios"):
            shutil.rmtree(ICONS / platform, ignore_errors=True)
            shutil.copytree(work / "mobile" / platform, ICONS / platform)


if __name__ == "__main__":
    main()

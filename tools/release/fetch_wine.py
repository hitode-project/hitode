#!/usr/bin/env python3
"""Fetch the pinned Wine build input of a release and verify it.

  python3 tools/release/fetch_wine.py OUT

Downloads the archives tools/release/wine.json names into OUT, checks their
SHA-256 and unpacks them: OUT/wine11-sse-strip is the userland the packages
embed (ADV_WINE_DIST), OUT/wine-11.0 its source, whose license texts the
release ships; OUT/wine-11.0.tar.xz is attached to the release as it is.
"""
import hashlib
import json
import shutil
import sys
import tarfile
import urllib.request
from pathlib import Path

PINS = Path(__file__).resolve().parent / "wine.json"


def fetch(entry: dict, out: Path) -> Path:
    archive = out / entry["url"].rsplit("/", 1)[1]
    if not archive.exists():
        with urllib.request.urlopen(entry["url"], timeout=120) as response, archive.open("wb") as f:
            shutil.copyfileobj(response, f)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    if digest != entry["sha256"]:
        archive.unlink()
        sys.exit(f"{archive.name}: sha256 {digest}, expected {entry['sha256']}")
    target = out / entry["directory"]
    if not target.exists():
        with tarfile.open(archive) as tar:
            tar.extractall(out, filter="tar")
    return target


def main() -> int:
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=True)
    pins = json.loads(PINS.read_text())
    for name in ("dist", "source"):
        print(f"{name}={fetch(pins[name], out)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

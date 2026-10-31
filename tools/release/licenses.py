#!/usr/bin/env python3
"""Collect the license texts of everything the launcher packages ship.

  python3 tools/release/licenses.py OUT VERSION WINE_SOURCE

OUT receives the texts, dependencies.json and README.txt; zip it as the
release's licenses archive. WINE_SOURCE is the unpacked wine-11.0 tree that
tools/release/fetch_wine.py writes. Run after tools/macos/build.py
--ffmpeg-only (FFmpeg's texts come from its source there) and after
`pnpm install` in apps/adv-launcher; crate texts come from the Cargo registry.
"""
import json
import os
import shutil
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
FREETYPE = ROOT / 'vendor/freetype'
sys.path.insert(0, str(ROOT / 'tools/macos'))
import package  # noqa: E402

out, VERSION, WINE_SOURCE = Path(sys.argv[1]), sys.argv[2], Path(sys.argv[3])
if out.exists():
    shutil.rmtree(out)
out.mkdir(parents=True)
shutil.copy2(ROOT / 'COPYING', out / 'Hitode-GPL-3.0-or-later.txt')

profile = json.loads((ROOT / 'tools/macos/ffmpeg.json').read_text())
target = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target'))
sources_found = sorted(target.glob(f"macos-ffmpeg/*/ffmpeg-{profile['version']}"))
if len(sources_found) != 1:
    sys.exit(f"expected one FFmpeg {profile['version']} source under {target}/macos-ffmpeg, found {len(sources_found)}")
source = sources_found[0]
(out / 'ffmpeg').mkdir()
for name in ['COPYING.LGPLv2.1', 'LICENSE.md']:
    shutil.copy2(source / name, out / 'ffmpeg' / name)
sources = [{'name': 'ffmpeg', 'versionInfo': profile['version'], 'downloadLocation': profile['url'],
            'sha256': profile['sha256'], 'license': 'LGPL-2.1-or-later', 'configuration': 'tools/macos/ffmpeg.json'}]

# Wine runs as the guest's Windows userland; its bundled libraries keep their own terms.
wine = out / 'wine'
wine.mkdir()
for name in ['COPYING.LIB', 'LICENSE', 'AUTHORS']:
    shutil.copy2(WINE_SOURCE / name, wine / name)
shutil.copy2(ROOT / 'vendor/wine/NOTICE', wine / 'NOTICE')
for lib in sorted((WINE_SOURCE / 'libs').iterdir()):
    texts = [f for f in lib.iterdir() if f.is_file()
             and f.name.upper().split('.')[0] in {'LICENSE', 'LICENSE_LLVM', 'COPYING', 'COPYRIGHT', 'AUTHORS'}]
    for text in texts:
        (wine / 'libs' / lib.name).mkdir(parents=True, exist_ok=True)
        shutil.copy2(text, wine / 'libs' / lib.name / text.name)
sources.append({'name': 'wine', 'versionInfo': '11.0', 'license': 'LGPL-2.1-or-later',
                'downloadLocation': 'https://dl.winehq.org/wine/source/11.0/wine-11.0.tar.xz',
                'sha256': 'c07a6857933c1fc60dff5448d79f39c92481c1e9db5aa628db9d0358446e0701',
                'configuration': 'tools/wine/build-media.sh'})

(out / 'freetype').mkdir()
for name in ['LICENSE.TXT', 'FTL.TXT', 'GPLv2.TXT']:
    shutil.copy2(FREETYPE / name, out / 'freetype' / name)
sources.append({'name': 'freetype', 'versionInfo': '2.13.3', 'license': 'FTL OR GPL-2.0-or-later',
                'downloadLocation': 'https://download.savannah.gnu.org/releases/freetype/freetype-2.13.3.tar.xz',
                'note': 'i386 libfreetype.so.6 loaded by Wine for font rendering'})

seen = set()
missing = []
for target in ['aarch64-apple-darwin', 'aarch64-linux-android', 'aarch64-apple-ios']:
    metadata = json.loads(package.run('cargo', 'metadata', '--format-version', '1', '--filter-platform', target,
                                      '--manifest-path', 'apps/adv-launcher/src-tauri/Cargo.toml', '--locked'))
    packages = {p['id']: p for p in metadata['packages']}
    nodes = {n['id']: n for n in metadata['resolve']['nodes']}
    todo = [p['id'] for p in packages.values() if p['name'] == 'adv-launcher']
    visited = set()
    while todo:
        pid = todo.pop()
        if pid in visited:
            continue
        visited.add(pid)
        todo += [d['pkg'] for d in nodes[pid]['deps'] if any(k['kind'] != 'dev' for k in d['dep_kinds'])]
        p = packages[pid]
        if p['source'] is None or (p['name'], p['version']) in seen:
            continue
        seen.add((p['name'], p['version']))
        try:
            package.rust_licenses(p, out / 'crates' / f"{p['name']}-{p['version']}")
        except ValueError:
            # No license file published anywhere: record the declared terms.
            d = out / 'crates' / f"{p['name']}-{p['version']}"
            d.mkdir(parents=True, exist_ok=True)
            (d / 'NOTICE.txt').write_text(f"{p['name']} {p['version']}\nLicense: {p['license']}\n"
                                          f"Authors: {', '.join(p.get('authors') or [])}\n"
                                          f"Source: {p.get('repository') or ''}\n")
            missing.append(p['name'])
        sources.append({'name': p['name'], 'versionInfo': p['version'], 'license': p['license'],
                        'downloadLocation': f"https://crates.io/api/v1/crates/{p['name']}/{p['version']}/download"})

for name, path in [('cpal', 'vendor/cpal'), ('symphonia-codec-adpcm', 'vendor/symphonia-codec-adpcm'),
                   ('softfloat', 'crates/adv-cpu-core/softfloat'), ('libtheora', 'vendor/libtheora'),
                   ('upscaling', 'vendor/upscaling'), ('fluidr3mono', 'vendor/fluidr3mono')]:
    package.copy_licenses(ROOT / path, out / 'vendor' / name)

modules = ROOT / 'apps/adv-launcher/node_modules'
for name in ['svelte', '@tauri-apps/api']:
    package.copy_licenses(modules / name, out / 'npm' / name.replace('/', '__'))
    version = json.loads((modules / name / 'package.json').read_text())['version']
    sources.append({'name': name, 'versionInfo': version, 'downloadLocation': f'https://www.npmjs.com/package/{name}'})

(out / 'dependencies.json').write_text(json.dumps(sources, indent=2) + '\n')
(out / 'README.txt').write_text(
    f'Hitode {VERSION} is free software under GPL-3.0-or-later (Hitode-GPL-3.0-or-later.txt).\n'
    f'Its complete source is the v{VERSION} tag of https://github.com/hitode-project/hitode.\n'
    'This archive holds the license texts of the bundled components; dependencies.json lists\n'
    'their exact versions and source locations.\n'
    'The packages embed an unmodified Wine 11.0 build (LGPL-2.1-or-later). Its source,\n'
    'wine-11.0.tar.xz, is attached to the same release; tools/wine/build-media.sh in the\n'
    'Hitode source records how it is configured and built. Wine loads FreeType 2.13.3\n'
    '(FreeType License, see freetype/) for font rendering.\n'
    'The Android app also contains AndroidX and Kotlin standard libraries (Apache-2.0),\n'
    'fetched by Gradle as recorded in the source tree.\n')
print(len(seen), 'crates; declared-only:', missing)

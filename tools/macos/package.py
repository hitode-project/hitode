#!/usr/bin/env python3
"""Package the static media runner with Hardened Runtime and JIT permission.

Usage: python3 tools/macos/package.py [--binary target/macos/adv-run]
       [--output target/macos/ADV.app] [--identity ID] [--notary-profile PROFILE]
Input: tools/macos/build.py output and its build.json, Cargo dependencies.
Missing crate notices are downloaded from their pinned upstream VCS revisions.
Output: a new .app under target/, bundled licenses/source instructions and
optional notarization/stapling. Stdout contains counts only. Signing credentials
stay in Keychain; identity may also come from ADV_CODESIGN_IDENTITY.
Tests: python3 -m unittest discover -s tools/macos -p 'test_*.py'
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tarfile
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
ENTITLEMENTS = {"com.apple.security.cs.allow-jit": True}


def run(*args: str | Path) -> str:
    return subprocess.check_output([str(x) for x in args], text=True, cwd=ROOT,
                                   stderr=subprocess.PIPE)


def dependencies(text: str) -> list[str]:
    return [line.strip().split(" (compatibility version", 1)[0]
            for line in text.splitlines()[1:] if " (compatibility version" in line]


def minimum_os(text: str) -> tuple[int, ...]:
    found = re.findall(r"\bminos ([\d.]+)", text)
    found += re.findall(r"cmd LC_VERSION_MIN_MACOSX\s+cmdsize \d+\s+version ([\d.]+)", text)
    if not found:
        raise ValueError("Mach-O has no macOS deployment target")
    return max(tuple(map(int, version.split('.'))) for version in found)


def system_library(name: str) -> bool:
    return name.startswith(("/usr/lib/", "/System/Library/"))


def copy_licenses(source: Path, destination: Path) -> int:
    files = [p for p in source.iterdir() if p.is_file() and
             p.name.upper().startswith(('LICENSE', 'COPYING', 'COPYRIGHT', 'NOTICE'))]
    if not files:
        # Some Rust crates put notices in a dedicated LICENSES directory.
        folder = source / 'LICENSES'
        if folder.is_dir():
            shutil.copytree(folder, destination / 'LICENSES')
            return len(list(folder.rglob('*')))
        raise ValueError(f'license text missing for {source.name}')
    destination.mkdir(parents=True, exist_ok=True)
    for file in files:
        shutil.copy2(file, destination / file.name)
    return len(files)


def rust_licenses(package: dict, destination: Path) -> int:
    path = Path(package['manifest_path']).parent
    if any(p.name.upper().startswith(('LICENSE', 'COPYING', 'COPYRIGHT', 'NOTICE')) for p in path.iterdir()):
        return copy_licenses(path, destination)
    # Workspace crates sometimes omit parent license files from their .crate.
    # Recover the notices from the exact published VCS revision, never HEAD.
    # SPDX license-list-data v3.27.0 supplies the standard text omitted upstream.
    if package['license'] == 'WTFPL':
        destination.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / 'vendor/licenses/WTFPL.txt', destination / 'LICENSE')
        shutil.copy2(path / 'Cargo.toml.orig', destination / 'NOTICE-Cargo.toml')
        return 2
    vcs = json.loads((path / '.cargo_vcs_info.json').read_text())
    revision = vcs['git']['sha1']
    repository = package['repository'].removesuffix('.git')
    if not repository.startswith('https://github.com/'):
        raise ValueError(f"missing license source: {package['name']}")
    cache = ROOT / 'target' / 'macos-license-cache'
    cache.mkdir(exist_ok=True)
    archive = cache / f'{revision}.tar.gz'
    if not archive.exists():
        url = f"https://codeload.github.com/{repository.removeprefix('https://github.com/')}/tar.gz/{revision}"
        with urllib.request.urlopen(url, timeout=60) as response:
            archive.write_bytes(response.read())
    with tarfile.open(archive) as tar:
        prefix = tar.getnames()[0].split('/')[0]
        directory = Path(prefix) / vcs.get('path_in_vcs', '')
        for parent in [directory, *directory.parents]:
            if parent == Path('.'):
                break
            matches = [m for m in tar.getmembers() if m.isfile() and
                       Path(m.name).parent == parent and
                       Path(m.name).name.upper().startswith(('LICENSE', 'COPYING', 'COPYRIGHT', 'NOTICE'))]
            if matches:
                destination.mkdir(parents=True, exist_ok=True)
                for member in matches:
                    (destination / Path(member.name).name).write_bytes(tar.extractfile(member).read())
                return len(matches)
    raise ValueError(f"license text absent from upstream source: {package['name']}")


def notices(resources: Path, build: dict) -> tuple[int, int]:
    licenses = resources / 'Licenses'
    licenses.mkdir()
    shutil.copy2(ROOT / 'COPYING', licenses / 'ADV-GPL-3.0.txt')
    sources = []
    ffmpeg = build['profile']
    source = ROOT / 'target/macos-ffmpeg' / build['ffmpeg_key'] / f"ffmpeg-{ffmpeg['version']}"
    destination = licenses / 'ffmpeg'
    destination.mkdir()
    for name in ['COPYING.LGPLv2.1', 'LICENSE.md']:
        shutil.copy2(source / name, destination / name)
    count = 3
    sources.append({'name': 'ffmpeg', 'versionInfo': ffmpeg['version'],
                    'downloadLocation': ffmpeg['url'], 'sha256': ffmpeg['sha256'],
                    'license': 'LGPL-2.1-or-later', 'configuration': ffmpeg})
    metadata = json.loads(run('cargo', 'metadata', '--format-version', '1',
                              '--filter-platform', 'aarch64-apple-darwin',
                              '--features', 'adv-shell/gpu,adv-shell/media', '--locked'))
    packages = {p['id']: p for p in metadata['packages']}
    nodes = {p['id']: p for p in metadata['resolve']['nodes']}
    todo = [p['id'] for p in packages.values() if p['name'] == 'adv-run']
    visited = set()
    rust_count = 0
    while todo:
        package_id = todo.pop()
        if package_id in visited:
            continue
        visited.add(package_id)
        package = packages[package_id]
        todo += [d['pkg'] for d in nodes[package_id]['deps']
                 if any(k['kind'] != 'dev' for k in d['dep_kinds'])]
        path = Path(package['manifest_path']).parent
        if package['source'] is None:
            continue  # In the accompanying repository source archive.
        name, version = package['name'], package['version']
        count += rust_licenses(package, licenses / f'{name}-{version}')
        sources.append({'name': name, 'versionInfo': version,
                        'license': package['license'],
                        'downloadLocation': f'https://crates.io/api/v1/crates/{name}/{version}/download'})
        rust_count += 1
    # Local patched dependencies and statically linked SoftFloat need notices too.
    for name, path in [('cpal', 'vendor/cpal'), ('symphonia-codec-adpcm', 'vendor/symphonia-codec-adpcm'),
                       ('softfloat', 'crates/adv-cpu-core/softfloat'), ('libtheora', 'vendor/libtheora')]:
        count += copy_licenses(ROOT / path, licenses / name)
    source = resources / 'Source'
    source.mkdir(exist_ok=True)
    (source / 'dependencies.json').write_text(json.dumps(sources, indent=2) + '\n')
    sha = run('git', 'rev-parse', 'HEAD').strip()
    subprocess.run(['git', 'archive', '--format=tar.gz', '-o', str(source / 'hitode.tar.gz'), 'HEAD'],
                   cwd=ROOT, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    (source / 'README.txt').write_text(
        f'ADV source revision: {sha}\n'
        'hitode.tar.gz contains the GPL project source, Cargo.lock and build scripts.\n'
        'dependencies.json lists exact upstream source URLs and the FFmpeg checksum/configuration.\n'
        'Download those URLs to obtain dependency sources. tools/macos/ffmpeg.json pins the decode-only configuration.\n'
        'Build on macOS with Xcode Command Line Tools and Rust:\n'
        'python3 tools/macos/build.py\n'
        'python3 tools/macos/package.py\n'
        'No game resources are included.\n')
    return count, rust_count


def package(binary: Path, output: Path, identity: str, profile: str | None) -> dict[str, int]:
    if profile and identity == '-':
        raise ValueError('notarization requires a Developer ID identity')
    commands = run('otool', '-l', binary)
    minimum = minimum_os(commands)
    if any(not system_library(d) for d in dependencies(run('otool', '-L', binary))):
        raise ValueError('runner links non-system dylibs; build with tools/macos/build.py')
    build = json.loads(binary.with_name('build.json').read_text())
    if hashlib.sha256(binary.read_bytes()).hexdigest() != build['binary_sha256']:
        raise ValueError('runner does not match its static build manifest')
    output.mkdir(parents=True, exist_ok=False)
    contents = output / 'Contents'
    macos, resources = (contents / name for name in ('MacOS', 'Resources'))
    for path in (macos, resources):
        path.mkdir(parents=True)
    executable = macos / 'adv-run'
    shutil.copy2(binary, executable)
    plist = {'CFBundleExecutable': 'adv-run', 'CFBundleIdentifier': 'org.hitode.player',
             'CFBundleName': 'ADV', 'CFBundlePackageType': 'APPL',
             'CFBundleVersion': '1', 'CFBundleShortVersionString': '0.1.0',
             'LSMinimumSystemVersion': '.'.join(map(str, minimum)),
             'NSHighResolutionCapable': True}
    (contents / 'Info.plist').write_bytes(plistlib.dumps(plist))
    license_count, rust_count = notices(resources, build)
    entitlements = resources / 'entitlements.plist'
    entitlements.write_bytes(plistlib.dumps(ENTITLEMENTS))
    timestamp = ['--timestamp'] if identity != '-' else ['--timestamp=none']
    run('codesign', '--force', '--sign', identity, '--options', 'runtime', *timestamp,
        '--entitlements', entitlements, output)
    run('codesign', '--verify', '--deep', '--strict', output)
    if profile:
        archive = output.with_suffix('.zip')
        run('ditto', '-c', '-k', '--keepParent', output, archive)
        response = json.loads(run('xcrun', 'notarytool', 'submit', archive,
                                  '--keychain-profile', profile, '--wait', '--output-format', 'json'))
        if response.get('status') != 'Accepted':
            raise ValueError('notarization was not accepted; inspect notarytool history')
        run('xcrun', 'stapler', 'staple', output)
        run('xcrun', 'stapler', 'validate', output)
        run('spctl', '-a', '-vv', '--type', 'execute', output)
    return {'dylibs': 0, 'license_files': license_count, 'rust_dependencies': rust_count,
            'signature_verified': 1, 'notarized': int(profile is not None)}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/macos/adv-run')
    parser.add_argument('--output', type=Path, default=ROOT / 'target/macos/ADV.app')
    parser.add_argument('--identity', default=os.environ.get('ADV_CODESIGN_IDENTITY', '-'))
    parser.add_argument('--notary-profile')
    args = parser.parse_args()
    output = args.output.resolve()
    if not output.is_relative_to((ROOT / 'target').resolve()) or output.suffix != '.app':
        parser.error('output must be a new .app under target/')
    print(json.dumps(package(args.binary.resolve(), output, args.identity, args.notary_profile)))


if __name__ == '__main__':
    try:
        main()
    except subprocess.CalledProcessError as error:
        diagnostics = ROOT / 'target/scratch/macos-package'
        diagnostics.mkdir(parents=True, exist_ok=True)
        (diagnostics / 'error.log').write_text(str(error.stderr or ''))
        sys.exit('packaging command failed; inspect the failed stage locally')
    except ValueError as error:
        sys.exit(str(error))

#!/usr/bin/env python3
"""Build an ARM macOS runner with a pinned, decode-only static FFmpeg.

With --ios it builds only that FFmpeg for iPhone arm64, for the iOS launcher.

ffmpeg-sys-next cannot select FFmpeg components in its source build, so this
script runs configure itself and hands the installed static libraries to the
crate's prebuilt-library path.

Usage: python3 tools/macos/build.py [--jobs N] [--ffmpeg-only | --ios]
Input: ffmpeg.json, Xcode Command Line Tools, Rust, cmake for test dependencies.
Output: target/macos/adv-run and its build.json, source/configuration and static
libraries under target/macos-ffmpeg (target/ios-ffmpeg with --ios; stdout then
names a folder whose cargo wrapper sets FFMPEG_DIR, to put first on PATH). Logs stay in target/scratch/macos-package.
Stdout contains build statistics only. No Homebrew runtime libraries are used.
Tests: python3 -m unittest discover -s tools/macos -p 'test_*.py'
"""
from __future__ import annotations
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
PROFILE = Path(__file__).with_name('ffmpeg.json')


def configure(profile: dict, prefix: Path, ios: bool = False) -> list[str]:
    if ios:
        sdk = subprocess.run(['xcrun', '--sdk', 'iphoneos', '--show-sdk-path'], check=True,
                             capture_output=True, text=True).stdout.strip()
        flags = f"-arch arm64 -mios-version-min={profile['minimum_ios']}"
        target = ['--enable-cross-compile', '--target-os=darwin', '--arch=aarch64',
                  '--cc=xcrun --sdk iphoneos clang', f'--sysroot={sdk}',
                  f'--extra-cflags={flags}', f'--extra-ldflags={flags}']
    else:
        target = [f"--extra-cflags=-mmacosx-version-min={profile['minimum_macos']}",
                  f"--extra-ldflags=-mmacosx-version-min={profile['minimum_macos']}"]
    return [f'--prefix={prefix}', '--enable-static', '--disable-shared', '--enable-pic',
            '--disable-everything', '--disable-autodetect', '--disable-programs', '--disable-doc',
            '--disable-network', '--disable-debug', '--enable-small', '--disable-avdevice',
            '--disable-avfilter', '--disable-encoders', '--disable-muxers',
            '--disable-gpl', '--disable-nonfree', '--disable-version3', *target,
            '--enable-demuxer=' + ','.join(profile['demuxers']),
            '--enable-decoder=' + ','.join(profile['decoders']),
            '--enable-parser=' + ','.join(profile['parsers'])]


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def build(jobs: int, ffmpeg_only: bool, ios: bool = False) -> dict:
    if platform.system() != 'Darwin' or platform.machine() != 'arm64':
        raise ValueError('this build targets ARM macOS')
    profile = json.loads(PROFILE.read_text())
    # Isolate Cargo's native dependency cache from dynamic development builds.
    key = hashlib.sha256(PROFILE.read_bytes() + json.dumps(configure(profile, Path('install'), ios)).encode()).hexdigest()[:16]
    base = ROOT / ('target/ios-ffmpeg' if ios else 'target/macos-ffmpeg') / key
    source = base / f"ffmpeg-{profile['version']}"
    prefix = base / 'install'
    base.mkdir(parents=True, exist_ok=True)
    log_dir = ROOT / 'target/scratch/macos-package'
    log_dir.mkdir(parents=True, exist_ok=True)
    archive = base.parent / f"ffmpeg-{profile['version']}.tar.xz"
    if not archive.exists():
        with urllib.request.urlopen(profile['url'], timeout=60) as response:
            archive.write_bytes(response.read())
    if digest(archive) != profile['sha256']:
        raise ValueError('FFmpeg source checksum mismatch')
    with (log_dir / ('build-static-ios.log' if ios else 'build-static.log')).open('w') as log:
        def run(*args, **kwargs):
            subprocess.run([str(a) for a in args], check=True, stdout=log, stderr=subprocess.STDOUT, **kwargs)
        if not (prefix / 'build.json').exists():
            if not source.exists():
                run('tar', '-xf', archive, '-C', base)
            run(source / 'configure', *configure(profile, prefix, ios), cwd=source)
            run('make', f'-j{jobs}', cwd=source)
            run('make', 'install', cwd=source)
            (prefix / 'build.json').write_text(json.dumps(profile, indent=2) + '\n')
        for library in ['avcodec', 'avformat', 'avutil', 'swresample', 'swscale']:
            if not (prefix / 'lib' / f'lib{library}.a').is_file():
                raise ValueError('static FFmpeg build is incomplete')
        if list((prefix / 'lib').glob('*.dylib')):
            raise ValueError('static FFmpeg prefix contains dynamic libraries')
        if ios:
            # Tauri's Xcode build passes only HOME and PATH to Cargo, so a cargo
            # wrapper first on PATH carries the FFmpeg location instead.
            bin_dir = ROOT / 'target/ios-ffmpeg/bin'
            bin_dir.mkdir(exist_ok=True)
            wrapper = bin_dir / 'cargo'
            wrapper.write_text('#!/bin/sh\n'
                               f"export FFMPEG_DIR='{prefix}'\n"
                               "export LIBCLANG_PATH='/Library/Developer/CommandLineTools/usr/lib'\n"
                               f"exec '{shutil.which('cargo')}' \"$@\"\n")
            wrapper.chmod(0o755)
            return {'static_libraries': 5, 'decoders': len(profile['decoders']), 'path': str(bin_dir)}
        if not ffmpeg_only:
            env = os.environ.copy()
            env.update(FFMPEG_DIR=str(prefix),
                       LIBCLANG_PATH='/Library/Developer/CommandLineTools/usr/lib',
                       MACOSX_DEPLOYMENT_TARGET=profile['minimum_macos'],
                       CARGO_TARGET_DIR=str(ROOT / 'target/macos-build' / key))
            # ffmpeg-sys-next's prebuilt-library path passes -lav* to the linker;
            # this prefix contains only .a files, so no FFmpeg dylib is linked.
            run('cargo', 'build', '--release', '-p', 'adv-run', '--features',
                'adv-shell/gpu,adv-shell/media', cwd=ROOT, env=env)
            output = ROOT / 'target/macos'
            output.mkdir(exist_ok=True)
            shutil.copy2(Path(env['CARGO_TARGET_DIR']) / 'release/adv-run', output / 'adv-run')
            build_info = {'ffmpeg_key': key, 'binary_sha256': digest(output / 'adv-run'), 'profile': profile}
            (output / 'build.json').write_text(json.dumps(build_info, indent=2) + '\n')
    return {'static_libraries': 5, 'decoders': len(profile['decoders']),
            'demuxers': len(profile['demuxers']), 'runner_built': int(not ffmpeg_only)}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--jobs', type=int, default=os.cpu_count() or 1)
    parser.add_argument('--ffmpeg-only', action='store_true')
    parser.add_argument('--ios', action='store_true', help='build only FFmpeg, for iPhone arm64')
    args = parser.parse_args()
    print(json.dumps(build(args.jobs, args.ffmpeg_only, args.ios)))

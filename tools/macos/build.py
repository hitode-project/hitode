#!/usr/bin/env python3
"""Build an ARM macOS runner with a pinned, decode-only static FFmpeg.

With --ios or --android it builds only that FFmpeg for iPhone arm64 or Android
arm64-v8a, for the mobile launchers.

ffmpeg-sys-next cannot select FFmpeg components in its source build, so this
script runs configure itself and hands the installed static libraries to the
crate's prebuilt-library path.

Usage: python3 tools/macos/build.py [--jobs N] [--ffmpeg-only | --ios | --android]
Input: ffmpeg.json, Xcode Command Line Tools, Rust, cmake for test dependencies,
ANDROID_NDK_HOME for --android.
Output: target/macos/adv-run and its build.json, source/configuration and static
libraries under target/macos-ffmpeg (target/<ios|android>-ffmpeg for a mobile
target). FFmpeg-only modes print a cargo wrapper folder to put first on PATH;
it selects FFmpeg and removes release build paths. Logs stay in target/scratch/macos-package.
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
import shlex
import shutil
import subprocess
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
PROFILE = Path(__file__).with_name('ffmpeg.json')


def ndk_toolchain() -> Path:
    return Path(os.environ['ANDROID_NDK_HOME']) / 'toolchains/llvm/prebuilt/darwin-x86_64'


def configure(profile: dict, prefix: Path, mobile: str | None = None) -> list[str]:
    if mobile == 'ios':
        sdk = subprocess.run(['xcrun', '--sdk', 'iphoneos', '--show-sdk-path'], check=True,
                             capture_output=True, text=True).stdout.strip()
        flags = f"-arch arm64 -mios-version-min={profile['minimum_ios']}"
        target = ['--enable-cross-compile', '--target-os=darwin', '--arch=aarch64',
                  '--cc=xcrun --sdk iphoneos clang', f'--sysroot={sdk}',
                  f'--extra-cflags={flags}', f'--extra-ldflags={flags}']
    elif mobile == 'android':
        tools = ndk_toolchain() / 'bin'
        target = ['--enable-cross-compile', '--target-os=android', '--arch=aarch64',
                  f"--cc={tools}/aarch64-linux-android{profile['minimum_android']}-clang",
                  f'--ar={tools}/llvm-ar', f'--nm={tools}/llvm-nm',
                  f'--ranlib={tools}/llvm-ranlib', f'--strip={tools}/llvm-strip']
    else:
        target = [f"--extra-cflags=-mmacosx-version-min={profile['minimum_macos']}",
                  f"--extra-ldflags=-mmacosx-version-min={profile['minimum_macos']}"]
    media = ['--enable-demuxer=mov', '--enable-decoder=aac', '--enable-parser=aac']
    if mobile != 'android':
        media += ['--enable-videotoolbox',
                  '--enable-hwaccel=h264_videotoolbox,hevc_videotoolbox,mpeg2_videotoolbox,mpeg4_videotoolbox']
    return [f'--prefix={prefix}', '--enable-static', '--disable-shared', '--enable-pic',
            '--disable-everything', '--disable-autodetect', '--disable-programs', '--disable-doc',
            '--disable-network', '--disable-debug', '--enable-small', '--disable-avdevice',
            '--disable-avfilter', '--disable-encoders', '--disable-muxers',
            '--disable-gpl', '--disable-nonfree', '--disable-version3', *target, *media,
            '--enable-demuxer=' + ','.join(profile['demuxers']),
            '--enable-decoder=' + ','.join(profile['decoders']),
            '--enable-parser=' + ','.join(profile['parsers'])]


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def remap_flags() -> list[str]:
    return [f'--remap-path-prefix={source}={target}' for source, target in [
        (Path.home(), '/home'),
        (Path(os.environ.get('CARGO_HOME', Path.home() / '.cargo')).resolve(), '/cargo'),
        (Path(os.environ.get('RUSTUP_HOME', Path.home() / '.rustup')).resolve(), '/rustup'),
        (ROOT, '/hitode'),
    ]]


def build(jobs: int, ffmpeg_only: bool, mobile: str | None = None) -> dict:
    if platform.system() != 'Darwin' or platform.machine() != 'arm64':
        raise ValueError('this build targets ARM macOS')
    profile = json.loads(PROFILE.read_text())
    # Isolate Cargo's native dependency cache from dynamic development builds.
    key = hashlib.sha256(PROFILE.read_bytes() + json.dumps(configure(profile, Path('install'), mobile)).encode()).hexdigest()[:16]
    base = ROOT / f"target/{mobile or 'macos'}-ffmpeg" / key
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
    with (log_dir / (f'build-static-{mobile}.log' if mobile else 'build-static.log')).open('w') as log:
        def run(*args, **kwargs):
            subprocess.run([str(a) for a in args], check=True, stdout=log, stderr=subprocess.STDOUT, **kwargs)
        if not (prefix / 'build.json').exists():
            if not source.exists():
                run('tar', '-xf', archive, '-C', base)
            run(source / 'configure', *configure(profile, prefix, mobile), cwd=source)
            run('make', f'-j{jobs}', cwd=source)
            run('make', 'install', cwd=source)
            (prefix / 'build.json').write_text(json.dumps(profile, indent=2) + '\n')
        for library in ['avcodec', 'avformat', 'avutil', 'swresample', 'swscale']:
            if not (prefix / 'lib' / f'lib{library}.a').is_file():
                raise ValueError('static FFmpeg build is incomplete')
        if list((prefix / 'lib').glob('*.dylib')) or list((prefix / 'lib').glob('*.so')):
            raise ValueError('static FFmpeg prefix contains dynamic libraries')
        if mobile or ffmpeg_only:
            # Tauri's Xcode build passes only HOME and PATH to Cargo, so a cargo
            # wrapper first on PATH carries the FFmpeg location instead.
            bin_dir = ROOT / f'target/{mobile or "macos"}-ffmpeg/bin'
            bin_dir.mkdir(exist_ok=True)
            wrapper = bin_dir / 'cargo'
            # bindgen parses FFmpeg's headers against the NDK's libc headers.
            sysroot = (f"export BINDGEN_EXTRA_CLANG_ARGS_aarch64_linux_android='--sysroot={ndk_toolchain()}/sysroot"
                       f" --target=aarch64-linux-android{profile['minimum_android']}'\n") if mobile == 'android' else ''
            wine_dist = os.environ.get('ADV_WINE_DIST')
            wine = (f"export ADV_WINE_DIST={shlex.quote(str(Path(wine_dist).resolve()))}\n"
                    if wine_dist else '')
            wrapper.write_text('#!/bin/sh\n'
                               f"export FFMPEG_DIR='{prefix}'\n"
                               f"{wine}"
                               "export LIBCLANG_PATH='/Library/Developer/CommandLineTools/usr/lib'\n"
                               "export CARGO_PROFILE_RELEASE_STRIP=debuginfo\n"
                               "case \" $* \" in *' --release '*)\n"
                               "sep=$(printf '\\037')\n"
                               "flags=\"${CARGO_ENCODED_RUSTFLAGS-$(printf '%s' \"${RUSTFLAGS-}\" | tr ' ' '\\037')}\"\n"
                               f"for mapping in {shlex.join(remap_flags())}; do\n"
                               "  flags=\"${flags:+$flags$sep}$mapping\"\n"
                               "done\n"
                               "export CARGO_ENCODED_RUSTFLAGS=\"$flags\"\n"
                               "esac\n"
                               f"{sysroot}exec '{shutil.which('cargo')}' \"$@\"\n")
            wrapper.chmod(0o755)
            return {'static_libraries': 5, 'decoders': len(profile['decoders']), 'path': str(bin_dir)}
        if not ffmpeg_only:
            env = os.environ.copy()
            env.update(FFMPEG_DIR=str(prefix),
                       LIBCLANG_PATH='/Library/Developer/CommandLineTools/usr/lib',
                       MACOSX_DEPLOYMENT_TARGET=profile['minimum_macos'],
                       CARGO_TARGET_DIR=str(ROOT / 'target/macos-build' / key))
            flags = env.get('CARGO_ENCODED_RUSTFLAGS')
            previous = flags.split('\x1f') if flags else []
            if flags is None:
                previous = env.get('RUSTFLAGS', '').split()
            env['CARGO_ENCODED_RUSTFLAGS'] = '\x1f'.join([*previous, *remap_flags()])
            env['CARGO_PROFILE_RELEASE_STRIP'] = 'debuginfo'
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
    parser.add_argument('--ios', dest='mobile', action='store_const', const='ios',
                        help='build only FFmpeg, for iPhone arm64')
    parser.add_argument('--android', dest='mobile', action='store_const', const='android',
                        help='build only FFmpeg, for Android arm64-v8a')
    args = parser.parse_args()
    print(json.dumps(build(args.jobs, args.ffmpeg_only, args.mobile)))

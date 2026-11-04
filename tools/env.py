#!/usr/bin/env python3
"""Toolchain environments for Cargo, shared by the repository's tools.

  python3 tools/env.py PLATFORM               print PLATFORM's environment as shell exports
  python3 tools/env.py PLATFORM -- COMMAND... run COMMAND in it

PLATFORM is mac, ios or android. Every platform gets LIBCLANG_PATH from the
Xcode clang and sccache as RUSTC_WRAPPER when it is installed. mac takes
FFMPEG_DIR from Homebrew ffmpeg@8. ios and android build the static FFmpeg of
tools/macos/build.py once per checkout under target/; android adds JDK 21, the
Android SDK and the NDK the Gradle project names (JAVA_HOME, ANDROID_HOME,
ANDROID_NDK_HOME, otherwise their Homebrew locations) and the sysroot bindgen
parses FFmpeg's headers against. Variables already set are kept.

Elsewhere than an Apple silicon Mac only `mac` exists and changes nothing.
Missing inputs are listed together, each with the step that provides it.
Tests: python3 -m unittest tools/test_env.py
"""
from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent
GRADLE = ROOT / 'apps/adv-launcher/src-tauri/gen/android/app/build.gradle.kts'
PLATFORMS = ('mac', 'ios', 'android')
# The files apps/adv-launcher/src-tauri/build/wine.rs requires before embedding.
WINE_REQUIRED = ('lib/wine/i386-windows/ntdll.dll', 'lib/wine/i386-unix/win32u.so',
                 'share/hitode/prefix/system.reg')


class Missing(ValueError):
    """Inputs the environment needs, each with the step that provides it."""

    def __init__(self, problems: list[str]):
        super().__init__('\n'.join(problems))
        self.problems = problems


def module(path: Path):
    spec = importlib.util.spec_from_file_location(path.stem, path)
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


def output(*args: str) -> str | None:
    try:
        return subprocess.run(args, check=True, capture_output=True, text=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return None


def apple() -> bool:
    return sys.platform == 'darwin' and os.uname().machine == 'arm64'


def android(env: dict) -> tuple[dict, list[str]]:
    """JDK, SDK and NDK locations, and what is missing of them."""
    brew = output('brew', '--prefix')
    java = (env.get('JAVA_HOME') or output('/usr/libexec/java_home', '-v', '21')
            or (brew and f'{brew}/opt/openjdk@21/libexec/openjdk.jdk/Contents/Home'))
    sdk = (env.get('ANDROID_HOME') or env.get('ANDROID_SDK_ROOT')
           or (brew and f'{brew}/share/android-commandlinetools'))
    version = re.search(r'ndkVersion = "([^"]+)"', GRADLE.read_text()).group(1)
    ndk = env.get('NDK_HOME') or env.get('ANDROID_NDK_HOME') or (sdk and f'{sdk}/ndk/{version}')
    problems = [f'missing {name}: {fix}' for name, path, fix in [
        ('JDK 21', java, 'brew install openjdk@21, or set JAVA_HOME'),
        ('Android SDK', sdk, 'brew install --cask android-commandlinetools, or set ANDROID_HOME'),
        (f'NDK {version}', ndk, f'sdkmanager --sdk_root="$ANDROID_HOME" "ndk;{version}", or set NDK_HOME'),
        ('Android build tools', sdk and next(Path(sdk, 'build-tools').glob('*/apksigner'), None),
         'sdkmanager --sdk_root="$ANDROID_HOME" "build-tools;36.0.0"'),
    ] if not path or not Path(path).exists()]
    return {'JAVA_HOME': java, 'ANDROID_HOME': sdk, 'NDK_HOME': ndk, 'ANDROID_NDK_HOME': ndk}, problems


def android_api() -> str:
    """The minimum Android API level FFmpeg and the native libraries target."""
    return json.loads((ROOT / 'tools/macos/ffmpeg.json').read_text())['minimum_android']


def wine_dist(env: dict) -> Path:
    """ADV_WINE_DIST, or the pinned Wine build fetched once into target/wine."""
    root = env.get('ADV_WINE_DIST')
    if not root:
        fetch = module(ROOT / 'tools/release/fetch_wine.py')
        out = ROOT / 'target/wine'
        out.mkdir(parents=True, exist_ok=True)
        root = fetch.fetch(json.loads(fetch.PINS.read_text())['dist'], out)
    missing = [f'incomplete ADV_WINE_DIST: missing {required}' for required in WINE_REQUIRED
               if not (Path(root) / required).is_file()]
    if missing:
        raise Missing(missing)
    return Path(root).resolve()


def cargo(platform: str, env: dict | None = None) -> dict:
    """The environment Cargo builds `platform` in; raises Missing."""
    if platform not in PLATFORMS:
        raise ValueError(f'unknown platform {platform}; expected one of {", ".join(PLATFORMS)}')
    result = dict(os.environ if env is None else env)
    if not apple():
        if platform != 'mac':
            raise Missing([f'{platform} builds on an Apple silicon Mac'])
        return result
    problems = []
    if 'RUSTC_WRAPPER' not in result and shutil.which('sccache'):
        result['RUSTC_WRAPPER'] = 'sccache'
    clang = output('xcrun', '--find', 'clang')
    if clang and 'LIBCLANG_PATH' not in result:
        result['LIBCLANG_PATH'] = str(Path(clang).parents[1] / 'lib')
    if platform == 'android':
        tools, problems = android(result)
        result.update({k: v for k, v in tools.items() if v})
    if platform == 'mac' and 'FFMPEG_DIR' not in result:
        prefix = output('brew', '--prefix', 'ffmpeg@8')
        if prefix and (Path(prefix) / 'include').is_dir():
            result['FFMPEG_DIR'] = prefix
        else:
            problems.append('missing FFmpeg 8: brew install ffmpeg@8, or set FFMPEG_DIR')
    if problems:
        raise Missing(problems)
    if platform != 'mac':
        result.setdefault('FFMPEG_DIR', str(mobile_ffmpeg(platform, result)))
    if platform == 'android':
        sysroot = Path(result['ANDROID_NDK_HOME']) / 'toolchains/llvm/prebuilt/darwin-x86_64/sysroot'
        result.setdefault('BINDGEN_EXTRA_CLANG_ARGS_aarch64_linux_android',
                          f'--sysroot={sysroot} --target=aarch64-linux-android{android_api()}')
    return result


def mobile_ffmpeg(platform: str, env: dict) -> Path:
    """The static FFmpeg prefix for `platform`, configured and built on first use."""
    build = module(ROOT / 'tools/macos/build.py')
    saved = dict(os.environ)
    # build.py reads the NDK from the process environment.
    os.environ.update(env)
    try:
        if not any((ROOT / f'target/{platform}-ffmpeg').glob('*/install/build.json')):
            print(f'building FFmpeg for {platform} once for this checkout (a few minutes)', file=sys.stderr)
        return build.static_ffmpeg(os.cpu_count() or 1, platform)[0]
    finally:
        os.environ.clear()
        os.environ.update(saved)


def exports(env: dict, base: dict) -> str:
    """Shell exports for what `env` adds to or changes in `base`."""
    return ''.join(f'export {k}={shlex.quote(v)}\n' for k, v in sorted(env.items()) if base.get(k) != v)


def main(argv: list[str]) -> int:
    command = argv[argv.index('--') + 1:] if '--' in argv else None
    args = argv[:argv.index('--')] if '--' in argv else argv
    if len(args) != 1 or args[0] not in PLATFORMS or command == []:
        print(__doc__.split('\n\n')[1], file=sys.stderr)
        return 2
    try:
        env = cargo(args[0])
    except Missing as missing:
        print(missing, file=sys.stderr)
        return 1
    if command is None:
        sys.stdout.write(exports(env, dict(os.environ)))
        return 0
    return subprocess.run(command, env=env).returncode


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))

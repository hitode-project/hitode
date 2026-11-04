#!/usr/bin/env python3
"""Run a launcher package build with its complete build environment.

Usage: python3 tools/macos/package_env.py mac|ios|android -- COMMAND...
Input: ADV_WINE_DIST with the complete stripped Wine installation; for android,
JDK 21 and the Android SDK (JAVA_HOME and ANDROID_HOME, otherwise their standard
macOS locations) with the NDK named by the Gradle project; for ios,
APPLE_DEVELOPMENT_TEAM.
Output: refreshes the build.py cargo wrapper so it carries ADV_WINE_DIST, installs
the launcher packages when missing, then runs COMMAND in apps/adv-launcher with the
wrapper first on PATH. It stops before building when an input is missing, since the
build would otherwise drop Wine without an error.
Tests: python3 -m unittest discover -s tools/macos -p 'test_*.py'
"""
from __future__ import annotations

import importlib.util
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
LAUNCHER = ROOT / 'apps/adv-launcher'
GRADLE = LAUNCHER / 'src-tauri/gen/android/app/build.gradle.kts'
# The files apps/adv-launcher/src-tauri/build/wine.rs requires before embedding.
WINE_REQUIRED = ('lib/wine/i386-windows/ntdll.dll', 'lib/wine/i386-unix/win32u.so',
                 'share/hitode/prefix/system.reg')


def wine_dist(env: dict) -> Path:
    root = env.get('ADV_WINE_DIST')
    if not root:
        raise ValueError('set ADV_WINE_DIST to the stripped Wine installation')
    for required in WINE_REQUIRED:
        if not (Path(root) / required).is_file():
            raise ValueError(f'incomplete ADV_WINE_DIST: missing {required}')
    return Path(root).resolve()


def output(*args: str) -> str | None:
    try:
        return subprocess.run(args, check=True, capture_output=True, text=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return None


def android(env: dict) -> dict:
    brew = output('brew', '--prefix')
    java = (env.get('JAVA_HOME') or output('/usr/libexec/java_home', '-v', '21')
            or (brew and f'{brew}/opt/openjdk@21/libexec/openjdk.jdk/Contents/Home'))
    sdk = (env.get('ANDROID_HOME') or env.get('ANDROID_SDK_ROOT')
           or (brew and f'{brew}/share/android-commandlinetools'))
    version = re.search(r'ndkVersion = "([^"]+)"', GRADLE.read_text()).group(1)
    ndk = env.get('NDK_HOME') or env.get('ANDROID_NDK_HOME') or (sdk and f'{sdk}/ndk/{version}')
    for name, path in [('JDK 21 (JAVA_HOME)', java), ('Android SDK (ANDROID_HOME)', sdk),
                       (f'NDK {version} (NDK_HOME)', ndk)]:
        if not path or not Path(path).is_dir():
            raise ValueError(f'missing {name}')
    return {'JAVA_HOME': java, 'ANDROID_HOME': sdk, 'NDK_HOME': ndk, 'ANDROID_NDK_HOME': ndk}


def environment(platform: str, env: dict) -> dict:
    result = dict(env, ADV_WINE_DIST=str(wine_dist(env)))
    clang = output('xcrun', '--find', 'clang')
    if clang and 'LIBCLANG_PATH' not in env:
        result['LIBCLANG_PATH'] = str(Path(clang).parents[1] / 'lib')
    if platform == 'android':
        result.update(android(env))
    if platform == 'ios' and not env.get('APPLE_DEVELOPMENT_TEAM'):
        raise ValueError('set APPLE_DEVELOPMENT_TEAM to the signing team')
    if 'RUSTC_WRAPPER' not in env and shutil.which('sccache'):
        result['RUSTC_WRAPPER'] = 'sccache'
    return result


def main(argv: list[str]) -> int:
    if len(argv) < 3 or argv[0] not in ('mac', 'ios', 'android') or argv[1] != '--':
        print(__doc__.split('\n\n')[1].splitlines()[0], file=sys.stderr)
        return 2
    platform, command = argv[0], argv[2:]
    try:
        env = environment(platform, dict(os.environ))
    except ValueError as error:
        print(f'package_env: {error}', file=sys.stderr)
        return 1
    # build.py reads the Wine and NDK locations from the process environment.
    os.environ.update(env)
    spec = importlib.util.spec_from_file_location('build', Path(__file__).with_name('build.py'))
    build = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(build)
    wrapper = build.build(os.cpu_count() or 1, platform == 'mac',
                          None if platform == 'mac' else platform)['path']
    env['PATH'] = f"{wrapper}{os.pathsep}{env.get('PATH', '')}"
    if not (LAUNCHER / 'node_modules').is_dir():
        subprocess.run(['pnpm', 'install', '--frozen-lockfile'], cwd=LAUNCHER, env=env, check=True)
    return subprocess.run(command, cwd=LAUNCHER, env=env).returncode


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))

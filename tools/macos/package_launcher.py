#!/usr/bin/env python3
"""Build, sign and optionally install launcher packages for the chosen platforms.

Usage: python3 tools/macos/package_launcher.py PLATFORM... | all
           [--install [DEVICE]] [--side-by-side NAME] [-- COMMAND...]
PLATFORM is mac, ios or android; several build one after another, all builds all.
Input: ARM macOS with Xcode, Rust, Node and pnpm. ADV_WINE_DIST may name the
stripped Wine installation; unset, the pinned build from tools/release/wine.json
is downloaded once into target/wine. android needs JDK 21 and the Android SDK
(JAVA_HOME and ANDROID_HOME, otherwise their Homebrew locations) with the NDK the
Gradle project names; ios needs APPLE_DEVELOPMENT_TEAM.
Output: target/package/<platform>/ holds Hitode.app (ad hoc signed), Hitode.ipa
(development signed) or Hitode.apk (aligned, signed with the SDK debug key).
--install puts each mobile package on the only connected device of its platform,
or on DEVICE (adb serial or devicectl identifier) when one mobile platform is chosen. --side-by-side NAME gives the APK the application ID
suffix NAME, so it installs beside a release-signed copy instead of replacing it.
With -- COMMAND and one platform, it only prepares that environment and runs
COMMAND in apps/adv-launcher. Every input is checked before compiling, because the launcher
build would otherwise drop Wine without an error.
Tests: python3 -m unittest discover -s tools/macos -p 'test_*.py'
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
LAUNCHER = ROOT / 'apps/adv-launcher'
TAURI = LAUNCHER / 'src-tauri'
OUTPUT = ROOT / 'target/package'


def module(path: Path):
    spec = importlib.util.spec_from_file_location(path.stem, path)
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


# Toolchain discovery shared with the other tools.
toolchain = module(ROOT / 'tools/env.py')
GRADLE = toolchain.GRADLE
WINE_REQUIRED = toolchain.WINE_REQUIRED
wine_dist = toolchain.wine_dist
output = toolchain.output
android = toolchain.android


TARGETS = {'mac': 'aarch64-apple-darwin', 'ios': 'aarch64-apple-ios', 'android': 'aarch64-linux-android'}


def preflight(platforms: list[str], env: dict) -> list[str]:
    """Every missing input at once, each with the step that provides it."""
    if os.uname().sysname != 'Darwin' or os.uname().machine != 'arm64':
        return ['packages build on an Apple silicon Mac, where tools/macos/build.py builds FFmpeg']
    problems = [f'missing {name}: {fix}' for name, fix in [
        ('pnpm', 'corepack enable pnpm, or npm install --global pnpm'),
        ('rustup', 'install Rust from https://rustup.rs'),
        ('xcrun', 'xcode-select --install')] if not shutil.which(name)]
    installed = (output('rustup', 'target', 'list', '--installed') or '').split()
    problems += [f'missing Rust target {TARGETS[p]}: rustup target add {TARGETS[p]}'
                 for p in platforms if shutil.which('rustup') and TARGETS[p] not in installed]
    if 'android' in platforms:
        problems += android(env)[1]
    if 'ios' in platforms:
        if not output('xcodebuild', '-version'):
            problems.append('missing Xcode: install it, then sudo xcode-select -s /Applications/Xcode.app')
        if not env.get('APPLE_DEVELOPMENT_TEAM'):
            problems.append('missing APPLE_DEVELOPMENT_TEAM: set it to your Apple signing team ID')
    if env.get('ADV_WINE_DIST'):
        problems += [f'incomplete ADV_WINE_DIST: missing {required}' for required in WINE_REQUIRED
                     if not (Path(env['ADV_WINE_DIST']) / required).is_file()]
    return problems


def environment(platform: str, env: dict) -> dict:
    result = dict(env)
    clang = output('xcrun', '--find', 'clang')
    if clang and 'LIBCLANG_PATH' not in env:
        result['LIBCLANG_PATH'] = str(Path(clang).parents[1] / 'lib')
    if platform == 'android':
        result.update(android(env)[0])
    if 'RUSTC_WRAPPER' not in env and shutil.which('sccache'):
        result['RUSTC_WRAPPER'] = 'sccache'
    return result


def run(args: list, env: dict, cwd: Path = LAUNCHER) -> None:
    subprocess.run([str(a) for a in args], cwd=cwd, env=env, check=True)


def build_tool(env: dict, name: str) -> Path:
    return max(Path(env['ANDROID_HOME'], 'build-tools').glob('*/' + name),
               key=lambda p: [int(x) for x in re.findall(r'\d+', p.parent.name)])


def debug_keystore(env: dict) -> Path:
    keystore = Path.home() / '.android/debug.keystore'
    if not keystore.exists():
        keystore.parent.mkdir(exist_ok=True)
        run([Path(env['JAVA_HOME']) / 'bin/keytool', '-genkeypair', '-keystore', keystore,
             '-storepass', 'android', '-alias', 'androiddebugkey', '-keypass', 'android',
             '-keyalg', 'RSA', '-validity', '10000', '-dname', 'CN=Android Debug,O=Android,C=US'], env)
    return keystore


def package(platform: str, env: dict, side_by_side: str | None) -> Path:
    out = OUTPUT / platform
    out.mkdir(parents=True, exist_ok=True)
    if platform == 'mac':
        run(['pnpm', 'tauri', 'build', '--ci', '--target', 'aarch64-apple-darwin', '--bundles', 'app'], env)
        built = Path(env.get('CARGO_TARGET_DIR', ROOT / 'target')) / 'aarch64-apple-darwin/release/bundle/macos/Hitode.app'
        app = out / 'Hitode.app'
        shutil.rmtree(app, ignore_errors=True)
        run(['ditto', built, app], env)
        run(['strip', '-x', app / 'Contents/MacOS/adv-launcher'], env)
        run(['codesign', '--force', '--sign', '-', '--options', 'runtime',
             '--entitlements', TAURI / 'macos.entitlements', app], env)
        run(['codesign', '--verify', '--strict', app], env)
        return app
    if platform == 'ios':
        if not (TAURI / 'icons/ios').is_dir():
            run([sys.executable, LAUNCHER / 'scripts/icons.py'], env)
        run(['pnpm', 'tauri', 'ios', 'build', '--ci', '--export-method', 'debugging', '--target', 'aarch64'], env)
        ipa = out / 'Hitode.ipa'
        shutil.copy2(TAURI / 'gen/apple/build/arm64/Hitode.ipa', ipa)
        return ipa
    if side_by_side:
        env = dict(env, HITODE_ANDROID_ID_SUFFIX=side_by_side)
    run(['pnpm', 'tauri', 'android', 'build', '--ci', '--apk', '--target', 'aarch64'], env)
    unsigned = TAURI / 'gen/android/app/build/outputs/apk/universal/release/app-universal-release-unsigned.apk'
    apk = out / (f'Hitode-{side_by_side}.apk' if side_by_side else 'Hitode.apk')
    with tempfile.TemporaryDirectory(dir=out) as temporary:
        aligned = Path(temporary) / 'aligned.apk'
        run([build_tool(env, 'zipalign'), '-f', '-P', '16', '4', unsigned, aligned], env)
        run([build_tool(env, 'apksigner'), 'sign', '--ks', debug_keystore(env), '--ks-pass', 'pass:android',
             '--ks-key-alias', 'androiddebugkey', '--key-pass', 'pass:android',
             '--v4-signing-enabled', 'false', '--out', apk, aligned], env)
    run([sys.executable, ROOT / 'tools/android/check-apk.py', apk], env)
    return apk


def ios_device(device: str | None) -> str:
    if device:
        return device
    with tempfile.NamedTemporaryFile(suffix='.json') as listing:
        subprocess.run(['xcrun', 'devicectl', 'list', 'devices', '--json-output', listing.name],
                       check=True, capture_output=True)
        devices = [d['identifier'] for d in json.load(open(listing.name))['result']['devices']
                   if d.get('connectionProperties', {}).get('tunnelState') == 'connected']
    if len(devices) != 1:
        raise ValueError(f'{len(devices)} connected iOS devices: pass --install DEVICE')
    return devices[0]


def install(platform: str, artifact: Path, device: str | None, env: dict) -> None:
    if platform == 'android':
        run(['adb', *(['-s', device] if device else []), 'install', '-r', artifact], env)
    elif platform == 'ios':
        run(['xcrun', 'devicectl', 'device', 'install', 'app', '--device', ios_device(device), artifact], env)


PLATFORMS = ('mac', 'ios', 'android')


def parse(argv: list[str]) -> tuple[argparse.Namespace, list[str], list[str] | None]:
    command = argv[argv.index('--') + 1:] if '--' in argv else None
    parser = argparse.ArgumentParser(description=__doc__.split('\n\n')[0],
                                     epilog='Add -- COMMAND to run COMMAND in the prepared environment of one platform.')
    parser.add_argument('platforms', nargs='+', choices=[*PLATFORMS, 'all'], metavar='PLATFORM',
                        help='mac, ios, android or all; several build one after another')
    parser.add_argument('--install', nargs='?', const='', metavar='DEVICE',
                        help='install each mobile package on the connected device, or on DEVICE')
    parser.add_argument('--side-by-side', metavar='NAME',
                        help='suffix the Android application ID so the APK installs beside a release')
    args = parser.parse_args(argv[:argv.index('--')] if command is not None else argv)
    platforms = [p for p in PLATFORMS if p in args.platforms or 'all' in args.platforms]
    if args.side_by_side and ('android' not in platforms or not re.fullmatch(r'[a-z][a-z0-9]*', args.side_by_side)):
        parser.error('--side-by-side takes a lowercase name and applies to android')
    if command is not None and len(platforms) != 1:
        parser.error('-- COMMAND takes one platform')
    if args.install and len([p for p in platforms if p != 'mac']) != 1:
        parser.error('--install DEVICE takes one mobile platform')
    return args, platforms, command


def main(argv: list[str]) -> int:
    args, platforms, command = parse(argv)
    problems = preflight(platforms, dict(os.environ))
    if problems:
        print('package_launcher: fix these first:', *problems, sep='\n  ', file=sys.stderr)
        return 1
    try:
        try:
            base = dict(os.environ, ADV_WINE_DIST=str(wine_dist(dict(os.environ))))
        except OSError as error:
            raise ValueError(f'could not download the pinned Wine build ({error}); run again to retry')
        for platform in platforms:
            env = environment(platform, base)
            # build.py reads the Wine and NDK locations from the process environment.
            os.environ.update(env)
            build = module(Path(__file__).with_name('build.py'))
            wrapper = build.build(os.cpu_count() or 1, platform == 'mac',
                                  None if platform == 'mac' else platform)['path']
            env['PATH'] = f"{wrapper}{os.pathsep}{base.get('PATH', '')}"
            if not (LAUNCHER / 'node_modules').is_dir():
                run(['pnpm', 'install', '--frozen-lockfile'], env)
            if command is not None:
                return subprocess.run(command, cwd=LAUNCHER, env=env).returncode
            artifact = package(platform, env, args.side_by_side)
            size = (sum(f.stat().st_size for f in artifact.rglob('*')) if artifact.is_dir()
                    else artifact.stat().st_size)
            print(f'{artifact.relative_to(ROOT)} {size / 2**20:.1f} MiB')
            if args.install is not None and platform != 'mac':
                install(platform, artifact, args.install or None, env)
    except ValueError as error:
        print(f'package_launcher: {error}', file=sys.stderr)
        return 1
    except OSError as error:
        print(f'package_launcher: {error}; a failed download succeeds on a later run', file=sys.stderr)
        return 1
    except subprocess.CalledProcessError as error:
        print(f'package_launcher: exit {error.returncode} from: {" ".join(map(str, error.cmd))}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))

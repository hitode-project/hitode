# Building

This page lists how to build, test and package each front end. Games are never part of a build: point `ADV_SAMPLES` at your own game folder when a command needs one.

## Requirements

- Rust stable with rustfmt, and Python 3.
- CMake, for the test-only Unicorn reference in `adv-unicorn`.
- For video: FFmpeg 8 headers and libraries, and libclang for bindgen. Set `FFMPEG_DIR` to the FFmpeg root with `include` and `lib` (and `bin` on Windows), and `LIBCLANG_PATH` to the folder holding libclang. The libraries must match the target architecture.
  - Development on a Mac may use Homebrew: `brew install ffmpeg@8`, then `export FFMPEG_DIR=$(brew --prefix ffmpeg@8) LIBCLANG_PATH=$(dirname "$(dirname "$(xcrun --find clang)")")/lib`. `tools/check.py` fills in these two values by itself when they are unset.
  - Release packages never use Homebrew: they link the pinned static FFmpeg that `tools/macos/build.py` builds (below).
  - Without FFmpeg, everything except `media` builds and tests: leave out `adv-shell/media` and test single crates with `-p`, since the workspace suites (`cargo test --workspace`, `cargo test-full`) build the launcher.
- For the launcher: Node.js and pnpm.

After cloning, install the pre-commit hook:

```sh
python3 tools/check.py --install-hook
```

The hook runs `tools/check.py` on what is staged: formatting, whitespace, privacy, layering, specs, the document list and, when the launcher changed, its type check and build. Run `pnpm --dir apps/adv-launcher install --frozen-lockfile` once before committing launcher changes.

## Command-line runner

`adv-run` runs a game with or without a window. `gpu` adds the wgpu renderer and `media` adds video through FFmpeg.

```sh
cargo build --release -p adv-run
FFMPEG_DIR=<ffmpeg> LIBCLANG_PATH=<libclang dir> \
  cargo build --release -p adv-run --features adv-shell/gpu,adv-shell/media
```

A dynamic media build needs the FFmpeg libraries next to the runner or on the library search path. Without `media`, video sources fail with `VFW_E_UNSUPPORTED_STREAM` and the report records the gap. `adv-run --help` lists the options.

## Tests

```sh
cargo test -p <crate>
python3 -m unittest discover -s tools/<folder>
```

- Run the tests of the crates you changed. The full workspace suite is for batched changes and baseline updates.
- Tests that need a game read it from `$ADV_SAMPLES`. They are ignored or skip when it is unset, so the default run needs no game files.
- Windows-only differential tests run a self-written fixture natively and under Hitode. Their build scripts are in `tools/fixtures/`.
- On AArch64, `tools/cpu/arm64-check.sh <ref>` runs the CPU suite once and judges it both normally and in strict mode. x64-only tests do not build there.
- Media tests need the FFmpeg variables above: `cargo test -p adv-device --features media`.
- Direct3D 9 builds only with the `gpu` feature, so `cargo test -p adv-dx` skips it without a word: test it with `cargo test -p adv-dx --features gpu`. Test files pulled in with `include!` are outside `cargo fmt`; format them with `rustfmt --edition 2024 <file>`.
- The workspace suite builds the launcher, so it needs the FFmpeg variables too.
- The default suite must pass on every supported host, so any failure in it is real. A test that needs particular hardware, a GPU backend or an architecture states it with `required-features`, a `cfg` or `#[ignore = "reason"]`. A test that fails intermittently gets its cause fixed, not a retry.

Every pull request runs the workspace on Apple Silicon (`macos-14`), with
independent formatting and strict Clippy gates. Three parallel test groups cover
core/system crates, graphics/runtime crates, and the launcher/JIT helper/storage
plugin. New workspace members join the core/system group unless assigned to
another group. Tests use the development profile with optimization level 1 and
no debug information; third-party dependencies retain the optimized settings in
`Cargo.toml`.
Compilation and execution are separate steps, limited to 12 and 5 minutes, with
a 20-minute job limit. Artifacts contain verbose build logs and Cargo timings.
Media jobs reuse the pinned, decode-only FFmpeg build used by release packages;
its libraries are cached independently from Cargo.
The full DAC rate matrices run every Monday at 03:17 UTC or through the manual
`slow-tests` input. Master pushes populate caches for later pull requests.

Linux core and hardware acceptance checks use `ubuntu-24.04-arm`; release-mode
Apple Silicon acceptance is also a gate. Windows x64 compatibility checks run
for shared CPU or x64 changes, Windows source in either side of the diff, native
fixtures, build configuration and performance tooling, plus weekly and manual
runs. They test the CPU and lint platform code without building the launcher or
iOS helper. Cargo caches are separated by job and test group. Mac jobs use three
workers; Linux and Windows jobs use four. The privacy scan runs on every PR.

## Launcher

The launcher in `apps/adv-launcher` is a Tauri app with a Svelte front end. It builds with `media` on every platform, so it needs the FFmpeg variables.

```sh
pnpm --dir apps/adv-launcher install --frozen-lockfile
cd apps/adv-launcher
FFMPEG_DIR=<ffmpeg> LIBCLANG_PATH=<libclang dir> pnpm tauri dev
```

`pnpm check` runs the type check, and `pnpm tauri build` builds a release.

For a macOS release, run `python3 tools/macos/build.py --ffmpeg-only`, then put its printed wrapper folder first on `PATH` and run `pnpm tauri build --target aarch64-apple-darwin --bundles app,dmg`. The wrapper selects the static FFmpeg, remaps build paths and strips release debug records. The bundle is signed ad hoc with Hardened Runtime and only the `allow-jit` entitlement. The version comes from `package.json`, and must match the Cargo workspace version.

## macOS release package

The release links a decode-only FFmpeg statically, so the app bundle carries no third-party dylibs.

```sh
python3 tools/macos/build.py
python3 tools/macos/package.py
```

- `build.py` downloads the FFmpeg source pinned in `tools/macos/ffmpeg.json`, checks its SHA-256 and builds only the listed demuxers, decoders and parsers. It writes `target/macos/adv-run` and `build.json`, with the log under `target/scratch/macos-package/`.
- `package.py` writes `target/macos/ADV.app`, or another new folder under `target` given with `--output`. It checks the load commands and the build record, signs with Hardened Runtime and the `allow-jit` entitlement only, and adds licenses, source locations and checksums.
- The default signature is ad hoc. For distribution, set `ADV_CODESIGN_IDENTITY` to a Developer ID and pass `--notary-profile` with a notarytool profile stored in the keychain. Keep both out of the repository.
- To add a format, update `ffmpeg.json` and the decode tests together. Release builds never fall back to Homebrew libraries.
- `python3 -m unittest discover -s tools/macos` tests the scripts without building FFmpeg.

The runner inside the bundle takes the usual arguments:

```sh
target/macos/ADV.app/Contents/MacOS/adv-run --audio \
  --data-dir target/investigations/<topic>/data "$ADV_SAMPLES/<game>/<game>.exe"
```

## Android launcher

The Android app is the launcher built for Android. It needs Android SDK platform and build tools 36, NDK r28c or newer, JDK 21 and the `aarch64-linux-android` Rust target, with `JAVA_HOME`, `ANDROID_HOME` and `NDK_HOME` set. Like the iOS app, it links a decode-only static FFmpeg built from `ffmpeg.json`.

```sh
ANDROID_NDK_HOME=$NDK_HOME python3 tools/macos/build.py --android
cd apps/adv-launcher
pnpm install --frozen-lockfile
PATH=../../target/android-ffmpeg/bin:$PATH \
  CARGO_PROFILE_RELEASE_STRIP=debuginfo pnpm tauri android build --apk --target aarch64
```

- `build.py --android` writes a `cargo` wrapper into `target/android-ffmpeg/bin` that sets `FFMPEG_DIR` and points bindgen at the NDK headers.

- The strip setting keeps the release profile's line tables out of the APK, which stores native libraries uncompressed. The release APK is `src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release-unsigned.apk`. Sign it with `apksigner` and a keystore kept outside the repository, since `cargo clean` empties `target/`. Keep that keystore: only the same key can update an installed copy without losing its library and saves.
- `--debug` builds a debuggable APK signed with the SDK debug key. Its native code is unoptimized, so use it for startup and lifecycle checks, not gameplay.
- `python3 tools/android/check-apk.py <apk>` checks 16 KiB ELF and ZIP alignment. These checks prove packaging properties, not execution on a 16 KiB device.
- `tools/android/run-cpu-tests.ps1` runs the CPU tests on a device through adb. [tools/android/README.md](../tools/android/README.md) covers its options.

Install and start the app:

```sh
adb install -r <signed apk>
adb shell am start -W -n org.hitode.launcher/.MainActivity
```

In the app, import a complete game folder, choose its `.exe` and code page, and tap the entry to start. The source folder is not modified.

## iOS launcher

The iOS app links a decode-only static FFmpeg built for iPhone arm64 from the same `ffmpeg.json` as the macOS release.

```sh
python3 tools/macos/build.py --ios
cd apps/adv-launcher
PATH=../../target/ios-ffmpeg/bin:$PATH APPLE_DEVELOPMENT_TEAM=<team> \
  pnpm tauri ios build --export-method debugging --target aarch64
```

- Tauri's Xcode build passes only `HOME` and `PATH` on to Cargo, so `FFMPEG_DIR` cannot be set directly. `build.py --ios` writes a `cargo` wrapper into `target/ios-ffmpeg/bin` that sets it and strips release debug records. Both iOS executables also strip linker debug records on export.
- The IPA is `src-tauri/gen/apple/build/arm64/Hitode.ipa`. Install it with `xcrun devicectl device install app --device <udid> <ipa>`.
- Select game folders through Files to read them at their original locations, including outside the app's container. Directory access is restored from bookmarks after relaunch, and cloud files are downloaded before discovery and game launch. Games already in Documents through Finder or `devicectl device copy to --domain-type appDataContainer` are also read in place. Saves and registry stay in the app's private data directory.
- The AppIcon images are not committed. `python3 apps/adv-launcher/scripts/icons.py` writes them to `src-tauri/icons/ios/`; copy them into `gen/apple/Assets.xcassets/AppIcon.appiconset/` before the first build.

## Local output

Before distribution, run `python3 tools/privacy/scan.py --artifacts <app> <apk> <ipa> <licenses.zip>`.
This scans binary contents and nested archives for local build paths; mount a DMG and pass its mounted directory too.
Release Cargo wrappers and JIT helper builds remap checkout and toolchain paths, including when Xcode invokes Cargo directly. The storage plugin's Swift source uses a stable source location for runtime diagnostics.

Build and run output stays under the ignored `target/` folder. Each worktree uses its own `target`, so builds do not wait on each other's Cargo lock or replace a binary another run is using. [investigation.md](investigation.md) describes where runs and analysis output go.

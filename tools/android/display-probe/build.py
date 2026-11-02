#!/usr/bin/env python3
"""Build and sign the Java-free SurfaceControl probe with Android SDK 36."""
import os
from pathlib import Path
import subprocess
import zipfile


def main():
    root = Path(__file__).resolve().parents[3]
    sdk = Path(os.environ["ANDROID_HOME"])
    target = Path(os.environ["CARGO_TARGET_DIR"]).resolve()
    build = target / "display-probe"
    build.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, RUSTC_WRAPPER="sccache")
    subprocess.run([
        "cargo", "ndk", "-t", "arm64-v8a", "--platform", "29", "build",
        "-p", "adv-display", "--example", "display-probe", "--release",
    ], cwd=root, env=env, check=True)
    tools = sdk / "build-tools" / "36.0.0"
    unsigned = build / "unsigned.apk"
    subprocess.run([
        tools / "aapt2", "link", "-I", sdk / "platforms/android-36/android.jar",
        "--manifest", Path(__file__).with_name("AndroidManifest.xml"), "-o", unsigned,
    ], check=True)
    with zipfile.ZipFile(unsigned, "a", compression=zipfile.ZIP_STORED) as apk:
        apk.write(target / "aarch64-linux-android/release/examples/libdisplay_probe.so",
                  "lib/arm64-v8a/libdisplay_probe.so")
    aligned = build / "aligned.apk"
    subprocess.run([tools / "zipalign", "-f", "-P", "16", "4", unsigned, aligned], check=True)
    signed = build / "display-probe.apk"
    subprocess.run([
        tools / "apksigner", "sign", "--ks", Path.home() / ".android/debug.keystore",
        "--ks-pass", "pass:android", "--key-pass", "pass:android", "--out", signed, aligned,
    ], check=True)
    subprocess.run([tools / "apksigner", "verify", signed], check=True)
    print(signed)


if __name__ == "__main__":
    main()

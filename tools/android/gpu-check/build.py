#!/usr/bin/env python3
"""Build the Game Loop APK that runs the GPU tests on cloud devices.

Needs ANDROID_HOME, ANDROID_NDK_HOME, CARGO_TARGET_DIR, cargo-ndk and a JDK in
JAVA_HOME (javac, d8, apksigner); prints the signed APK's path.
"""
import json
import os
import shutil
from pathlib import Path
import subprocess
import sys
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from gpu_test import android_env, tests  # noqa: E402

NDK = ["cargo", "ndk", "-t", "arm64-v8a", "--platform", "29"]


def main():
    root = Path(__file__).resolve().parents[3]
    sdk = Path(os.environ["ANDROID_HOME"])
    ndk = Path(os.environ["ANDROID_NDK_HOME"])
    target = Path(os.environ["CARGO_TARGET_DIR"]).resolve()
    build = target / "gpu-check"
    build.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, RUSTC_WRAPPER="sccache")
    subprocess.run(NDK + ["build", "-p", "adv-gpu", "--example", "gpu-check", "--release"],
                   cwd=root, env=env, check=True)
    binaries = tests(root, android_env(env, ndk), "aarch64-linux-android")
    strip = next((ndk / "toolchains/llvm/prebuilt").iterdir()) / "bin/llvm-strip"
    tools = sdk / "build-tools" / "36.0.0"
    unsigned = build / "unsigned.apk"
    subprocess.run([
        tools / "aapt2", "link", "-I", sdk / "platforms/android-36/android.jar",
        "--manifest", Path(__file__).with_name("AndroidManifest.xml"), "-o", unsigned,
    ], check=True)
    # Test Lab rejects APKs without code; one empty class satisfies it.
    stub = build / "stub"
    shutil.rmtree(stub, ignore_errors=True)
    (stub / "org/hitode/gpucheck").mkdir(parents=True)
    (stub / "org/hitode/gpucheck/Stub.java").write_text(
        "package org.hitode.gpucheck;\nfinal class Stub {}\n")
    javac = Path(os.environ["JAVA_HOME"]) / "bin/javac"
    subprocess.run([javac, "--release", "11", "-d", stub, stub / "org/hitode/gpucheck/Stub.java"],
                   check=True)
    subprocess.run([tools / "d8", "--min-api", "29", "--output", stub,
                    stub / "org/hitode/gpucheck/Stub.class"], check=True)
    with zipfile.ZipFile(unsigned, "a", compression=zipfile.ZIP_DEFLATED) as apk:
        apk.write(stub / "classes.dex", "classes.dex")
        apk.write(target / "aarch64-linux-android/release/examples/libgpu_check.so",
                  "lib/arm64-v8a/libgpu_check.so")
        for name, executable in binaries:
            name = name.replace(":", "_").replace("-", "_")
            stripped = build / f"libtest_{name}.so"
            subprocess.run([strip, "-o", stripped, executable], check=True)
            apk.write(stripped, f"lib/arm64-v8a/libtest_{name}.so")
            print(f"packaged {name}")
    aligned = build / "aligned.apk"
    subprocess.run([tools / "zipalign", "-f", "-P", "16", "4", unsigned, aligned], check=True)
    signed = build / "gpu-check.apk"
    subprocess.run([
        tools / "apksigner", "sign", "--ks", Path.home() / ".android/debug.keystore",
        "--ks-pass", "pass:android", "--key-pass", "pass:android", "--out", signed, aligned,
    ], check=True)
    subprocess.run([tools / "apksigner", "verify", signed], check=True)
    print(signed)


if __name__ == "__main__":
    main()

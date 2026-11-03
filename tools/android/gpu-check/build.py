#!/usr/bin/env python3
"""Build the Game Loop APK that runs the GPU tests on cloud devices.

Needs ANDROID_HOME, ANDROID_NDK_HOME, CARGO_TARGET_DIR, cargo-ndk and a Java
runtime for apksigner; prints the signed APK's path.
"""
import json
import os
from pathlib import Path
import subprocess
import zipfile

# (package, features, test targets): the GPU suites of the native display path.
SUITES = [
    ("adv-gpu", [], ["--lib", "--test", "conformance"]),
    ("adv-display", [], ["--lib", "--test", "upscale", "--test", "video"]),
    ("adv-device", ["native-display"], ["--test", "surfaces", "--test", "software_ingest"]),
]
NDK = ["cargo", "ndk", "-t", "arm64-v8a", "--platform", "29"]


def tests(root, env, ndk):
    """Build every suite and return (name, executable) for each test binary.

    Plain cargo with the NDK toolchain, since cargo-ndk drops the JSON messages.
    """
    clang = next((ndk / "toolchains/llvm/prebuilt").iterdir()) / "bin/aarch64-linux-android29-clang"
    env = dict(env, CC_aarch64_linux_android=str(clang),
               CXX_aarch64_linux_android=f"{clang}++",
               AR_aarch64_linux_android=str(clang.with_name("llvm-ar")),
               CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=str(clang))
    found = []
    for package, features, targets in SUITES:
        command = ["cargo", "test", "--no-run", "--message-format=json",
                   "--target", "aarch64-linux-android", "-p", package, *targets]
        if features:
            command += ["--features", ",".join(features)]
        output = subprocess.run(command, cwd=root, env=env, check=True,
                                stdout=subprocess.PIPE, text=True).stdout
        for line in output.splitlines():
            message = json.loads(line)
            if message.get("reason") == "compiler-artifact" and message.get("executable"):
                name = message["target"]["name"].replace("-", "_")
                found.append((name, Path(message["executable"])))
    return found


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
    binaries = tests(root, env, ndk)
    strip = next((ndk / "toolchains/llvm/prebuilt").iterdir()) / "bin/llvm-strip"
    tools = sdk / "build-tools" / "36.0.0"
    unsigned = build / "unsigned.apk"
    subprocess.run([
        tools / "aapt2", "link", "-I", sdk / "platforms/android-36/android.jar",
        "--manifest", Path(__file__).with_name("AndroidManifest.xml"), "-o", unsigned,
    ], check=True)
    with zipfile.ZipFile(unsigned, "a", compression=zipfile.ZIP_DEFLATED) as apk:
        apk.write(target / "aarch64-linux-android/release/examples/libgpu_check.so",
                  "lib/arm64-v8a/libgpu_check.so")
        for name, executable in binaries:
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

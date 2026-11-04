#!/usr/bin/env python3
"""Run the native display path's GPU test suites on this machine or a phone.

  python3 tools/gpu_test.py host [FILTER ...]      # macOS Metal, or Linux with VK_ICD_FILENAMES
  python3 tools/gpu_test.py android [FILTER ...]   # the adb device (ANDROID_SERIAL picks one)

Every suite is built with its features and run with ADV_GPU_REQUIRE_BACKEND=1,
so a missing GPU fails instead of skipping. FILTERs go to every test binary.
Android needs ANDROID_NDK_HOME; CARGO_TARGET_DIR is honored as usual.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

# (package, features, test targets): the GPU suites of the native display path.
SUITES = [
    ("adv-gpu", [], ["--lib", "--test", "conformance"]),
    ("adv-display", [], ["--lib", "--test", "upscale", "--test", "video"]),
    ("adv-device", ["gpu", "test-support"], ["--lib", "--test", "surfaces", "--test", "software_ingest"]),
    ("adv-dx", ["gpu"], ["--test", "shader_gpu"]),
]
TIMEOUT = 900
REMOTE = "/data/local/tmp/gpu-test"


def android_env(env, ndk):
    """Plain cargo with the NDK toolchain, since cargo-ndk drops the JSON messages."""
    clang = next((ndk / "toolchains/llvm/prebuilt").iterdir()) / "bin/aarch64-linux-android29-clang"
    return dict(env, CC_aarch64_linux_android=str(clang),
                CXX_aarch64_linux_android=f"{clang}++",
                AR_aarch64_linux_android=str(clang.with_name("llvm-ar")),
                CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=str(clang))


def tests(root, env, target=None):
    """Build every suite and return (name, executable) for each test binary."""
    found = []
    for package, features, targets in SUITES:
        command = ["cargo", "test", "--no-run", "--message-format=json", "-p", package, *targets]
        if target:
            command += ["--target", target]
        if features:
            command += ["--features", ",".join(features)]
        output = subprocess.run(command, cwd=root, env=env, check=True,
                                stdout=subprocess.PIPE, text=True).stdout
        for line in output.splitlines():
            message = json.loads(line)
            if message.get("reason") == "compiler-artifact" and message.get("executable"):
                name = f"{package}:{message['target']['name']}"
                found.append((name, Path(message["executable"])))
    return found


def run(name, command):
    try:
        result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                text=True, timeout=TIMEOUT)
    except subprocess.TimeoutExpired:
        print(f"TIMEOUT {name} after {TIMEOUT} s")
        return False
    summary = [line for line in result.stdout.splitlines() if line.startswith("test result")]
    if result.returncode != 0:
        print(result.stdout)
        print(f"FAILED {name}")
        return False
    print(f"ok     {name}: {summary[-1][len('test result: ok. '):] if summary else 'no tests'}")
    return True


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("where", choices=["host", "android"])
    parser.add_argument("filters", nargs="*")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    env = dict(os.environ, ADV_GPU_REQUIRE_BACKEND="1")
    if args.where == "host":
        binaries = tests(root, env)
        results = [run(name, [str(path), *args.filters]) for name, path in binaries]
    else:
        env = android_env(env, Path(os.environ["ANDROID_NDK_HOME"]))
        binaries = tests(root, env, "aarch64-linux-android")
        subprocess.run(["adb", "shell", "mkdir", "-p", REMOTE], check=True)
        results = []
        for name, path in binaries:
            remote = f"{REMOTE}/{path.name}"
            subprocess.run(["adb", "push", "-q", str(path), remote], check=True)
            filters = " ".join(args.filters)
            results.append(run(name, ["adb", "shell",
                                      f"cd {REMOTE} && ADV_GPU_REQUIRE_BACKEND=1 ./{path.name} {filters}"]))
    failed = results.count(False)
    print(f"{len(results) - failed} of {len(results)} test binaries passed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()

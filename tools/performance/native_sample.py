#!/usr/bin/env python3
"""Sample a game running natively (32-bit under WOW64, or 64-bit) for a baseline.

Usage (Windows, 64-bit Python):
  python tools/performance/native_sample.py sample RUN_DIRECTORY (--pid PID | --name GAME.exe)
         [--seconds 10] [--interval-ms 1] [--threads 1] [--while-key ctrl] [--wait]
  python tools/performance/native_sample.py report RUN_DIRECTORY [--top 25] [--llvm DIR]

`sample` measures every thread's CPU time over the window and suspends the
--threads busiest ones (ranked over half a second) about once per interval to
read their program counter. --while-key waits until the key is held, then
samples only while it stays held (e.g. ctrl while the game skips). --wait
waits for --name to start. It writes into RUN_DIRECTORY:
  native-samples.tsv  perf_ns, tid, pc
  native-sample.json  {"pid", "wow64", "wall_seconds", "threads": [{"tid",
                      "cpu_seconds", "sampled"}], "modules": [{"base", "size",
                      "path"}]}

`report` prints CPU per thread (a native game spreads work over threads that
adv-run runs on one), then per sampled thread the share of each module and the
hottest functions as module!name+offset. Names come from exports; addresses
between exports are named after the nearest direct call target (sub_RVA),
found by tools/survey/module_profile.py when llvm-objdump is available.

Suspending threads perturbs timing: read shares, not absolute times. Run
directories hold guest addresses and stay local (docs/investigation.md §2).
"""

from __future__ import annotations

import argparse
import bisect
from collections import Counter, defaultdict
import ctypes as C
from ctypes import wintypes as W
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import time

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "survey"))

KEYS = {"ctrl": 0x11, "control": 0x11, "shift": 0x10, "alt": 0x12, "space": 0x20,
        "enter": 0x0D, "return": 0x0D}
# WOW64_CONTEXT: ContextFlags at 0, Eip at 0xB8; CONTEXT (x64): flags at 48, Rip at 248.
WOW64_CONTEXT_SIZE, WOW64_CONTROL, EIP_OFFSET = 0x2CC, 0x10001, 0xB8
CONTEXT_SIZE, CONTEXT_FLAGS_OFFSET, CONTROL, RIP_OFFSET = 1232, 48, 0x100001, 248
OUTSIDE = "(outside modules)"


# ---------------------------------------------------------------- sampling

def windows():
    """Bindings, loaded lazily so `report` and the tests run anywhere."""
    import pc_sample as pc

    kernel = pc.kernel
    api = pc.api
    return {
        "pc": pc,
        "IsWow64Process": api(kernel, "IsWow64Process", W.BOOL, W.HANDLE, C.POINTER(W.BOOL)),
        "Wow64SuspendThread": api(kernel, "Wow64SuspendThread", W.DWORD, W.HANDLE),
        "Wow64GetThreadContext": api(kernel, "Wow64GetThreadContext", W.BOOL, W.HANDLE, C.c_void_p),
        "GetAsyncKeyState": api(C.WinDLL("user32"), "GetAsyncKeyState", C.c_short, C.c_int),
    }


def find_process(pc, name, wait):
    wanted = name.lower()
    while True:
        snapshot = pc.Snapshot(pc.SNAPSHOT_PROCESSES, 0)
        entry = pc.ProcessEntry(size=C.sizeof(pc.ProcessEntry))
        found = []
        try:
            more = pc.FirstProcess(snapshot, C.byref(entry))
            while more:
                if entry.name.lower() == wanted:
                    found.append(entry.pid)
                more = pc.NextProcess(snapshot, C.byref(entry))
        finally:
            pc.CloseHandle(snapshot)
        if len(found) == 1:
            return found[0]
        if len(found) > 1:
            raise SystemExit(f"{len(found)} processes named {name}; pass --pid")
        if not wait:
            raise SystemExit(f"no process named {name}; start it or pass --wait")
        time.sleep(0.5)


def open_threads(pc, pid):
    snapshot = pc.Snapshot(pc.SNAPSHOT_THREADS, 0)
    entry = pc.ThreadEntry(size=C.sizeof(pc.ThreadEntry))
    threads = {}
    try:
        more = pc.FirstThread(snapshot, C.byref(entry))
        while more:
            if entry.pid == pid:
                handle = pc.OpenThread(pc.THREAD_SUSPEND_CONTEXT_QUERY, False, entry.tid)
                if handle:
                    threads[entry.tid] = handle
            more = pc.NextThread(snapshot, C.byref(entry))
    finally:
        pc.CloseHandle(snapshot)
    return threads


def cpu_seconds(pc, handle):
    try:
        return pc.cpu_time(handle) / 1e7
    except OSError:  # the thread exited
        return None


class Reader:
    """Suspends one thread and reads its program counter, WOW64 or native."""

    def __init__(self, bindings, wow64):
        self.pc = bindings["pc"]
        size = WOW64_CONTEXT_SIZE if wow64 else CONTEXT_SIZE
        self.storage = C.create_string_buffer(size + 15)
        self.context = (C.addressof(self.storage) + 15) & ~15
        self.size = size
        if wow64:
            self.suspend, self.get = bindings["Wow64SuspendThread"], bindings["Wow64GetThreadContext"]
            self.flags_at, self.flags, self.read = 0, WOW64_CONTROL, (EIP_OFFSET, C.c_uint32)
        else:
            self.suspend, self.get = self.pc.SuspendThread, self.pc.GetThreadContext
            self.flags_at, self.flags, self.read = CONTEXT_FLAGS_OFFSET, CONTROL, (RIP_OFFSET, C.c_uint64)

    def __call__(self, handle):
        C.memset(self.context, 0, self.size)
        C.c_uint32.from_address(self.context + self.flags_at).value = self.flags
        if self.suspend(handle) == 0xFFFFFFFF:
            return None
        try:
            if not self.get(handle, self.context):
                return None
            offset, kind = self.read
            return kind.from_address(self.context + offset).value
        finally:
            self.pc.ResumeThread(handle)


def key_code(text):
    if text is None:
        return None
    return KEYS.get(text.lower()) or int(text, 0)


def sample(arguments):
    if struct.calcsize("P") != 8:
        raise SystemExit("run this with 64-bit Python")
    bindings = windows()
    pc = bindings["pc"]
    pid = arguments.pid or find_process(pc, arguments.name, arguments.wait)
    process = pc.check(pc.OpenProcess(pc.PROCESS_QUERY_READ, False, pid))
    flag = W.BOOL()
    pc.check(bindings["IsWow64Process"](process, C.byref(flag)))
    wow64 = bool(flag.value)
    key = key_code(arguments.while_key)
    held = lambda: bindings["GetAsyncKeyState"](key) & 0x8000 != 0
    if key is not None:
        print(f"process {pid}: waiting for key {key:#x} to be held", flush=True)
        while not held():
            time.sleep(0.02)
    threads = open_threads(pc, pid)
    start = {tid: cpu_seconds(pc, handle) for tid, handle in threads.items()}
    time.sleep(0.5)
    recent = sorted(((cpu_seconds(pc, h) or 0) - (start[t] or 0), t) for t, h in threads.items())
    chosen = [tid for _, tid in recent[::-1][:arguments.threads]]
    read = Reader(bindings, wow64)
    rows = []
    start = {tid: cpu_seconds(pc, handle) for tid, handle in threads.items()}
    began = time.perf_counter()
    end = began + arguments.seconds
    while time.perf_counter() < end and (key is None or held()):
        time.sleep(arguments.interval_ms / 1000)
        for tid in chosen:
            at = read(threads[tid])
            if at is not None:
                rows.append((time.perf_counter_ns(), tid, at))
    wall = time.perf_counter() - began
    finished = {tid: cpu_seconds(pc, handle) for tid, handle in threads.items()}
    directory = arguments.run_directory
    directory.mkdir(parents=True, exist_ok=True)
    with (directory / "native-samples.tsv").open("w", encoding="utf-8") as stream:
        stream.write("perf_ns\ttid\tpc\n")
        stream.writelines(f"{t}\t{tid}\t{at}\n" for t, tid, at in rows)
    used = [{"tid": tid, "cpu_seconds": round(finished[tid] - start[tid], 4), "sampled": tid in chosen}
            for tid in threads if start[tid] is not None and finished[tid] is not None]
    used.sort(key=lambda row: -row["cpu_seconds"])
    (directory / "native-sample.json").write_text(json.dumps({
        "pid": pid, "wow64": wow64, "wall_seconds": round(wall, 4), "threads": used,
        "modules": pc.modules(process)}, indent=1), encoding="utf-8")
    for handle in threads.values():
        pc.CloseHandle(handle)
    pc.CloseHandle(process)
    print(f"process {pid} ({'WOW64' if wow64 else 'native'}): {len(rows)} samples "
          f"of {len(chosen)} thread(s) over {wall:.1f} s")


# ---------------------------------------------------------------- report

class Names:
    """module!name+offset for addresses, from exports and approximate function starts."""

    def __init__(self, modules, llvm=None, disassemble=True):
        self.modules = sorted(modules, key=lambda m: m["base"])
        self.bases = [m["base"] for m in self.modules]
        self.llvm, self.disassemble = llvm, disassemble
        self.tables = {}

    def module(self, address):
        index = bisect.bisect_right(self.bases, address) - 1
        if index >= 0 and address < self.modules[index]["base"] + self.modules[index]["size"]:
            return self.modules[index]
        return None

    def table(self, path):
        """Sorted [(rva, name)] for one module file; empty if unreadable."""
        if path not in self.tables:
            self.tables[path] = function_table(path, self.llvm, self.disassemble)
        return self.tables[path]

    def function(self, address):
        module = self.module(address)
        if module is None:  # JIT-compiled or unpacked code
            return OUTSIDE, OUTSIDE
        name = os.path.basename(module["path"])
        rva = address - module["base"]
        table = self.table(module["path"])
        index = bisect.bisect_right(table, (rva, "￿")) - 1
        if index < 0:
            return name, f"{name}+{rva:#x}"
        start, function = table[index]
        return name, f"{name}!{function}+{rva - start:#x}"


def function_table(path, llvm=None, disassemble=True):
    try:
        import module_profile as profile

        module = profile.Module(Path(path).read_bytes())
    except (OSError, ValueError, struct.error):
        return []
    named = dict(module.exports())
    starts = set(named)
    if disassemble:
        try:
            starts.update(profile.function_starts(module, profile.instructions(Path(path), llvm)))
        except (SystemExit, OSError, subprocess.CalledProcessError):  # no LLVM, or a refused file
            pass
    return sorted((rva, named.get(rva, f"sub_{rva:x}")) for rva in starts)


def report(directory, top, names):
    meta = json.loads((directory / "native-sample.json").read_text(encoding="utf-8"))
    rows = [line.split("\t") for line in
            (directory / "native-samples.tsv").read_text(encoding="utf-8").splitlines()[1:]]
    wall = meta["wall_seconds"] or 1
    print(f"process {meta['pid']} ({'WOW64' if meta['wow64'] else 'native'}), "
          f"{wall:.1f} s window, {len(rows)} samples")
    print("\nthreads by CPU (100% = one core)")
    total = sum(t["cpu_seconds"] for t in meta["threads"])
    for thread in meta["threads"]:
        if thread["cpu_seconds"] >= 0.01 * wall:
            print(f"  {thread['tid']:>7} {thread['cpu_seconds'] * 100 / wall:6.1f}%"
                  f"{'  sampled' if thread['sampled'] else ''}")
    print(f"  all     {total * 100 / wall:6.1f}%")
    by_thread = defaultdict(list)
    for _, tid, at in rows:
        by_thread[int(tid)].append(int(at))
    for tid, addresses in by_thread.items():
        modules, functions = Counter(), Counter()
        for address in addresses:
            module, function = names.function(address)
            modules[module] += 1
            functions[function.rsplit("+", 1)[0]] += 1
        count = len(addresses)
        print(f"\nthread {tid}: {count} samples")
        for module, hits in modules.most_common(top):
            print(f"  {hits * 100 / count:5.1f}%  {module}")
        print("  hottest functions:")
        for function, hits in functions.most_common(top):
            print(f"  {hits * 100 / count:5.1f}%  {function}")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    run = commands.add_parser("sample", help="sample a running process")
    run.add_argument("run_directory", type=Path)
    target = run.add_mutually_exclusive_group(required=True)
    target.add_argument("--pid", type=int)
    target.add_argument("--name", help="executable name, e.g. game.exe")
    run.add_argument("--wait", action="store_true", help="wait for --name to start")
    run.add_argument("--seconds", type=float, default=10.0)
    run.add_argument("--interval-ms", type=float, default=1.0)
    run.add_argument("--threads", type=int, default=1, help="busiest threads to sample")
    run.add_argument("--while-key", help="sample only while this key is held: "
                                         "ctrl, shift, alt, space, enter or a VK code")
    show = commands.add_parser("report", help="summarize a sampled run directory")
    show.add_argument("run_directory", type=Path)
    show.add_argument("--top", type=int, default=25)
    show.add_argument("--llvm", help="directory holding llvm-objdump")
    show.add_argument("--exports-only", action="store_true",
                      help="name addresses by exports only (no disassembly)")
    arguments = parser.parse_args(argv)
    if arguments.command == "sample":
        sample(arguments)
    else:
        meta = json.loads((arguments.run_directory / "native-sample.json").read_text(encoding="utf-8"))
        report(arguments.run_directory, arguments.top,
               Names(meta["modules"], arguments.llvm, not arguments.exports_only))


if __name__ == "__main__":
    main()

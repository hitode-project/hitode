#!/usr/bin/env python3
"""Sample the busiest thread of a running adv-run by suspending it periodically.

Usage (Windows):
  python tools/performance/pc_sample.py RUN_DIRECTORY [--pid PID] [--seconds 10]
                                        [--dump-regions 1011db06,1011de9d]

Writes into RUN_DIRECTORY, the same directory the runner was given as
ADV_JIT_MAP_DIR (all local output; none of it belongs in the repository):
  pc-samples.tsv       perf_ns, unix_ns, rip, thread_cpu_100ns, suspend_ns
  sample-modules.json  {"pid", "tid", "modules": [{"base", "size", "path"}]}
  native-*.bin, native-dumps.json
                       machine code of the newest native region compiled for
                       each --dump-regions guest start, for native_hotspots.py

The sampled thread is the one with the most CPU time. Each sample suspends it
briefly, which perturbs timing; compare against an unsampled control run.
Attribute the result with attribute_pc.py.
"""

import argparse
import ctypes as C
from ctypes import wintypes as W
import json
from pathlib import Path
import time

kernel = C.WinDLL("kernel32", use_last_error=True)
psapi = C.WinDLL("psapi", use_last_error=True)


def api(library, name, result, *arguments):
    function = getattr(library, name)
    function.restype = result
    function.argtypes = arguments
    return function


class ThreadEntry(C.Structure):
    _fields_ = [("size", W.DWORD), ("usage", W.DWORD), ("tid", W.DWORD),
                ("pid", W.DWORD), ("base", W.LONG), ("delta", W.LONG),
                ("flags", W.DWORD)]


class ProcessEntry(C.Structure):
    _fields_ = [("size", W.DWORD), ("usage", W.DWORD), ("pid", W.DWORD),
                ("heap", C.c_void_p), ("module", W.DWORD), ("threads", W.DWORD),
                ("parent", W.DWORD), ("priority", W.LONG), ("flags", W.DWORD),
                ("name", W.WCHAR * 260)]


class ModuleInfo(C.Structure):
    _fields_ = [("base", C.c_void_p), ("size", W.DWORD), ("entry", C.c_void_p)]


OpenProcess = api(kernel, "OpenProcess", W.HANDLE, W.DWORD, W.BOOL, W.DWORD)
ReadMemory = api(kernel, "ReadProcessMemory", W.BOOL, W.HANDLE, C.c_void_p,
                 C.c_void_p, C.c_size_t, C.POINTER(C.c_size_t))
OpenThread = api(kernel, "OpenThread", W.HANDLE, W.DWORD, W.BOOL, W.DWORD)
CloseHandle = api(kernel, "CloseHandle", W.BOOL, W.HANDLE)
SuspendThread = api(kernel, "SuspendThread", W.DWORD, W.HANDLE)
ResumeThread = api(kernel, "ResumeThread", W.DWORD, W.HANDLE)
GetThreadContext = api(kernel, "GetThreadContext", W.BOOL, W.HANDLE, C.c_void_p)
GetThreadTimes = api(kernel, "GetThreadTimes", W.BOOL, W.HANDLE,
                     *[C.POINTER(W.FILETIME)] * 4)
Snapshot = api(kernel, "CreateToolhelp32Snapshot", W.HANDLE, W.DWORD, W.DWORD)
FirstThread = api(kernel, "Thread32First", W.BOOL, W.HANDLE, C.POINTER(ThreadEntry))
NextThread = api(kernel, "Thread32Next", W.BOOL, W.HANDLE, C.POINTER(ThreadEntry))
FirstProcess = api(kernel, "Process32FirstW", W.BOOL, W.HANDLE, C.POINTER(ProcessEntry))
NextProcess = api(kernel, "Process32NextW", W.BOOL, W.HANDLE, C.POINTER(ProcessEntry))
EnumModules = api(psapi, "EnumProcessModulesEx", W.BOOL, W.HANDLE,
                  C.POINTER(W.HMODULE), W.DWORD, C.POINTER(W.DWORD), W.DWORD)
GetModuleInformation = api(psapi, "GetModuleInformation", W.BOOL, W.HANDLE,
                           W.HMODULE, C.POINTER(ModuleInfo), W.DWORD)
ModuleName = api(psapi, "GetModuleFileNameExW", W.DWORD, W.HANDLE, W.HMODULE,
                 W.LPWSTR, W.DWORD)

PROCESS_QUERY_READ = 0x0410
THREAD_SUSPEND_CONTEXT_QUERY = 0x004A
SNAPSHOT_PROCESSES, SNAPSHOT_THREADS = 2, 4
CONTEXT_SIZE = 1232
CONTEXT_FLAGS_OFFSET, CONTEXT_CONTROL = 48, 0x100001
RIP_OFFSET = 248


def check(ok):
    if not ok:
        raise C.WinError(C.get_last_error())
    return ok


def cpu_time(thread):
    created, exited, system, user = (W.FILETIME() for _ in range(4))
    check(GetThreadTimes(thread, *(C.byref(t) for t in (created, exited, system, user))))
    ticks = lambda t: (t.dwHighDateTime << 32) | t.dwLowDateTime
    return ticks(system) + ticks(user)


def runner_pid():
    snapshot = Snapshot(SNAPSHOT_PROCESSES, 0)
    entry = ProcessEntry(size=C.sizeof(ProcessEntry))
    found = []
    try:
        more = FirstProcess(snapshot, C.byref(entry))
        while more:
            if entry.name.lower() == "adv-run.exe":
                found.append(entry.pid)
            more = NextProcess(snapshot, C.byref(entry))
    finally:
        CloseHandle(snapshot)
    if len(found) != 1:
        raise SystemExit(f"expected one adv-run.exe, found {len(found)}; pass --pid")
    return found[0]


def busiest_thread(pid):
    snapshot = Snapshot(SNAPSHOT_THREADS, 0)
    entry = ThreadEntry(size=C.sizeof(ThreadEntry))
    candidates = []
    try:
        more = FirstThread(snapshot, C.byref(entry))
        while more:
            if entry.pid == pid:
                handle = OpenThread(THREAD_SUSPEND_CONTEXT_QUERY, False, entry.tid)
                if handle:
                    candidates.append((cpu_time(handle), entry.tid, handle))
            more = NextThread(snapshot, C.byref(entry))
    finally:
        CloseHandle(snapshot)
    if not candidates:
        raise SystemExit(f"no accessible thread in process {pid}")
    # Rank by recent CPU time: a compile thread busy during startup can have
    # more total time than the guest thread that is busy now.
    time.sleep(0.5)
    candidates = [(cpu_time(handle) - start, tid, handle) for start, tid, handle in candidates]
    candidates.sort(reverse=True)
    for _, _, handle in candidates[1:]:
        CloseHandle(handle)
    return candidates[0][1], candidates[0][2]


def modules(process):
    handles = (W.HMODULE * 1024)()
    needed = W.DWORD()
    result = []
    if EnumModules(process, handles, C.sizeof(handles), C.byref(needed), 3):
        for handle in handles[:needed.value // C.sizeof(W.HMODULE)]:
            info = ModuleInfo()
            name = C.create_unicode_buffer(32768)
            if (GetModuleInformation(process, handle, C.byref(info), C.sizeof(info))
                    and ModuleName(process, handle, name, len(name))):
                result.append({"base": info.base, "size": info.size, "path": name.value})
    return result


def sample(thread, seconds, interval):
    storage = C.create_string_buffer(CONTEXT_SIZE + 15)
    context = (C.addressof(storage) + 15) & ~15
    rows = []
    end = time.perf_counter() + seconds
    while time.perf_counter() < end:
        time.sleep(interval)
        C.memset(context, 0, CONTEXT_SIZE)
        C.c_uint32.from_address(context + CONTEXT_FLAGS_OFFSET).value = CONTEXT_CONTROL
        before, wall = time.perf_counter_ns(), time.time_ns()
        if SuspendThread(thread) == 0xFFFFFFFF:
            break
        try:
            check(GetThreadContext(thread, context))
            rip = C.c_uint64.from_address(context + RIP_OFFSET).value
            cpu = cpu_time(thread)
        finally:
            check(ResumeThread(thread) != 0xFFFFFFFF)
        rows.append((before, wall, rip, cpu, time.perf_counter_ns() - before))
    return rows


def dump_regions(process, pid, directory, wanted):
    """Copies the newest native code of each wanted guest region start."""
    latest = {}
    path = directory / f"jit-{pid}.tsv"
    if path.exists():
        for line in path.read_text().splitlines():
            fields = line.split("\t")
            ns, base, size, guest = map(int, fields[:4])
            if guest in wanted:
                latest[guest] = (ns, base, size, fields[4])
    dumps = []
    for guest, (ns, base, size, mode) in sorted(latest.items()):
        data = C.create_string_buffer(size)
        read = C.c_size_t()
        if ReadMemory(process, base, data, size, C.byref(read)) and read.value == size:
            name = f"native-{guest:08x}-{mode}.bin"
            (directory / name).write_bytes(data.raw)
            dumps.append({"guest": guest, "ns": ns, "base": base, "size": size, "mode": mode,
                          "file": name})
    (directory / "native-dumps.json").write_text(json.dumps(dumps, indent=2))
    return dumps


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("run_directory", type=Path)
    parser.add_argument("--pid", type=int)
    parser.add_argument("--seconds", type=float, default=10.0)
    parser.add_argument("--interval-ms", type=float, default=1.0)
    parser.add_argument("--dump-regions", default="",
                        help="comma-separated hexadecimal guest region starts")
    arguments = parser.parse_args()
    directory = arguments.run_directory
    directory.mkdir(parents=True, exist_ok=True)
    pid = arguments.pid or runner_pid()
    process = check(OpenProcess(PROCESS_QUERY_READ, False, pid))
    tid, thread = busiest_thread(pid)
    try:
        (directory / "sample-modules.json").write_text(json.dumps(
            {"pid": pid, "tid": tid, "modules": modules(process)}, indent=2), encoding="utf-8")
        rows = sample(thread, arguments.seconds, arguments.interval_ms / 1000)
        with (directory / "pc-samples.tsv").open("w", encoding="utf-8") as stream:
            stream.write("perf_ns\tunix_ns\trip\tthread_cpu_100ns\tsuspend_ns\n")
            for row in rows:
                stream.write("\t".join(map(str, row)) + "\n")
        wanted = {int(x, 16) for x in arguments.dump_regions.split(",") if x}
        dumps = dump_regions(process, pid, directory, wanted) if wanted else []
    finally:
        CloseHandle(thread)
        CloseHandle(process)
    busy = (rows[-1][3] - rows[0][3]) * 100 / max(rows[-1][0] - rows[0][0], 1) * 100 if rows else 0
    print(f"{len(rows)} samples of thread {tid}, {busy:.0f}% busy, {len(dumps)} native dumps")


if __name__ == "__main__":
    main()

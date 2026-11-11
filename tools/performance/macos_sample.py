#!/usr/bin/env python3
"""Time-sliced macOS sampling of a running Hitode process, attributed to guest code.

Usage:
  python3 tools/performance/macos_sample.py record RUN_DIRECTORY --pid PID [--seconds 30]
  python3 tools/performance/macos_sample.py report RUN_DIRECTORY [--thread adv-execution]
                                            [--from MS --to MS] [--top 15]
                                            [--instructions]

record runs /usr/bin/sample for consecutive one-second slices and writes
slice-<unix ms>.txt files into RUN_DIRECTORY. Start the process with
ADV_JIT_MAP_DIR=RUN_DIRECTORY so its jit-<pid>.tsv maps generated code back to
guest region starts; without it JIT samples stay unattributed.

report prints, per slice, the threads' busy share and their share in generated
code, then the guest regions hottest over the chosen slices (all by default).
--thread matches every thread whose name contains it, such as all workers.
--instructions also attributes JIT samples to the guest instruction and part
of its code that insn-<pid>.tsv records, which a large region needs: a region
start says little about where inside it time went.
Read shares, not times: each slice also contains the sampler's attach.
Output stays local; none of it belongs in the repository.
"""

import argparse
from collections import Counter
from pathlib import Path
import re
import subprocess
import time

from attribute_pc import JitMap

IDLE = {"poll", "mach_msg2_trap", "__psynch_cvwait", "__workq_kernreturn", "kevent_id",
        "kevent", "semaphore_wait_trap", "semaphore_wait_signal_trap", "__select",
        "__pselect", "__semwait_signal", "__ulock_wait", "__ulock_wait2"}
NODE = re.compile(r"^([ +!:|]*)(\d+) (.*)$")
ADDRESS = re.compile(r"\[(0x[0-9a-f]+)\]")


def thread_leaves(text, thread):
    """(name, address or None, count) for each leaf of the threads whose headers contain `thread`."""
    lines = text.split("\n")
    try:
        start = lines.index("Call graph:")
    except ValueError:
        return []
    nodes, inside = [], False
    for line in lines[start + 1:]:
        if line.startswith("Total number in stack"):
            break
        if not line.strip():
            continue
        if re.match(r"^    \d+ Thread_", line):
            inside = thread in line
            continue
        found = NODE.match(line)
        if inside and found:
            label = found.group(3)
            address = ADDRESS.search(label)
            name = re.sub(r"\s+\(in .*", "", label).strip()
            nodes.append((len(found.group(1)), int(found.group(2)), name,
                          int(address.group(1), 16) if address else None))
    children = [0] * len(nodes)
    stack = []
    for index, (depth, count, _, _) in enumerate(nodes):
        while stack and nodes[stack[-1]][0] >= depth:
            stack.pop()
        if stack:
            children[stack[-1]] += count
        stack.append(index)
    return [(name, address, count - children[index])
            for index, (_, count, name, address) in enumerate(nodes) if count > children[index]]


def record(directory, pid, seconds):
    directory.mkdir(parents=True, exist_ok=True)
    for _ in range(seconds):
        path = directory / f"slice-{int(time.time() * 1000)}.txt"
        done = subprocess.run(["/usr/bin/sample", str(pid), "1", "1", "-mayDie", "-file", str(path)],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if done.returncode:
            break


def report(directory, thread, start, end, top, instructions):
    jit = JitMap(directory)
    regions = Counter()
    insts, parts = Counter(), Counter()
    slices = sorted(directory.glob("slice-*.txt"), key=lambda p: int(p.stem.split("-")[1]))
    first = int(slices[0].stem.split("-")[1]) if slices else 0
    print("t_ms\tbusy%\tjit%\ttop_region")
    for path in slices:
        at = int(path.stem.split("-")[1])
        leaves = thread_leaves(path.read_text(encoding="utf-8", errors="replace"), thread)
        total = sum(n for _, _, n in leaves)
        if not total:
            continue
        busy = sum(n for name, _, n in leaves if name not in IDLE)
        here, inst, part = Counter(), Counter(), Counter()
        jitted = 0
        for name, address, count in leaves:
            if name.startswith("???") and address is not None:
                jitted += count
                found = jit.lookup(address, at * 1_000_000 + 1_000_000_000)
                here[f"{found['region']:08x}" if found else "unmapped"] += count
                if found and found.get("pc"):
                    inst[f"{found['pc']:08x} {found['part']}"] += count
                    part[found["part"]] += count
        best = here.most_common(1)[0][0] if here else "-"
        print(f"{at - first}\t{100 * busy // total}\t{100 * jitted // total}\t{best}")
        if (start is None or at - first >= start) and (end is None or at - first <= end):
            regions.update(here)
            insts.update(inst)
            parts.update(part)
    total = sum(regions.values())
    print(f"\nguest regions over the chosen slices ({total} JIT samples)")
    for region, count in regions.most_common(top):
        print(f"{count:7}  {100 * count / max(total, 1):5.1f}%  {region}")
    if instructions:
        for title, counter in (("parts of generated code", parts),
                               ("guest instructions and the part of their code", insts)):
            print(f"\n{title}")
            for key, count in counter.most_common(top):
                print(f"{count:7}  {100 * count / max(total, 1):5.1f}%  {key}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    recording = commands.add_parser("record")
    recording.add_argument("directory", type=Path)
    recording.add_argument("--pid", type=int, required=True)
    recording.add_argument("--seconds", type=int, default=30)
    reporting = commands.add_parser("report")
    reporting.add_argument("directory", type=Path)
    reporting.add_argument("--thread", default="adv-execution")
    reporting.add_argument("--from", dest="start", type=int)
    reporting.add_argument("--to", dest="end", type=int)
    reporting.add_argument("--top", type=int, default=15)
    reporting.add_argument("--instructions", action="store_true")
    args = parser.parse_args()
    if args.command == "record":
        record(args.directory, args.pid, args.seconds)
    else:
        report(args.directory, args.thread, args.start, args.end, args.top, args.instructions)

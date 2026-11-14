#!/usr/bin/env python3
"""Time-sliced macOS sampling of a running Hitode process, attributed to guest code.

Usage:
  python3 tools/performance/macos_sample.py record RUN_DIRECTORY --pid PID [--seconds 30]
  python3 tools/performance/macos_sample.py report RUN_DIRECTORY [--thread adv-execution]
                                            [--from MS --to MS] [--top 15]
                                            [--instructions]
  python3 tools/performance/macos_sample.py code RUN_DIRECTORY [--thread adv-execution]
                                            [--region HEX] [--from MS --to MS]
                                            [--listing 5]
  python3 tools/performance/macos_sample.py loops RUN_DIRECTORY --region HEX
  python3 tools/performance/macos_sample.py coverage RUN_DIRECTORY... [--thread adv-execution]
                                            [--top 15]

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

code splits the generated code of one guest region (the hottest by default)
by what each host instruction does, from the code-<pid>.bin the runner writes
beside its maps: guest memory, CpuState and Runtime accesses, branches, flags,
the countdown, x87 state words, floating-point arithmetic and moves, compares
and other integer work (register roles from A64-REG-1). Static counts are one
pass over each guest instruction's inline code (without Cold, X87Publish,
X87Wide and SimdPublish), in the unit holding the most samples; sampled counts cover every unit of the region. --listing prints
the hottest guest instructions' code with per-instruction samples. Needs the
capstone Python package.

loops needs no samples. For every unit placed for one guest region, ordinary
(mode a64) or a computation's optimized form (a64-opt), it lists each loop a
backward branch closes, innermost first, with its host instructions split into
stack accesses (spills and reloads), other memory, branches and the rest. Two
forms of the same loop compare by these static counts.
coverage joins the JIT samples with the loop steps of opt-<pid>.tsv. Per hot
region it gives the share of its samples in optimized forms, and for the rest
the loop that encloses it and the furthest step that loop reached: installed
(the form exists but runs were not admitted), a proof or form refusal with the
instruction, only promoted, or no loop at all. Several run directories also get
a summary by step, each run weighted equally, which ranks what to cover next.
A samples.tsv in the directory, the live debugger's `samples` output (run with
ADV_DEBUG=1), takes the place of the slices: that sampler runs in process and
keeps its rate on a loaded machine.
Read shares, not times: each slice also contains the sampler's attach.
Output stays local; none of it belongs in the repository.
"""

import argparse
import bisect
from collections import Counter, defaultdict
from pathlib import Path
import re
import subprocess
import time

from attribute_pc import JitMap

IDLE = {"poll", "mach_msg2_trap", "__psynch_cvwait", "__workq_kernreturn", "kevent_id",
        "kevent", "semaphore_wait_trap", "semaphore_wait_signal_trap", "__select",
        "__pselect", "__semwait_signal", "__ulock_wait", "__ulock_wait2", "__psynch_mutexwait"}
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


LOOP_STEPS = ("promoted", "extent")
DONE = ("installed", "built", "requested")


def loop_steps(directory):
    """Per loop entry its opt-<pid>.tsv steps in order, and each head's extent end."""
    steps, extents = defaultdict(list), {}
    for path in sorted(directory.glob("opt-*.tsv")):
        for line in path.read_text(encoding="utf-8").splitlines():
            _, eip, step, at, detail = (line.split("\t") + ["-", "-"])[:5]
            if step == "extent":
                extents[int(eip)] = max(extents.get(int(eip), 0), int(at))
            steps[int(eip)].append((step, at, detail))
    return steps, extents


def furthest(steps):
    """The furthest step an entry reached, with the refused instruction if any."""
    names = [step for step, _, _ in steps]
    for done in DONE[:2]:
        if done in names:
            return done, ""
    # A request ends in a form or its reject.
    refusals = [s for s in steps if s[0] not in LOOP_STEPS]
    if refusals:
        step, at, detail = refusals[-1]
        where = f"{int(at):08x} " if at.isdigit() and int(at) else ""
        return step, where + detail
    return ("promoted" if "promoted" in names else "no loop"), ""


def enclosing(region, steps, extents):
    """The loop entry that speaks for a region: itself, else the enclosing
    extent that got furthest (installed first), else the widest."""
    if any(step not in LOOP_STEPS for step, _, _ in steps.get(region, ())):
        return region
    heads = [head for head, end in extents.items() if head <= region < end]
    if not heads:
        return region if region in steps else None
    installed = [h for h in heads if furthest(steps[h])[0] in DONE]
    return max(installed or heads, key=lambda head: extents[head] - head)


def jit_samples(directory, thread, jit):
    """(JIT map entry or None, count) for each sample in generated code."""
    debugger = directory / "samples.tsv"
    if debugger.exists():
        lines = debugger.read_text(encoding="utf-8").splitlines()
        start = next((i for i, line in enumerate(lines) if line.startswith("unix_us\t")), len(lines))
        header = lines[start].split("\t") if start < len(lines) else []
        for line in lines[start + 1:]:
            row = dict(zip(header, line.split("\t")))
            if row.get("kind") == "Guest" and row.get("pc", "-") != "-":
                yield jit.lookup(int(row["pc"], 16), int(row["unix_us"]) * 1000), 1
        return
    for path in sorted(directory.glob("slice-*.txt")):
        at = int(path.stem.split("-")[1])
        for name, address, count in thread_leaves(path.read_text(encoding="utf-8", errors="replace"), thread):
            if name.startswith("???") and address is not None:
                yield jit.lookup(address, at * 1_000_000 + 1_000_000_000), count


def coverage(directories, thread, top):
    summary, runs = Counter(), 0
    for directory in directories:
        jit = JitMap(directory)
        steps, extents = loop_steps(directory)
        regions, optimized = Counter(), Counter()
        for found, count in jit_samples(directory, thread, jit):
            region = found["region"] if found else None
            regions[region] += count
            if found and found["mode"] == "a64-opt":
                optimized[region] += count
        total = sum(regions.values())
        if not total:
            print(f"== {directory}: no JIT samples")
            continue
        runs += 1
        rows = top
        print(f"== {directory}: {total} JIT samples, {100 * sum(optimized.values()) / total:.1f}% optimized")
        print("  share  opt%  region    loop      step         detail")
        for region, count in regions.most_common():
            share = count / total
            if region is None:
                state, detail, loop = "unmapped", "", None
            else:
                loop = enclosing(region, steps, extents)
                state, detail = furthest(steps[loop]) if loop is not None else ("no loop", "")
            summary["optimized"] += optimized[region] / total
            rest = (count - optimized[region]) / total
            summary[state if state not in DONE else "not admitted"] += rest
            if rows:
                rows -= 1
                name = f"{region:08x}" if region is not None else "unmapped"
                head = f"{loop:08x}" if loop is not None else "-"
                print(f"  {100 * share:5.1f} {100 * optimized[region] / count:5.0f}  {name}  {head:8}  "
                      f"{state:12} {detail[:70]}")
    if runs > 1:
        print(f"\nJIT samples by step, {runs} runs weighted equally")
        for state, share in summary.most_common():
            print(f"  {100 * share / runs:5.1f}%  {state}")


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


FP_ARITH = re.compile(r"^(f(add|sub|mul|div|n?m(add|sub)|nmul|sqrt|abs|neg|max|min|maxnm|minnm|"
                      r"cmpe?|ccmpe?|csel)|frint\w*|fcvt\w*|[su]cvtf)$")
BRANCHES = {"b", "bl", "br", "blr", "ret", "cbz", "cbnz", "tbz", "tbnz"}
FLAGS = {"mrs", "msr", "rmif", "setf8", "setf16", "cfinv", "axflag", "xaflag"}
COMPARES = {"cmp", "cmn", "tst", "ccmp", "ccmn"}
KINDS = ("guest memory", "CpuState", "Runtime", "other memory", "branch", "flags", "countdown",
         "x87 state", "fp arithmetic", "fp move", "compare", "integer")
# Parts placed after the unit's inline path; ordinary passes do not run them.
OUT_OF_LINE = {"Cold", "X87Publish", "X87Wide", "SimdPublish"}
X87_VALUES = {f"{bank}{n}" for bank in "vqdsh" for n in range(24, 32)}
XMM_VALUES = {f"{bank}{n}" for bank in "vqdsh" for n in range(16, 24)}


def simd(name):
    return name[0] in "vqdshb" and name[1:].isdigit()


def host_kind(insn, x87):
    """What one disassembled host instruction does, by A64-REG-1 roles."""
    read, written = (set(map(insn.reg_name, regs)) for regs in insn.regs_access())
    memory = [op for op in insn.operands if op.type == 3]  # ARM64_OP_MEM
    if memory:
        base = insn.reg_name(memory[0].mem.base)
        return {"x27": "guest memory", "x28": "guest memory", "x5": "CpuState",
                "x29": "Runtime", "fp": "Runtime"}.get(base, "other memory")
    name = insn.mnemonic
    if name in BRANCHES or name.startswith("b."):
        return "branch"
    if name in FLAGS:
        return "flags"
    if written & {"x9", "w9"}:
        return "countdown"
    if x87 and written & {"w13", "w14", "w15", "x7", "w7", "x8", "w8"}:
        return "x87 state"
    if FP_ARITH.match(name):
        return "fp arithmetic"
    if any(simd(r) for r in read | written):
        return "fp move"
    if name in COMPARES:
        return "compare"
    return "integer"


def code(directory, thread, region, start, end, listing):
    import capstone
    arch = getattr(capstone, "CS_ARCH_AARCH64", None) or capstone.CS_ARCH_ARM64
    disassembler = capstone.Cs(arch, capstone.CS_MODE_ARM)
    disassembler.detail = True
    jit = JitMap(directory)
    # Units by (ns, base): their bytes sit in code-<pid>.bin in jit-<pid>.tsv row order.
    placed = {}
    for path in sorted(directory.glob("jit-*.tsv")):
        blob = path.with_name(f"code-{path.stem.split('-', 1)[1]}.bin")
        if not blob.exists():
            continue
        at = 0
        for line in path.read_text(encoding="utf-8").splitlines():
            ns, base, size = map(int, line.split("\t")[:3])
            placed[(ns, base)] = (blob, at, size)
            at += size
    if not placed:
        raise SystemExit("no code-*.bin: run with ADV_JIT_MAP_DIR set to this directory")
    hits = Counter()  # (ns, base, offset)
    regions = Counter()
    slices = sorted(directory.glob("slice-*.txt"), key=lambda p: int(p.stem.split("-")[1]))
    first = int(slices[0].stem.split("-")[1]) if slices else 0
    for path in slices:
        at = int(path.stem.split("-")[1])
        if (start is not None and at - first < start) or (end is not None and at - first > end):
            continue
        for name, address, count in thread_leaves(path.read_text(encoding="utf-8", errors="replace"),
                                                  thread):
            if name.startswith("???") and address is not None:
                found = jit.lookup(address, at * 1_000_000 + 1_000_000_000)
                if found:
                    hits[(found["ns"], found["base"], address - found["base"])] += count
                    regions[found["region"]] += count
    if region is None:
        if not regions:
            raise SystemExit("no JIT samples for this thread")
        region = regions.most_common(1)[0][0]
    units = Counter()
    for (ns, base, _), count in hits.items():
        if jit.lookup(base, ns)["region"] == region:
            units[(ns, base)] += count
    if not units:
        raise SystemExit(f"no samples in region {region:08x}")
    total = sum(regions.values())
    static, sampled = Counter(), Counter()
    by_part, by_class = Counter(), defaultdict(lambda: [set(), 0, 0])
    guests = defaultdict(lambda: {"samples": 0, "code": []})
    hottest = units.most_common(1)[0][0]
    for unit, unit_samples in units.items():
        ns, base = unit
        blob, at, size = placed[unit]
        with blob.open("rb") as handle:
            handle.seek(at)
            body = handle.read(size)
        stretches = jit.ranges.get(unit, [])
        rows = []
        for insn in disassembler.disasm(body, 0):
            index = bisect.bisect_right(stretches, (insn.address, float("inf"))) - 1
            pc, part = (stretches[index][2], stretches[index][3]) if index >= 0 else (0, "prologue")
            rows.append((insn, pc, part))
        # A guest instruction is x87 when its code touches the x87 stack or an x87 part.
        x87_guests, xmm_guests = set(), set()
        for insn, pc, part in rows:
            names = {insn.reg_name(r) for regs in insn.regs_access() for r in regs}
            if names & X87_VALUES or part.startswith("X87"):
                x87_guests.add(pc)
            elif names & XMM_VALUES:
                xmm_guests.add(pc)
        for insn, pc, part in rows:
            kind = host_kind(insn, pc in x87_guests)
            weight = sum(hits.get((ns, base, insn.address + n), 0) for n in range(4))
            guest_class = "x87" if pc in x87_guests else "sse" if pc in xmm_guests else "integer"
            sampled[kind] += weight
            by_part[part] += weight
            by_class[guest_class][2] += weight
            guests[pc]["samples"] += weight
            if unit == hottest and part not in OUT_OF_LINE:
                static[kind] += 1
                by_class[guest_class][0].add(pc)
                by_class[guest_class][1] += 1
            if unit == hottest:
                guests[pc]["code"].append((weight, part, kind, insn.mnemonic, insn.op_str))
    region_samples = sum(units.values())
    host = sum(static.values())
    print(f"region {region:08x}: {region_samples} of {total} JIT samples in {len(units)} units; "
          f"static counts from the hottest unit ({host} inline host instructions)")
    print(f"\n{'host instruction':16} {'static':>7} {'static%':>7} {'samples':>8} {'sampled%':>8}")
    for kind in sorted(KINDS, key=lambda k: -sampled[k]):
        print(f"{kind:16} {static[kind]:7} {100 * static[kind] / max(host, 1):7.1f} "
              f"{sampled[kind]:8} {100 * sampled[kind] / max(region_samples, 1):8.1f}")
    print(f"\n{'part':16} {'samples':>8} {'sampled%':>8}")
    for part, count in by_part.most_common():
        print(f"{part:16} {count:8} {100 * count / max(region_samples, 1):8.1f}")
    print(f"\n{'guest class':16} {'guest':>6} {'host/guest':>10} {'samples':>8} {'sampled%':>8}")
    for name, (pcs, count, weight) in sorted(by_class.items(), key=lambda item: -item[1][2]):
        print(f"{name:16} {len(pcs):6} {count / max(len(pcs), 1):10.1f} {weight:8} "
              f"{100 * weight / max(region_samples, 1):8.1f}")
    for pc, guest in sorted(guests.items(), key=lambda item: -item[1]["samples"])[:listing]:
        print(f"\n== {pc:08x}: {guest['samples']} samples")
        for weight, part, kind, mnemonic, operands in guest["code"]:
            print(f"{weight:6} {part:10} {kind:14} {mnemonic} {operands}")


def loop_bodies(insns):
    """The host instructions of each loop a backward branch closes, innermost first."""
    bodies = []
    for index, insn in enumerate(insns):
        if insn.mnemonic in BRANCHES or insn.mnemonic.startswith("b."):
            target = next((op.imm for op in insn.operands if op.type == 2), None)
            if target is not None and target <= insn.address:
                first = next(i for i, other in enumerate(insns) if other.address >= target)
                bodies.append(insns[first:index + 1])
    return sorted(bodies, key=len)


def loop_kinds(body):
    """Stack accesses (spills and reloads), other memory, branches and the rest."""
    kinds = Counter()
    for insn in body:
        memory = [op for op in insn.operands if op.type == 3]
        if memory and insn.reg_name(memory[0].mem.base) == "sp":
            kinds["stack"] += 1
        elif memory:
            kinds["memory"] += 1
        elif insn.mnemonic in BRANCHES or insn.mnemonic.startswith("b."):
            kinds["branch"] += 1
        else:
            kinds["other"] += 1
    return kinds


def loops(directory, region):
    import capstone
    arch = getattr(capstone, "CS_ARCH_AARCH64", None) or capstone.CS_ARCH_ARM64
    disassembler = capstone.Cs(arch, capstone.CS_MODE_ARM)
    disassembler.detail = True
    found = False
    for path in sorted(directory.glob("jit-*.tsv")):
        blob = path.with_name(f"code-{path.stem.split('-', 1)[1]}.bin")
        if not blob.exists():
            continue
        at = 0
        for line in path.read_text(encoding="utf-8").splitlines():
            ns, _, size, guest, mode = line.split("\t")[:5]
            size = int(size)
            if int(guest) == region:
                found = True
                with blob.open("rb") as handle:
                    handle.seek(at)
                    code = handle.read(size)
                print(f"unit {mode} at {ns}: {size // 4} host instructions")
                for body in loop_bodies(list(disassembler.disasm(code, 0))):
                    kinds = loop_kinds(body)
                    print(f"  loop {body[0].address:#06x}-{body[-1].address:#06x}: {len(body):4} host "
                          + " ".join(f"{k} {kinds[k]}" for k in ("stack", "memory", "branch", "other")))
            at += size
    if not found:
        raise SystemExit(f"no unit for region {region:08x}: run with ADV_JIT_MAP_DIR set to this directory")


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
    coding = commands.add_parser("code")
    coding.add_argument("directory", type=Path)
    coding.add_argument("--thread", default="adv-execution")
    coding.add_argument("--region", type=lambda text: int(text, 16))
    coding.add_argument("--from", dest="start", type=int)
    coding.add_argument("--to", dest="end", type=int)
    coding.add_argument("--listing", type=int, default=5)
    covering = commands.add_parser("coverage")
    covering.add_argument("directories", type=Path, nargs="+")
    covering.add_argument("--thread", default="adv-execution")
    covering.add_argument("--top", type=int, default=15)
    looping = commands.add_parser("loops")
    looping.add_argument("directory", type=Path)
    looping.add_argument("--region", type=lambda text: int(text, 16), required=True)
    args = parser.parse_args()
    if args.command == "coverage":
        coverage(args.directories, args.thread, args.top)
    elif args.command == "loops":
        loops(args.directory, args.region)
    elif args.command == "record":
        record(args.directory, args.pid, args.seconds)
    elif args.command == "code":
        code(args.directory, args.thread, args.region, args.start, args.end, args.listing)
    else:
        report(args.directory, args.thread, args.start, args.end, args.top, args.instructions)

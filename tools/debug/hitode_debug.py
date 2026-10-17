#!/usr/bin/env python3
"""Interactive debugger for a running Hitode session (adv-run --debug, or the
launcher / adv-run started with ADV_DEBUG=1).

Usage:
  python3 tools/debug/hitode_debug.py list
  python3 tools/debug/hitode_debug.py [--pid PID | --socket PATH] [COMMAND ...]
  python3 tools/debug/hitode_debug.py --snapshot FILE tl|prof|analyze [OPTIONS]

Without COMMAND it opens a prompt. Session commands (see `help`) go to the
runner as they are; these are shaped here:
  tl [SECONDS] [--gap MS]   frame gaps of the timeline, busy or waiting
  prof [SECONDS] [--slice MS] [--mark N] [--window S] [--range A:B]
                            sampled execution thread by slice, hottest guest
                            units (module+offset) and host functions
  snap [LABEL], pull [NAME] save a snapshot under target/investigations/debug
  analyze [--gap MS]        snapshot now and explain each stall in it
  watch [--gap MS]          pull, save and analyze every new snapshot until
                            the session ends; reports go to stdout and
                            target/investigations/debug/<pid>-analysis.txt
Alt+Shift+F12 in the game window marks the moment and keeps a snapshot; a
busy frame gap of 250 ms or more keeps one by itself 2 s later. `snaps`
lists them and `pull` fetches one; `--snapshot` runs `tl`, `prof` and
`analyze` over a pulled snapshot after the session has ended. Output stays local: snapshots hold
game text and must not enter the repository.
"""

import argparse
from collections import Counter, defaultdict
import json
import os
from pathlib import Path
import shlex
import socket
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
OUTPUT = ROOT / "target" / "investigations" / "debug"


def socket_dir():
    return Path(tempfile.gettempdir()) / "hitode-debug"


class Session:
    def __init__(self, path):
        self.path = path
        self.pid = int(path.name.split("-")[0])
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(str(path))
        self.reader = self.sock.makefile("r", encoding="utf-8")
        self.next_id = 0

    def ask(self, line):
        """(ok, text) for one request line."""
        self.next_id += 1
        self.sock.sendall(f"{self.next_id} {line}\n".encode())
        reply = self.reader.readline()
        if not reply:
            raise ConnectionError("the session closed the connection")
        reply = json.loads(reply)
        return reply["ok"], reply["text"]

    def text(self, line):
        ok, text = self.ask(line)
        if not ok:
            raise RuntimeError(text)
        return text


class Snapshot:
    """A pulled snapshot answering the requests `tl` and `prof` make."""

    SECTIONS = {"timeline": "timeline (last 30 s)", "samples": "samples (last 30 s)",
                "modules": "modules", "marks": "marks", "info": "info"}

    def __init__(self, path=None, text=None):
        self.sections, title = {}, None
        text = path.read_text(encoding="utf-8") if text is None else text
        for line in text.splitlines():
            if line.startswith("== "):
                title = line[3:]
                self.sections[title] = []
            elif title is not None:
                self.sections[title].append(line)
        self.info = dict(line.split("=", 1) for line in self.sections.get("info", []) if "=" in line)
        self.pid = int(self.info.get("pid", 0))

    def text(self, line):
        title = self.SECTIONS.get(line.split()[0])
        if title not in self.sections:
            raise RuntimeError(f"the snapshot has no {title or line!r} section")
        return "\n".join(self.sections[title])


def sessions():
    """(path, alive) for every socket, newest first."""
    found = []
    for path in socket_dir().glob("*.sock"):
        try:
            os.kill(int(path.name.split("-")[0]), 0)
            alive = True
        except (OSError, ValueError):
            alive = False
        found.append((path, alive))
    return sorted(found, key=lambda item: item[0].stat().st_mtime, reverse=True)


def table(text):
    """Rows of a TSV reply with a header line, as dicts."""
    lines = [line for line in text.splitlines() if line]
    if not lines:
        return []
    header = lines[0].split("\t")
    return [dict(zip(header, line.split("\t"))) for line in lines[1:]]


def frame_gaps(rows, gap_ms):
    """Frames that followed a gap of at least gap_ms: (end_us, gap_ms, busy%, row)."""
    gaps = []
    for previous, row in zip(rows, rows[1:]):
        gap = (int(row["unix_us"]) - int(previous["unix_us"])) / 1000
        if gap >= gap_ms:
            busy = 100 * int(row["busy_us"]) / 1000 / gap if gap else 0
            gaps.append((int(row["unix_us"]), gap, min(busy, 100.0), row))
    return gaps


def parse_modules(text):
    modules = []
    for line in text.splitlines():
        parts = line.split(None, 3)
        if len(parts) == 4:
            modules.append((int(parts[0], 16), int(parts[1], 16), parts[3]))
    return modules


def place(eip, modules):
    for base, size, name in modules:
        if base <= eip < base + size:
            return f"{name}+{eip - base:#x}"
    return f"{eip:#010x}"


def slices(samples, slice_us):
    """Per slice start: Counter of sample kinds."""
    by_slice = defaultdict(Counter)
    for sample in samples:
        start = int(sample["unix_us"]) // slice_us * slice_us
        by_slice[start][sample["kind"]] += 1
    return dict(sorted(by_slice.items()))


def readable(name):
    """A Rust v0 symbol as its path (adv_cpu_core::memory::Memory::write)."""
    if not name.startswith("_R"):
        return name
    text, parts, i = name[2:], [], 0
    while i < len(text):
        digits = len(text[i:]) - len(text[i:].lstrip("0123456789"))
        if digits:
            length = int(text[i:i + digits])
            ident = text[i + digits:i + digits + length]
            if len(ident) == length and (ident[:1].isalpha() or ident[:1] == "_"):
                parts.append(ident)
                i += digits + length
                continue
            i += digits
        elif text.startswith("Cs", i) or text[i] in "Bs":
            # Crate hashes, back-references and disambiguators end at '_'.
            end = text.find("_", i)
            i = end + 1 if end >= 0 and text[i + 1:end].isalnum() or end == i + 1 else i + 1
        else:
            i += 1
    return "::".join(parts) or name


def symbolize(pid, addresses, image=None):
    """Host addresses to names through atos on macOS; empty elsewhere. With
    image=(path, base) it reads the binary, so the process may have ended."""
    if sys.platform != "darwin" or not addresses:
        return {}
    target = ["-o", image[0], "-l", image[1]] if image else ["-p", str(pid)]
    try:
        out = subprocess.run(["atos", *target, *addresses], capture_output=True,
                             text=True, timeout=60).stdout.splitlines()
    except (OSError, subprocess.TimeoutExpired):
        return {}
    return {address: readable(name.split(" (in ")[0]) for address, name in zip(addresses, out)
            if not name.startswith("0x")}


def host_names(session, samples):
    """pc -> readable name for the host pcs among samples."""
    pcs = Counter(s["pc"] for s in samples if s["kind"] in ("Host", "HostFromGuest"))
    info = getattr(session, "info", {})
    image = (info["host_image"], info["host_base"]) if "host_base" in info else None
    names = symbolize(session.pid, [pc for pc, _ in pcs.most_common(200)], image)
    for sample in samples:
        pc = sample["pc"]
        if pc != "-" and pc not in names:
            names[pc] = f"{sample['image']}!{readable(sample['symbol']) or pc}"
    return names


def regions(units, modules, span=0x400):
    """Counter of guest unit EIPs -> [(count, label, lo, hi)], units of one
    module within `span` of each other merged, busiest first."""
    ranked = []
    current = None
    for eip in sorted(units):
        module = next(((base, name) for base, size, name in modules if base <= eip < base + size), None)
        if current and current[0] == module and eip - current[3] <= span:
            current[1] += units[eip]
            current[3] = eip
        else:
            if current:
                ranked.append(current)
            current = [module, units[eip], eip, eip]
    if current:
        ranked.append(current)
    out = []
    for module, count, lo, hi in ranked:
        if module:
            base, name = module
            label = f"{name}+{lo - base:#x}" + (f"..{hi - base:#x}" if hi != lo else "")
        else:
            label = f"{lo:#010x}" + (f"..{hi:#010x}" if hi != lo else "")
        out.append((count, label, lo, hi))
    return sorted(out, key=lambda r: -r[0])


def parse_slow(lines):
    """Rows of a `slow` table: (total, interpreted, faulted, helper, eip, rest)."""
    rows = []
    for line in lines:
        parts = line.split(None, 5)
        if len(parts) >= 5 and parts[0].isdigit():
            rows.append((*map(int, parts[:4]), int(parts[4], 16), parts[5] if len(parts) > 5 else ""))
    return rows


def explain_stall(gap, busy, shares, guest, host_from_guest, host_only):
    """One-line verdicts from a stall's shares (fractions of its samples)."""
    verdicts = []
    if busy < 50:
        verdicts.append(f"waiting: the execution thread was in guest steps only {busy:.0f}% of the gap; "
                        "see the waits under `threads`")
    if shares["Idle"] >= 0.5:
        verdicts.append("sampled idle: blocked in the host (wait or sleep)")
    if shares["Guest"] >= 0.4 and guest:
        verdicts.append(f"generated code: {guest[0][1]} holds {guest[0][0]} samples")
    if shares["HostFromGuest"] >= 0.2 and host_from_guest:
        name, count = host_from_guest[0]
        verdicts.append(f"host calls from guest code: {name} ({count} samples)")
    if shares["Host"] >= 0.2 and host_only:
        name, count = host_only[0]
        kind = "translation" if "translate" in name else "host work outside guest code"
        verdicts.append(f"{kind}: {name} ({count} samples)")
    return verdicts or ["no dominant cause in the samples"]


def analyze(snapshot, gap_ms=100.0, top=6):
    """Report text explaining each frame gap of at least gap_ms in a snapshot."""
    rows = table(snapshot.text("timeline"))
    samples = table(snapshot.text("samples")) if "samples (last 30 s)" in snapshot.sections else []
    modules = parse_modules(snapshot.text("modules"))
    slow = parse_slow(snapshot.sections.get("slow paths (since previous snapshot)")
                      or snapshot.sections.get("slow paths (whole session)", []))
    names = host_names(snapshot, samples)
    out = []
    if len(rows) < 2:
        return "fewer than two frames in the snapshot"
    first = int(rows[0]["unix_us"])
    intervals = sorted((int(b["unix_us"]) - int(a["unix_us"])) / 1000 for a, b in zip(rows, rows[1:]))
    out.append(f"{len(rows)} frames over {(int(rows[-1]['unix_us']) - first) / 1e6:.1f} s; interval p50 "
               f"{intervals[len(intervals) // 2]:.1f} ms, p95 {intervals[int(len(intervals) * 0.95)]:.1f} ms, "
               f"max {intervals[-1]:.1f} ms; {len(samples)} samples")
    marks = [line.split("\t") for line in snapshot.sections.get("marks", []) if "\t" in line]
    for at, label in marks:
        if int(at) >= first:
            out.append(f"mark t={(int(at) - first) / 1e6:.2f}s {label}")
    gaps = frame_gaps(rows, gap_ms)
    if not gaps:
        out.append(f"no frame gap of {gap_ms:.0f} ms or more")
    for end, gap, busy, row in sorted(gaps, key=lambda g: -g[1])[:8]:
        start = end - gap * 1000
        inside = [s for s in samples if start <= int(s["unix_us"]) <= end]
        kinds = Counter(s["kind"] for s in inside)
        total = max(len(inside), 1)
        shares = {k: kinds[k] / total for k in ("Guest", "HostFromGuest", "Host", "Idle")}
        out.append("")
        out.append(f"stall t={(start - first) / 1e6:.2f}..{(end - first) / 1e6:.2f}s frame {row['frame']}: "
                   f"{gap:.0f} ms, busy {busy:.0f}%, host_calls {row['host_calls']}, "
                   f"native {row['native_entries']}, interpreted {row['interpreter_entries']}")
        out.append("  samples: " + ", ".join(f"{k} {100 * v:.0f}%" for k, v in shares.items())
                   + f" of {len(inside)}")
        busy_samples = [s for s in inside if s["kind"] != "Idle"]
        guest = regions(Counter(int(s["eip"], 16) for s in busy_samples if s["eip"] != "-"), modules)
        for count, label, _, _ in guest[:top]:
            out.append(f"  guest   {count:6} {100 * count / total:5.1f}% {label}")
        from_guest = Counter(names[s["pc"]] for s in inside if s["kind"] == "HostFromGuest")
        callers = defaultdict(Counter)
        for s in inside:
            if s["kind"] == "HostFromGuest":
                callers[names[s["pc"]]][place(int(s["eip"], 16), modules)] += 1
        for name, count in from_guest.most_common(top):
            caller = callers[name].most_common(1)[0][0]
            out.append(f"  called  {count:6} {100 * count / total:5.1f}% {name[:90]} from {caller}")
        host_only = Counter(names[s["pc"]] for s in inside if s["kind"] == "Host")
        for name, count in host_only.most_common(top):
            out.append(f"  host    {count:6} {100 * count / total:5.1f}% {name[:110]}")
        for count, label, lo, hi in guest[:3]:
            near = [r for r in slow if lo - 0x40 <= r[4] <= hi + 0x400]
            for total_runs, interpreted, faulted, helper, eip, rest in near[:4]:
                out.append(f"  slow    {total_runs:8} at {rest} (interpreted {interpreted}, "
                           f"helper {helper}) near {label}")
        if not inside:
            out.append("  => no samples in this gap (sampler off, or a snapshot from before samples were kept)"
                       + ("; waiting" if busy < 50 else "; busy"))
            continue
        for verdict in explain_stall(gap, busy, shares, guest, from_guest.most_common(1),
                                     host_only.most_common(1)):
            out.append(f"  => {verdict}")
    return "\n".join(out)


def show_analysis(session, args):
    parser = argparse.ArgumentParser(prog="analyze")
    parser.add_argument("--gap", type=float, default=100.0, help="smallest frame gap in ms")
    options = parser.parse_args(args)
    if not isinstance(session, Snapshot):
        name, _, body = session.text("snap analyze").partition("\n")
        write(name, body)
        session = Snapshot(text=body)
    print(analyze(session, options.gap))


def watch(session, args):
    """Pulls each new snapshot, saves it and prints its analysis."""
    parser = argparse.ArgumentParser(prog="watch")
    parser.add_argument("--gap", type=float, default=100.0)
    options = parser.parse_args(args)
    report = OUTPUT / f"{session.pid}-analysis.txt"
    seen = set()
    print(f"watching session {session.pid}; reports also go to {report}", flush=True)
    while True:
        try:
            names = [n for n in session.text("snaps").splitlines() if n]
        except (ConnectionError, OSError):
            print("session ended", flush=True)
            return
        for name in names:
            if name in seen:
                continue
            seen.add(name)
            _, _, body = session.text(f"pull {name}").partition("\n")
            path = write(name, body)
            text = f"== {name} ({path})\n{analyze(Snapshot(text=body), options.gap)}\n"
            print(text, flush=True)
            with report.open("a", encoding="utf-8") as file:
                file.write(text + "\n")
        time.sleep(2)


def show_timeline(session, args):
    parser = argparse.ArgumentParser(prog="tl")
    parser.add_argument("seconds", nargs="?", type=int, default=30)
    parser.add_argument("--gap", type=float, default=50.0)
    options = parser.parse_args(args)
    rows = table(session.text(f"timeline {options.seconds}"))
    if len(rows) < 2:
        print("fewer than two frames in that window")
        return
    first = int(rows[0]["unix_us"])
    intervals = sorted((int(b["unix_us"]) - int(a["unix_us"])) / 1000 for a, b in zip(rows, rows[1:]))
    print(f"{len(rows)} frames; interval p50 {intervals[len(intervals) // 2]:.1f} ms, "
          f"p95 {intervals[int(len(intervals) * 0.95)]:.1f} ms, max {intervals[-1]:.1f} ms")
    print(f"gaps >= {options.gap:.0f} ms (t from window start; busy = execution thread in guest steps):")
    for end, gap, busy, row in frame_gaps(rows, options.gap):
        print(f"  t={(end - first) / 1e6:7.2f}s gap {gap:8.1f} ms busy {busy:5.1f}% "
              f"host_calls {row['host_calls']:>8} native {row['native_entries']:>8} "
              f"interpreted {row['interpreter_entries']:>6}")


def show_profile(session, args):
    parser = argparse.ArgumentParser(prog="prof")
    parser.add_argument("seconds", nargs="?", type=int, default=30)
    parser.add_argument("--slice", type=int, default=250, help="slice in ms")
    parser.add_argument("--mark", type=int, help="centre on mark N (from `marks`, newest is -1)")
    parser.add_argument("--window", type=float, default=10.0, help="seconds around the mark")
    parser.add_argument("--range", help="A:B seconds from the window start, e.g. a stall from `tl`")
    parser.add_argument("--top", type=int, default=20)
    options = parser.parse_args(args)
    samples = table(session.text(f"samples {options.seconds if options.mark is None else 60}"))
    if options.mark is not None:
        marks = [line.split("\t") for line in session.text("marks").splitlines() if line]
        at = int(marks[options.mark][0])
        half = options.window * 1e6 / 2
        samples = [s for s in samples if abs(int(s["unix_us"]) - at) <= half]
    if options.range and samples:
        start = int(samples[0]["unix_us"])
        low, high = (start + float(edge) * 1e6 for edge in options.range.split(":"))
        samples = [s for s in samples if low <= int(s["unix_us"]) <= high]
    if not samples:
        print("no samples in that window (`profile` shows the sampler rate)")
        return
    first = int(samples[0]["unix_us"])
    print(f"{len(samples)} samples; per {options.slice} ms slice: Guest/HostFromGuest/Host/Idle %")
    for start, kinds in slices(samples, options.slice * 1000).items():
        total = sum(kinds.values())
        shares = " ".join(f"{100 * kinds[k] / total:5.1f}" for k in ("Guest", "HostFromGuest", "Host", "Idle"))
        bar = "#" * round(20 * (total - kinds["Idle"]) / total)
        print(f"  t={(start - first) / 1e6:7.2f}s {shares}  {bar}")
    modules = parse_modules(session.text("modules"))
    busy = [s for s in samples if s["kind"] != "Idle"]
    units = Counter(s["eip"] for s in busy if s["eip"] != "-")
    print(f"guest units (start EIP) in {len(busy)} busy samples:")
    for eip, count in units.most_common(options.top):
        print(f"  {count:6} {100 * count / len(busy):5.1f}% {eip} {place(int(eip, 16), modules)}")
    hosts = Counter(s["pc"] for s in busy if s["kind"] in ("Host", "HostFromGuest"))
    names = symbolize(session.pid, [pc for pc, _ in hosts.most_common(200)])
    by_name = Counter()
    for pc, count in hosts.items():
        sample = next(s for s in busy if s["pc"] == pc)
        by_name[names.get(pc) or f"{sample['image']}!{sample['symbol'] or pc}"] += count
    print("host functions in busy samples:")
    for name, count in by_name.most_common(options.top):
        print(f"  {count:6} {100 * count / len(busy):5.1f}% {name[:140]}")


def write(name, body):
    OUTPUT.mkdir(parents=True, exist_ok=True)
    path = OUTPUT / f"{name}.txt"
    path.write_text(body, encoding="utf-8")
    return path


def save(text, fallback):
    name, _, body = text.partition("\n")
    path = write(name or fallback, body)
    print(f"saved {path}")
    for line in body.splitlines()[:12]:
        print(f"  {line}")


def run(session, line):
    words = shlex.split(line)
    if not words:
        return
    command, args = words[0], words[1:]
    if command == "tl":
        show_timeline(session, args)
    elif command == "prof":
        show_profile(session, args)
    elif command == "analyze":
        show_analysis(session, args)
    elif command == "watch":
        watch(session, args)
    elif command in ("snap", "pull"):
        save(session.text(line), command)
    else:
        ok, text = session.ask(line)
        print(text if ok else f"error: {text}")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--pid", type=int)
    parser.add_argument("--socket", type=Path)
    parser.add_argument("--snapshot", type=Path, help="read a pulled snapshot instead of a session")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    options = parser.parse_args()
    if options.command[:1] == ["list"]:
        for path, alive in sessions():
            print(f"{'live' if alive else 'dead'}  {path}")
        return
    if options.snapshot is not None:
        snapshot = Snapshot(options.snapshot)
        command, args = options.command[0], options.command[1:]
        {"tl": show_timeline, "prof": show_profile, "analyze": show_analysis}[command](snapshot, args)
        return
    path = options.socket
    if path is None:
        live = [p for p, alive in sessions() if alive and (options.pid is None or p.name.startswith(f"{options.pid}-"))]
        if not live:
            sys.exit("no live session; start adv-run with --debug or set ADV_DEBUG=1")
        path = live[0]
    session = Session(path)
    if options.command:
        run(session, shlex.join(options.command))
        return
    try:
        import readline
        history = Path.home() / ".hitode_debug_history"
        if history.exists():
            readline.read_history_file(history)
    except (ImportError, OSError):
        readline = None
    print(f"attached to {path.name}; `help` lists commands, Ctrl+D leaves")
    try:
        while True:
            try:
                line = input("hitode> ")
            except EOFError:
                print()
                break
            try:
                run(session, line)
            except (RuntimeError, ValueError, IndexError, SystemExit) as error:
                print(f"error: {error}")
    finally:
        if readline is not None:
            try:
                readline.write_history_file(history)
            except OSError:
                pass


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Interactive debugger for a running Hitode session (adv-run --debug, or the
launcher / adv-run started with ADV_DEBUG=1).

Usage:
  python3 tools/debug/hitode_debug.py list
  python3 tools/debug/hitode_debug.py [--pid PID | --socket PATH] [COMMAND ...]

Without COMMAND it opens a prompt. Session commands (see `help`) go to the
runner as they are; these are shaped here:
  tl [SECONDS] [--gap MS]   frame gaps of the timeline, busy or waiting
  prof [SECONDS] [--slice MS] [--mark N] [--window S]
                            sampled execution thread by slice, hottest guest
                            units (module+offset) and host functions
  snap [LABEL], pull [NAME] save a snapshot under target/investigations/debug
Ctrl+Shift+F12 in the game window marks the moment and keeps a snapshot;
`snaps` lists them and `pull` fetches one. Output stays local: snapshots hold
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


def symbolize(pid, addresses):
    """Host addresses to names through atos on macOS; empty elsewhere."""
    if sys.platform != "darwin" or not addresses:
        return {}
    try:
        out = subprocess.run(["atos", "-p", str(pid), *addresses], capture_output=True,
                             text=True, timeout=60).stdout.splitlines()
    except (OSError, subprocess.TimeoutExpired):
        return {}
    return {address: readable(name.split(" (in ")[0]) for address, name in zip(addresses, out)}


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
    parser.add_argument("--top", type=int, default=20)
    options = parser.parse_args(args)
    samples = table(session.text(f"samples {options.seconds if options.mark is None else 60}"))
    if options.mark is not None:
        marks = [line.split("\t") for line in session.text("marks").splitlines() if line]
        at = int(marks[options.mark][0])
        half = options.window * 1e6 / 2
        samples = [s for s in samples if abs(int(s["unix_us"]) - at) <= half]
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


def save(text, fallback):
    name, _, body = text.partition("\n")
    OUTPUT.mkdir(parents=True, exist_ok=True)
    path = OUTPUT / f"{name or fallback}.txt"
    path.write_text(body, encoding="utf-8")
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
    elif command in ("snap", "pull"):
        save(session.text(line), command)
    else:
        ok, text = session.ask(line)
        print(text if ok else f"error: {text}")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--pid", type=int)
    parser.add_argument("--socket", type=Path)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    options = parser.parse_args()
    if options.command[:1] == ["list"]:
        for path, alive in sessions():
            print(f"{'live' if alive else 'dead'}  {path}")
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

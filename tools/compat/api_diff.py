#!/usr/bin/env python3
"""Align local native/adv-host API TSV traces and report normalized divergences.

Usage: python tools/compat/api_diff.py native.tsv adv.tsv --output diff.json
Input: five-column adv traces or six-column native traces (entry sequence last).
Output: call counts, normalized scalar/category context, first and bounded later
divergences. Never emits raw pointer values, strings, process IDs or source paths.
This is a triage hint, not proof of causality. Unknown signatures are not decoded.
"""
from __future__ import annotations

import argparse
from collections import Counter, defaultdict
from dataclasses import dataclass, field
import fnmatch
import json
from pathlib import Path
import re

DEFAULT_IGNORE = (
    "*!GetTickCount", "*!GetTickCount64", "*!QueryPerformanceCounter",
    "*!QueryPerformanceFrequency", "*!timeGetTime", "*!GetCurrentProcessId", "*!GetCurrentThreadId",
    "*!OutputDebugString*",
    "<win32>.dll!*", "*!<*>",
)
VOLATILE_RETURNS = {"GetTickCount", "GetTickCount64", "timeGetTime", "GetMessageTime",
                    "GetCurrentProcessId", "GetCurrentThreadId", "GetProcessId", "GetThreadId"}
ID_PARAMS = {"OpenProcess": {2}, "OpenThread": {2}, "PostThreadMessageA": {0}, "PostThreadMessageW": {0}}
PATH_LENGTHS = {"GetCurrentDirectoryA", "GetCurrentDirectoryW", "RtlGetCurrentDirectory_U",
                "GetTempPathA", "GetTempPathW", "GetTempPath2A", "GetTempPath2W",
                "GetModuleFileNameA", "GetModuleFileNameW"}


def canonical(api: str) -> str:
    module, name = api.split("!", 1)
    module = module.lower()
    if not module.endswith(".dll"):
        module += ".dll"
    if module == "kernelbase.dll":
        module = "kernel32.dll"
    if module == "ntdll.dll" and name.startswith("Zw"):
        name = "Nt" + name[2:]
    return f"{module}!{name}"


@dataclass
class Signature:
    result: str
    params: list[tuple[str, int]]
    result_bits: int = 64


def load_signatures(path: Path) -> dict[str, Signature]:
    result = {}
    for row in path.read_text(encoding="utf-8").splitlines():
        if not row or row.startswith("#"):
            continue
        fields = row.split("\t")
        params = [p.rsplit(":", 3) for p in fields[5].split("|") if p]
        result[canonical(fields[0] + "!" + fields[1])] = Signature(
            effective_kind(fields[4], fields[3]), [(effective_kind(p[1], p[-2]), int(p[-1])) for p in params], scalar_bits(fields[4]))
    return result


def effective_kind(type_name: str, kind: str) -> str:
    return "ptr" if type_name.rsplit(".", 1)[-1] in ("LPARAM", "WPARAM", "LRESULT") else kind


def window_word(name: str, raw: list[str]) -> bool:
    if len(raw) < 2 or not re.fullmatch(r"(?:Get|Set)(?:Window|Class)Long(?:Ptr)?[AW]", name):
        return False
    index = integer(raw[1])
    integer_indices = (-12, -16, -20) if "Window" in name else (-26, -18, -20, -32)
    return index not in {i & 0xffffffff for i in integer_indices}


def scalar_bits(type_name: str) -> int:
    tail = type_name.rsplit(".", 1)[-1]
    if tail in ("BOOLEAN", "BYTE", "UCHAR", "CHAR", "u8", "i8"):
        return 8
    if tail in ("WORD", "USHORT", "SHORT", "ATOM", "u16", "i16"):
        return 16
    return 64


def integer(value: str) -> int | None:
    try:
        return int(value.removeprefix("h:").removeprefix("atom:"), 16)
    except ValueError:
        return None


@dataclass
class Record:
    process: int
    thread: int
    ordinal: int
    line: int
    api: str
    args: list[str]
    ret: str
    sequence: int | None = None
    unobserved: list[str] = field(default_factory=list)
    lookup_symbol: str | None = None

    def view(self) -> dict:
        result = {"call": self.ordinal, "line": self.line, "api": self.api,
                  "args": self.args, "ret": self.ret}
        if self.sequence is not None:
            result["entry_sequence"] = self.sequence
        if self.unobserved:
            result["unobserved"] = self.unobserved
        if self.lookup_symbol:
            result["lookup_symbol"] = self.lookup_symbol
        return result


class Normalizer:
    def __init__(self):
        self.handles: dict[int, dict[int, str]] = defaultdict(dict)
        self.next_handle: Counter = Counter()
        self.resources = defaultdict(dict)
        self.next_resource = Counter()

    def resource(self, raw: str, process: int, kind: str) -> str:
        n = integer(raw)
        if n is None or n == 0xffffffff:
            return f"{kind}:invalid"
        key = process, kind
        if n not in self.resources[key]:
            self.next_resource[key] += 1
            self.resources[key][n] = f"{kind}{self.next_resource[key]}"
        return self.resources[key][n]

    def value(self, raw: str, kind: str, process: int) -> str:
        n = integer(raw)
        if kind == "void":
            return "void"
        if kind == "handle":
            if n in (0, 0xffffffff, 0xfffffffe, 0xfffffffd, 0xfffffffc) or n is not None and 0x80000000 <= n <= 0x80000006:
                return f"handle:{n:x}"
            if n is None:
                return "handle:unknown"
            table = self.handles[process]
            if n not in table:
                self.next_handle[process] += 1
                table[n] = f"h{self.next_handle[process]}"
            return table[n]
        if kind in ("ptr", "aggregate", "str8", "str16"):
            if n == 0 or raw == "null":
                return "null"
            if kind in ("str8", "str16"):
                if raw.startswith("atom:") or n is not None and n <= 0xffff:
                    return f"atom:{n:x}" if n is not None else "atom:unknown"
                return "string:present"
            return "pointer:present"
        if n is not None:
            if kind in ("i32", "u32", "f32"):
                n &= 0xffffffff
            return f"0x{n:x}"
        # Typed traces may include a timestamp or temporary path. Never echo text.
        if re.search(r"[\\/]|temp|tmp", raw, re.I):
            return "path:present"
        return "scalar:unparsed"


def read_trace(path: Path, signatures: dict[str, Signature], ignore=(), covered=None,
               include=(), root_pid="1000") -> tuple[list[Record], dict]:
    normal = Normalizer()
    records = []
    processes, threads, ordinals = {}, {}, Counter()
    # adv-host's initial process is 0x1000. It has no initial process marker;
    # later scheduler switches must join that implicit prefix to the same owner.
    current = root_pid
    known_names = {api.split("!", 1)[1] for api in signatures}
    pending_lookup = None
    stats = {"rows": 0, "ignored": 0, "malformed": 0, "unknown_signatures": 0,
             "children_untraced": 0, "capture_stopped": False, "hooks_unavailable": 0,
             "outside_native_coverage": 0, "_covered": set()}
    with path.open(encoding="utf-8-sig", errors="replace") as source:
        for line_number, row in enumerate(source, 1):
            fields = row.rstrip("\r\n").split("\t")
            if row.startswith("# resolve\t") and len(fields) >= 3:
                symbol = fields[2].split("!", 1)[-1]
                pending_lookup = symbol if symbol in known_names else None
            if row.startswith("# hook\t") and len(fields) >= 2:
                stats["_covered"].add(canonical(fields[1]))
            if row.startswith("# process\t"):
                try:
                    current = f"{int(fields[1], 16):x}"
                except (ValueError, IndexError):
                    stats["malformed"] += 1
                continue
            if row.startswith("# child_untraced"):
                stats["children_untraced"] += 1
            if row.startswith("# capture_stop"):
                stats["capture_stopped"] = True
            if row.startswith("# hooks_unavailable"):
                stats["hooks_unavailable"] = integer(fields[-1]) or 0
            if row.startswith("# terminal\t") and len(fields) >= 7:
                fields = fields[2:7]
                fields[4] = "terminal"
            elif row.startswith("#") or not row.strip():
                continue
            if len(fields) not in (5, 6) or not fields[0].isdigit() or "!" not in fields[2]:
                stats["malformed"] += 1
                continue
            api = canonical(fields[2])
            lookup, pending_lookup = pending_lookup, None
            stats["rows"] += 1
            if current not in processes:
                processes[current] = len(processes) + 1
            process = processes[current]
            key = (process, fields[0])
            if key not in threads:
                # Native collectors explicitly label their primary thread 1.
                siblings = [v for k, v in threads.items() if k[0] == process]
                threads[key] = int(fields[0]) if len(fields) == 6 else len(siblings) + 1
            thread = threads[key]
            ordinals[(process, thread)] += 1
            if covered is not None and api not in covered:
                stats["outside_native_coverage"] += 1
                continue
            if (include and not any(fnmatch.fnmatchcase(api, pattern) for pattern in include)
                    or any(fnmatch.fnmatchcase(api, pattern) for pattern in ignore)):
                stats["ignored"] += 1
                continue
            signature = signatures.get(api)
            raw = fields[3].split(",") if fields[3] else []
            args, missing = [], []
            native = len(fields) == 6
            if api.endswith("!GetProcAddress") and native and len(raw) > 1 and raw[1].startswith("s8:"):
                try:
                    symbol = bytes.fromhex(raw[1][3:]).decode("ascii")
                    lookup = symbol if symbol in known_names else None
                except (ValueError, UnicodeError):
                    lookup = None
            elif not api.endswith("!GetProcAddress"):
                lookup = None
            if not signature:
                stats["unknown_signatures"] += 1
                args = ["unknown" for _ in raw]
                ret = "unknown"
                missing.append("signature")
            else:
                offset = 0
                name = api.split("!", 1)[1]
                for index, (kind, width) in enumerate(signature.params):
                    if index == 2 and window_word(name, raw):
                        kind = "ptr"
                    if offset >= len(raw):
                        args.append("missing")
                        missing.append(f"arg{index}")
                        continue
                    value = raw[offset]
                    if not native and width == 2 and offset + 1 < len(raw):
                        low, high = integer(value), integer(raw[offset + 1])
                        if low is not None and high is not None:
                            value = f"{low | high << 32:x}"
                    offset += 1 if native else width
                    if index == 0 and name in ("TlsGetValue", "TlsSetValue", "TlsFree", "FlsGetValue", "FlsSetValue", "FlsFree"):
                        args.append(normal.resource(value, process, name[:3].lower()))
                    elif index == 0 and name in ("DecodePointer", "DecodeSystemPointer"):
                        args.append("encoded_pointer:opaque")
                    else:
                        args.append("id:volatile" if index in ID_PARAMS.get(name, set())
                                    else normal.value(value, kind, process))
                    if kind in ("str8", "str16"):
                        missing.append(f"arg{index}:string_content")
                    elif kind in ("ptr", "aggregate"):
                        missing.append(f"arg{index}:pointee")
                raw_result = fields[4]
                numeric_result = integer(raw_result)
                if numeric_result is not None and signature.result_bits < 64:
                    raw_result = f"{numeric_result & ((1 << signature.result_bits) - 1):x}"
                ret = ("terminal" if fields[4] == "terminal" else "time_or_id:volatile" if name in VOLATILE_RETURNS
                       else normal.value(raw_result, "ptr" if signature.result in ("str8", "str16") or window_word(name, raw) else signature.result, process))
                if name in PATH_LENGTHS and numeric_result:
                    # Keep zero/failure and insufficient-buffer outcomes, mask path-dependent length.
                    capacity_index = 2 if name.startswith("GetModuleFileName") else 0
                    capacity = integer(raw[capacity_index]) if len(raw) > capacity_index else None
                    ret = "path_length:fits" if capacity is not None and numeric_result < capacity else "path_length:required"
                if name in ("TlsAlloc", "FlsAlloc"):
                    ret = normal.resource(raw_result, process, name[:3].lower())
                if name in ("TlsFree", "FlsFree") and numeric_result and raw:
                    normal.resources[(process, name[:3].lower())].pop(integer(raw[0]), None)
                if name in ("EncodePointer", "EncodeSystemPointer"):
                    ret = "encoded_pointer:opaque"
                # Reused numeric handles must represent a new lifetime after closure.
                close = name in ("CloseHandle", "DeleteObject", "DeleteDC", "DestroyWindow", "CloseServiceHandle")
                if close and integer(fields[4]) and raw:
                    normal.handles[process].pop(integer(raw[0]), None)
                if name == "RegCloseKey" and integer(fields[4]) == 0 and raw:
                    normal.handles[process].pop(integer(raw[0]), None)
            sequence = int(fields[5]) if native and fields[5].isdigit() else None
            records.append(Record(process, thread, ordinals[(process, thread)], line_number,
                                  api, args, ret, sequence, missing, lookup))
    stats["processes"] = len(processes)
    stats["threads"] = len(threads)
    stats["hooked_apis"] = len(stats["_covered"])
    return records, stats


class HandlePairs:
    """Pair first observed handles on aligned calls, tolerating unpaired gap calls."""
    def __init__(self):
        self.forward, self.reverse = {}, {}

    def equal(self, a: str, b: str, process: int) -> bool:
        am, bm = re.fullmatch(r"(h|tls|fls)\d+", a), re.fullmatch(r"(h|tls|fls)\d+", b)
        if am and bm and am.group(1) == bm.group(1):
            left, right = (process, a), (process, b)
            if left in self.forward:
                return self.forward[left] == right
            if right in self.reverse:
                return False
            self.forward[left] = right
            self.reverse[right] = left
            return True
        return a == b


def align(left: list[Record], right: list[Record], window: int):
    """Bounded lookahead by API name, with a two-call anchor where available.

    Gap calls remain divergences unless the caller explicitly ignores their API.
    Avoid quadratic whole-trace alignment on long polling runs.
    """
    i = j = 0
    while i < len(left) and j < len(right):
        if left[i].api == right[j].api:
            yield i, j, "pair"
            i += 1
            j += 1
            continue
        # A run of polling/enumeration calls is a gap, even when longer than
        # the lookahead window. Keep reporting it; only explicit ignores hide it.
        if i and left[i].api == left[i - 1].api:
            previous = left[i].api
            while i < len(left) and left[i].api == previous:
                yield i, j, "native_only"
                i += 1
            continue
        if j and right[j].api == right[j - 1].api:
            previous = right[j].api
            while j < len(right) and right[j].api == previous:
                yield i, j, "adv_only"
                j += 1
            continue
        candidates = []
        for di in range(min(window + 1, len(left) - i)):
            for dj in range(min(window + 1, len(right) - j)):
                if di + dj == 0 or left[i + di].api != right[j + dj].api:
                    continue
                following = (i + di + 1 < len(left) and j + dj + 1 < len(right)
                             and left[i + di + 1].api == right[j + dj + 1].api)
                candidates.append((di + dj, not following, di, dj))
        if candidates:
            _, _, di, dj = min(candidates)
            for offset in range(di):
                yield i + offset, j, "native_only"
            for offset in range(dj):
                yield i + di, j + offset, "adv_only"
            i += di
            j += dj
        else:
            yield i, j, "order"
            i += 1
            j += 1
    # Bounded captures do not prove that the other side omitted the remaining calls.
    if i < len(left) or j < len(right):
        yield i, j, "capture_tail"


def compare(left: list[Record], right: list[Record], window=8, context=3, limit=10,
            native_thread=None, adv_thread=None) -> dict:
    left_groups, right_groups = defaultdict(list), defaultdict(list)
    for record in left:
        left_groups[(record.process, record.thread)].append(record)
    for record in right:
        right_groups[(record.process, record.thread)].append(record)
    pairs = ([(native_thread, adv_thread)] if native_thread and adv_thread else
             [(key, key) for key in sorted(set(left_groups) | set(right_groups))])
    divergences, thread_results = [], []
    handles = HandlePairs()
    for native_key, adv_key in pairs:
        a, b = left_groups[native_key], right_groups[adv_key]
        counts = Counter()
        if not a or not b:
            thread_results.append({"native_thread": list(native_key), "adv_thread": list(adv_key),
                                   "native_calls": len(a), "adv_calls": len(b), "status": "unpaired_thread"})
            continue
        for i, j, kind in align(a, b, window):
            if kind == "pair":
                differences = []
                if a[i].lookup_symbol and b[j].lookup_symbol and a[i].lookup_symbol != b[j].lookup_symbol:
                    differences.append("symbol")
                for index in range(max(len(a[i].args), len(b[j].args))):
                    av = a[i].args[index] if index < len(a[i].args) else "missing"
                    bv = b[j].args[index] if index < len(b[j].args) else "missing"
                    if not handles.equal(av, bv, native_key[0]):
                        differences.append(f"arg{index}")
                if not handles.equal(a[i].ret, b[j].ret, native_key[0]):
                    differences.append("return")
                if not differences:
                    counts["matched"] += 1
                    continue
                kind = "value"
            else:
                differences = ["sequence"]
            counts[kind] += 1
            if len(divergences) < limit:
                divergences.append({"kind": kind, "fields": differences,
                    "native_thread": list(native_key), "adv_thread": list(adv_key),
                    "native": a[i].view() if i < len(a) and kind != "adv_only" else None,
                    "adv": b[j].view() if j < len(b) and kind != "native_only" else None,
                    "native_context": [r.view() for r in a[max(0, i-context):i+context+1]],
                    "adv_context": [r.view() for r in b[max(0, j-context):j+context+1]]})
        thread_results.append({"native_thread": list(native_key), "adv_thread": list(adv_key),
                               "native_calls": len(a), "adv_calls": len(b), "counts": dict(counts)})
    meaningful = [d for d in divergences if d["kind"] != "capture_tail"]
    return {"first_divergence": meaningful[0] if meaningful else None,
            "divergences": divergences, "threads": thread_results,
            "thread_pairing": "explicit" if native_thread else "first observed within process; main first",
            "causality": "candidate only; inspect context, coverage and unobserved fields"}


def thread_key(value: str) -> tuple[int, int]:
    try:
        process, thread = map(int, value.split(":"))
        if process < 1 or thread < 1:
            raise ValueError
        return process, thread
    except ValueError:
        raise argparse.ArgumentTypeError("thread must be positive PROCESS:THREAD") from None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("native", type=Path)
    parser.add_argument("adv", type=Path)
    parser.add_argument("--signatures", type=Path, default=Path(__file__).resolve().parents[2] / "data/abi/signatures.tsv")
    parser.add_argument("--ignore", action="append", default=[], help="case-sensitive module!API glob")
    parser.add_argument("--include", action="append", default=[], help="compare only these module!API globs")
    parser.add_argument("--ignore-file", type=Path, help="one glob per line; # comments")
    parser.add_argument("--no-default-ignore", action="store_true")
    parser.add_argument("--window", type=int, default=8)
    parser.add_argument("--context", type=int, default=3)
    parser.add_argument("--limit", type=int, default=10)
    parser.add_argument("--native-thread", type=thread_key)
    parser.add_argument("--adv-thread", type=thread_key)
    parser.add_argument("--adv-root-pid", default="1000", help="implicit initial guest PID in hex (default: 1000)")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if not 0 <= args.window <= 64 or not 0 <= args.context <= 20 or not 1 <= args.limit <= 100:
        parser.error("window/context/limit out of range")
    if bool(args.native_thread) != bool(args.adv_thread):
        parser.error("specify both thread selectors")
    try:
        root_pid = f"{int(args.adv_root_pid, 16):x}"
    except ValueError:
        parser.error("adv-root-pid must be hexadecimal")
    ignore = list(() if args.no_default_ignore else DEFAULT_IGNORE) + args.ignore
    if args.ignore_file:
        ignore += [line.strip() for line in args.ignore_file.read_text(encoding="utf-8").splitlines()
                   if line.strip() and not line.lstrip().startswith("#")]
    signatures = load_signatures(args.signatures)
    native, native_stats = read_trace(args.native, signatures, ignore, include=args.include)
    covered = native_stats.pop("_covered") or None
    adv, adv_stats = read_trace(args.adv, signatures, ignore, covered, args.include, root_pid)
    adv_stats.pop("_covered")
    result = compare(native, adv, args.window, args.context, args.limit, args.native_thread, args.adv_thread)
    result.update(native_stats=native_stats, adv_stats=adv_stats, ignored_apis=ignore, included_apis=args.include)
    result["warnings"] = []
    if native_stats["children_untraced"]:
        result["warnings"].append("Native child processes were not collected; child differences are unassessed.")
    if native_stats["hooks_unavailable"]:
        result["warnings"].append("Native hook coverage is incomplete; consult local hook diagnostics.")
    if native_stats["malformed"] or adv_stats["malformed"]:
        result["warnings"].append("Malformed records were skipped; alignment is partial.")
    if any(t.get("status") == "unpaired_thread" for t in result["threads"]):
        result["warnings"].append("Some process/thread streams lack a counterpart and were not compared.")
    if not native or not adv:
        result["warnings"].append("One side has no comparable calls; no equivalence conclusion is possible.")
    output = json.dumps(result, indent=2, ensure_ascii=False) + "\n"
    if args.output:
        args.output.write_text(output, encoding="utf-8")
        first = result["first_divergence"]
        print(json.dumps({"native_calls": len(native), "adv_calls": len(adv),
                          "first_kind": first["kind"] if first else None,
                          "first_api": (first["native"] or first["adv"])["api"] if first else None,
                          "warnings": len(result["warnings"])}))
    else:
        print(output, end="")


if __name__ == "__main__":
    main()

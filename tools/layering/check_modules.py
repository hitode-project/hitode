#!/usr/bin/env python3
"""Check Rust module dependencies against A64A-DEP-1."""

from __future__ import annotations

from bisect import bisect_right
from pathlib import Path
import re
import sys

SRCDIR = Path(__file__).resolve().parents[2] / "crates/adv-cpu-a64/src"

EMIT = {"runtime", "asm", "encode"}
LOWER = EMIT | {"translate::front", "facts"}
RULES = {
    "encode": set(),
    "asm": set(),
    "cache": set(),
    "jit_map": set(),
    "runtime": {"asm", "encode"},
    "facts": EMIT,
    "translate::front": set(),
    "translate::lower": LOWER,
    "translate::jit": LOWER | {"translate::lower"},
    "translate::opt": LOWER | {"translate::lower"},
    "native::store": {"cache", "facts", "runtime"},
    "native::compile": {"translate::jit", "translate::opt", "translate::front", "facts"},
    "native::plan": {"native::store", "native::compile", "facts"},
    "native::offload": {"native::store", "native::plan", "runtime"},
    "cpu": {"executor"},
}
RULES["native::run"] = set(RULES) - {"cpu"}
RULES["executor"] = RULES["native::run"]
# lib.rs exposes the crate's public entry points.
RULES["lib"] = {"cache", "cpu", "executor"}

# Longest matching Rust module path wins. Files below the same role are
# implementation details of that role.
ROLES = {tuple(role.split("::")): role for role in RULES if role != "lib"}
ROLES.update({
    ("native",): "native::run",
    ("native", "head"): "native::store",
    ("translate",): "translate::jit",
    ("translate", "access"): "translate::lower",
    ("translate", "body"): "translate::lower",
})

# (Current file, disallowed dependency or parent glob). Delete an entry
# in the same change that removes its last occurrence.
TRANSITIONAL: set[tuple[str, str]] = {
    # Unsupported SSE control classes retain the in-place interpreter helper.
    ("translate/simd.rs", "interpreter form sse-mxcsr"),
    # Module responsibilities still colocated in the implementation.
    ("asm.rs", "encode"),
    ("native/head.rs", "asm"),
    ("native/head.rs", "encode"),
    ("native/head.rs", "native::plan"),
    ("native/head.rs", "translate::opt"),
    ("native/offload.rs", "executor"),
    ("native/offload.rs", "native::compile"),
    ("native/offload.rs", "native::run"),
    ("native/offload.rs", "translate::jit"),
    ("native/offload.rs", "translate::opt"),
    ("native/plan.rs", "cache"),
    ("native/plan.rs", "encode"),
    ("native/plan.rs", "native::run"),
    ("native/plan.rs", "runtime"),
    ("native/plan.rs", "translate::jit"),
    ("native/store.rs", "encode"),
    ("native/store.rs", "native::run"),
    ("native/store.rs", "translate::jit"),
    ("translate/access.rs", "translate::jit"),
    ("translate/body.rs", "translate::jit"),
    ("translate/opt/back.rs", "translate::jit"),
    ("translate/opt/back/batch.rs", "translate::jit"),
    ("translate/opt/back/inline.rs", "translate::jit"),
    ("translate/opt/back/resident.rs", "translate::jit"),
    ("translate/opt/front.rs", "translate::jit"),
    ("translate/opt/front/cells.rs", "translate::jit"),
    ("translate/opt/front/facts.rs", "translate::jit"),
    ("translate/opt/front/integer.rs", "translate::jit"),
    ("translate/opt/front/live.rs", "translate::jit"),
    ("translate/opt/front/model.rs", "translate::jit"),
    ("translate/opt/front/passes.rs", "translate::jit"),
    ("translate/opt/front/simd.rs", "translate::jit"),
    ("translate/opt/front/x87.rs", "translate::jit"),
    # Decoder and interpreter references awaiting their module moves.
    ("native.rs", "adv_cpu_core::interpreter"),
    ("translate/front/decode.rs", "adv_cpu_core::interpreter (tests)"),
    ("translate/front/decode.rs", "translate::jit"),
    ("translate/front/decode.rs", "adv_cpu_core::interpreter"),
    ("translate/front/plans.rs", "encode"),
    ("translate/front/plans.rs", "translate::jit"),
    ("translate/front/quick_form.rs", "adv_cpu_core::interpreter"),
    ("translate/front/trace.rs", "translate::jit"),
    # Existing test assemblies and reference interpreters.
    ("executor.rs", "iced_x86 (tests)"),
    ("executor/differential.rs", "iced_x86 (tests)"),
    ("executor/differential/native_instructions.rs", "iced_x86 (tests)"),
    ("executor/differential/native_instructions/channels.rs", "iced_x86 (tests)"),
    ("executor/differential/native_instructions/helper_gaps.rs", "iced_x86 (tests)"),
    ("executor/differential/native_instructions/loop_units.rs", "iced_x86 (tests)"),
    ("executor/differential/native_instructions/native_forms.rs", "iced_x86 (tests)"),
    ("executor/differential/native_instructions/pixel_loops.rs", "iced_x86 (tests)"),
    ("executor/differential/native_instructions/x87_wide.rs", "iced_x86 (tests)"),
    ("executor/differential/resident.rs", "iced_x86 (tests)"),
    ("executor/host_call_tests.rs", "iced_x86 (tests)"),
    ("native/offload.rs", "iced_x86 (tests)"),
    ("translate/body.rs", "iced_x86 (tests)"),
    ("translate/computation.rs", "iced_x86 (tests)"),
    ("translate/integer.rs", "iced_x86 (tests)"),
    ("translate/opt/front/tests.rs", "adv_cpu_core::interpreter (tests)"),
    ("translate/opt/front/tests.rs", "iced_x86 (tests)"),
    ("translate/parts.rs", "iced_x86 (tests)"),
    ("translate/residency.rs", "iced_x86 (tests)"),
    ("translate/simd.rs", "iced_x86 (tests)"),
    ("translate/strings.rs", "iced_x86 (tests)"),
    ("translate/tests.rs", "adv_cpu_core::interpreter (tests)"),
    ("translate/tests.rs", "iced_x86 (tests)"),
    ("translate/tests/opt_back/inline.rs", "adv_cpu_core::interpreter (tests)"),
    ("translate/x87.rs", "iced_x86 (tests)"),
}

INTERPRETER_FORM = re.compile(r"\bself\.helper\([^;]+\); // transitional interpreter form: ([\w-]+)$", re.M)

TOKEN = re.compile(
    r'(?P<comment>//[^\n]*|/\*)'
    r'|(?P<literal>[bc]?r(?P<hashes>\#*)".*?"(?P=hashes)|"(?:\\.|[^"\\])*"|'
    r"'(?:\\(?:u\{[\da-fA-F_]+\}|x[\da-fA-F]{2}|.)|[^'\\\n])'"
    r')|::|\w+|[^\s]',
    re.DOTALL,
)
BLOCK_COMMENT = re.compile(r"/\*|\*/")
CLOSE = {"(": ")", "[": "]", "{": "}"}


def lex(source: str) -> tuple[list[str], list[int]]:
    tokens, lines = [], []
    newlines = [match.start() for match in re.finditer("\n", source)]
    end = 0
    while match := TOKEN.search(source, end):
        end = match.end()
        if match.group() == "/*":
            depth = 1
            for delimiter in BLOCK_COMMENT.finditer(source, match.end()):
                depth += 1 if delimiter.group() == "/*" else -1
                if depth == 0:
                    end = delimiter.end()
                    break
            continue
        if match.group("comment"):
            continue
        tokens.append('""' if match.group("literal") else match.group())
        lines.append(bisect_right(newlines, match.start()) + 1)
    return tokens, lines


def delimiters(tokens: list[str]) -> dict[int, int]:
    stack = []
    ends = {}
    for i, token in enumerate(tokens):
        if token in CLOSE:
            stack.append(i)
        elif token in CLOSE.values():
            start = stack.pop()
            ends[start] = i
    return ends


def cfg_without_test(tokens: list[str], ends: dict[int, int],
                     start: int, end: int) -> bool | None:
    """Evaluate a cfg predicate with test=false and other flags unknown."""
    if tokens[start:end] == ["test"]:
        return False
    name = tokens[start]
    if name not in {"all", "any", "not"} or tokens[start + 1] != "(":
        return None
    values = []
    i = start + 2
    while i < end - 1:
        stop = i
        while stop < end - 1 and tokens[stop] != ",":
            stop = ends.get(stop, stop) + 1
        values.append(cfg_without_test(tokens, ends, i, stop))
        i = stop + 1
    if name == "not":
        return None if values[0] is None else not values[0]
    if name == "all":
        return False if False in values else None if None in values else True
    return True if True in values else None if None in values else False


def use_tree(tokens: list[str], i: int) -> tuple[list[tuple[str, ...]], int]:
    """Flatten Rust's grouped import tree into its individual paths."""
    path = []
    if tokens[i] == "::":
        i += 1
    while tokens[i] != "{":
        path.append(tokens[i])
        i += 1
        if tokens[i] != "::":
            if tokens[i] == "as":
                i += 2
            return [tuple(path)], i
        i += 1
    i += 1
    paths = []
    while tokens[i] != "}":
        children, i = use_tree(tokens, i)
        paths.extend(tuple(path) + child for child in children)
        if tokens[i] == ",":
            i += 1
    return paths, i + 1


def external(path: tuple[str, ...]) -> str | None:
    if path[0] == "iced_x86":
        return "iced_x86"
    if path[:2] in {("adv_cpu_core", "interpreter"), ("crate", "interpreter")}:
        return "adv_cpu_core::interpreter"
    return None


def inspect(source: str, module: tuple[str, ...]):
    """Collect imports, qualified external paths and test scopes."""
    tokens, lines = lex(source)
    ends = delimiters(tokens)
    imports, external_paths, test_modules = [], [], set()

    def walk(start: int, end: int, scope: tuple[str, ...], in_test: bool) -> None:
        only_test = False
        i = start
        while i < end:
            token = tokens[i]
            if token == "#" and tokens[i + 1] == "[":
                bracket = ends[i + 1]
                if tokens[i + 2:i + 4] == ["cfg", "("]:
                    only_test |= cfg_without_test(tokens, ends, i + 4, bracket - 1) is False
                elif tokens[i + 2:bracket] == ["test"]:
                    only_test = True
                i = bracket + 1
                continue
            if token == "mod" and tokens[i + 2] in {"{", ";"}:
                name = scope + (tokens[i + 1],)
                if tokens[i + 2] == "{":
                    stop = ends[i + 2]
                    walk(i + 3, stop, name, in_test or only_test)
                    i = stop
                else:
                    if in_test or only_test:
                        test_modules.add(name)
                    i += 2
                only_test = False
            elif token == "use" and tokens[i + 1] != "<":
                paths, stop = use_tree(tokens, i + 1)
                imports.extend((scope, path, lines[i], in_test or only_test) for path in paths)
                i = stop
                only_test = False
            elif token == "{":
                stop = ends[i]
                walk(i + 1, stop, scope, in_test or only_test)
                i = stop
                only_test = False
            elif token in {";", ","}:
                only_test = False
            elif tokens[i + 1:i + 2] == ["::"]:
                if dep := external(tuple(tokens[i:i + 3:2])):
                    external_paths.append((dep, lines[i], in_test or only_test))
            i += 1

    walk(0, len(tokens), module, False)
    return imports, external_paths, test_modules


def module_path(file: str) -> tuple[str, ...]:
    path = Path(file).with_suffix("").parts
    return path[:-1] if path[-1] in {"lib", "mod"} else path


def role(module: tuple[str, ...]) -> str | None:
    for length in range(len(module), 0, -1):
        if name := ROLES.get(module[:length]):
            return name
    return "lib" if not module else None


def import_path(scope: tuple[str, ...], path: tuple[str, ...]) -> tuple[str, ...]:
    if path[0] == "crate":
        return path[1:]
    if path[0] == "self":
        return scope + path[1:]
    if path[0] == "super":
        parents = 0
        while parents < len(path) and path[parents] == "super":
            parents += 1
        return scope[:-parents] + path[parents:]
    return scope + path


def disallowed(source: str, scope: tuple[str, ...], path: tuple[str, ...],
               modules: set[tuple[str, ...]], in_test: bool) -> str | None:
    if dep := external(path):
        if dep == "iced_x86":
            allowed = source == "translate::front"
        else:
            allowed = source in {"executor", "native::offload"}
        return None if allowed else dep + (" (tests)" if in_test else "")
    if in_test:
        return None
    target = import_path(scope, path)
    if path[-1] == "*" and len(target) <= len(scope) and scope[:len(target) - 1] == target[:-1]:
        return f"parent glob {'::'.join(('crate', *target))}"
    for length in range(len(target), 0, -1):
        if target[:length] in modules:
            dest = role(target[:length])
            if dest and dest != source and dest != "jit_map" and dest not in RULES[source]:
                return dest
            break
    return None


def violations(sources: dict[str, str], transitional=TRANSITIONAL) -> list[str]:
    modules = {module_path(file) for file in sources}
    parsed = {file: inspect(text, module_path(file)) for file, text in sources.items()}
    tests = set().union(*(result[2] for result in parsed.values()))
    found: dict[tuple[str, str], int] = {}
    errors = []
    for file, (imports, external_paths, _) in parsed.items():
        module = module_path(file)
        test_file = any(module[:len(test)] == test for test in tests)
        source = role(module) or "test"
        if source == "test" and not test_file:
            errors.append(f"{file}: no module role in tools/layering/check_modules.py")
            continue
        for match in INTERPRETER_FORM.finditer(sources[file]):
            found[(file, f"interpreter form {match[1]}")] = sources[file].count("\n", 0, match.start()) + 1
        for scope, path, line, in_test in imports:
            if dep := disallowed(source, scope, path, modules, in_test or test_file):
                found.setdefault((file, dep), line)
        for dep, line, in_test in external_paths:
            path = tuple(dep.split("::"))
            if dep := disallowed(source, module, path, modules, in_test or test_file):
                found.setdefault((file, dep), line)
    for (file, dep), line in found.items():
        if (file, dep) not in transitional:
            errors.append(f"{file}:{line}: {role(module_path(file)) or 'test'} -> {dep}: disallowed")
    for file, dep in transitional - found.keys():
        errors.append(f"{file} -> {dep}: transitional edge is gone; drop it")
    return sorted(errors)


def main() -> int:
    sources = {path.relative_to(SRCDIR).as_posix(): path.read_text(encoding="utf-8")
               for path in SRCDIR.rglob("*.rs")}
    errors = violations(sources)
    for error in errors:
        print(error)
    print("a64 module dependencies:", "FAILED" if errors else "ok")
    return int(bool(errors))


if __name__ == "__main__":
    sys.exit(main())

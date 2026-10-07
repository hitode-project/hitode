#!/usr/bin/env python3
"""Compare scripted x86 DLL exports on native Windows, interpreter and JIT.

Usage (build first with tools/fixtures/build-dll-driver.ps1):
  python tools/performance/dll_exports.py --runner target/release/adv-run.exe
  python tools/performance/dll_exports.py --script target/local-script.json \
      --sample-dir engine/module --runner target/release/adv-run.exe

Inputs: the x86 fixture EXE, version-1 JSON and repository-owned probe DLL, or a
sample directory relative to $ADV_SAMPLES containing the DLL/dependencies/data.
Output: per-call counts, scalar comparisons, memory hashes, timing statistics and
SHA-256 provenance in summary.json; isolated execution images and overlays under
target/investigations/dll-exports. No memory bytes or pointer values are reported.
Each run has cmd.txt recording its command, environment and binary hashes.

Native QPC measures the loop. adv-run QPC is a guest clock and is diagnostic only:
guest timings use host perf_counter_ns observations of begin/end JSONL markers.
Polling has scheduling/I/O error; intervals below --min-ms receive no ratio.
Warmup mutates the same state; loading, warmup and memory hashing are excluded.
This measures a scripted module workload, not actual game frame rate or power.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path, PureWindowsPath
import shutil
import subprocess
import time
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
EVENT_FILE = 'dll-driver.jsonl'
POLL_SECONDS = 0.001


def file_hash(path: Path) -> str:
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def relative_input(value: str) -> Path:
    """Keep script DLL/file inputs inside the copied execution image."""
    windows = PureWindowsPath(value)
    path = Path(value.replace('\\', '/'))
    if not value or windows.drive or windows.root or path.is_absolute() or '..' in path.parts:
        raise ValueError('DLL and buffer input paths must be relative to the execution image')
    return path


def sample_directory(relative: str) -> Path:
    """Resolve real sample inputs only through ADV_SAMPLES."""
    root = os.environ.get('ADV_SAMPLES')
    if not root:
        raise ValueError('--sample-dir requires ADV_SAMPLES')
    base = Path(root).resolve()
    path = (base / relative_input(relative)).resolve()
    if not path.is_relative_to(base) or not path.is_dir():
        raise ValueError('sample directory must exist inside ADV_SAMPLES')
    return path


class Events:
    """Validate the JSONL protocol and attach host observations to loop markers."""

    def __init__(self) -> None:
        self.offset = 0
        self.pending = b''
        self.active: str | None = None
        self.started = 0
        self.elapsed: int | None = None
        self.rows: list[dict[str, Any]] = []
        self.complete = False
        self.max_poll_gap_ns = 0
        self.last_poll_ns: int | None = None

    def consume(self, data: bytes, observed_ns: int) -> None:
        self.pending += data
        while b'\n' in self.pending:
            line, self.pending = self.pending.split(b'\n', 1)
            event = json.loads(line)
            kind = event.get('event')
            if self.complete:
                raise ValueError('events after completion')
            if kind == 'error':
                raise ValueError(f"driver rejected script: {event['message']}")
            if kind == 'begin':
                if self.active is not None or any(r['id'] == event['id'] for r in self.rows):
                    raise ValueError('duplicate or nested loop')
                self.active = event['id']
                self.started = observed_ns
                self.elapsed = None
            elif kind == 'end':
                if self.active != event['id'] or self.elapsed is not None:
                    raise ValueError('unpaired end marker')
                self.elapsed = observed_ns - self.started
            elif kind == 'result':
                if self.active != event['id'] or self.elapsed is None:
                    raise ValueError('result without completed loop')
                event['observed_loop_ns'] = self.elapsed
                self.rows.append(event)
                self.active = None
            elif kind == 'complete':
                if self.active is not None or event['calls'] != len(self.rows) or not self.rows:
                    raise ValueError('incomplete export calls')
                self.complete = True
            else:
                raise ValueError('unknown driver event')

    def poll(self, path: Path) -> None:
        now = time.perf_counter_ns()
        if self.last_poll_ns is not None:
            self.max_poll_gap_ns = max(self.max_poll_gap_ns, now - self.last_poll_ns)
        self.last_poll_ns = now
        try:
            with path.open('rb') as stream:
                stream.seek(self.offset)
                data = stream.read()
        except FileNotFoundError:
            return
        self.offset += len(data)
        self.consume(data, time.perf_counter_ns())

    def finish(self, expected_ids: list[str]) -> list[dict[str, Any]]:
        if self.pending or not self.complete or [r['id'] for r in self.rows] != expected_ids:
            raise ValueError('driver did not complete the scripted call sequence')
        return self.rows


def signature(row: dict[str, Any]) -> tuple[Any, ...]:
    """Compare only ABI-defined returns and requested memory hash ranges."""
    kind = row['return_kind']
    result = (row['eax'], row['edx']) if kind == 'u64' else (
        (row['eax'],) if kind == 'u32' else ((row['is_null'],) if kind == 'pointer' else ()))
    if kind not in ('u32', 'u64', 'pointer', 'void'):
        raise ValueError('unsupported result kind')
    hashes = tuple((h['buffer'], h['offset'], h['size'], h['fnv1a64']) for h in row['hashes'])
    return row['id'], row['iterations'], row['warmup'], kind, result, hashes


def compare(runs: dict[str, dict[str, Any]], min_ms: float) -> dict[str, Any]:
    """Gate cost ratios on a full per-function return/hash comparison."""
    native = runs['native']['calls']
    if any(len(run['calls']) != len(native) for run in runs.values()):
        raise ValueError('different call counts')
    rows = []
    total_native = {variant: 0 for variant in runs if variant != 'native'}
    total_guest = dict(total_native)
    for index, reference in enumerate(native):
        frequency = int(reference['qpc_frequency_hex'], 16)
        if frequency <= 0:
            raise ValueError('invalid native QPC frequency')
        baseline_ns = int(reference['qpc_ticks_hex'], 16) * 1_000_000_000 / frequency
        entry: dict[str, Any] = {
            'id': reference['id'], 'iterations': reference['iterations'],
            'native_loop_ms': baseline_ns / 1_000_000, 'variants': {},
        }
        for variant, run in runs.items():
            if variant == 'native':
                continue
            actual = run['calls'][index]
            matches = signature(actual) == signature(reference)
            measured_ns = actual['observed_loop_ns']
            # Polling delay can exceed the requested interval under heavy load.
            threshold_ns = max(min_ms * 1_000_000, 10 * run['max_poll_gap_ns'])
            measurable = baseline_ns >= threshold_ns and measured_ns >= threshold_ns
            ratio = measured_ns / baseline_ns if matches and measurable else None
            entry['variants'][variant] = {
                'matches': matches, 'host_loop_ms': measured_ns / 1_000_000,
                'cost_ratio': ratio,
                'ratio_status': 'measured' if ratio is not None else ('different_output' if not matches else 'interval_too_short'),
            }
            if ratio is not None:
                total_native[variant] += baseline_ns
                total_guest[variant] += measured_ns
        rows.append(entry)
    matches = all(v['matches'] for r in rows for v in r['variants'].values())
    return {
        'all_match': matches, 'calls': rows,
        'module_cost_ratio': {v: total_guest[v] / total_native[v] if matches and total_native[v] else None for v in total_native},
        'measured_calls': {v: sum(r['variants'][v]['cost_ratio'] is not None for r in rows) for v in total_native},
        'scope': 'scripted export loops; native QPC vs host marker observations; excludes load, warmup and hashes',
    }


def check_report(report: dict[str, Any]) -> dict[str, Any]:
    """Require natural strict exit; retain statistical runtime fields only."""
    api = report.get('api', {})
    if (report.get('stop_reason') != 'guest_exit' or report.get('guest_exit_code') != 0 or
            report.get('failure') is not None or api.get('mode') != 'strict' or api.get('acceptance_eligible') is not True or
            api.get('warnings') or report.get('compile_failures', 0)):
        raise ValueError('adv-run did not complete a strict, warning-free guest execution')
    names = ('elapsed_seconds', 'host_calls', 'native_entries', 'interpreter_entries',
             'compilations', 'compile_seconds', 'compile_failures')
    return {name: report[name] for name in names if name in report}


def run_variant(variant: str, directory: Path, fixture: Path, modules: Path,
                script: dict[str, Any], runner: Path | None, timeout: float,
                runner_args: list[str]) -> dict[str, Any]:
    directory.mkdir()
    image = directory / 'image'
    shutil.copytree(modules, image)
    executable = image / 'fixture_dll_driver.exe'
    shutil.copy2(fixture, executable)
    (image / 'dll-driver.json').write_text(json.dumps(script, ensure_ascii=True), encoding='utf-8')
    # Never pick up a stale event/report from a copied source directory.
    events_path = image / EVENT_FILE
    if events_path.exists():
        events_path.unlink()
    report_path = directory / 'runtime.json'
    environment = os.environ.copy()
    command = [str(executable)]
    if variant != 'native':
        assert runner is not None
        binary_directory = directory / 'bin'
        binary_directory.mkdir()
        fixed_runner = binary_directory / runner.name
        shutil.copy2(runner, fixed_runner)
        for dll in runner.parent.glob('*.dll'):
            shutil.copy2(dll, binary_directory / dll.name)
        command = [str(fixed_runner), str(executable), '--headless', '--backend', variant,
                   '--seconds', str(timeout), '--data-dir', str(directory / 'data'),
                   '--report', str(report_path), '--api-mode', 'strict',
                   '--codec', 'guest', '--codec-kernels', 'guest', *runner_args]
        events_path = directory / 'data' / 'overlay' / EVENT_FILE
    dll_path = image / relative_input(script['dll'])
    hashes = {'exe_sha256': file_hash(executable), 'dll_sha256': file_hash(dll_path),
              'script_sha256': file_hash(image / 'dll-driver.json')}
    if variant != 'native':
        hashes['runner_sha256'] = file_hash(Path(command[0]))
    # Recording explicitly set ADV_* flags is enough; do not dump unrelated secrets.
    recorded_env = {k: v for k, v in environment.items() if k.startswith('ADV_')}
    (directory / 'cmd.txt').write_text(
        subprocess.list2cmdline(command) + '\n' + json.dumps({'environment': recorded_env, **hashes}, indent=2) + '\n',
        encoding='utf-8')
    observer = Events()
    runtime_report = None
    started = time.perf_counter_ns()
    process = subprocess.Popen(command, cwd=image, env=environment, stdout=subprocess.DEVNULL,
                               stderr=subprocess.DEVNULL,
                               creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
    try:
        while True:
            observer.poll(events_path)
            if process.poll() is not None:
                observer.poll(events_path)
                break
            if (time.perf_counter_ns() - started) / 1e9 > timeout + 5:
                raise TimeoutError('export driver exceeded the host deadline')
            time.sleep(POLL_SECONDS)
    finally:
        if process.poll() is None:
            process.kill()
        process.wait()
        if report_path.exists():
            try:
                runtime_report = json.loads(report_path.read_text(encoding='utf-8'))
            finally:
                report_path.unlink()  # Full diagnostics can contain instruction addresses.
    if process.returncode != 0:
        raise ValueError(f'{variant} driver exited with status {process.returncode}')
    rows = observer.finish([call['id'] for call in script['calls']])
    statistics = check_report(runtime_report or {}) if variant != 'native' else {}
    return {'calls': rows, 'process_ms': (time.perf_counter_ns() - started) / 1e6,
            'max_poll_gap_ns': observer.max_poll_gap_ns, 'runtime': statistics, **hashes}


def positive(value: str) -> float:
    number = float(value)
    if not math.isfinite(number) or number <= 0:
        raise argparse.ArgumentTypeError('must be positive and finite')
    return number


def validate_runner_args(arguments: list[str]) -> None:
    """Keep workload, result capture and guest-only execution fixed across runs."""
    reserved = {'--exe', '--backend', '--seconds', '--data-dir', '--report', '--api-mode',
                '--codec', '--codec-kernels', '--trace', '--host-profile', '--schedule-profile',
                '--profile', '--screenshot', '--hidden', '--headless', '--control-stdin',
                '--self-test-input', '--frames'}
    if any(argument.split('=', 1)[0] in reserved for argument in arguments):
        raise ValueError('--runner-arg cannot override workload, capture or execution policy')


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--fixture', type=Path, default=ROOT / 'target/fixtures-dll-driver/fixture_dll_driver.exe')
    parser.add_argument('--script', type=Path, default=ROOT / 'tools/fixtures/dll-driver-example.json')
    parser.add_argument('--sample-dir', help='DLL/dependency/data directory relative to ADV_SAMPLES')
    parser.add_argument('--runner', type=Path, help='built adv-run; required unless --variants native')
    parser.add_argument('--variants', nargs='+', choices=('native', 'interpreter', 'jit'), default=['native', 'interpreter', 'jit'])
    parser.add_argument('--runner-arg', action='append', default=[], help='extra adv-run option, e.g. --runner-arg=--sync-compilation')
    parser.add_argument('--timeout', type=positive, default=60.0)
    parser.add_argument('--min-ms', type=positive, default=20.0)
    parser.add_argument('--output', type=Path, default=ROOT / 'target/investigations/dll-exports' / time.strftime('%Y%m%d-%H%M%S'))
    args = parser.parse_args(argv)
    try:
        if 'native' not in args.variants or len(set(args.variants)) != len(args.variants):
            raise ValueError('variants must include native and have no duplicates')
        if args.variants != ['native'] and args.runner is None:
            raise ValueError('interpreter/JIT require --runner')
        validate_runner_args(args.runner_arg)
        fixture = args.fixture.resolve()
        runner = args.runner.resolve() if args.runner else None
        modules = sample_directory(args.sample_dir) if args.sample_dir else fixture.parent
        script = json.loads(args.script.read_text(encoding='utf-8'))
        relative_input(script['dll'])
        for buffer in script.get('buffers', []):
            if 'file' in buffer:
                relative_input(buffer['file'])
        output = args.output.resolve()
        if not output.is_relative_to(ROOT / 'target/investigations') or output.exists():
            raise ValueError('output must be a new directory inside target/investigations')
        if output.is_relative_to(modules):
            raise ValueError('output cannot be inside the module input directory')
        output.mkdir(parents=True)
        # Freeze all inputs once, so another build/edit cannot change later variants.
        inputs = output / 'inputs'
        frozen_modules = inputs / 'image'
        shutil.copytree(modules, frozen_modules)
        frozen_fixture = inputs / 'fixture_dll_driver.exe'
        shutil.copy2(fixture, frozen_fixture)
        if runner:
            frozen_bin = inputs / 'bin'
            frozen_bin.mkdir()
            shutil.copy2(runner, frozen_bin / runner.name)
            for dll in runner.parent.glob('*.dll'):
                shutil.copy2(dll, frozen_bin / dll.name)
            runner = frozen_bin / runner.name
        runs = {variant: run_variant(variant, output / variant, frozen_fixture, frozen_modules, script, runner,
                                     args.timeout, args.runner_arg) for variant in args.variants}
        summary = {'version': 1, 'poll_interval_ms': POLL_SECONDS * 1000,
                   'comparison': compare(runs, args.min_ms), 'runs': runs}
        (output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n', encoding='utf-8')
        print(json.dumps({'all_match': summary['comparison']['all_match'],
                          'calls': len(script['calls']),
                          'module_cost_ratio': summary['comparison']['module_cost_ratio']}))
        return 0 if summary['comparison']['all_match'] else 1
    except (OSError, ValueError, KeyError, TimeoutError) as error:
        parser.exit(1, f'dll_exports: {error}\n')


if __name__ == '__main__':
    raise SystemExit(main())

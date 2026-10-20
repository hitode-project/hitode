#!/usr/bin/env python3
"""Compare two built adv-run binaries on a local fixed-frame prefix.

No assets, screenshots or traces are collected. Raw reports and stdout can still
contain private paths: keep the output directory local and ignored by Git.
Final-frame equality is a necessary gate, not full execution/audio equivalence.
Use --mode hidden to retain the interactive compiler policy and presentation
thread without showing a window; requested hardware audio is muted in that mode.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shutil
import statistics
import subprocess
import tempfile
import time
from typing import Any


def positive_metric(report: dict[str, Any], key: str) -> float:
    value = report.get(key)
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError(f"missing or nonnumeric {key}")
    if not math.isfinite(value) or value <= 0:
        raise ValueError(f"invalid {key}: {value!r}")
    return float(value)


def signature(report: dict[str, Any], frames: int) -> tuple[Any, ...]:
    """Reject incomplete, failed, or observably different fixed-frame work."""
    if report.get('failure') is not None:
        raise ValueError(f"guest failure: {report['failure']}")
    if type(report.get('frames')) is not int or report['frames'] != frames:
        raise ValueError(f"expected exactly {frames} frames, got {report.get('frames')!r}")
    for name in ('width', 'height'):
        if type(report.get(name)) is not int or report[name] <= 0:
            raise ValueError(f"invalid {name}")
    digest = report.get('frame_fnv1a64', '')
    if not isinstance(digest, str) or not re.fullmatch('[0-9a-fA-F]{16}', digest):
        raise ValueError('missing final-frame checksum')
    positive_metric(report, 'elapsed_seconds')
    if report.get('first_frame_seconds') is not None:
        positive_metric(report, 'first_frame_seconds')
    for name in ('stop_reason', 'backend'):
        if not isinstance(report.get(name), str) or not report[name]:
            raise ValueError(f"missing {name}")
    missing = report.get('missing_apis')
    if not isinstance(missing, list) or not all(isinstance(x, str) for x in missing):
        raise ValueError('missing API diagnostics')
    return (
        frames, report['width'], report['height'], digest.lower(),
        report['stop_reason'], report.get('guest_exit_code'), report['backend'],
        report.get('compile_strategy'), report.get('region_linking'),
        report.get('codepage'), tuple(sorted(missing)),
    )


def summarize(rows: list[dict[str, Any]], frames: int) -> dict[str, Any]:
    if not rows:
        raise ValueError('no completed runs')
    expected = signature(rows[0]['report'], frames)
    for row in rows:
        if signature(row['report'], frames) != expected:
            raise ValueError('work differs across runs; speedup is not comparable')
        positive_metric(row, 'process_seconds')
    grouped = {label: [r for r in rows if r['variant'] == label]
               for label in ('baseline', 'candidate')}
    if not grouped['baseline'] or len(grouped['baseline']) != len(grouped['candidate']):
        raise ValueError('need equal nonzero baseline and candidate run counts')
    result: dict[str, Any] = {'frames': frames, 'runs_per_variant': len(grouped['baseline']),
                              'final_frame_checksum': expected[3]}
    for metric in ('elapsed_seconds', 'process_seconds', 'first_frame_seconds'):
        values: dict[str, float] = {}
        for label, group in grouped.items():
            source = [r if metric == 'process_seconds' else r['report'] for r in group]
            if any(s.get(metric) is None for s in source):
                break
            values[label] = statistics.median(positive_metric(s, metric) for s in source)
        if len(values) == 2:
            result[metric] = {**values, 'speedup': values['baseline'] / values['candidate']}
    result['scope'] = ('Cold process / fixed frame prefix, including real waits. '
                       'Final-frame equality does not prove intermediate-frame, audio, '
                       'scheduler fairness or exception equivalence.')
    return result


def file_hash(path: Path) -> str:
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', required=True, type=Path)
    parser.add_argument('--candidate', required=True, type=Path)
    parser.add_argument('--exe', required=True, help='EXE path relative to ADV_SAMPLES')
    parser.add_argument('--frames', type=int, default=120)
    parser.add_argument('--pairs', type=int, default=3)
    parser.add_argument('--timeout', type=float, default=300.0)
    parser.add_argument('--codepage', choices=('auto', '932', '936'), default='auto')
    parser.add_argument('--backend', choices=('jit', 'interpreter'), default='jit')
    parser.add_argument('--audio', action='store_true')
    parser.add_argument('--mode', choices=('headless', 'hidden'), default='headless')
    parser.add_argument('--auto-dialog-ms', type=int)
    parser.add_argument('--seed-data', type=Path, help='copy this overlay separately for each run')
    parser.add_argument('--output', type=Path, default=Path('target/perf-runtime'))
    args = parser.parse_args()
    if args.frames <= 0 or args.pairs <= 0 or not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error('frames, pairs and timeout must be positive')
    if args.auto_dialog_ms is not None and not 0 <= args.auto_dialog_ms <= 0xffffffff:
        parser.error('auto-dialog-ms must fit an unsigned 32-bit integer')
    root_string = os.environ.get('ADV_SAMPLES')
    if not root_string:
        parser.error('ADV_SAMPLES must name the local sample root')
    root = Path(root_string).resolve(strict=True)
    exe = (root / args.exe).resolve(strict=True)
    if not exe.is_relative_to(root) or not exe.is_file():
        parser.error('exe must be a file inside ADV_SAMPLES')
    binaries = {label: getattr(args, label).resolve(strict=True)
                for label in ('baseline', 'candidate')}
    hashes = {label: file_hash(path) for label, path in binaries.items()}
    if hashes['baseline'] == hashes['candidate']:
        parser.error('binary hashes are identical; verify separate build target directories')
    if args.seed_data is not None:
        args.seed_data = args.seed_data.resolve(strict=True)
        if not args.seed_data.is_dir():
            parser.error('seed-data must be a directory')
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)  # Never overwrite prior evidence.
    rows: list[dict[str, Any]] = []
    metadata = {'binary_sha256': hashes, 'sample': str(exe.relative_to(root)),
                'frames': args.frames, 'pairs': args.pairs, 'audio': args.audio,
                'backend': args.backend, 'codepage': args.codepage,
                'mode': args.mode, 'order': []}
    environment = dict(os.environ)
    if args.mode == 'hidden' and args.audio:
        environment['ADV_AUDIO_MUTE_DEVICE'] = '1'
    try:
        for pair in range(args.pairs):
            order = ('baseline', 'candidate') if pair % 2 == 0 else ('candidate', 'baseline')
            for label in order:
                name = f'{pair + 1:02d}-{label}'
                metadata['order'].append(name)
                report_path = output / f'{name}.json'
                with tempfile.TemporaryDirectory(prefix='adv-perf-') as temporary:
                    overlay = Path(temporary) / 'data'
                    if args.seed_data is not None:
                        shutil.copytree(args.seed_data, overlay)
                    else:
                        overlay.mkdir()
                    command = [str(binaries[label]), str(exe), '--' + args.mode, '--backend',
                               args.backend, '--compile', 'fast', '--frames', str(args.frames),
                               '--codepage', args.codepage, '--data-dir', str(overlay),
                               '--report', str(report_path)]
                    if args.audio:
                        command.append('--audio')
                    if args.auto_dialog_ms is not None:
                        command += ['--auto-dialog-ms', str(args.auto_dialog_ms)]
                    started = time.perf_counter()
                    with (output / f'{name}.log').open('wb') as log:
                        subprocess.run(command, stdout=log, stderr=subprocess.STDOUT,
                                       timeout=args.timeout, check=True, env=environment,
                                       creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
                    elapsed = time.perf_counter() - started
                report = json.loads(report_path.read_text(encoding='utf-8'))
                signature(report, args.frames)
                rows.append({'variant': label, 'process_seconds': elapsed, 'report': report})
                print(f'{name}: process={elapsed:.6f}s report={report["elapsed_seconds"]:.6f}s', flush=True)
        result = summarize(rows, args.frames)
        (output / 'summary.json').write_text(json.dumps(result, indent=2) + '\n', encoding='utf-8')
        print(json.dumps(result, indent=2))
    finally:
        (output / 'environment.json').write_text(json.dumps(metadata, indent=2) + '\n', encoding='utf-8')
        (output / 'runs.json').write_text(json.dumps(rows, indent=2) + '\n', encoding='utf-8')


if __name__ == '__main__':
    main()

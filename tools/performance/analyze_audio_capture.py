#!/usr/bin/env python3
"""Read-only analysis of callback f32 PCM, its timeline, and guest PCM dumps.

Requires NumPy. Zero output is a candidate, not an underrun measurement. Source
confirmation is restricted to one active, fully covered source with no format
change; optional callback missing-frame counters remain separate evidence.
The tool never opens an audio device and writes only to --output.
"""
from __future__ import annotations

import argparse
import csv
from dataclasses import dataclass, field
import hashlib
import json
import math
from pathlib import Path
import re

import numpy as np


def fields(line: str) -> dict[str, str]:
    return dict(re.findall(r"(\w+)=([^\s]+)", line))


def runs(mask: np.ndarray) -> list[tuple[int, int]]:
    changes = np.flatnonzero(mask[1:] != mask[:-1]) + 1
    edges = np.r_[0, changes, len(mask)]
    return [(int(a), int(b)) for a, b in zip(edges[:-1], edges[1:]) if mask[a]]


def timeline(path: Path) -> tuple[list[dict], list[dict]]:
    epochs, packets = [], []
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        f = fields(line)
        if line.startswith("device "):
            epochs.append({"rate": int(f["rate"]), "channels": int(f["channels"]),
                           "first_sample": int(f["first_file_sample"]),
                           "muted": f.get("hardware_muted") == "true"})
        elif line.startswith("callback="):
            if not epochs:
                raise ValueError("callback precedes format header")
            packet = {k: int(v) if v.isdecimal() else v for k, v in f.items()}
            packet["epoch"] = len(epochs) - 1
            packets.append(packet)
    if not epochs or not packets:
        raise ValueError("empty callback timeline")
    return epochs, packets


def rms(x: np.ndarray) -> float:
    valid = x[np.isfinite(x)]
    return float(np.sqrt(np.mean(np.square(valid.astype(np.float64))))) if valid.size else 0.0


def peak(x: np.ndarray) -> float:
    valid = x[np.isfinite(x)]
    return float(np.max(np.abs(valid))) if valid.size else 0.0


def output_channels(stereo: np.ndarray, channels: int) -> np.ndarray:
    if channels == 1:
        return stereo.mean(axis=1, keepdims=True)
    if channels == 2:
        return stereo
    out = np.zeros((len(stereo), channels))
    out[:, :2] = stereo
    return out


def db_gain(value: int) -> float:
    return 0.0 if value <= -10000 else 10.0 ** (value / 2000.0)


def signed32(value: str) -> int:
    number = int(value, 16)
    return number - 2**32 if number >= 2**31 else number


@dataclass
class Source:
    id: str
    generation: int
    start_us: int
    rate: int
    channels: int
    bits: int
    end_us: float = math.inf
    loop: bool = False
    volume: list[tuple[int, int]] = field(default_factory=list)
    pan: list[tuple[int, int]] = field(default_factory=list)
    chunks: list[dict] = field(default_factory=list)
    path: Path | None = None
    changed_format: bool = False
    data: np.ndarray | None = None
    starts: np.ndarray | None = None

    def gain(self, pts_us: np.ndarray) -> np.ndarray:
        def values(events: list[tuple[int, int]]) -> np.ndarray:
            at, value = zip(*events)
            indices = np.searchsorted(at, pts_us, side="right") - 1
            return np.array(value)[np.maximum(indices, 0)]
        volume, pan = values(self.volume), values(self.pan)
        gain = np.where(volume <= -10000, 0.0, np.power(10.0, volume / 2000.0))
        return np.column_stack((gain * np.power(10.0, -np.maximum(pan, 0) / 2000.0),
                                gain * np.power(10.0, np.minimum(pan, 0) / 2000.0)))

    def samples(self, indices: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
        if self.data is None:
            if self.path is None or not self.path.exists() or not self.path.stat().st_size:
                return np.zeros((len(indices), self.channels)), np.zeros(len(indices), dtype=bool)
            dtype = "<i2" if self.bits == 16 else "u1"
            self.data = np.memmap(self.path, dtype=dtype, mode="r")
            self.starts = np.array([c["frames"] for c in self.chunks], dtype=np.int64)
        rows = np.searchsorted(self.starts, indices, side="right") - 1
        covered = rows >= 0
        offsets = np.zeros(len(indices), dtype=np.int64)
        for row in np.unique(rows[covered]):
            selected = rows == row
            c = self.chunks[int(row)]
            relative = indices[selected] - c["frames"]
            covered[selected] = relative < c["count"]
            offsets[selected] = c["file_offset"] // (self.bits // 8) + relative * self.channels
        covered &= (offsets >= 0) & (offsets + self.channels <= len(self.data))
        out = np.zeros((len(indices), self.channels))
        for channel in range(self.channels):
            out[covered, channel] = self.data[offsets[covered] + channel]
        if self.bits == 16:
            out /= 32768.0
        else:
            out = (out - 128.0) / 128.0
        return out, covered

    def expected(self, pts_us: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
        phase = (pts_us - self.start_us) * self.rate / 1_000_000.0
        integer = np.floor(phase).astype(np.int64)
        fraction = phase - integer
        a, valid_a = self.samples(integer)
        b, valid_b = self.samples(integer + 1)
        output = a + (b - a) * fraction[:, None]
        if self.channels == 1:
            output = np.repeat(output, 2, axis=1)
        return output[:, :2] * self.gain(pts_us), valid_a & valid_b


def source_log(directory: Path) -> tuple[list[Source], dict]:
    path = directory / "sound.log"
    if not path.exists():
        return [], {"available": False}
    sources, active, volumes, pans = [], {}, {}, {}
    accepted_false = 0
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.startswith("api "):
            m = re.search(r"us=(\d+).*method=(\w+) args=\[([^]]+)\]", line)
            if not m:
                continue
            at, method, args = int(m[1]), m[2], m[3].split(", ")
            identity = args[0]
            if method in ("SetVolume", "SetPan"):
                value = signed32(args[1])
                (volumes if method == "SetVolume" else pans)[identity] = value
                if identity in active:
                    (active[identity].volume if method == "SetVolume" else active[identity].pan).append((at, value))
            elif method == "Stop" and identity in active:
                source = active.pop(identity)
                source.end_us = min(source.end_us, at)
            elif method in ("SetFrequency", "SetFormat", "SetCurrentPosition") and identity in active:
                active[identity].changed_format = True
        elif line.startswith("start "):
            f = fields(line)
            identity = f["id"]
            if identity in active:
                source = active.pop(identity)
                source.end_us = min(source.end_us, int(f["us"]))
            source = Source(identity, int(f["generation"]), int(f["us"]),
                            int(f["rate"]), int(f["channels"]), int(f["bits"]),
                            loop=f["loop"] != "0")
            source.volume.append((source.start_us, volumes.get(identity, 0)))
            source.pan.append((source.start_us, pans.get(identity, 0)))
            if not source.loop:
                frames = (int(f["bytes"]) - int(f.get("base", 0))) / (source.channels * source.bits // 8)
                source.end_us = source.start_us + frames / source.rate * 1_000_000
            active[identity] = source
            sources.append(source)
        elif line.startswith("end "):
            f = fields(line)
            if f["id"] in active and active[f["id"]].generation == int(f["generation"]):
                source = active.pop(f["id"])
                source.end_us = min(source.end_us, int(f["us"]))
        elif line.startswith("dump-chunk "):
            f = fields(line)
            source = active.get(f["id"])
            if source is not None and source.generation == int(f["generation"]):
                source.chunks.append({k: int(f[k]) for k in ("frames", "count", "pts", "file_offset", "us")})
                name = re.search(r'path="(.*)"', line)[1].replace("\\", "/").split("/")[-1]
                source.path = directory / "source-dumps" / name
        elif line.startswith("queue ") and "accepted=false" in line:
            accepted_false += 1
    return sources, {"available": True, "streams": len(sources), "rejected_queue_records": accepted_false,
                     "chunk_records": sum(len(s.chunks) for s in sources)}


def checkpoints(directory: Path) -> tuple[list[dict], float | None]:
    actions_path = directory / "actions.json"
    actions = json.loads(actions_path.read_text(encoding="utf-8")) if actions_path.exists() else []
    report_ack = {}
    for action in actions:
        command = action.get("command", "")
        if command.startswith("report "):
            name = command[7:].replace("\\", "/").split("/")[-1]
            report_ack[name] = action["ack_seconds"]
    rows, offsets = [], []
    for path in directory.glob("*.json"):
        report = json.loads(path.read_text(encoding="utf-8"))
        if not isinstance(report, dict) or "pcm" not in report:
            continue
        clock = re.search(r"Guest contexts \(clock (\d+) us\)", report.get("runtime", ""))
        row = {"name": path.stem, "elapsed_seconds": report["elapsed_seconds"],
               "clock_us": int(clock[1]) if clock else None, "pcm": report["pcm"],
               "audio": report.get("audio", {})}
        if path.name in report_ack and clock:
            offsets.append(int(clock[1]) / 1_000_000 - report_ack[path.name])
        rows.append(row)
    rows.sort(key=lambda row: row["elapsed_seconds"])
    for i, row in enumerate(rows):
        if i:
            row["previous"] = rows[i - 1]["name"]
            row["missing_output_delta"] = row["pcm"]["missing_output_frames"] - rows[i - 1]["pcm"]["missing_output_frames"]
    return rows, float(np.median(offsets)) if offsets else None


def write_csv(path: Path, rows: list[dict]) -> None:
    if not rows:
        path.write_text("", encoding="utf-8")
        return
    keys = list(dict.fromkeys(k for row in rows for k in row))
    with path.open("w", encoding="utf-8", newline="") as stream:
        writer = csv.DictWriter(stream, keys)
        writer.writeheader()
        writer.writerows(rows)


def analyze(args: argparse.Namespace) -> dict:
    directory, output = args.directory.resolve(), args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    epochs, packets = timeline(directory / args.timeline)
    capture = directory / args.pcm
    if capture.stat().st_size % 4:
        raise ValueError("capture length is not a whole number of f32 samples")
    channels = {epoch["channels"] for epoch in epochs}
    if len(channels) != 1:
        raise ValueError("channel layout changed: split epochs before waveform analysis")
    channels = channels.pop()
    x = np.memmap(capture, dtype="<f4", mode="r").reshape(-1, channels)
    cursor, packet_integrity, pts_errors = 0, [], []
    for packet in packets:
        if packet["file_sample"] != cursor or packet["samples"] % channels:
            packet_integrity.append(packet)
        cursor = packet["file_sample"] + packet["samples"]
    if cursor != x.size or packet_integrity:
        raise ValueError(f"timeline/file mismatch: last sample={cursor}, file={x.size}, discontinuities={len(packet_integrity)}")
    for epoch_no, epoch in enumerate(epochs):
        epoch["end_sample"] = epochs[epoch_no + 1]["first_sample"] if epoch_no + 1 < len(epochs) else x.size
        epoch["first_frame"] = epoch["first_sample"] // channels
        epoch["end_frame"] = epoch["end_sample"] // channels
        rows = [p for p in packets if p["epoch"] == epoch_no]
        if not rows or epoch["rate"] <= 0 or epoch["channels"] <= 0 or epoch["first_sample"] != rows[0]["file_sample"]:
            raise ValueError(f"invalid epoch {epoch_no} format or first_file_sample")
        epoch["first_pts_us"] = rows[0]["pts_us"]
        epoch["duration_seconds"] = (epoch["end_frame"] - epoch["first_frame"]) / epoch["rate"]
        for previous, current in zip(rows, rows[1:]):
            expected = previous["pts_us"] + previous["samples"] / channels / epoch["rate"] * 1_000_000
            pts_errors.append(current["pts_us"] - expected)
    source_streams, source_summary = source_log(directory) if args.sources else ([], {"available": False, "disabled": True})
    checks, session_offset = checkpoints(directory)
    finite = np.isfinite(x)
    absolute = np.abs(np.where(finite, x, 0))
    stats = {"frames": len(x), "samples": x.size, "file_bytes": capture.stat().st_size,
             "dtype": "little-endian f32", "channels": channels,
             "recorded_seconds": sum(e["duration_seconds"] for e in epochs),
             "nonfinite_samples": int(np.count_nonzero(~finite)), "peak": float(absolute.max()),
             "clipped_samples_at_abs_1": int(np.count_nonzero(absolute >= 1)),
             "out_of_range_samples": int(np.count_nonzero(absolute > 1)), "rms": rms(x),
             "exact_zero_frames": int(np.count_nonzero(np.all(x == 0, axis=1)))}
    packet_starts = np.array([p["file_sample"] // channels for p in packets])

    def point(frame: int) -> dict:
        index = max(0, int(np.searchsorted(packet_starts, frame, side="right")) - 1)
        p = packets[index]
        epoch = epochs[p["epoch"]]
        pts = p["pts_us"] + (frame - packet_starts[index]) / epoch["rate"] * 1_000_000
        return {"epoch": p["epoch"], "callback": p["callback"], "pts_us": pts,
                "session_seconds_estimate": pts / 1_000_000 - session_offset if session_offset is not None else None}

    zeros, boundaries, jumps, comparisons, packet_rows = [], [], [], [], []
    previous_missing: dict[str, int] = {}
    counter_duration_ms = 0.0
    missing_counter_frames = 0
    stream_counter_frames = 0
    stream_counter_duration_ms = 0.0
    for packet in packets:
        epoch = epochs[packet["epoch"]]
        a = packet["file_sample"] // channels
        b = a + packet["samples"] // channels
        row = {"file_frame_start": a, "file_frame_end": b, **point(a),
               "duration_ms": (b - a) / epoch["rate"] * 1000,
               "rms": rms(x[a:b]), "peak": peak(x[a:b]),
               "nonfinite_samples": int(np.count_nonzero(~np.isfinite(x[a:b]))),
               "exact_zero_frames": int(np.all(x[a:b] == 0, axis=1).sum()),
               "lock_miss": packet.get("lock_miss")}
        for counter in ("missing_output_frames", "missing_stream_frames"):
            if counter in packet:
                value = int(packet[counter])
                delta = value - previous_missing.get(counter, 0)
                row[counter] = value
                row[counter + "_delta"] = delta
                if delta < 0:
                    raise ValueError(f"cumulative {counter} regressed; counter resets need an explicit epoch convention")
                previous_missing[counter] = value
                if counter == "missing_output_frames":
                    missing_counter_frames += delta
                    counter_duration_ms += delta / epoch["rate"] * 1000
                else:
                    stream_counter_frames += delta
                    stream_counter_duration_ms += delta / epoch["rate"] * 1000
        packet_rows.append(row)
    for epoch_no, epoch in enumerate(epochs):
        begin, end, rate = epoch["first_frame"], epoch["end_frame"], epoch["rate"]
        data = x[begin:end]
        delta = np.max(np.abs(np.diff(data.astype(np.float64), axis=0)), axis=1)
        for local in np.flatnonzero(delta >= args.jump):
            at = begin + int(local) + 1
            location = point(at)
            packet_at = int(np.searchsorted(packet_starts, at))
            distance = min(abs(at - int(packet_starts[k])) for k in [max(0, packet_at - 1), min(len(packet_starts) - 1, packet_at)])
            jumps.append({"file_frame": at, **location, "jump": float(delta[local]), "callback_boundary_distance_frames": distance,
                          "classification": "jump_candidate_not_automatically_an_error"})
        for local_begin, local_end in runs(np.all(data == 0, axis=1)):
            if local_end - local_begin < math.ceil(args.min_zero_ms / 1000 * rate):
                continue
            a, b = begin + local_begin, begin + local_end
            location = point(a)
            row = {"file_frame_start": a, "file_frame_end": b, **location,
                   "duration_ms": (b - a) / rate * 1000,
                   "before_peak": peak(x[max(begin, a - 192):a]) if a > begin else 0,
                   "after_peak": peak(x[b:min(end, b + 192)]) if b < end else 0,
                   "classification": "unclassified_zero_output"}
            pts = location["pts_us"] + np.arange(b - a) / rate * 1_000_000
            # Any partial overlap disqualifies a single-source claim. A zero
            # interval can span many rapidly cancelled/started voices in skip.
            active = [s for s in source_streams if s.start_us <= pts[-1] and pts[0] < s.end_us]
            row["active_sources"] = len(active)
            if not active and source_summary.get("available"):
                row["classification"] = "no_active_source_not_an_underrun_claim"
            elif len(active) == 1:
                source = active[0]
                row["source_id"] = source.id
                row["source_generation"] = source.generation
                if source.start_us <= pts[0] and pts[-1] < source.end_us and not source.changed_format and source.bits in (8, 16) and source.channels <= 2 and source.chunks:
                    expected, covered = source.expected(pts)
                    expected = output_channels(expected, channels)
                    row["source_covered_fraction"] = float(covered.mean())
                    row["expected_source_rms"] = rms(expected)
                    row["expected_source_peak"] = float(np.max(np.abs(expected)))
                    if covered.all():
                        if np.all(expected == 0):
                            row["classification"] = "covered_source_silence_not_an_underrun"
                        elif rms(expected) > args.source_rms and np.max(np.abs(expected)) > args.source_peak:
                            row["classification"] = "nonzero_source_suppressed_in_output"
            zeros.append(row)
        for packet in (p for p in packets if p["epoch"] == epoch_no):
            at = packet["file_sample"] // channels
            if at == begin:
                continue
            boundaries.append({"file_frame": at, **point(at), "jump": float(np.max(np.abs(x[at] - x[at - 1]))),
                               "missing_output_frames": packet.get("missing_output_frames"),
                               "missing_stream_frames": packet.get("missing_stream_frames")})
    # A separate isolated-source comparison verifies declared format/channel/rate
    # on covered nonzero PCM, without deriving underruns from silence alone.
    for source in source_streams:
        if source.changed_format or source.bits not in (8, 16) or source.channels > 2 or not source.chunks:
            continue
        for epoch in epochs:
            base_pts = epoch["first_pts_us"]
            for seconds in np.arange(max(source.start_us / 1e6 + 0.08, base_pts / 1e6), min(source.end_us / 1e6, base_pts / 1e6 + epoch["duration_seconds"]) - 0.1, 1.0):
                first = epoch["first_frame"] + round((seconds * 1e6 - base_pts) * epoch["rate"] / 1e6)
                count = round(epoch["rate"] * 0.1)
                if first < epoch["first_frame"] or first + count > epoch["end_frame"]:
                    continue
                pts = base_pts + (np.arange(first, first + count) - epoch["first_frame"]) / epoch["rate"] * 1e6
                if any(other is not source and other.start_us < pts[-1] and other.end_us > pts[0] for other in source_streams):
                    continue
                expected, covered = source.expected(pts)
                expected = output_channels(expected, channels)
                if not covered.all() or rms(expected) <= args.source_rms:
                    continue
                actual = x[first:first + count]
                if not np.isfinite(actual).all():
                    continue
                error = actual.astype(float) - expected
                snr = 10 * math.log10(max(float(np.sum(expected**2)), 1e-30) / max(float(np.sum(error**2)), 1e-30))
                comparisons.append({"source_id": source.id, "generation": source.generation, **point(first),
                                    "duration_ms": 100, "snr_db": snr, "expected_rms": rms(expected),
                                    "actual_rms": rms(actual), "zero_frames": int(np.all(actual == 0, axis=1).sum())})
    classified = {}
    for row in zeros:
        c = classified.setdefault(row["classification"], {"runs": 0, "duration_ms": 0.0})
        c["runs"] += 1
        c["duration_ms"] += row["duration_ms"]
    callback_counters = [p for p in packets if "missing_output_frames" in p]
    with capture.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    result = {"capture": str(capture), "sha256": digest,
              "waveform": stats, "epochs": epochs, "timeline": {"packets": len(packets),
              "continuity_max_abs_pts_error_us": max(map(abs, pts_errors), default=0),
              "lock_miss_packets": sum(p.get("lock_miss") == "true" for p in packets),
              "callback_missing_counters_available": bool(callback_counters),
              "callback_missing_output_frame_deltas": missing_counter_frames if callback_counters else None,
              "callback_missing_coverage_ms": counter_duration_ms if callback_counters else None,
              "callback_missing_stream_counter_available": "missing_stream_frames" in previous_missing,
              "callback_missing_stream_frame_deltas": stream_counter_frames if "missing_stream_frames" in previous_missing else None,
              "callback_missing_stream_coverage_ms": stream_counter_duration_ms if "missing_stream_frames" in previous_missing else None},
              "session_time_estimation": {"pts_to_session_offset_seconds": session_offset,
              "method": "median checkpoint guest clock minus report ACK; approximate, not the restarting host_us field"},
              "thresholds": {"minimum_zero_ms": args.min_zero_ms, "jump": args.jump,
              "source_rms": args.source_rms, "source_peak": args.source_peak},
              "source_log": source_summary, "zero_classifications": classified,
              "jump_candidates": len(jumps), "callback_boundary_jumps_over_threshold": sum(r["jump"] >= args.jump for r in boundaries),
              "isolated_source_windows": len(comparisons), "checkpoints": checks,
              "limitations": ["zero output alone is not an underrun", "missing_output_frames counts any missing active stream, even with audible BGM", "file offsets concatenate device epochs; epoch joins are excluded from jump candidates", "source reconstruction is nominal linear interpolation; it does not reproduce unlogged queue arrival or device clock correction", "only single-source covered zero intervals receive source-suppression classification; cancellation/multiple streams remain unclassified"]}
    write_csv(output / "zero-intervals.csv", zeros)
    write_csv(output / "jump-candidates.csv", jumps)
    write_csv(output / "callback-boundaries.csv", boundaries)
    write_csv(output / "callback-packets.csv", packet_rows)
    write_csv(output / "isolated-source-windows.csv", comparisons)
    (output / "analysis.json").write_text(json.dumps(result, indent=2, ensure_ascii=False, allow_nan=False), encoding="utf-8")
    print(json.dumps({"waveform": stats, "zero_classifications": classified,
                      "jump_candidates": len(jumps), "source_windows": len(comparisons), "output": str(output)}, indent=2))
    return result


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--pcm", default="actual.f32")
    parser.add_argument("--timeline", default="actual.timeline.txt")
    parser.add_argument("--min-zero-ms", type=float, default=0.25)
    parser.add_argument("--jump", type=float, default=0.02)
    parser.add_argument("--source-rms", type=float, default=0.0001)
    parser.add_argument("--source-peak", type=float, default=0.001)
    parser.add_argument("--sources", action="store_true", help="optional old queued-PCM source reconstruction; default is lightweight waveform/timeline analysis")
    args = parser.parse_args()
    for name in ("min_zero_ms", "jump", "source_rms", "source_peak"):
        if not math.isfinite(getattr(args, name)) or getattr(args, name) < 0:
            parser.error(f"--{name.replace('_', '-')} must be finite and nonnegative")
    analyze(args)


if __name__ == "__main__":
    main()

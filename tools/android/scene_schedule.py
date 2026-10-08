"""Strict, game-independent scene checkpoints and binary-independent references."""
from __future__ import annotations

import hashlib
import json
import math
from pathlib import Path
import re


def valid_acknowledgement(command: str, reply: str | None) -> bool:
    verb = command.split()[0]
    expected = "control ok " + {"dialog-track": "dialog"}.get(verb, verb)
    if reply is None or not (reply == expected or reply.startswith(expected + " ")):
        return False
    if verb == "await-frame":
        fields = dict(token.split("=", 1) for token in reply.split()[3:] if "=" in token)
        words = command.split()
        try:
            if (fields.get("hash") != words[1].lower() or fields.get("frozen") != "true"
                    or int(fields["frame"]) <= 0 or int(fields["start_frame"]) < 0
                    or ("current" not in words and int(fields["frame"]) <= int(fields["start_frame"]))):
                return False
            release = words[words.index("release") + 1] if "release" in words else (
                words[words.index("hold") + 1] if "hold" in words else None)
            return fields.get("release") == (release or "none") and (
                release is None or fields.get("release_posted") == "true")
        except (ValueError, KeyError, IndexError):
            return False
    return True


def digest_json(value: object) -> str:
    return hashlib.sha256(json.dumps(value, ensure_ascii=False, sort_keys=True,
                                     separators=(",", ":")).encode("utf-8")).hexdigest()


def exact_hash(value: object, digits: int = 16) -> str:
    if not isinstance(value, str) or not re.fullmatch(rf"[0-9a-fA-F]{{{digits}}}", value):
        raise ValueError(f"hash must have exactly {digits} hexadecimal digits")
    return value.lower()


def validate_assertions(value: object) -> dict:
    if not isinstance(value, dict):
        raise ValueError("checkpoint assert must be a dotted-field object")
    for field, predicate in value.items():
        if not isinstance(field, str) or not re.fullmatch(r"[A-Za-z0-9_]+(?:\.[A-Za-z0-9_]+)*", field):
            raise ValueError("assert field must be a dotted report path")
        if not isinstance(predicate, dict) or not predicate or set(predicate) - {"eq", "min", "max"}:
            raise ValueError("assert predicate must contain eq/min/max")
        for operation, expected in predicate.items():
            if isinstance(expected, (dict, list)):
                raise ValueError("assert expected value must be a scalar")
            if operation != "eq" and (type(expected) not in (int, float) or not math.isfinite(expected)):
                raise ValueError("assert min/max needs a finite number")
            if type(expected) is float and not math.isfinite(expected):
                raise ValueError("assert expected value must be finite")
    return value


def assert_report(report: dict, assertions: dict, name: str) -> None:
    for field, predicate in assertions.items():
        actual = report
        for component in field.split("."):
            if not isinstance(actual, dict) or component not in actual:
                raise RuntimeError(f"{name}: missing asserted report field {field}")
            actual = actual[component]
        for operation, expected in predicate.items():
            if operation == "eq":
                passed = type(actual) is type(expected) and actual == expected
            else:
                numeric = type(actual) in (int, float) and math.isfinite(actual)
                passed = numeric and (actual >= expected if operation == "min" else actual <= expected)
            if not passed:
                raise RuntimeError(f"{name}: {field}={actual!r} failed {operation} {expected!r}")


def validate_await(value: object) -> dict:
    if not isinstance(value, dict) or set(value) - {
            "frame_fnv1a64", "timeout_ms", "hold_key", "release_key", "current", "probes"}:
        raise ValueError("await requires a frame hash, timeout, and optional key/pixel fields")
    result = dict(value)
    result["frame_fnv1a64"] = exact_hash(result.get("frame_fnv1a64"))
    timeout = result.get("timeout_ms")
    if type(timeout) is not int or not 1 <= timeout <= 600000:
        raise ValueError("await timeout_ms must be in 1..600000")
    for name in ("hold_key", "release_key"):
        if name in result and (type(result[name]) is not int or not 0 <= result[name] <= 255):
            raise ValueError(f"await {name} must be a key in 0..255")
    if "current" in result and type(result["current"]) is not bool:
        raise ValueError("await current must be boolean")
    if "hold_key" in result:
        if result.get("current"):
            raise ValueError("current cannot accept a held input before a new frame")
        if result.get("release_key", result["hold_key"]) != result["hold_key"]:
            raise ValueError("gate must release its held key")
    probes = result.get("probes", [])
    if not isinstance(probes, list) or len(probes) > 16:
        raise ValueError("await probes must contain at most 16 pixels")
    for probe in probes:
        if not isinstance(probe, dict) or set(probe) != {"x", "y", "bgra"}:
            raise ValueError("probe requires x, y, bgra")
        if any(type(probe[key]) is not int or not 0 <= probe[key] < 8192 for key in ("x", "y")):
            raise ValueError("probe coordinate must be in 0..8191")
        if (not isinstance(probe["bgra"], list) or len(probe["bgra"]) != 4
                or any(type(value) is not int or not 0 <= value <= 255 for value in probe["bgra"])):
            raise ValueError("probe bgra requires four bytes")
    return result


def await_command(spec: dict) -> str:
    words = ["await-frame", spec["frame_fnv1a64"], str(spec["timeout_ms"])]
    for name, option in (("hold_key", "hold"), ("release_key", "release")):
        if name in spec:
            words += [option, str(spec[name])]
    if spec.get("current"):
        words.append("current")
    if spec.get("probes"):
        words += ["probes", ";".join(",".join(map(str, [probe["x"], probe["y"], *probe["bgra"]]))
                                     for probe in spec["probes"])]
    return " ".join(words)


def load_scene(path: Path) -> dict | None:
    document = json.loads(path.read_text(encoding="utf-8"))
    if isinstance(document, list):
        return None  # The existing timed-schedule validator owns this format.
    if (not isinstance(document, dict) or document.get("schema") != "adv-scene-v1"
            or set(document) - {"schema", "timeout_seconds", "steps", "description"}):
        raise ValueError("scene schedule must use schema adv-scene-v1")
    timeout = document.get("timeout_seconds")
    if type(timeout) not in (int, float) or not math.isfinite(timeout) or not 1 <= timeout <= 86400:
        raise ValueError("scene timeout_seconds must be finite and within 1..86400")
    steps = document.get("steps")
    if not isinstance(steps, list) or not 1 <= len(steps) <= 512:
        raise ValueError("scene requires 1..512 ordered steps")
    names = set()
    for index, step in enumerate(steps):
        if not isinstance(step, dict) or len({"command", "checkpoint", "delay_ms"} & step.keys()) != 1:
            raise ValueError("scene step requires exactly one command/checkpoint/delay_ms")
        if "command" in step:
            command = step["command"]
            if (set(step) != {"command"} or not isinstance(command, str) or not command.strip()
                    or "\n" in command or "\r" in command):
                raise ValueError("scene command must be a single nonempty line")
            if command.strip() == "close" and index != len(steps) - 1:
                raise ValueError("close must be the last scene step")
        elif "delay_ms" in step:
            if set(step) != {"delay_ms"} or type(step["delay_ms"]) is not int or not 1 <= step["delay_ms"] <= 60000:
                raise ValueError("scene delay_ms must be in 1..60000")
        else:
            if set(step) - {"checkpoint", "await", "observe", "assert", "reset_metrics"}:
                raise ValueError("unknown scene checkpoint field")
            name = step["checkpoint"]
            if not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9_-]+", name) or name in names:
                raise ValueError("checkpoint names must be unique filename-safe names")
            names.add(name)
            if step.get("observe") is True and "await" not in step:
                pass  # An explicitly ungated observation cannot verify fixed work.
            elif "observe" in step:
                raise ValueError("observe must be true and cannot accompany await")
            else:
                step["await"] = validate_await(step.get("await"))
            step["assert"] = validate_assertions(step.get("assert", {}))
            if "reset_metrics" in step and type(step["reset_metrics"]) is not bool:
                raise ValueError("reset_metrics must be boolean")
    if not names:
        raise ValueError("scene needs at least one exact checkpoint")
    return document


def validate_reference(reference: dict, binding: dict, steps: list[dict]) -> None:
    if any(step.get("observe") for step in steps):
        raise ValueError("ungated observations cannot use a fixed-work reference")
    if not isinstance(reference, dict) or reference.get("schema") != "adv-scene-reference-v1":
        raise ValueError("reference must use schema adv-scene-reference-v1")
    if reference.get("status") != "approved":
        raise ValueError("recorded reference is observation only; approve independently before validation")
    if reference.get("binding") != binding:
        raise ValueError("scene reference asset/font/seed/schedule binding differs")
    checkpoints = reference.get("checkpoints")
    if not isinstance(checkpoints, dict):
        raise ValueError("reference checkpoints must be an object")
    expected_names = {step["checkpoint"] for step in steps if "checkpoint" in step}
    if set(checkpoints) != expected_names:
        raise ValueError("reference checkpoint names differ from the scene")
    for step in steps:
        if "checkpoint" in step:
            expected = checkpoints[step["checkpoint"]]
            if not isinstance(expected, dict) or exact_hash(expected.get("frame_fnv1a64")) != step["await"]["frame_fnv1a64"]:
                raise ValueError("reference exact hash differs from the scene gate")
            validate_assertions(expected.get("assert", {}))

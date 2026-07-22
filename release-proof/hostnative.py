#!/usr/bin/env python3
"""Record the macOS-only remainder beside Linux release-proof evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import select
import subprocess
import sys
import tempfile
from pathlib import Path

SCRIPT_DIRECTORY = Path(__file__).resolve().parent
if str(SCRIPT_DIRECTORY) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIRECTORY))

from evidence import (  # noqa: E402
    FailureArtifactStore,
    parse_pytest_counts,
    redact_text,
)

SCHEMA_VERSION = 1
CONTAINER_REPORT_NAME = "release-proof.json"
HOST_REPORT_NAME = "release-proof-macos.json"
SPEECH_PROBE_TEXT = "Spice host native release proof"
KQUEUE_TEST_COMMAND = (
    "uv",
    "run",
    "--locked",
    "pytest",
    "-q",
    "tests/test_livebusevents.py",
    "-k",
    "kqueue",
)


class HostNativeError(RuntimeError):
    """The macOS companion could not establish its narrow claim."""


def collect_host_native_evidence(
    root: Path,
    evidence_dir: Path,
) -> dict[str, object]:
    resolved_root = root.expanduser().resolve(strict=True)
    resolved_evidence = evidence_dir.expanduser().resolve(strict=True)
    failures = FailureArtifactStore(resolved_evidence)
    if platform.system() != "Darwin":
        raise HostNativeError("host-native release proof requires macOS")
    if not hasattr(select, "kqueue"):
        raise HostNativeError("host-native release proof requires kqueue")

    container_path = resolved_evidence / CONTAINER_REPORT_NAME
    container_bytes = container_path.read_bytes()
    container = json.loads(container_bytes)
    boundary = container.get("claim_boundary") if isinstance(container, dict) else None
    if not isinstance(boundary, dict) or boundary.get("operating_system") != "linux":
        raise HostNativeError(
            f"container evidence does not retain its Linux claim: {container_path}"
        )

    kqueue = _run_command(
        KQUEUE_TEST_COMMAND,
        cwd=resolved_root,
        failures=failures,
        gate="macos-kqueue",
    )
    try:
        kqueue_counts = parse_pytest_counts(kqueue.stdout)
    except ValueError as exc:
        raise HostNativeError(str(exc)) from exc
    appearance = _appearance(resolved_root, failures)
    speech = _speech(resolved_root, failures)
    report: dict[str, object] = {
        "schema_version": SCHEMA_VERSION,
        "claim_boundary": {
            "operating_system": "macos",
            "container_operating_system": "linux",
            "container_evidence_unchanged": True,
        },
        "platform": {
            "system": platform.system(),
            "release": platform.release(),
            "machine": platform.machine(),
        },
        "container_evidence": {
            "filename": CONTAINER_REPORT_NAME,
            "sha256": hashlib.sha256(container_bytes).hexdigest(),
        },
        "checks": {
            "kqueue-or-fsevents": {
                "status": "passed",
                "backend": "kqueue",
                "tests": kqueue_counts,
            },
            "appearance": appearance,
            "speech": speech,
        },
    }
    _write_json(resolved_evidence / HOST_REPORT_NAME, report)
    if container_path.read_bytes() != container_bytes:
        raise HostNativeError("host-native companion changed container evidence")
    return report


def _run_command(
    command: list[str] | tuple[str, ...],
    *,
    cwd: Path,
    failures: FailureArtifactStore,
    gate: str,
) -> subprocess.CompletedProcess[str]:
    argv = [str(part) for part in command]
    completed = subprocess.run(
        argv,
        check=False,
        capture_output=True,
        cwd=cwd,
        text=True,
    )
    if completed.returncode == 0:
        return completed
    environment = dict(os.environ)  # env-policy: allow
    diagnostic = failures.record(
        gate,
        argv,
        completed.returncode,
        completed.stdout,
        completed.stderr,
        environment=environment,
    )
    detail = redact_text(completed.stderr or completed.stdout, environment).strip()
    suffix = f": {detail}" if detail else ""
    raise HostNativeError(f"{gate} failed; diagnostic={diagnostic}{suffix}")


def _appearance(root: Path, failures: FailureArtifactStore) -> dict[str, object]:
    command = ["defaults", "read", "-g", "AppleInterfaceStyle"]
    completed = subprocess.run(
        command,
        check=False,
        capture_output=True,
        cwd=root,
        text=True,
    )
    if completed.returncode == 0:
        style = completed.stdout.strip().casefold() or "dark"
    elif completed.returncode == 1:
        style = "light"
    else:
        environment = dict(os.environ)  # env-policy: allow
        diagnostic = failures.record(
            "macos-appearance",
            command,
            completed.returncode,
            completed.stdout,
            completed.stderr,
            environment=environment,
        )
        raise HostNativeError(f"appearance probe failed; diagnostic={diagnostic}")
    return {"status": "passed", "style": style}


def _speech(root: Path, failures: FailureArtifactStore) -> dict[str, object]:
    with tempfile.TemporaryDirectory(prefix="spice-release-speech-") as raw:
        audio = Path(raw) / "probe.aiff"
        _run_command(
            ["/usr/bin/say", "-o", str(audio), SPEECH_PROBE_TEXT],
            cwd=root,
            failures=failures,
            gate="macos-speech",
        )
        try:
            content = audio.read_bytes()
        except OSError as exc:
            raise HostNativeError(f"speech probe did not produce audio: {exc}") from exc
        if len(content) == 0:
            raise HostNativeError("speech probe produced empty audio")
    return {
        "status": "passed",
        "backend": "/usr/bin/say",
        "bytes": len(content),
        "sha256": hashlib.sha256(content).hexdigest(),
    }


def _write_json(path: Path, payload: dict[str, object]) -> None:
    temporary = path.with_name(f".{path.name}.tmp")
    temporary.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    temporary.replace(path)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--evidence-dir", required=True, type=Path)
    arguments = parser.parse_args()
    try:
        report = collect_host_native_evidence(
            Path(__file__).resolve().parent.parent,
            arguments.evidence_dir,
        )
    except (HostNativeError, OSError, json.JSONDecodeError) as exc:
        environment = dict(os.environ)  # env-policy: allow
        safe_error = redact_text(str(exc), environment)
        FailureArtifactStore(arguments.evidence_dir).record(
            "macos-companion",
            [sys.executable, str(Path(__file__).resolve())],
            2,
            "",
            safe_error,
            environment=environment,
        )
        print(f"release-proof host-native: {safe_error}", file=sys.stderr)
        return 2
    print(json.dumps(report, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

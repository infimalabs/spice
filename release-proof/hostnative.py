#!/usr/bin/env python3
"""Record the macOS-only remainder beside Linux release-proof evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import select
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from threading import Event, Thread, Timer

SCRIPT_DIRECTORY = Path(__file__).resolve().parent
if str(SCRIPT_DIRECTORY) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIRECTORY))
PROJECT_ROOT = SCRIPT_DIRECTORY.parent
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

from evidence import (  # noqa: E402
    FailureArtifactStore,
    redact_text,
)
from spice.serve.livebus import (  # noqa: E402
    LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S,
    _KqueueWatch,  # pyright: ignore[reportPrivateUsage]
)

SCHEMA_VERSION = 1
CONTAINER_REPORT_NAME = "release-proof.json"
HOST_REPORT_NAME = "release-proof-macos.json"
SPEECH_PROBE_TEXT = "Spice host native release proof"
HOST_COMMAND_TIMEOUT_SECONDS = 30.0
KQUEUE_EVENT_TIMEOUT_SECONDS = 5.0
OBJECT_ID_PATTERN = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})")


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

    container_source_commit = _container_source_commit(container, container_path)
    checkout_head = _checkout_head(resolved_root, failures)
    if checkout_head != container_source_commit:
        raise HostNativeError(
            "checkout HEAD does not match container source commit: "
            f"checkout={checkout_head} container={container_source_commit}"
        )

    kqueue = _probe_kqueue_event()
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
        "source_identity": {
            "agreement": "exact",
            "checkout_head": checkout_head,
            "container_source_commit": container_source_commit,
        },
        "checks": {
            "kqueue-or-fsevents": kqueue,
            "appearance": appearance,
            "speech": speech,
        },
    }
    _write_json(resolved_evidence / HOST_REPORT_NAME, report)
    if container_path.read_bytes() != container_bytes:
        raise HostNativeError("host-native companion changed container evidence")
    return report


def _container_source_commit(container: object, container_path: Path) -> str:
    source_identity = (
        container.get("source_identity") if isinstance(container, dict) else None
    )
    source = (
        source_identity.get("source") if isinstance(source_identity, dict) else None
    )
    commit = source.get("commit") if isinstance(source, dict) else None
    if not isinstance(commit, str) or OBJECT_ID_PATTERN.fullmatch(commit) is None:
        raise HostNativeError(
            f"container evidence has no valid source commit: {container_path}"
        )
    return commit


def _checkout_head(root: Path, failures: FailureArtifactStore) -> str:
    environment = dict(os.environ)  # env-policy: allow
    environment.update(
        {
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_CONFIG_NOSYSTEM": "1",
            "LC_ALL": "C",
        }
    )
    completed = _run_command(
        ["git", "-C", str(root), "rev-parse", "--verify", "HEAD^{commit}"],
        cwd=root,
        failures=failures,
        gate="macos-source-identity",
        environment=environment,
    )
    head = completed.stdout.strip()
    if OBJECT_ID_PATTERN.fullmatch(head) is None:
        raise HostNativeError(f"checkout HEAD is not a full Git object ID: {head!r}")
    return head


def _probe_kqueue_event() -> dict[str, object]:
    """Exercise the production kqueue watcher with one real filesystem write."""
    if not hasattr(select, "kqueue"):
        raise HostNativeError("host-native release proof requires kqueue")

    with tempfile.TemporaryDirectory(prefix="spice-release-kqueue-") as raw:
        watched = Path(raw) / "event"
        watched.write_bytes(b"before\n")
        activated = Event()
        cancelled = Event()
        stop = Event()
        writer_errors: list[str] = []

        def write_event() -> None:
            if not activated.wait(timeout=KQUEUE_EVENT_TIMEOUT_SECONDS):
                writer_errors.append("kqueue watcher did not publish activation")
                return
            if cancelled.is_set():
                return
            try:
                with watched.open("ab", buffering=0) as stream:
                    stream.write(b"after\n")
                    os.fsync(stream.fileno())
            except OSError as exc:
                writer_errors.append(f"kqueue event write failed: {exc}")

        writer = Thread(
            target=write_event,
            name="release-proof-kqueue-writer",
            daemon=True,
        )
        deadline = Timer(KQUEUE_EVENT_TIMEOUT_SECONDS, stop.set)
        watch = _KqueueWatch()
        started = time.monotonic()
        writer.start()
        deadline.start()
        try:
            try:
                observed = watch.wait((watched,), stop, activated=activated)
            except (OSError, RuntimeError) as exc:
                raise HostNativeError(
                    f"production kqueue event probe failed: {exc}"
                ) from exc
        finally:
            elapsed_ms = round((time.monotonic() - started) * 1000, 3)
            cancelled.set()
            activated.set()
            stop.set()
            deadline.cancel()
            watch.close()
            writer.join(timeout=LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S)

        if writer.is_alive():
            raise HostNativeError("kqueue event writer did not stop within its bound")
        if writer_errors:
            raise HostNativeError(writer_errors[0])
        if not observed:
            raise HostNativeError(
                "production kqueue watcher observed no filesystem event before deadline"
            )

    return {
        "status": "passed",
        "backend": "kqueue",
        "production_path": "spice.serve.livebus._KqueueWatch",
        "event": "filesystem-write",
        "timeout_seconds": KQUEUE_EVENT_TIMEOUT_SECONDS,
        "elapsed_ms": elapsed_ms,
    }


def _run_command(
    command: list[str] | tuple[str, ...],
    *,
    cwd: Path,
    failures: FailureArtifactStore,
    gate: str,
    environment: dict[str, str] | None = None,
    accepted_returncodes: tuple[int, ...] = (0,),
) -> subprocess.CompletedProcess[str]:
    argv = [str(part) for part in command]
    effective_environment = (
        environment
        if environment is not None
        else dict(os.environ)  # env-policy: allow
    )
    try:
        completed = subprocess.run(
            argv,
            check=False,
            capture_output=True,
            cwd=cwd,
            env=environment,
            text=True,
            timeout=HOST_COMMAND_TIMEOUT_SECONDS,
        )
    except subprocess.TimeoutExpired as exc:
        stdout = _timeout_output(exc.stdout)
        stderr = _timeout_output(exc.stderr)
        diagnostic = failures.record(
            gate,
            argv,
            124,
            stdout,
            stderr,
            environment=effective_environment,
        )
        raise HostNativeError(
            f"{gate} exceeded {HOST_COMMAND_TIMEOUT_SECONDS:g}s; "
            f"diagnostic={diagnostic}"
        ) from exc
    if completed.returncode in accepted_returncodes:
        return completed
    diagnostic = failures.record(
        gate,
        argv,
        completed.returncode,
        completed.stdout,
        completed.stderr,
        environment=effective_environment,
    )
    detail = redact_text(
        completed.stderr or completed.stdout, effective_environment
    ).strip()
    suffix = f": {detail}" if detail else ""
    raise HostNativeError(f"{gate} failed; diagnostic={diagnostic}{suffix}")


def _timeout_output(output: bytes | str | None) -> str:
    if output is None:
        return ""
    if isinstance(output, bytes):
        return output.decode("utf-8", errors="replace")
    return output


def _appearance(root: Path, failures: FailureArtifactStore) -> dict[str, object]:
    command = ["defaults", "read", "-g", "AppleInterfaceStyle"]
    completed = _run_command(
        command,
        cwd=root,
        failures=failures,
        gate="macos-appearance",
        accepted_returncodes=(0, 1),
    )
    if completed.returncode == 0:
        style = completed.stdout.strip().casefold() or "dark"
    else:
        style = "light"
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

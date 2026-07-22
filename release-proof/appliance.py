#!/usr/bin/env python3
"""Build, export, and validate the disposable local release-proof appliance."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import secrets
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
from collections.abc import Callable
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, NoReturn

SCRIPT_DIRECTORY = Path(__file__).resolve().parent
PROJECT_ROOT = SCRIPT_DIRECTORY.parent
if str(SCRIPT_DIRECTORY) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIRECTORY))
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

from evidence import (  # noqa: E402
    FailureArtifactStore,
    failure_policy_payload,
    redact_text,
)
from spice.process.groups import (  # noqa: E402
    popen_new_process_group_kwargs,
    terminate_process_group,
)

SCHEMA_VERSION = 1
FAILURE_REPORT_NAME = "release-proof-failure.json"
LINUX_REPORT_NAME = "release-proof.json"
MACOS_REPORT_NAME = "release-proof-macos.json"
ENGINE_CHOICES = ("docker", "podman")
OBJECT_ID_PATTERN = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})")
RUN_ID_PATTERN = re.compile(r"[0-9a-f]{12,32}")
SHA256_PATTERN = re.compile(r"[0-9a-f]{64}")
SOURCE_TIMEOUT_SECONDS = 120.0
ENGINE_PROBE_TIMEOUT_SECONDS = 30.0
ENGINE_BUILD_TIMEOUT_SECONDS = 3600.0
ENGINE_OBJECT_TIMEOUT_SECONDS = 120.0
HOST_NATIVE_TIMEOUT_SECONDS = 120.0
PROCESS_CLEANUP_TIMEOUT_SECONDS = 2.0
MAX_STATUS_TEXT_BYTES = 4096
OBJECT_NAME_COMMIT_LENGTH = 12

CommandRunner = Callable[[list[str], Path, float], subprocess.CompletedProcess[str]]
Clock = Callable[[], str]


class ProofFailure(RuntimeError):
    """One appliance phase failed without broadening its authority boundary."""

    def __init__(
        self,
        phase: str,
        message: str,
        *,
        command: list[str] | tuple[str, ...] = (),
        returncode: int = 1,
        stdout: str = "",
        stderr: str = "",
        diagnostic_recorded: bool = False,
    ) -> None:
        super().__init__(message)
        self.phase = phase
        self.command = tuple(str(part) for part in command)
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr
        self.diagnostic_recorded = diagnostic_recorded


class CommandDeadline(RuntimeError):
    """A child process exceeded its named appliance deadline."""

    def __init__(
        self,
        command: list[str],
        timeout_seconds: float,
        stdout: str,
        stderr: str,
    ) -> None:
        self.command = tuple(command)
        self.timeout_seconds = timeout_seconds
        self.stdout = stdout
        self.stderr = stderr
        super().__init__(f"command exceeded {timeout_seconds:g}s: {' '.join(command)}")


class TerminationRequested(RuntimeError):
    """The process received a termination signal that permits bounded cleanup."""

    def __init__(self, signum: int) -> None:
        self.signum = signum
        super().__init__(f"release proof interrupted by signal {signum}")


def run_release_proof(
    root: Path,
    engine: str,
    output: Path,
    *,
    command_runner: CommandRunner | None = None,
    which: Callable[[str], str | None] = shutil.which,
    system_name: str | None = None,
    run_id: str | None = None,
    clock: Clock | None = None,
) -> dict[str, object]:
    """Run one proof and return the same status published or printed by the CLI."""
    effective_clock = clock or _utc_now
    started_at = effective_clock()
    runner = command_runner or _run_process
    selected_system = system_name or platform.system()
    state: dict[str, Any] = {
        "engine": {"name": engine, "version": None},
        "source": {},
        "objects": {"image": None, "container": None},
        "cleanup": {"container": "not-created", "image": "not-created"},
    }
    resolved_root = root.expanduser().resolve(strict=True)

    try:
        resolved_output = _resolve_output(resolved_root, output)
    except ProofFailure as failure:
        return _unpublished_failure(
            failure,
            state,
            started_at=started_at,
            finished_at=effective_clock(),
        )

    try:
        resolved_output.parent.mkdir(parents=True, exist_ok=True)
        temporary_workspace = tempfile.TemporaryDirectory(
            prefix=f".{resolved_output.name}.release-proof-",
            dir=resolved_output.parent,
        )
    except OSError as exc:
        return _unpublished_failure(
            ProofFailure(
                "output-preflight", f"could not create private output staging: {exc}"
            ),
            state,
            started_at=started_at,
            finished_at=effective_clock(),
        )
    with temporary_workspace as raw_workspace:
        return _run_in_workspace(
            resolved_root,
            resolved_output,
            Path(raw_workspace),
            engine,
            state,
            runner,
            which,
            selected_system,
            run_id or secrets.token_hex(8),
            effective_clock,
            started_at,
        )


def _run_in_workspace(
    root: Path,
    output: Path,
    workspace: Path,
    engine: str,
    state: dict[str, Any],
    runner: CommandRunner,
    which: Callable[[str], str | None],
    system_name: str,
    run_id: str,
    clock: Clock,
    started_at: str,
) -> dict[str, object]:
    failure_staging = workspace / "failure"
    failure_staging.mkdir()
    failures = FailureArtifactStore(failure_staging)
    success_staging = workspace / "success"
    try:
        receipt = _execute_proof(
            root,
            engine,
            success_staging,
            workspace,
            state,
            failures,
            runner,
            which,
            system_name,
            run_id,
        )
    except KeyboardInterrupt:
        failure = ProofFailure(
            "signal", "release proof interrupted by SIGINT", returncode=130
        )
    except TerminationRequested as exc:
        failure = ProofFailure("signal", str(exc), returncode=128 + exc.signum)
    except ProofFailure as exc:
        failure = exc
    except Exception as exc:  # pragma: no cover - defensive status boundary
        failure = ProofFailure("internal", f"unexpected proof failure: {exc}")
    else:
        try:
            _publish_directory(success_staging, output)
        except ProofFailure as exc:
            failure = exc
        else:
            return _success_payload(
                output,
                state,
                receipt,
                system_name=system_name,
                started_at=started_at,
                finished_at=clock(),
            )
    return _publish_failure_output(
        output,
        failure_staging,
        failures,
        failure,
        state,
        started_at=started_at,
        finished_at=clock(),
    )


def _success_payload(
    output: Path,
    state: dict[str, Any],
    receipt: dict[str, object],
    *,
    system_name: str,
    started_at: str,
    finished_at: str,
) -> dict[str, object]:
    return {
        "schema_version": SCHEMA_VERSION,
        "status": "passed",
        "engine": state["engine"],
        "source": state["source"],
        "output": str(output),
        "platform": system_name,
        "receipt_sha256": _sha256(output / LINUX_REPORT_NAME),
        "host_native_companion": (
            MACOS_REPORT_NAME if system_name == "Darwin" else None
        ),
        "started_at": started_at,
        "finished_at": finished_at,
        "artifacts": receipt["artifacts"],
    }


def _publish_failure_output(
    output: Path,
    failure_staging: Path,
    failures: FailureArtifactStore,
    failure: ProofFailure,
    state: dict[str, Any],
    *,
    started_at: str,
    finished_at: str,
) -> dict[str, object]:
    if not failure.diagnostic_recorded:
        _record_failure(failures, failure)
    payload = _failure_payload(
        failure,
        state,
        failure_staging,
        started_at=started_at,
        finished_at=finished_at,
        output_published=True,
    )
    _write_json(failure_staging / FAILURE_REPORT_NAME, payload)
    try:
        _publish_directory(failure_staging, output)
    except ProofFailure:
        payload["output_published"] = False
    return payload


def _execute_proof(
    root: Path,
    engine: str,
    success_staging: Path,
    workspace: Path,
    state: dict[str, Any],
    failures: FailureArtifactStore,
    runner: CommandRunner,
    which: Callable[[str], str | None],
    system_name: str,
    run_id: str,
) -> dict[str, object]:
    _preflight_engine(root, engine, run_id, state, failures, runner, which)
    commit, tree, context = _prepare_source_context(
        root, workspace, state, failures, runner
    )
    _transfer_engine_artifacts(
        root,
        context,
        success_staging,
        engine,
        commit,
        run_id,
        state,
        failures,
        runner,
    )
    receipt = _validate_linux_bundle(success_staging, commit, tree)
    if system_name == "Darwin":
        _run_host_native(root, success_staging, commit, failures, runner)
    return receipt


def _preflight_engine(
    root: Path,
    engine: str,
    run_id: str,
    state: dict[str, Any],
    failures: FailureArtifactStore,
    runner: CommandRunner,
    which: Callable[[str], str | None],
) -> None:
    if engine not in ENGINE_CHOICES:
        _raise_recorded_failure(
            failures,
            ProofFailure("engine-preflight", f"unsupported container engine: {engine}"),
        )
    if which(engine) is None:
        _raise_recorded_failure(
            failures,
            ProofFailure(
                "engine-preflight",
                f"container engine is not executable: {engine}",
                command=[engine, "--version"],
                returncode=127,
            ),
        )
    if RUN_ID_PATTERN.fullmatch(run_id) is None:
        raise ProofFailure("internal", "invalid release-proof run identifier")

    version = _checked_command(
        [engine, "--version"],
        cwd=root,
        timeout_seconds=ENGINE_PROBE_TIMEOUT_SECONDS,
        phase="engine-preflight",
        failures=failures,
        runner=runner,
    )
    version_text = (version.stdout or version.stderr).strip()
    state["engine"]["version"] = _bounded_status_text(
        redact_text(version_text, dict(os.environ))  # env-policy: allow
    )


def _prepare_source_context(
    root: Path,
    workspace: Path,
    state: dict[str, Any],
    failures: FailureArtifactStore,
    runner: CommandRunner,
) -> tuple[str, str, Path]:
    status = _checked_command(
        ["git", "-C", str(root), "status", "--porcelain=v1", "--untracked-files=all"],
        cwd=root,
        timeout_seconds=SOURCE_TIMEOUT_SECONDS,
        phase="source-preflight",
        failures=failures,
        runner=runner,
    )
    if status.stdout.strip():
        _raise_recorded_failure(
            failures,
            ProofFailure(
                "source-preflight",
                "release proof requires a clean worktree",
                command=status.args,
                stdout=status.stdout,
            ),
        )
    commit = _git_object_id(root, "HEAD^{commit}", failures, runner)
    tree = _git_object_id(root, "HEAD^{tree}", failures, runner)
    state["source"] = {"commit": commit, "tree": tree}

    context = workspace / "source"
    _checked_command(
        [str(root / "scripts" / "release-proof-source"), str(context)],
        cwd=root,
        timeout_seconds=SOURCE_TIMEOUT_SECONDS,
        phase="source-export",
        failures=failures,
        runner=runner,
    )
    _validate_exported_source(context, commit, tree)
    return commit, tree, context


def _transfer_engine_artifacts(
    root: Path,
    context: Path,
    success_staging: Path,
    engine: str,
    commit: str,
    run_id: str,
    state: dict[str, Any],
    failures: FailureArtifactStore,
    runner: CommandRunner,
) -> None:
    short_commit = commit[:OBJECT_NAME_COMMIT_LENGTH]
    object_stem = f"spice-release-proof-{short_commit}-{run_id}"
    image = f"spice-release-proof:{short_commit}-{run_id}"
    container = object_stem
    state["objects"] = {"image": image, "container": container}
    primary_failure: ProofFailure | None = None
    image_created = False
    container_created = False
    success_staging.mkdir()
    try:
        _checked_command(
            [
                engine,
                "build",
                "--file",
                str(context / "release-proof" / "Containerfile"),
                "--tag",
                image,
                str(context),
            ],
            cwd=root,
            timeout_seconds=ENGINE_BUILD_TIMEOUT_SECONDS,
            phase="engine-build",
            failures=failures,
            runner=runner,
        )
        image_created = True
        state["cleanup"]["image"] = "pending"
        _checked_command(
            [engine, "create", "--name", container, image, "artifact-carrier"],
            cwd=root,
            timeout_seconds=ENGINE_OBJECT_TIMEOUT_SECONDS,
            phase="engine-create",
            failures=failures,
            runner=runner,
        )
        container_created = True
        state["cleanup"]["container"] = "pending"
        _checked_command(
            [engine, "cp", f"{container}:/artifacts/.", str(success_staging)],
            cwd=root,
            timeout_seconds=ENGINE_OBJECT_TIMEOUT_SECONDS,
            phase="engine-copy",
            failures=failures,
            runner=runner,
        )
    except ProofFailure as exc:
        primary_failure = exc
    finally:
        cleanup_failures = _cleanup_engine_objects(
            root,
            engine,
            image if image_created else None,
            container if container_created else None,
            state,
            failures,
            runner,
        )
    if primary_failure is not None:
        raise primary_failure
    if cleanup_failures:
        raise cleanup_failures[0]


def _run_host_native(
    root: Path,
    success_staging: Path,
    commit: str,
    failures: FailureArtifactStore,
    runner: CommandRunner,
) -> None:
    linux_bytes = (success_staging / LINUX_REPORT_NAME).read_bytes()
    _checked_command(
        [
            sys.executable,
            str(root / "release-proof" / "hostnative.py"),
            "--evidence-dir",
            str(success_staging),
        ],
        cwd=root,
        timeout_seconds=HOST_NATIVE_TIMEOUT_SECONDS,
        phase="host-native",
        failures=failures,
        runner=runner,
    )
    _validate_macos_companion(success_staging, commit, linux_bytes)


def _cleanup_engine_objects(
    root: Path,
    engine: str,
    image: str | None,
    container: str | None,
    state: dict[str, Any],
    failures: FailureArtifactStore,
    runner: CommandRunner,
) -> list[ProofFailure]:
    cleanup_failures: list[ProofFailure] = []
    targets = (
        ("container", container, [engine, "container", "rm", container or ""]),
        ("image", image, [engine, "image", "rm", image or ""]),
    )
    for kind, target, command in targets:
        if target is None:
            continue
        try:
            _checked_command(
                command,
                cwd=root,
                timeout_seconds=ENGINE_OBJECT_TIMEOUT_SECONDS,
                phase=f"cleanup-{kind}",
                failures=failures,
                runner=runner,
            )
        except ProofFailure as exc:
            state["cleanup"][kind] = "failed"
            cleanup_failures.append(exc)
        else:
            state["cleanup"][kind] = "removed"
    return cleanup_failures


def _checked_command(
    command: list[str],
    *,
    cwd: Path,
    timeout_seconds: float,
    phase: str,
    failures: FailureArtifactStore,
    runner: CommandRunner,
) -> subprocess.CompletedProcess[str]:
    try:
        completed = runner(command, cwd, timeout_seconds)
    except CommandDeadline as exc:
        failure = ProofFailure(
            phase,
            str(exc),
            command=exc.command,
            returncode=124,
            stdout=exc.stdout,
            stderr=exc.stderr,
        )
        _raise_recorded_failure(failures, failure)
    except OSError as exc:
        failure = ProofFailure(
            phase,
            f"could not execute command: {exc}",
            command=command,
            returncode=127,
        )
        _raise_recorded_failure(failures, failure)
    if completed.returncode != 0:
        failure = ProofFailure(
            phase,
            f"{phase} command exited {completed.returncode}",
            command=command,
            returncode=completed.returncode,
            stdout=completed.stdout,
            stderr=completed.stderr,
        )
        _raise_recorded_failure(failures, failure)
    return completed


def _raise_recorded_failure(
    failures: FailureArtifactStore, failure: ProofFailure
) -> NoReturn:
    _record_failure(failures, failure)
    failure.diagnostic_recorded = True
    raise failure


def _record_failure(failures: FailureArtifactStore, failure: ProofFailure) -> None:
    command = list(failure.command) or [sys.executable, str(Path(__file__).resolve())]
    failures.record(
        failure.phase,
        command,
        failure.returncode,
        failure.stdout,
        failure.stderr or str(failure),
        environment=dict(os.environ),  # env-policy: allow
    )


def _git_object_id(
    root: Path,
    revision: str,
    failures: FailureArtifactStore,
    runner: CommandRunner,
) -> str:
    completed = _checked_command(
        ["git", "-C", str(root), "rev-parse", "--verify", revision],
        cwd=root,
        timeout_seconds=SOURCE_TIMEOUT_SECONDS,
        phase="source-preflight",
        failures=failures,
        runner=runner,
    )
    value = completed.stdout.strip()
    if OBJECT_ID_PATTERN.fullmatch(value) is None:
        _raise_recorded_failure(
            failures,
            ProofFailure(
                "source-preflight",
                f"Git returned an invalid full object ID for {revision}",
                command=completed.args,
                stdout=completed.stdout,
            ),
        )
    return value


def _validate_exported_source(context: Path, commit: str, tree: str) -> None:
    path = context / ".release-proof" / "source.json"
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise ProofFailure("source-export", f"invalid exported source identity: {exc}")
    source = payload.get("source") if isinstance(payload, dict) else None
    if not isinstance(source, dict) or (
        source.get("commit"),
        source.get("tree"),
    ) != (commit, tree):
        raise ProofFailure(
            "source-export",
            "exported source identity does not match invocation HEAD",
        )


def _validate_linux_bundle(
    directory: Path, commit: str, tree: str
) -> dict[str, object]:
    _require_regular_inventory(directory, required_count=3)
    report_path = directory / LINUX_REPORT_NAME
    try:
        report = json.loads(report_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise ProofFailure("artifact-validation", f"invalid Linux report: {exc}")
    if not isinstance(report, dict) or report.get("schema_version") != SCHEMA_VERSION:
        raise ProofFailure("artifact-validation", "unexpected Linux report schema")
    boundary = report.get("claim_boundary")
    if not isinstance(boundary, dict) or (
        boundary.get("operating_system"),
        boundary.get("host_native_companion"),
    ) != ("linux", MACOS_REPORT_NAME):
        raise ProofFailure("artifact-validation", "invalid Linux claim boundary")
    identity = report.get("source_identity")
    source = identity.get("source") if isinstance(identity, dict) else None
    if not isinstance(source, dict) or (
        source.get("commit"),
        source.get("tree"),
    ) != (commit, tree):
        raise ProofFailure(
            "artifact-validation", "Linux report source identity mismatch"
        )
    artifacts = report.get("artifacts")
    if not isinstance(artifacts, dict):
        raise ProofFailure("artifact-validation", "Linux report has no artifacts")
    wheel_name = _validate_artifact_record(directory, artifacts.get("wheel"), ".whl")
    sdist_name = _validate_artifact_record(directory, artifacts.get("sdist"), ".tar.gz")
    expected = {LINUX_REPORT_NAME, wheel_name, sdist_name}
    actual = {entry.name for entry in directory.iterdir()}
    if actual != expected:
        raise ProofFailure(
            "artifact-validation",
            f"unexpected Linux artifact inventory: {sorted(actual)}",
        )
    if artifacts.get("installed_wheel_sha256") != artifacts["wheel"]["sha256"]:
        raise ProofFailure(
            "artifact-validation", "installed wheel digest is not the carried wheel"
        )
    if artifacts.get("sdist_rebuilt_from_sha256") != artifacts["sdist"]["sha256"]:
        raise ProofFailure(
            "artifact-validation", "rebuilt source digest is not the carried sdist"
        )
    return report


def _validate_artifact_record(directory: Path, record: object, suffix: str) -> str:
    if not isinstance(record, dict):
        raise ProofFailure("artifact-validation", f"missing {suffix} artifact record")
    filename = record.get("filename")
    byte_count = record.get("bytes")
    digest = record.get("sha256")
    if (
        not isinstance(filename, str)
        or Path(filename).name != filename
        or not filename.endswith(suffix)
    ):
        raise ProofFailure("artifact-validation", f"invalid {suffix} filename")
    if not isinstance(byte_count, int) or byte_count < 0:
        raise ProofFailure("artifact-validation", f"invalid {filename} byte count")
    if not isinstance(digest, str) or SHA256_PATTERN.fullmatch(digest) is None:
        raise ProofFailure("artifact-validation", f"invalid {filename} digest")
    path = directory / filename
    _require_regular_file(path)
    if path.stat().st_size != byte_count:
        raise ProofFailure("artifact-validation", f"size mismatch for {filename}")
    if _sha256(path) != digest:
        raise ProofFailure("artifact-validation", f"SHA-256 mismatch for {filename}")
    return filename


def _validate_macos_companion(directory: Path, commit: str, linux_bytes: bytes) -> None:
    _require_regular_inventory(directory, required_count=4)
    linux_path = directory / LINUX_REPORT_NAME
    if linux_path.read_bytes() != linux_bytes:
        raise ProofFailure("host-native", "host companion changed Linux evidence")
    path = directory / MACOS_REPORT_NAME
    try:
        report = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise ProofFailure("host-native", f"invalid macOS report: {exc}")
    if not isinstance(report, dict):
        raise ProofFailure("host-native", "unexpected macOS report schema")
    boundary = report.get("claim_boundary") if isinstance(report, dict) else None
    identity = report.get("source_identity") if isinstance(report, dict) else None
    container = report.get("container_evidence") if isinstance(report, dict) else None
    if report.get("schema_version") != SCHEMA_VERSION or not isinstance(boundary, dict):
        raise ProofFailure("host-native", "unexpected macOS report schema")
    if (
        boundary.get("operating_system"),
        boundary.get("container_operating_system"),
        boundary.get("container_evidence_unchanged"),
    ) != ("macos", "linux", True):
        raise ProofFailure("host-native", "invalid macOS claim boundary")
    if not isinstance(identity, dict) or (
        identity.get("agreement"),
        identity.get("checkout_head"),
        identity.get("container_source_commit"),
    ) != ("exact", commit, commit):
        raise ProofFailure("host-native", "macOS source identity mismatch")
    if not isinstance(container, dict) or (
        container.get("filename"),
        container.get("sha256"),
    ) != (LINUX_REPORT_NAME, hashlib.sha256(linux_bytes).hexdigest()):
        raise ProofFailure("host-native", "macOS container evidence mismatch")


def _require_regular_inventory(directory: Path, *, required_count: int) -> None:
    try:
        entries = list(directory.iterdir())
    except OSError as exc:
        raise ProofFailure("artifact-validation", f"cannot read artifact bundle: {exc}")
    if len(entries) != required_count:
        raise ProofFailure(
            "artifact-validation",
            f"artifact bundle has {len(entries)} entries; expected {required_count}",
        )
    for entry in entries:
        _require_regular_file(entry)


def _require_regular_file(path: Path) -> None:
    try:
        mode = path.lstat().st_mode
    except OSError as exc:
        raise ProofFailure("artifact-validation", f"cannot inspect {path.name}: {exc}")
    if not stat.S_ISREG(mode):
        raise ProofFailure(
            "artifact-validation", f"artifact is not a regular file: {path.name}"
        )


def _resolve_output(root: Path, output: Path) -> Path:
    resolved = output.expanduser().resolve()
    if resolved.exists():
        raise ProofFailure("output-preflight", f"output already exists: {resolved}")
    if resolved == root or root in resolved.parents:
        raise ProofFailure(
            "output-preflight",
            f"output must be outside the source worktree: {resolved}",
        )
    return resolved


def _publish_directory(staging: Path, output: Path) -> None:
    if output.exists():
        raise ProofFailure(
            "output-publication", f"output appeared during proof: {output}"
        )
    try:
        staging.rename(output)
    except OSError as exc:
        raise ProofFailure("output-publication", f"could not publish output: {exc}")


def _failure_payload(
    failure: ProofFailure,
    state: dict[str, Any],
    failure_staging: Path,
    *,
    started_at: str,
    finished_at: str,
    output_published: bool,
) -> dict[str, object]:
    environment = dict(os.environ)  # env-policy: allow
    diagnostics: list[dict[str, object]] = []
    directory = failure_staging / "failures"
    if directory.is_dir():
        for path in sorted(directory.iterdir()):
            _require_regular_file(path)
            diagnostics.append(
                {
                    "filename": path.relative_to(failure_staging).as_posix(),
                    "bytes": path.stat().st_size,
                    "sha256": _sha256(path),
                }
            )
    return {
        "schema_version": SCHEMA_VERSION,
        "status": "failed",
        "phase": failure.phase,
        "exit_code": failure.returncode,
        "message": _bounded_status_text(redact_text(str(failure), environment)),
        "engine": state["engine"],
        "source": state["source"],
        "objects": state["objects"],
        "cleanup": state["cleanup"],
        "diagnostics": diagnostics,
        "diagnostic_policy": failure_policy_payload(),
        "output_published": output_published,
        "started_at": started_at,
        "finished_at": finished_at,
    }


def _unpublished_failure(
    failure: ProofFailure,
    state: dict[str, Any],
    *,
    started_at: str,
    finished_at: str,
) -> dict[str, object]:
    return {
        "schema_version": SCHEMA_VERSION,
        "status": "failed",
        "phase": failure.phase,
        "exit_code": failure.returncode,
        "message": _bounded_status_text(
            redact_text(str(failure), dict(os.environ))  # env-policy: allow
        ),
        "engine": state["engine"],
        "source": state["source"],
        "objects": state["objects"],
        "cleanup": state["cleanup"],
        "diagnostics": [],
        "diagnostic_policy": failure_policy_payload(),
        "output_published": False,
        "started_at": started_at,
        "finished_at": finished_at,
    }


def _run_process(
    command: list[str], cwd: Path, timeout_seconds: float
) -> subprocess.CompletedProcess[str]:
    process = subprocess.Popen(
        command,
        cwd=cwd,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        **popen_new_process_group_kwargs(),
    )
    try:
        stdout, stderr = process.communicate(timeout=timeout_seconds)
    except subprocess.TimeoutExpired as exc:
        partial_stdout = _output_text(exc.stdout)
        partial_stderr = _output_text(exc.stderr)
        terminate_process_group(
            process,
            timeout_seconds=PROCESS_CLEANUP_TIMEOUT_SECONDS,
        )
        try:
            stdout, stderr = process.communicate(
                timeout=PROCESS_CLEANUP_TIMEOUT_SECONDS
            )
        except subprocess.TimeoutExpired:
            process.kill()
            stdout, stderr = process.communicate()
        raise CommandDeadline(
            command,
            timeout_seconds,
            stdout or partial_stdout,
            stderr or partial_stderr,
        ) from exc
    except BaseException:
        terminate_process_group(
            process,
            timeout_seconds=PROCESS_CLEANUP_TIMEOUT_SECONDS,
        )
        process.communicate()
        raise
    return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)


def _output_text(value: bytes | str | None) -> str:
    if value is None:
        return ""
    if isinstance(value, bytes):
        return value.decode("utf-8", errors="replace")
    return value


def _bounded_status_text(value: str) -> str:
    encoded = value.encode("utf-8")
    if len(encoded) <= MAX_STATUS_TEXT_BYTES:
        return value
    marker = "... release-proof status truncated ..."
    marker_bytes = marker.encode("utf-8")
    prefix = encoded[: MAX_STATUS_TEXT_BYTES - len(marker_bytes)].decode(
        "utf-8", errors="ignore"
    )
    return prefix + marker


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _write_json(path: Path, payload: dict[str, object]) -> None:
    temporary = path.with_name(f".{path.name}.tmp")
    temporary.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    temporary.replace(path)


def _utc_now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def _request_termination(signum: int, _frame: object) -> None:
    raise TerminationRequested(signum)


def main() -> int:
    parser = argparse.ArgumentParser(
        prog="scripts/release-proof",
        description="Build and export the disposable local release proof.",
    )
    parser.add_argument("--engine", choices=ENGINE_CHOICES, required=True)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()

    previous_term = signal.getsignal(signal.SIGTERM)
    signal.signal(signal.SIGTERM, _request_termination)
    try:
        result = run_release_proof(
            PROJECT_ROOT,
            arguments.engine,
            arguments.output,
        )
    finally:
        signal.signal(signal.SIGTERM, previous_term)
    stream = sys.stdout if result["status"] == "passed" else sys.stderr
    print(json.dumps(result, sort_keys=True), file=stream)
    if result["status"] == "passed":
        return 0
    exit_code = result.get("exit_code")
    return exit_code if isinstance(exit_code, int) and exit_code != 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())

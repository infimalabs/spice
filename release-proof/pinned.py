#!/usr/bin/env python3
"""Bind every release gate to one immutable commit snapshot."""

from __future__ import annotations

import argparse
import json
import os
import platform
import shlex
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

SCRIPT_DIRECTORY = Path(__file__).resolve().parent
if str(SCRIPT_DIRECTORY) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIRECTORY))

from evidence import (  # noqa: E402
    FAILURE_DIRNAME,
    FailureArtifactStore,
    redact_text,
)
from rehearse import (  # noqa: E402
    RECEIPT_NAME,
    RehearsalError,
    _run,
    _sha256,
    _write_json,
)

SCHEMA_VERSION = 1
EXIT_FAILURE = 2
BINDING_NAME = "release-proof-binding.json"
SNAPSHOT_DIRNAME = "source"
EXPORTER_RELATIVE = Path("scripts/release-proof-source")
INITIALIZER_RELATIVE = Path("release-proof/init-source.py")
TOOLCHAIN_RELATIVE = Path("release-proof/toolchain.py")
REHEARSAL_RELATIVE = Path("release-proof/rehearse.py")
TOOLCHAIN_RECORD_RELATIVE = Path(".git/release-proof-toolchain.json")
PROVISION_COMMAND = ("npm", "ci")
CONTAINER_ENGINES = ("docker", "podman")
IDENTITY_REVISIONS = (("commit", "HEAD^{commit}"), ("tree", "HEAD^{tree}"))


class PinError(RuntimeError):
    """The rehearsal evidence could not be bound to one immutable commit."""


def _identity(
    root: Path,
    failures: FailureArtifactStore | None = None,
) -> dict[str, str]:
    """Resolve the exact commit and tree a checkout exposes right now."""
    return {
        name: _run(
            ["git", "rev-parse", revision],
            cwd=root,
            capture=True,
            failures=failures,
            gate="identity",
        ).stdout.strip()
        for name, revision in IDENTITY_REVISIONS
    }


def materialize(
    root: Path,
    workspace: Path,
    failures: FailureArtifactStore,
) -> tuple[Path, dict[str, object]]:
    """Export the boundary commit into a repository nothing can advance."""
    snapshot = workspace / SNAPSHOT_DIRNAME
    _run(
        [str(root / EXPORTER_RELATIVE), str(snapshot)],
        cwd=root,
        capture=True,
        failures=failures,
        gate="source-export",
    )
    initialized = _run(
        [sys.executable, str(snapshot / INITIALIZER_RELATIVE), str(snapshot)],
        cwd=snapshot,
        capture=True,
        failures=failures,
        gate="source-initialize",
    )
    return snapshot, json.loads(initialized.stdout)


def _exported_source(identities: dict[str, object]) -> dict[str, object]:
    source = identities.get("source")
    if not isinstance(source, dict):
        raise PinError(f"snapshot carries no source identity: {identities!r}")
    return source


def verify_boundary(
    boundary: dict[str, str],
    identities: dict[str, object],
) -> dict[str, object]:
    """Prove the exported snapshot carries the commit the audit selected."""
    source = _exported_source(identities)
    exported = {name: source.get(name) for name in boundary}
    if exported != boundary:
        raise PinError(
            "the exported snapshot is not the selected boundary commit:\n"
            + json.dumps(
                {"selected": boundary, "exported": exported},
                indent=2,
                sort_keys=True,
            )
        )
    return source


def _failed_gate(
    name: str,
    command: list[str],
    detail: str,
) -> dict[str, object]:
    """Record a gate that ran against the pinned snapshot and came back red."""
    return {
        "gate": name,
        "status": "failed",
        "command": command,
        "detail": redact_text(detail, dict(os.environ)),  # env-policy: allow
    }


def provision_gate(
    snapshot: Path,
    failures: FailureArtifactStore,
) -> dict[str, object]:
    """Install the browser toolchain the pinned lockfile declares."""
    try:
        _run(PROVISION_COMMAND, cwd=snapshot, failures=failures, gate="npm-ci")
    except RehearsalError as exc:
        return _failed_gate("browser-toolchain", list(PROVISION_COMMAND), str(exc))
    return {
        "gate": "browser-toolchain",
        "status": "ran",
        "command": list(PROVISION_COMMAND),
    }


def toolchain_gate(snapshot: Path) -> dict[str, object]:
    """Resolve the declared toolchain, or record it as explicitly not run."""
    record = snapshot / TOOLCHAIN_RECORD_RELATIVE
    command = [
        sys.executable,
        str(snapshot / TOOLCHAIN_RELATIVE),
        "--output",
        str(record),
    ]
    print(f"+ {shlex.join(command)}", flush=True)
    completed = subprocess.run(
        command,
        check=False,
        cwd=snapshot,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if completed.returncode == 0:
        return {"gate": "declared-toolchain", "status": "ran", "record": str(record)}
    payload: dict[str, object] = {
        "schema_version": SCHEMA_VERSION,
        "gate": "declared-toolchain",
        "status": "not-run",
        "reason": "the declared release-proof toolchain does not resolve here",
        "detail": redact_text(
            completed.stderr.strip(),
            dict(os.environ),  # env-policy: allow
        ),
        "host": _host_identity(),
    }
    _write_json(record, payload)
    return payload


def appliance_gate(which=shutil.which) -> dict[str, object]:
    """Record whether the container appliance can run beside the host gates."""
    engines = [name for name in CONTAINER_ENGINES if which(name) is not None]
    if engines:
        return {
            "gate": "container-appliance",
            "status": "available",
            "engines": engines,
        }
    return {
        "gate": "container-appliance",
        "status": "not-run",
        "reason": "no container engine is installed: " + ", ".join(CONTAINER_ENGINES),
    }


def rehearse_pinned(snapshot: Path, artifacts: Path) -> dict[str, object]:
    """Run the snapshot's own rehearsal so the harness is pinned too."""
    command = [
        sys.executable,
        str(snapshot / REHEARSAL_RELATIVE),
        "--artifacts",
        str(artifacts),
    ]
    print(f"+ {shlex.join(command)}", flush=True)
    completed = subprocess.run(command, check=False, cwd=snapshot)
    if completed.returncode == 0:
        return {"gate": "rehearsal", "status": "ran", "exit_code": 0}
    failure = _failed_gate(
        "rehearsal",
        command,
        f"the pinned rehearsal exited {completed.returncode}",
    )
    failure["exit_code"] = completed.returncode
    failure["diagnostics"] = str(artifacts / FAILURE_DIRNAME)
    return failure


def _host_identity() -> dict[str, str]:
    return {
        "platform": platform.platform(),
        "python": platform.python_version(),
        "system": platform.system().casefold(),
    }


def _receipt_evidence(artifacts: Path) -> dict[str, object]:
    receipt = artifacts / RECEIPT_NAME
    if not receipt.exists():
        return {"filename": RECEIPT_NAME, "status": "absent"}
    return {
        "filename": RECEIPT_NAME,
        "bytes": receipt.stat().st_size,
        "sha256": _sha256(receipt),
    }


def _gate_names(gates: list[dict[str, object]], status: str) -> list[str]:
    return sorted(str(gate["gate"]) for gate in gates if gate.get("status") == status)


def _bind_identity(before: dict[str, str], after: dict[str, str]) -> dict[str, str]:
    if before != after:
        raise PinError(
            "the pinned snapshot moved while the gates ran:\n"
            + json.dumps({"before": before, "after": after}, indent=2, sort_keys=True)
        )
    return after


def run_pinned_proof(
    root: Path,
    artifacts: Path,
    workspace: Path,
) -> dict[str, object]:
    """Run every release gate from one immutable snapshot and bind the proof."""
    root = root.resolve(strict=True)
    artifacts = artifacts.resolve()
    failures = FailureArtifactStore(artifacts)
    boundary = _identity(root, failures)
    snapshot, identities = materialize(root, workspace.resolve(strict=True), failures)
    source = verify_boundary(boundary, identities)
    before = _identity(snapshot, failures)
    gates = [
        provision_gate(snapshot, failures),
        toolchain_gate(snapshot),
        appliance_gate(),
        rehearse_pinned(snapshot, artifacts),
    ]
    after = _bind_identity(before, _identity(snapshot, failures))
    binding: dict[str, object] = {
        "schema_version": SCHEMA_VERSION,
        "boundary": boundary,
        "not_run": _gate_names(gates, "not-run"),
        "failed": _gate_names(gates, "failed"),
        "snapshot": {
            "path": str(snapshot),
            "before": before,
            "after": after,
            "exported_source": source,
        },
        "origin_worktree": _origin_evidence(root, boundary, failures),
        "gates": gates,
        "evidence": _receipt_evidence(artifacts),
        "host": _host_identity(),
    }
    artifacts.mkdir(parents=True, exist_ok=True)
    _write_json(artifacts / BINDING_NAME, binding)
    return binding


def _origin_evidence(
    root: Path,
    boundary: dict[str, str],
    failures: FailureArtifactStore,
) -> dict[str, object]:
    """Record the live checkout's drift, which the pinned proof survives."""
    after = _identity(root, failures)
    return {
        "path": str(root),
        "before": boundary,
        "after": after,
        "advanced_during_run": after != boundary,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifacts", required=True, type=Path)
    parser.add_argument("--workspace", type=Path)
    arguments = parser.parse_args()
    workspace = arguments.workspace
    if workspace is None:
        workspace = Path(tempfile.mkdtemp(prefix="spice-release-pin-"))
    workspace.mkdir(parents=True, exist_ok=True)
    try:
        binding = run_pinned_proof(
            Path(__file__).resolve().parent.parent,
            arguments.artifacts,
            workspace,
        )
    except (OSError, PinError, RehearsalError, ValueError) as exc:
        safe_error = redact_text(str(exc), dict(os.environ))  # env-policy: allow
        print(f"release-proof pinned proof: {safe_error}", file=sys.stderr)
        return EXIT_FAILURE
    print(json.dumps(binding, sort_keys=True))
    if binding["failed"]:
        print(
            f"release-proof pinned proof: red gates {binding['failed']} are bound to "
            f"{binding['boundary']}",
            file=sys.stderr,
        )
        return EXIT_FAILURE
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

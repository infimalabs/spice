"""Pinned-boundary contracts: every gate binds to one immutable snapshot."""

from __future__ import annotations

import json
import shutil
from pathlib import Path
from typing import Any, cast

import pytest

from tests.test_releaseproofhelpers import (
    CONTAINERFILE,
    PINNED,
    REHEARSAL,
    SOURCE_EXPORTER,
    SOURCE_INITIALIZER,
    _git,
    _source_repository,
    _test_sha256,
)


def _pinned_repository(root: Path) -> tuple[Path, dict[str, object]]:
    """Build an origin checkout that carries the real snapshot machinery."""
    repository, _initial = _source_repository(root)
    (repository / "scripts").mkdir()
    (repository / "release-proof").mkdir()
    shutil.copy2(SOURCE_EXPORTER, repository / "scripts" / "release-proof-source")
    shutil.copy2(SOURCE_INITIALIZER, repository / "release-proof" / "init-source.py")
    _git(repository, "add", "--all")
    _git(repository, "commit", "--quiet", "--message", "boundary")
    return repository, {
        "commit": _git(repository, "rev-parse", "HEAD^{commit}"),
        "tree": _git(repository, "rev-parse", "HEAD^{tree}"),
        "commit_epoch": int(_git(repository, "show", "-s", "--format=%ct", "HEAD")),
    }


def _stub_rehearsal(monkeypatch, during_gates=lambda: None) -> dict[str, object]:
    """Stand in for the long rehearsal and publish a receipt it can be bound to."""
    receipt = {"schema_version": 1, "tests": {"python": {"passed": 7, "total": 7}}}

    def rehearse(_snapshot: Path, artifacts: Path) -> dict[str, object]:
        during_gates()
        artifacts.mkdir(parents=True, exist_ok=True)
        (artifacts / "release-proof.json").write_text(
            json.dumps(receipt, sort_keys=True) + "\n", encoding="utf-8"
        )
        return {"gate": "rehearsal", "status": "ran", "exit_code": 0}

    monkeypatch.setattr(PINNED, "rehearse_pinned", rehearse)
    monkeypatch.setattr(
        PINNED,
        "provision_gate",
        lambda _snapshot, _failures: {"gate": "browser-toolchain", "status": "ran"},
    )
    monkeypatch.setattr(
        PINNED,
        "toolchain_gate",
        lambda _snapshot: {"gate": "declared-toolchain", "status": "ran"},
    )
    monkeypatch.setattr(
        PINNED,
        "appliance_gate",
        lambda: {"gate": "container-appliance", "status": "not-run"},
    )
    return receipt


def test_pinned_proof_binds_every_gate_to_the_exported_boundary_commit(
    tmp_path, monkeypatch
):
    repository, boundary = _pinned_repository(tmp_path)
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    artifacts = tmp_path / "artifacts"
    _stub_rehearsal(monkeypatch)

    binding = PINNED.run_pinned_proof(repository, artifacts, workspace)
    snapshot = Path(cast(dict[str, Any], binding["snapshot"])["path"])

    assert binding["boundary"] == {
        "commit": boundary["commit"],
        "tree": boundary["tree"],
    }
    assert binding["snapshot"]["exported_source"] == boundary
    assert binding["snapshot"]["before"] == binding["snapshot"]["after"]
    assert binding["origin_worktree"] == {
        "path": str(repository.resolve()),
        "before": binding["boundary"],
        "after": binding["boundary"],
        "advanced_during_run": False,
    }
    assert binding["evidence"] == {
        "filename": "release-proof.json",
        "bytes": (artifacts / "release-proof.json").stat().st_size,
        "sha256": _test_sha256(artifacts / "release-proof.json"),
    }
    assert (binding["failed"], binding["not_run"]) == ([], ["container-appliance"])
    assert (snapshot / "payload.txt").read_text(encoding="utf-8") == (
        "tracked release source\n"
    )
    assert (
        json.loads(
            (artifacts / "release-proof-binding.json").read_text(encoding="utf-8")
        )
        == binding
    )


def test_pinned_proof_survives_the_origin_advancing_while_the_gates_run(
    tmp_path, monkeypatch
):
    repository, boundary = _pinned_repository(tmp_path)
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    artifacts = tmp_path / "artifacts"

    def integrate_a_peer_commit() -> None:
        (repository / "payload.txt").write_text("peer integration\n", encoding="utf-8")
        _git(repository, "add", "payload.txt")
        _git(repository, "commit", "--quiet", "--message", "peer")

    _stub_rehearsal(monkeypatch, during_gates=integrate_a_peer_commit)

    binding = PINNED.run_pinned_proof(repository, artifacts, workspace)
    snapshot = Path(cast(dict[str, Any], binding["snapshot"])["path"])
    advanced = _git(repository, "rev-parse", "HEAD^{commit}")

    assert binding["origin_worktree"]["advanced_during_run"] is True
    assert binding["origin_worktree"]["after"] == {
        "commit": advanced,
        "tree": _git(repository, "rev-parse", "HEAD^{tree}"),
    }
    assert binding["boundary"] == {
        "commit": boundary["commit"],
        "tree": boundary["tree"],
    }
    assert binding["snapshot"]["before"] == binding["snapshot"]["after"]
    assert binding["snapshot"]["exported_source"] == boundary
    assert (snapshot / "payload.txt").read_text(encoding="utf-8") == (
        "tracked release source\n"
    )
    assert (
        advanced == boundary["commit"],
        _git(snapshot, "status", "--porcelain"),
    ) == (
        False,
        "",
    )


def test_pinned_proof_publishes_a_binding_for_a_red_gate_and_names_the_not_run(
    tmp_path, monkeypatch
):
    repository, boundary = _pinned_repository(tmp_path)
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    artifacts = tmp_path / "artifacts"
    _stub_rehearsal(monkeypatch)
    monkeypatch.setattr(
        PINNED,
        "rehearse_pinned",
        lambda _snapshot, artifacts: {
            "gate": "rehearsal",
            "status": "failed",
            "exit_code": 2,
        },
    )

    binding = PINNED.run_pinned_proof(repository, artifacts, workspace)
    published = json.loads(
        (artifacts / "release-proof-binding.json").read_text(encoding="utf-8")
    )

    assert binding["failed"] == ["rehearsal"]
    assert binding["not_run"] == ["container-appliance"]
    assert binding["evidence"] == {
        "filename": "release-proof.json",
        "status": "absent",
    }
    assert binding["boundary"] == {
        "commit": boundary["commit"],
        "tree": boundary["tree"],
    }
    assert binding["snapshot"]["before"] == binding["snapshot"]["after"]
    assert published == binding


def test_pinned_proof_refuses_evidence_that_is_not_bound_to_the_boundary():
    selected = {"commit": "a" * 40, "tree": "b" * 40}
    exported = {"commit": "c" * 40, "tree": "d" * 40, "commit_epoch": 1}

    with pytest.raises(PINNED.PinError) as boundary_error:
        PINNED.verify_boundary(selected, {"source": exported})
    with pytest.raises(PINNED.PinError) as drift_error:
        PINNED._bind_identity(selected, {"commit": "c" * 40, "tree": "d" * 40})

    assert (
        "c" * 40 in str(boundary_error.value),
        "a" * 40 in str(boundary_error.value),
        "moved while the gates ran" in str(drift_error.value),
    ) == (True, True, True)


def test_pinned_proof_records_an_unresolvable_toolchain_as_explicitly_not_run(
    tmp_path,
):
    snapshot = tmp_path / "source"
    (snapshot / "release-proof").mkdir(parents=True)
    (snapshot / ".git").mkdir()
    (snapshot / "release-proof" / "toolchain.py").write_text(
        "import sys\n"
        "print('No module named build', file=sys.stderr)\n"
        "raise SystemExit(1)\n",
        encoding="utf-8",
    )

    gate = PINNED.toolchain_gate(snapshot)
    recorded = REHEARSAL._load_git_private_json(
        snapshot, "release-proof-toolchain.json"
    )

    assert (gate["gate"], gate["status"], gate["detail"]) == (
        "declared-toolchain",
        "not-run",
        "No module named build",
    )
    assert gate["reason"] == (
        "the declared release-proof toolchain does not resolve here"
    )
    assert recorded == gate


def test_pinned_proof_reports_container_engine_availability_exactly():
    absent = PINNED.appliance_gate(which=lambda _name: None)
    present = PINNED.appliance_gate(which=lambda name: f"/fake/{name}")

    assert absent == {
        "gate": "container-appliance",
        "status": "not-run",
        "reason": "no container engine is installed: docker, podman",
    }
    assert present == {
        "gate": "container-appliance",
        "status": "available",
        "engines": ["docker", "podman"],
    }


def test_pinned_host_driver_runs_the_container_preparation_steps():
    containerfile = CONTAINERFILE.read_text(encoding="utf-8")
    declared = [
        line.removeprefix("RUN ").removesuffix(" \\")
        for line in containerfile.splitlines()
        if line.startswith("RUN python3 release-proof/") or line == "RUN npm ci"
    ]

    assert declared == [
        f"python3 {PINNED.INITIALIZER_RELATIVE} /proof/source",
        "npm ci",
        f"python3 {PINNED.TOOLCHAIN_RELATIVE}",
        f"python3 {PINNED.REHEARSAL_RELATIVE} --artifacts /proof/artifacts",
    ]
    assert PINNED.PROVISION_COMMAND == ("npm", "ci")

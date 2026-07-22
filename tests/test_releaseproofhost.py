"""Direct-host receipt and linked-worktree release-proof behavior."""

from __future__ import annotations

import importlib.util
import json
import subprocess
import zipfile
from pathlib import Path
from typing import Any


PROJECT_ROOT = Path(__file__).resolve().parent.parent
REHEARSAL_SCRIPT = PROJECT_ROOT / "release-proof" / "rehearse.py"


def _load_rehearsal() -> Any:
    spec = importlib.util.spec_from_file_location(
        "spice_release_proof_host_rehearsal", REHEARSAL_SCRIPT
    )
    if spec is None or spec.loader is None:
        raise RuntimeError(f"could not load release rehearsal: {REHEARSAL_SCRIPT}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


REHEARSAL = _load_rehearsal()


def _git(repository: Path, *arguments: str) -> str:
    completed = subprocess.run(
        ["git", "-C", str(repository), *arguments],
        check=True,
        capture_output=True,
        text=True,
    )
    return completed.stdout.strip()


def _repository(root: Path) -> Path:
    root.mkdir()
    _git(root, "init", "--quiet", "--initial-branch=main")
    _git(root, "config", "user.email", "proof@example.invalid")
    _git(root, "config", "user.name", "Release Proof Fixture")
    (root / "pyproject.toml").write_text(
        '[project]\nname = "spice-harness"\nversion = "9.8.7"\n',
        encoding="utf-8",
    )
    _git(root, "add", "pyproject.toml")
    _git(root, "commit", "--quiet", "--message", "fixture")
    return root


def _write_wheel(path: Path) -> None:
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr("spice/__init__.py", "namespace package\n")


def test_direct_host_rehearsal_emits_citable_container_absence(tmp_path, monkeypatch):
    root = _repository(tmp_path / "source")
    artifacts = tmp_path / "artifacts"
    monkeypatch.setattr(REHEARSAL.platform, "system", lambda: "Darwin")
    monkeypatch.setattr(
        REHEARSAL,
        "verify_packaging_toolchain",
        lambda _root, _failures: list(REHEARSAL.PACKAGING_MODULES),
    )
    monkeypatch.setattr(
        REHEARSAL,
        "_run_source_gates",
        lambda _root, _scratch, _failures: {
            "python": {"passed": 1, "total": 1},
            "ruff": {"passed": True},
            "browser": {"counts": {"failed": 0, "passed": 1, "total": 1}},
            "mutation": {"probe.py": {"killed": 1, "mutants": 1}},
        },
    )
    monkeypatch.setattr(
        REHEARSAL, "_materialize_committed_source", lambda _root, _scratch: root
    )

    def build(_source, output, version, _failures, *, project_root):
        del project_root
        sdist = output / f"spice_harness-{version}.tar.gz"
        wheel = output / f"spice_harness-{version}-py3-none-any.whl"
        sdist.write_bytes(b"canonical sdist\n")
        _write_wheel(wheel)
        return sdist, wheel

    def rebuild(_sdist, version, scratch, _failures, *, project_root):
        del project_root
        wheel = scratch / f"spice_harness-{version}-py3-none-any.whl"
        _write_wheel(wheel)
        return wheel

    monkeypatch.setattr(REHEARSAL, "_build_canonical_artifacts", build)
    monkeypatch.setattr(REHEARSAL, "_validate_installed_wheel", lambda *_args: None)
    monkeypatch.setattr(REHEARSAL, "_rebuild_wheel_from_sdist", rebuild)

    receipt = REHEARSAL.rehearse(root, artifacts)

    assert receipt["claim_boundary"] == {
        "operating_system": "macos",
        "claim": "host-artifact-rehearsal",
        "container_provenance": "absent",
    }
    assert receipt["source_identity"] == {
        "availability": "absent",
        "reason": (
            "container-only proof record is not produced by a direct host "
            "artifact rehearsal"
        ),
        "record": "release-proof-identities.json",
        "scope": "container-only",
    }
    assert receipt["toolchain"] == {
        "availability": "absent",
        "reason": (
            "container-only proof record is not produced by a direct host "
            "artifact rehearsal"
        ),
        "record": "release-proof-toolchain.json",
        "scope": "container-only",
    }
    assert (
        json.loads((artifacts / REHEARSAL.RECEIPT_NAME).read_text(encoding="utf-8"))
        == receipt
    )

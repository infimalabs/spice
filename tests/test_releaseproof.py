"""Hermetic source and toolchain boundary contracts."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import stat
import subprocess
import sys
import zipfile
from pathlib import Path
from typing import Any, cast

PROJECT_ROOT = Path(__file__).resolve().parent.parent
SOURCE_EXPORTER = PROJECT_ROOT / "scripts" / "release-proof-source"
SOURCE_INITIALIZER = PROJECT_ROOT / "release-proof" / "init-source.py"
CONTAINERFILE = PROJECT_ROOT / "release-proof" / "Containerfile"
TOOLCHAIN_DECLARATION = PROJECT_ROOT / "release-proof" / "toolchain.json"
REHEARSAL_SCRIPT = PROJECT_ROOT / "release-proof" / "rehearse.py"
BASE_IMAGE = "mcr.microsoft.com/playwright:v1.61.0-noble"
BASE_DIGEST = "sha256:57b65fdc9ceabe0ef613124c7bbe2babcf9362c4d85e382fe3b03604e84b428a"


def _load_rehearsal() -> Any:
    spec = importlib.util.spec_from_file_location(
        "spice_release_proof_rehearsal", REHEARSAL_SCRIPT
    )
    if spec is None or spec.loader is None:
        raise RuntimeError(f"could not load release rehearsal: {REHEARSAL_SCRIPT}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


REHEARSAL = _load_rehearsal()


def _git(repository: Path, *arguments: str, environment=None) -> str:
    completed = subprocess.run(
        ["git", "-C", str(repository), *arguments],
        check=True,
        capture_output=True,
        env=environment,
        text=True,
    )
    return completed.stdout.strip()


def _source_repository(
    root: Path,
    *,
    object_format: str = "sha1",
    tracked_ignored: bool = False,
) -> tuple[Path, dict[str, object]]:
    repository = root / "source"
    repository.mkdir()
    ignored_tracked_rule = "/tracked-ignored.txt\n" if tracked_ignored else ""
    (repository / ".gitignore").write_text(
        ".cache/\n.spice/\n.venv/\nbuild/\ndist/\nnode_modules/\n"
        + ignored_tracked_rule,
        encoding="utf-8",
    )
    (repository / "payload.txt").write_text(
        "tracked release source\n", encoding="utf-8"
    )
    if tracked_ignored:
        (repository / "tracked-ignored.txt").write_text(
            "tracked source despite its ignore rule\n",
            encoding="utf-8",
        )
    _git(
        repository,
        "init",
        "--quiet",
        "--initial-branch=main",
        f"--object-format={object_format}",
    )
    _git(repository, "config", "user.email", "proof@example.invalid")
    _git(repository, "config", "user.name", "Release Proof Fixture")
    _git(repository, "add", ".gitignore", "payload.txt")
    if tracked_ignored:
        _git(repository, "add", "--force", "tracked-ignored.txt")
    commit_environment = {
        "GIT_AUTHOR_DATE": "1700000000 +0000",
        "GIT_COMMITTER_DATE": "1700000000 +0000",
        "LC_ALL": "C",
        "PATH": os.environ["PATH"],  # env-policy: allow
        "TZ": "UTC",
    }
    _git(
        repository,
        "commit",
        "--quiet",
        "--message",
        "fixture",
        environment=commit_environment,
    )
    source = {
        "commit": _git(repository, "rev-parse", "HEAD^{commit}"),
        "tree": _git(repository, "rev-parse", "HEAD^{tree}"),
        "commit_epoch": 1700000000,
    }
    return repository, source


def _write_ignored_residue(repository: Path, marker: str) -> None:
    for relative in (
        ".cache/download",
        ".spice/operations.sqlite3",
        ".venv/bin/python",
        "build/stale.py",
        "dist/stale.whl",
        "node_modules/playwright/cache",
    ):
        path = repository / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(f"{marker}:{relative}\n", encoding="utf-8")


def _export(repository: Path, destination: Path) -> dict[str, object]:
    completed = subprocess.run(
        [str(SOURCE_EXPORTER), str(destination)],
        cwd=repository,
        check=True,
        capture_output=True,
        text=True,
    )
    assert completed.stdout.strip() == str(destination)
    return json.loads(
        (destination / ".release-proof/source.json").read_text(encoding="utf-8")
    )


def _file_inventory(root: Path) -> list[str]:
    return sorted(
        path.relative_to(root).as_posix() for path in root.rglob("*") if path.is_file()
    )


def _content_identity(root: Path) -> list[tuple[str, int, str]]:
    identity = []
    for path in sorted(
        candidate for candidate in root.rglob("*") if candidate.is_file()
    ):
        identity.append(
            (
                path.relative_to(root).as_posix(),
                stat.S_IMODE(path.stat().st_mode),
                hashlib.sha256(path.read_bytes()).hexdigest(),
            )
        )
    return identity


def _initialize(context: Path) -> dict[str, object]:
    completed = subprocess.run(
        [sys.executable, str(SOURCE_INITIALIZER), str(context)],
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(completed.stdout)


def test_source_export_contains_exact_head_and_is_stable_across_ignored_residue(
    tmp_path,
):
    repository, source = _source_repository(tmp_path)
    _write_ignored_residue(repository, "first run")
    first = tmp_path / "context-one"
    first_provenance = _export(repository, first)

    _write_ignored_residue(repository, "second run")
    second = tmp_path / "context-two"
    second_provenance = _export(repository, second)

    expected_provenance = {"schema_version": 1, "source": source}
    assert first_provenance == expected_provenance
    assert second_provenance == expected_provenance
    assert _file_inventory(first) == [
        ".gitignore",
        ".release-proof/source.json",
        "payload.txt",
    ]
    assert _content_identity(first) == _content_identity(second)


def test_synthetic_repository_keeps_source_identity_and_clean_git_semantics(tmp_path):
    repository, source = _source_repository(tmp_path)
    _write_ignored_residue(repository, "host-only")
    first = tmp_path / "context-one"
    second = tmp_path / "context-two"
    _export(repository, first)
    _export(repository, second)

    first_identities = _initialize(first)
    second_identities = _initialize(second)
    expected = {
        "schema_version": 1,
        "source": source,
        "synthetic": {
            "commit": _git(first, "rev-parse", "HEAD^{commit}"),
            "tree": _git(first, "rev-parse", "HEAD^{tree}"),
        },
    }

    assert first_identities == expected
    assert second_identities == expected
    assert (
        json.loads(
            (first / ".git/release-proof-identities.json").read_text(encoding="utf-8")
        )
        == expected
    )
    assert _git(first, "status", "--porcelain=v1", "--untracked-files=all") == ""
    assert _git(first, "log", "-1", "--format=%s") == (
        "Synthetic release-proof source snapshot"
    )


def test_synthetic_repository_preserves_a_force_tracked_ignored_path(tmp_path):
    repository, source = _source_repository(tmp_path, tracked_ignored=True)
    context = tmp_path / "context"
    _export(repository, context)

    identities = _initialize(context)

    assert identities["source"] == source
    assert _git(context, "ls-files", "tracked-ignored.txt") == "tracked-ignored.txt"
    assert (context / "tracked-ignored.txt").read_text(encoding="utf-8") == (
        "tracked source despite its ignore rule\n"
    )
    assert _git(context, "cat-file", "-t", str(source["tree"])) == "tree"
    assert _git(context, "status", "--porcelain=v1", "--untracked-files=all") == ""


def test_synthetic_repository_preserves_sha256_object_format_and_provenance(
    tmp_path,
):
    repository, source = _source_repository(tmp_path, object_format="sha256")
    context = tmp_path / "context"
    _export(repository, context)

    identities = _initialize(context)
    synthetic = cast(dict[str, object], identities["synthetic"])

    assert identities["source"] == source
    assert _git(context, "rev-parse", "--show-object-format") == "sha256"
    assert {
        len(str(source["commit"])),
        len(str(source["tree"])),
        len(str(synthetic["commit"])),
        len(str(synthetic["tree"])),
    } == {64}
    assert (
        source["commit"] == synthetic["commit"],
        source["tree"] == synthetic["tree"],
    ) == (False, False)
    assert _git(context, "status", "--porcelain=v1", "--untracked-files=all") == ""


def test_rehearsal_declares_every_gate_and_runs_during_the_container_build():
    containerfile = CONTAINERFILE.read_text(encoding="utf-8")

    assert REHEARSAL.PYTHON_GATE_COMMAND == ("uv", "run", "--locked", "pytest")
    assert REHEARSAL.RUFF_GATE_COMMAND == (
        "uv",
        "run",
        "--locked",
        "ruff",
        "check",
        ".",
    )
    assert REHEARSAL.BROWSER_GATE_COMMAND == (
        "node",
        "tests/browser/run_release_smokes.js",
    )
    assert REHEARSAL.MUTATION_GATE_COMMAND == (
        "uv",
        "run",
        "--locked",
        "spice",
        "study",
        "mutations",
        "spice/config/layers.py",
        "--test",
        "tests/test_configlayer.py",
        "--max-mutants",
        "20",
        "--timeout",
        "30",
        "--ratchet",
        "tests/mutation-ratchet.json",
        "--json",
    )
    assert REHEARSAL.CHECKS == (
        "python",
        "ruff",
        "browser-release-manifest",
        "deterministic-mutation-cohort",
        "build-sdist",
        "build-wheel",
        "metadata",
        "isolated-install",
        "installed-imports",
        "installed-console",
        "sdist-rebuild",
        "wheel-member-content",
        "clean-worktree",
    )
    assert (
        "RUN python3 release-proof/rehearse.py --artifacts /proof/artifacts"
        in containerfile
    )


def test_canonical_artifacts_are_built_once_outside_the_source_and_checked_exactly(
    tmp_path, monkeypatch
):
    root = tmp_path / "source"
    root.mkdir()
    artifacts = tmp_path / "artifacts"
    calls: list[tuple[tuple[str, ...], Path]] = []

    def build_tools(command, *, cwd, **_kwargs):
        argv = tuple(command)
        calls.append((argv, cwd))
        if "--sdist" in argv:
            (artifacts / "spice_harness-1.2.3.tar.gz").write_bytes(b"sdist\n")
        if "--wheel" in argv:
            _write_test_wheel(
                artifacts / "spice_harness-1.2.3-py3-none-any.whl",
                {"spice/__init__.py": b"namespace package\n"},
                year=2024,
            )
        return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

    monkeypatch.setattr(REHEARSAL, "_run", build_tools)

    sdist, wheel = REHEARSAL._build_canonical_artifacts(root, artifacts, "1.2.3")

    assert (sdist.name, wheel.name) == (
        "spice_harness-1.2.3.tar.gz",
        "spice_harness-1.2.3-py3-none-any.whl",
    )
    assert tuple(cwd for _command, cwd in calls) == (
        artifacts,
        artifacts,
        artifacts,
    )
    assert (
        sum("--sdist" in command for command, _cwd in calls),
        sum("--wheel" in command for command, _cwd in calls),
        calls[-1][0],
    ) == (
        1,
        1,
        (
            sys.executable,
            "-m",
            "twine",
            "check",
            str(sdist),
            str(wheel),
        ),
    )


def test_rehearsal_materializes_only_the_committed_source_boundary(tmp_path):
    repository, _source = _source_repository(tmp_path)
    ignored = repository / "build" / "lib" / "spice" / "stale.py"
    ignored.parent.mkdir(parents=True)
    ignored.write_text("host-only residue\n", encoding="utf-8")
    scratch = tmp_path / "scratch"
    scratch.mkdir()

    materialized = REHEARSAL._materialize_committed_source(repository, scratch)

    assert _content_identity(materialized) == [
        (
            ".gitignore",
            0o644,
            hashlib.sha256((repository / ".gitignore").read_bytes()).hexdigest(),
        ),
        (
            "payload.txt",
            0o644,
            hashlib.sha256((repository / "payload.txt").read_bytes()).hexdigest(),
        ),
    ]
    assert ((materialized / "build" / "lib" / "spice" / "stale.py").exists(),) == (
        False,
    )


def test_mutation_rehearsal_requires_the_exact_committed_cohort(tmp_path):
    ratchet = tmp_path / "tests" / "mutation-ratchet.json"
    ratchet.parent.mkdir()
    expected = {
        "spice/config/layers.py": {
            "killed": 13,
            "mutants": 20,
            "score": 0.65,
            "survived": 7,
            "timed_out": 0,
        }
    }
    ratchet.write_text(
        json.dumps({"version": 1, "modules": expected}),
        encoding="utf-8",
    )
    output = json.dumps(
        {
            "artifactKind": "spice.study.mutations",
            "reports": [
                {
                    "path": "spice/config/layers.py",
                    **expected["spice/config/layers.py"],
                    "results": [],
                }
            ],
            "ratchetRegressions": [],
        }
    )

    assert REHEARSAL.verify_mutation_cohort(tmp_path, output) == expected


def test_wheel_member_comparison_ignores_only_outer_zip_container_bytes(tmp_path):
    canonical = tmp_path / "canonical.whl"
    rebuilt = tmp_path / "rebuilt.whl"
    members = {
        "spice/__init__.py": b"namespace package\n",
        "spice_harness.dist-info/METADATA": b"Name: spice-harness\n",
    }
    _write_test_wheel(
        canonical,
        members,
        year=2024,
        compression=zipfile.ZIP_STORED,
    )
    _write_test_wheel(
        rebuilt,
        members,
        year=2025,
        compression=zipfile.ZIP_DEFLATED,
    )

    assert (
        canonical.read_bytes() == rebuilt.read_bytes(),
        REHEARSAL.wheel_member_mismatches(canonical, rebuilt),
    ) == (False, [])


def test_wheel_member_comparison_catalogs_every_exact_delta(tmp_path):
    canonical = tmp_path / "canonical.whl"
    rebuilt = tmp_path / "rebuilt.whl"
    _write_test_wheel(
        canonical,
        {
            "changed.txt": b"canonical\n",
            "missing.txt": b"missing\n",
            "same.txt": b"same\n",
        },
        year=2024,
    )
    _write_test_wheel(
        rebuilt,
        {
            "changed.txt": b"rebuilt\n",
            "extra.txt": b"extra\n",
            "same.txt": b"same\n",
        },
        year=2025,
    )

    assert REHEARSAL.wheel_member_mismatches(canonical, rebuilt) == [
        {
            "kind": "missing-from-rebuilt",
            "member": "missing.txt",
            "canonical_sha256": hashlib.sha256(b"missing\n").hexdigest(),
        },
        {
            "kind": "extra-in-rebuilt",
            "member": "extra.txt",
            "rebuilt_sha256": hashlib.sha256(b"extra\n").hexdigest(),
        },
        {
            "kind": "content-changed",
            "member": "changed.txt",
            "canonical_sha256": hashlib.sha256(b"canonical\n").hexdigest(),
            "rebuilt_sha256": hashlib.sha256(b"rebuilt\n").hexdigest(),
        },
    ]


def test_rehearsal_receipt_carries_the_artifacts_it_installs_and_rebuilds(
    tmp_path, monkeypatch
):
    root = tmp_path / "source"
    git_dir = root / ".git"
    git_dir.mkdir(parents=True)
    (root / "pyproject.toml").write_text(
        '[project]\nname = "spice-harness"\nversion = "9.8.7"\n',
        encoding="utf-8",
    )
    (git_dir / "release-proof-identities.json").write_text(
        json.dumps({"schema_version": 1, "source": {}, "synthetic": {}}),
        encoding="utf-8",
    )
    (git_dir / "release-proof-toolchain.json").write_text(
        json.dumps({"schema_version": 1, "resolved": {}}),
        encoding="utf-8",
    )
    artifacts = tmp_path / "artifacts"
    carried: list[tuple[str, Path]] = []
    rebuilt_hashes: list[str] = []

    monkeypatch.setattr(
        REHEARSAL,
        "_run_source_gates",
        lambda _root: {"spice/config/layers.py": {"killed": 13, "mutants": 20}},
    )

    def build_canonical(_root, artifact_dir, version):
        artifact_dir.mkdir()
        sdist = artifact_dir / f"spice_harness-{version}.tar.gz"
        wheel = artifact_dir / f"spice_harness-{version}-py3-none-any.whl"
        sdist.write_bytes(b"canonical sdist\n")
        _write_test_wheel(
            wheel,
            {"spice/__init__.py": b"namespace package\n"},
            year=2024,
        )
        return sdist, wheel

    def validate_installed(_root, wheel, _version, _scratch):
        carried.append(("installed", wheel))

    def rebuild_from_sdist(sdist, version, scratch):
        carried.append(("rebuilt", sdist))
        rebuilt = scratch / f"spice_harness-{version}-py3-none-any.whl"
        _write_test_wheel(
            rebuilt,
            {"spice/__init__.py": b"namespace package\n"},
            year=2025,
        )
        rebuilt_hashes.append(_test_sha256(rebuilt))
        return rebuilt

    def clean_status(command, **_kwargs):
        return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

    monkeypatch.setattr(REHEARSAL, "_build_canonical_artifacts", build_canonical)
    monkeypatch.setattr(
        REHEARSAL,
        "_materialize_committed_source",
        lambda _root, _scratch: root,
    )
    monkeypatch.setattr(REHEARSAL, "_validate_installed_wheel", validate_installed)
    monkeypatch.setattr(REHEARSAL, "_rebuild_wheel_from_sdist", rebuild_from_sdist)
    monkeypatch.setattr(REHEARSAL, "_run", clean_status)

    receipt = REHEARSAL.rehearse(root, artifacts)
    sdist = artifacts / "spice_harness-9.8.7.tar.gz"
    wheel = artifacts / "spice_harness-9.8.7-py3-none-any.whl"

    assert carried == [("installed", wheel), ("rebuilt", sdist)]
    assert receipt["artifacts"] == {
        "sdist": {"filename": sdist.name, "sha256": _test_sha256(sdist)},
        "wheel": {"filename": wheel.name, "sha256": _test_sha256(wheel)},
        "installed_wheel_sha256": _test_sha256(wheel),
        "sdist_rebuilt_from_sha256": _test_sha256(sdist),
    }
    assert receipt["wheel_member_comparison"] == {
        "canonical_members": 1,
        "rebuilt_members": 1,
        "mismatches": [],
        "rebuilt_wheel_sha256": rebuilt_hashes[0],
        "outer_archive_reproducibility": "deferred",
    }
    assert (
        _test_sha256(wheel) == rebuilt_hashes[0],
        receipt["checks"],
    ) == (False, list(REHEARSAL.CHECKS))
    assert tuple(sorted(path.name for path in artifacts.iterdir())) == (
        "rehearsal.json",
        wheel.name,
        sdist.name,
    )
    assert json.loads((artifacts / "rehearsal.json").read_text(encoding="utf-8")) == (
        receipt
    )


def test_container_declares_immutable_base_and_complete_resolved_toolchain():
    declaration = json.loads(TOOLCHAIN_DECLARATION.read_text(encoding="utf-8"))
    containerfile = CONTAINERFILE.read_text(encoding="utf-8")

    assert declaration == {
        "schema_version": 1,
        "base": {
            "image": BASE_IMAGE,
            "digest": BASE_DIGEST,
            "platforms": ["linux/amd64", "linux/arm64"],
        },
        "pinned": {
            "build": "1.3.0",
            "pip": "25.1.1",
            "playwright": "1.61.0",
            "setuptools": "80.9.0",
            "twine": "6.1.0",
            "uv": "0.11.23",
            "wheel": "0.45.1",
        },
    }
    assert f"FROM {BASE_IMAGE}@{BASE_DIGEST}" in containerfile
    assert "COPY --chown=pwuser:pwuser . /proof/source" in containerfile
    assert "RUN python3 release-proof/init-source.py /proof/source" in containerfile
    assert "RUN npm ci" in containerfile
    assert "--output .git/release-proof-toolchain.json" in containerfile
    for name, version in (
        ("BUILD", "1.3.0"),
        ("PIP", "25.1.1"),
        ("SETUPTOOLS", "80.9.0"),
        ("TWINE", "6.1.0"),
        ("UV", "0.11.23"),
        ("WHEEL", "0.45.1"),
    ):
        assert f"ARG {name}_VERSION={version}" in containerfile
    assert SOURCE_EXPORTER.stat().st_mode & stat.S_IXUSR == stat.S_IXUSR


def _write_test_wheel(
    path: Path,
    members: dict[str, bytes],
    *,
    year: int,
    compression: int = zipfile.ZIP_STORED,
) -> None:
    with zipfile.ZipFile(path, "w") as archive:
        for name, content in members.items():
            info = zipfile.ZipInfo(name, date_time=(year, 1, 1, 0, 0, 0))
            info.compress_type = compression
            archive.writestr(info, content)


def _test_sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()

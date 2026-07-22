"""Hermetic source and toolchain boundary contracts."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import stat
import subprocess
import sys
import tomllib
import zipfile
from pathlib import Path
from typing import Any, cast

import pytest

PROJECT_ROOT = Path(__file__).resolve().parent.parent
SOURCE_EXPORTER = PROJECT_ROOT / "scripts" / "release-proof-source"
SOURCE_INITIALIZER = PROJECT_ROOT / "release-proof" / "init-source.py"
CONTAINERFILE = PROJECT_ROOT / "release-proof" / "Containerfile"
TOOLCHAIN_RELATIVE_PATH = "release-proof/toolchain.json"
TOOLCHAIN_DECLARATION = PROJECT_ROOT / TOOLCHAIN_RELATIVE_PATH
PYPROJECT = PROJECT_ROOT / "pyproject.toml"
REHEARSAL_SCRIPT = PROJECT_ROOT / "release-proof" / "rehearse.py"
EVIDENCE_SCRIPT = PROJECT_ROOT / "release-proof" / "evidence.py"
HOSTNATIVE_SCRIPT = PROJECT_ROOT / "release-proof" / "hostnative.py"
APPLIANCE_SCRIPT = PROJECT_ROOT / "release-proof" / "appliance.py"
BASE_IMAGE = "mcr.microsoft.com/playwright:v1.61.0-noble"
BASE_DIGEST = "sha256:57b65fdc9ceabe0ef613124c7bbe2babcf9362c4d85e382fe3b03604e84b428a"
FAKE_COPY_FAILURE_EXIT_CODE = 41


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


def _load_evidence() -> Any:
    spec = importlib.util.spec_from_file_location(
        "spice_release_proof_evidence", EVIDENCE_SCRIPT
    )
    if spec is None or spec.loader is None:
        raise RuntimeError(f"could not load release evidence: {EVIDENCE_SCRIPT}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


EVIDENCE = _load_evidence()


def _load_hostnative() -> Any:
    spec = importlib.util.spec_from_file_location(
        "spice_release_proof_hostnative", HOSTNATIVE_SCRIPT
    )
    if spec is None or spec.loader is None:
        raise RuntimeError(f"could not load host-native proof: {HOSTNATIVE_SCRIPT}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


HOSTNATIVE = _load_hostnative()


def _load_appliance() -> Any:
    spec = importlib.util.spec_from_file_location(
        "spice_release_proof_appliance", APPLIANCE_SCRIPT
    )
    if spec is None or spec.loader is None:
        raise RuntimeError(
            f"could not load release-proof appliance: {APPLIANCE_SCRIPT}"
        )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


APPLIANCE = _load_appliance()


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
        "packaging-toolchain",
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


def test_locked_packaging_pins_match_the_container_toolchain_declaration():
    """One declaration governs both runs, so host evidence is container evidence.

    The container installs its packaging tools from Containerfile ARGs while a
    host run resolves them from uv.lock. They are only interchangeable while
    both agree with release-proof/toolchain.json, so that agreement is a gate
    rather than a convention.
    """
    declared = json.loads(TOOLCHAIN_DECLARATION.read_text(encoding="utf-8"))["pinned"]
    project = tomllib.loads(PYPROJECT.read_text(encoding="utf-8"))
    group = project["dependency-groups"]["dev"]
    distributions = set(REHEARSAL.PACKAGING_TOOLCHAIN.values())

    assert sorted(
        requirement
        for requirement in group
        if requirement.split("==")[0] in distributions
    ) == sorted(f"{name}=={declared[name]}" for name in distributions)


DECLARED_PACKAGING_PINS = {
    "build": "1.3.0",
    "setuptools": "80.9.0",
    "twine": "6.1.0",
    "wheel": "0.45.1",
}


def _toolchain_source(tmp_path: Path) -> Path:
    root = tmp_path / "declared"
    (root / "release-proof").mkdir(parents=True)
    (root / TOOLCHAIN_RELATIVE_PATH).write_text(
        json.dumps({"schema_version": 1, "pinned": dict(DECLARED_PACKAGING_PINS)}),
        encoding="utf-8",
    )
    return root


def _resolved_packaging(missing: list[str] | None = None, **overrides: str):
    report = {
        "missing": missing or [],
        "resolved": dict(DECLARED_PACKAGING_PINS) | overrides,
    }

    def probe(command, **_kwargs):
        return subprocess.CompletedProcess(
            command, 0, stdout=json.dumps(report), stderr=""
        )

    return probe


def test_packaging_preflight_names_the_tool_whose_version_drifted(
    tmp_path, monkeypatch
):
    """Drift is reported by name, because the pins are the proof's whole point.

    A host resolving a different twine than the container declares would still
    build artifacts, just not artifacts the container's evidence describes.
    """
    root = _toolchain_source(tmp_path)
    monkeypatch.setattr(REHEARSAL, "_run", _resolved_packaging(twine="5.0.0"))

    with pytest.raises(REHEARSAL.RehearsalError) as failure:
        REHEARSAL.verify_packaging_toolchain(root)

    message = str(failure.value)
    assert (
        "twine" in message,
        TOOLCHAIN_RELATIVE_PATH in message,
        '"twine": "5.0.0"' in message,
        '"twine": "6.1.0"' in message,
    ) == (True, True, True, True)


def test_packaging_preflight_names_uv_before_spending_a_subprocess(
    tmp_path, monkeypatch
):
    """Without uv there is no locked surface at all, so say so and stop.

    This is the host-checkout failure the container never sees, and it has to
    read as an install instruction rather than as a missing-module traceback
    from somewhere deep in the artifact phase.
    """
    root = _toolchain_source(tmp_path)

    def refuse_to_run(command, **_kwargs):
        raise AssertionError(f"spent a subprocess without uv present: {command}")

    monkeypatch.setattr(REHEARSAL.shutil, "which", lambda _name: None)
    monkeypatch.setattr(REHEARSAL, "_run", refuse_to_run)

    with pytest.raises(REHEARSAL.RehearsalError) as failure:
        REHEARSAL.verify_packaging_toolchain(root)

    message = str(failure.value)
    assert ("uv" in message, "PATH" in message, "locked toolchain" in message) == (
        True,
        True,
        True,
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
            *REHEARSAL.packaging_python_command(root),
            "-m",
            "twine",
            "check",
            str(sdist),
            str(wheel),
        ),
    )


def test_packaging_steps_run_from_the_locked_project_toolchain(tmp_path, monkeypatch):
    project = tmp_path / "checkout"
    source = tmp_path / "exported"
    artifacts = tmp_path / "artifacts"
    scratch = tmp_path / "scratch"
    scratch.mkdir()
    calls: list[tuple[str, ...]] = []

    def build_tools(command, *, cwd, **_kwargs):
        argv = tuple(command)
        calls.append(argv)
        if "--sdist" in argv:
            (artifacts / "spice_harness-1.2.3.tar.gz").write_bytes(b"sdist\n")
        if "--wheel" in argv:
            _write_test_wheel(
                cwd / "spice_harness-1.2.3-py3-none-any.whl",
                {"spice/__init__.py": b"namespace package\n"},
                year=2024,
            )
        return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

    monkeypatch.setattr(REHEARSAL, "_run", build_tools)
    monkeypatch.setattr(
        REHEARSAL,
        "_extract_sdist",
        lambda _sdist, destination, _version: destination,
    )

    sdist, _wheel = REHEARSAL._build_canonical_artifacts(
        source, artifacts, "1.2.3", None, project_root=project
    )
    rebuilt = REHEARSAL._rebuild_wheel_from_sdist(
        sdist, "1.2.3", scratch, None, project_root=project
    )

    locked = REHEARSAL.packaging_python_command(project)
    assert (
        [command[: len(locked)] for command in calls],
        rebuilt.name,
    ) == ([locked] * 4, "spice_harness-1.2.3-py3-none-any.whl")
    assert locked == (
        "uv",
        "run",
        "--locked",
        "--project",
        str(project),
        "python",
        "-P",
    )


def test_packaging_preflight_names_every_missing_module(tmp_path, monkeypatch):
    root = tmp_path / "checkout"

    missing_modules = _resolved_packaging(missing=["build.__main__", "twine"])

    monkeypatch.setattr(REHEARSAL, "_run", missing_modules)

    with pytest.raises(REHEARSAL.RehearsalError) as failure:
        REHEARSAL.verify_packaging_toolchain(root)

    message = str(failure.value)
    assert (
        "build.__main__, twine failed to import" in message,
        "uv lock" in message,
    ) == (True, True)


def test_packaging_preflight_records_the_toolchain_it_proved(tmp_path, monkeypatch):
    root = _toolchain_source(tmp_path)
    probes: list[tuple[str, ...]] = []
    resolve = _resolved_packaging()

    def present_modules(command, **kwargs):
        probes.append(tuple(command))
        return resolve(command, **kwargs)

    monkeypatch.setattr(REHEARSAL, "_run", present_modules)

    proven = REHEARSAL.verify_packaging_toolchain(root)

    prefix = REHEARSAL.packaging_python_command(root)
    assert (proven, probes[0][: len(prefix)], probes[0][len(prefix)]) == (
        DECLARED_PACKAGING_PINS,
        prefix,
        "-c",
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


def test_failure_artifacts_are_deterministic_bounded_and_secret_redacted(tmp_path):
    store = EVIDENCE.FailureArtifactStore(tmp_path)
    environment = {
        "PATH": os.environ["PATH"],  # env-policy: allow
        "SERVICE_TOKEN": "environment-secret-value",
    }
    token_url = "https://user:pass@example.test/api?access_token=url-secret&safe=yes"

    for index in range(EVIDENCE.MAX_FAILURE_ARTIFACTS + 3):
        store.record(
            "browser gate",
            ["probe", token_url],
            index + 1,
            "environment-secret-value\n" + ("x" * EVIDENCE.MAX_FAILURE_BYTES),
            f"request failed: {token_url}\n",
            environment=environment,
        )

    files = sorted((tmp_path / EVIDENCE.FAILURE_DIRNAME).glob("*.log"))
    last = files[-1].read_text(encoding="utf-8")
    assert (
        len(files),
        files[0].name,
        files[-1].name,
        max(path.stat().st_size for path in files) <= EVIDENCE.MAX_FAILURE_BYTES,
    ) == (
        EVIDENCE.MAX_FAILURE_ARTIFACTS,
        "01-browser-gate.log",
        "08-overflow.log",
        True,
    )
    assert (
        "environment-secret-value" in last,
        "url-secret" in last,
        "user:pass" in last,
        "<redacted-env:SERVICE_TOKEN>" in last,
        "access_token=%3Credacted%3E" in last,
    ) == (False, False, False, True, True)


def test_url_credentials_redact_userinfo_query_and_fragment_values():
    diagnostic = (
        "request="
        "https://user:pass@example.test/callback?api_key=query-secret&safe=yes"
        "#/route?access_token=fragment-secret&state=ok\n"
        "redirect=https://example.test/done#token=second-fragment&tab=summary"
    )

    assert EVIDENCE.redact_text(diagnostic, {}) == (
        "request="
        "https://<redacted>@example.test/callback?api_key=%3Credacted%3E&safe=yes"
        "#/route?access_token=%3Credacted%3E&state=ok\n"
        "redirect=https://example.test/done#token=%3Credacted%3E&tab=summary"
    )


def test_pytest_count_evidence_uses_the_final_summary():
    output = (
        "bringing up nodes...\n"
        "................................\n"
        "998 passed, 4 skipped, 2 xfailed, 7 deselected in 12.34s\n"
    )

    assert EVIDENCE.parse_pytest_counts(output) == {
        "passed": 998,
        "skipped": 4,
        "xfailed": 2,
        "deselected": 7,
        "total": 1004,
    }


def test_host_native_companion_records_macos_beside_unchanged_linux_proof(
    tmp_path, monkeypatch
):
    evidence_dir = tmp_path / "artifacts"
    evidence_dir.mkdir()
    container_path = evidence_dir / "release-proof.json"
    source_commit = _git(PROJECT_ROOT, "rev-parse", "HEAD^{commit}")
    container_path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "claim_boundary": {"operating_system": "linux"},
                "source_identity": {
                    "schema_version": 1,
                    "source": {
                        "commit": source_commit,
                        "tree": "0" * len(source_commit),
                        "commit_epoch": 1,
                    },
                    "synthetic": {
                        "commit": "1" * len(source_commit),
                        "tree": "2" * len(source_commit),
                    },
                },
            },
            sort_keys=True,
        )
        + "\n",
        encoding="utf-8",
    )
    container_before = container_path.read_bytes()

    def host_command(command, **_kwargs):
        if command[0] == "git":
            return subprocess.CompletedProcess(
                command,
                0,
                stdout=source_commit + "\n",
                stderr="",
            )
        audio = Path(command[2])
        audio.write_bytes(b"FORM\x00\x00native speech")
        return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

    monkeypatch.setattr(HOSTNATIVE.platform, "system", lambda: "Darwin")
    monkeypatch.setattr(HOSTNATIVE.platform, "release", lambda: "25.5.0")
    monkeypatch.setattr(HOSTNATIVE.platform, "machine", lambda: "arm64")
    monkeypatch.setattr(HOSTNATIVE.select, "kqueue", object(), raising=False)
    monkeypatch.setattr(HOSTNATIVE, "_run_command", host_command)
    monkeypatch.setattr(
        HOSTNATIVE,
        "_probe_kqueue_event",
        lambda: {
            "status": "passed",
            "backend": "kqueue",
            "production_path": "spice.serve.livebus._KqueueWatch",
            "event": "filesystem-write",
            "timeout_seconds": 5.0,
            "elapsed_ms": 12.5,
        },
    )
    monkeypatch.setattr(
        HOSTNATIVE,
        "_appearance",
        lambda _root, _failures: {"status": "passed", "style": "dark"},
    )

    report = HOSTNATIVE.collect_host_native_evidence(PROJECT_ROOT, evidence_dir)

    assert report["claim_boundary"] == {
        "operating_system": "macos",
        "container_operating_system": "linux",
        "container_evidence_unchanged": True,
    }
    assert report["source_identity"] == {
        "agreement": "exact",
        "checkout_head": source_commit,
        "container_source_commit": source_commit,
    }
    assert report["checks"] == {
        "kqueue-or-fsevents": {
            "status": "passed",
            "backend": "kqueue",
            "production_path": "spice.serve.livebus._KqueueWatch",
            "event": "filesystem-write",
            "timeout_seconds": 5.0,
            "elapsed_ms": 12.5,
        },
        "appearance": {"status": "passed", "style": "dark"},
        "speech": {
            "status": "passed",
            "backend": "/usr/bin/say",
            "bytes": len(b"FORM\x00\x00native speech"),
            "sha256": hashlib.sha256(b"FORM\x00\x00native speech").hexdigest(),
        },
    }
    assert (
        container_path.read_bytes(),
        json.loads(
            (evidence_dir / "release-proof-macos.json").read_text(encoding="utf-8")
        ),
    ) == (container_before, report)


@pytest.mark.skipif(sys.platform != "darwin", reason="requires native kqueue")
def test_host_native_probe_observes_a_real_bounded_kqueue_event():
    result = HOSTNATIVE._probe_kqueue_event()

    assert result["status"] == "passed"
    assert result["backend"] == "kqueue"
    assert result["production_path"] == "spice.serve.livebus._KqueueWatch"
    assert result["event"] == "filesystem-write"
    assert result["timeout_seconds"] == HOSTNATIVE.KQUEUE_EVENT_TIMEOUT_SECONDS
    assert (
        0
        <= result["elapsed_ms"]
        <= (
            (
                HOSTNATIVE.KQUEUE_EVENT_TIMEOUT_SECONDS
                + HOSTNATIVE.LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S
            )
            * 1000
        )
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
    root, artifacts = _release_receipt_fixture(tmp_path)
    carried: list[tuple[str, Path]] = []
    rebuilt_hashes: list[str] = []

    monkeypatch.setattr(
        REHEARSAL,
        "_run_source_gates",
        lambda _root, _scratch, _failures: {
            "python": {"passed": 999, "total": 999},
            "ruff": {"passed": True},
            "browser": {
                "schemaVersion": 1,
                "counts": {"failed": 0, "passed": 45, "skipped": 1, "total": 46},
                "scenarios": [{"path": "serve_smoke.js", "status": "passed"}],
                "externalState": [
                    {"path": "live_smoke.js", "reason": "requires live state"}
                ],
            },
            "mutation": {"spice/config/layers.py": {"killed": 13, "mutants": 20}},
        },
    )

    monkeypatch.setattr(
        REHEARSAL,
        "verify_packaging_toolchain",
        lambda _root, _failures: dict(DECLARED_PACKAGING_PINS),
    )

    def build_canonical(_root, artifact_dir, version, _failures, *, project_root):
        carried.append(("built", project_root))
        sdist = artifact_dir / f"spice_harness-{version}.tar.gz"
        wheel = artifact_dir / f"spice_harness-{version}-py3-none-any.whl"
        sdist.write_bytes(b"canonical sdist\n")
        _write_test_wheel(
            wheel,
            {"spice/__init__.py": b"namespace package\n"},
            year=2024,
        )
        return sdist, wheel

    def validate_installed(_root, wheel, _version, _scratch, _failures):
        carried.append(("installed", wheel))

    def rebuild_from_sdist(sdist, version, scratch, _failures, *, project_root):
        carried.append(("rebuilt", project_root))
        carried.append(("rebuilt-from", sdist))
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

    assert carried == [
        ("built", root),
        ("installed", wheel),
        ("rebuilt", root),
        ("rebuilt-from", sdist),
    ]
    _assert_release_receipt(receipt, artifacts, sdist, wheel, rebuilt_hashes[0])


def test_packaging_preflight_stops_the_rehearsal_before_the_source_gates(
    tmp_path, monkeypatch
):
    """The whole point of moving this check first: fail in seconds, not minutes.

    The observed failure ran the full test, browser and mutation gates and only
    then discovered the artifact phase had no toolchain, so the source gates are
    wired to fail here if they are ever reached.
    """
    root, artifacts = _release_receipt_fixture(tmp_path)

    def refuse_source_gates(*_arguments):
        raise AssertionError("source gates ran before the packaging toolchain existed")

    monkeypatch.setattr(REHEARSAL, "_run_source_gates", refuse_source_gates)
    monkeypatch.setattr(REHEARSAL.shutil, "which", lambda _name: None)

    with pytest.raises(REHEARSAL.RehearsalError) as failure:
        REHEARSAL.rehearse(root, artifacts)

    assert "uv" in str(failure.value)


def _release_receipt_fixture(tmp_path: Path) -> tuple[Path, Path]:
    root = tmp_path / "source"
    git_dir = root / ".git"
    git_dir.mkdir(parents=True)
    (root / "release-proof").mkdir(parents=True)
    (root / TOOLCHAIN_RELATIVE_PATH).write_text(
        json.dumps({"schema_version": 1, "pinned": dict(DECLARED_PACKAGING_PINS)}),
        encoding="utf-8",
    )
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
    return root, tmp_path / "artifacts"


def _assert_release_receipt(
    receipt,
    artifacts: Path,
    sdist: Path,
    wheel: Path,
    rebuilt_hash: str,
) -> None:
    assert receipt["artifacts"] == {
        "sdist": {
            "filename": sdist.name,
            "bytes": sdist.stat().st_size,
            "sha256": _test_sha256(sdist),
        },
        "wheel": {
            "filename": wheel.name,
            "bytes": wheel.stat().st_size,
            "sha256": _test_sha256(wheel),
        },
        "installed_wheel_sha256": _test_sha256(wheel),
        "sdist_rebuilt_from_sha256": _test_sha256(sdist),
    }
    assert receipt["content_comparison"] == {
        "canonical_members": 1,
        "rebuilt_members": 1,
        "mismatches": [],
        "rebuilt_wheel_sha256": rebuilt_hash,
        "outer_archive_reproducibility": "deferred",
    }
    assert (
        _test_sha256(wheel) == rebuilt_hash,
        receipt["artifact_rehearsal"]["checks"],
    ) == (False, list(REHEARSAL.CHECKS))
    assert receipt["tests"] == {
        "python": {"passed": 999, "total": 999},
        "ruff": {"passed": True},
    }
    assert receipt["browser"]["counts"] == {
        "failed": 0,
        "passed": 45,
        "skipped": 1,
        "total": 46,
    }
    # The receipt carries the packaging pins the artifact chain actually ran on,
    # so host evidence states its own toolchain instead of implying the image's.
    assert receipt["artifact_rehearsal"]["packaging_toolchain"] == (
        DECLARED_PACKAGING_PINS
    )
    assert receipt["claim_boundary"] == {
        "operating_system": "linux",
        "host_native_companion": "release-proof-macos.json",
        "host_native_checks": ["kqueue-or-fsevents", "appearance", "speech"],
    }
    assert tuple(sorted(path.name for path in artifacts.iterdir())) == (
        "release-proof.json",
        wheel.name,
        sdist.name,
    )
    assert json.loads(
        (artifacts / "release-proof.json").read_text(encoding="utf-8")
    ) == (receipt)


class _ApplianceRunner:
    def __init__(
        self,
        engine: str,
        *,
        failure_mode: str | None = None,
    ) -> None:
        self.engine = engine
        self.failure_mode = failure_mode
        self.commit = "a" * 40
        self.tree = "b" * 40
        self.calls: list[tuple[list[str], Path, float]] = []

    def __call__(
        self, command: list[str], cwd: Path, timeout_seconds: float
    ) -> subprocess.CompletedProcess[str]:
        self.calls.append((list(command), cwd, timeout_seconds))
        if command[0] == "git":
            if "status" in command:
                stdout = " M tracked.py\n" if self.failure_mode == "dirty" else ""
            elif command[-1] == "HEAD^{commit}":
                stdout = self.commit + "\n"
            else:
                stdout = self.tree + "\n"
            return subprocess.CompletedProcess(command, 0, stdout=stdout, stderr="")
        if Path(command[0]).name == "release-proof-source":
            context = Path(command[1])
            provenance = context / ".release-proof" / "source.json"
            provenance.parent.mkdir(parents=True)
            provenance.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "source": {
                            "commit": self.commit,
                            "tree": self.tree,
                            "commit_epoch": 1,
                        },
                    }
                ),
                encoding="utf-8",
            )
            return subprocess.CompletedProcess(command, 0, stdout="", stderr="")
        if command[0] == self.engine:
            verb = command[1]
            if verb == "--version":
                return subprocess.CompletedProcess(
                    command,
                    0,
                    stdout=f"{self.engine} version 99.0\n",
                    stderr="",
                )
            if verb == "build" and self.failure_mode == "deadline":
                raise APPLIANCE.CommandDeadline(
                    command,
                    timeout_seconds,
                    "environment-secret-value\n",
                    _credential_url(),
                )
            if verb == "cp":
                if self.failure_mode == "signal":
                    raise KeyboardInterrupt
                if self.failure_mode == "copy":
                    return subprocess.CompletedProcess(
                        command,
                        FAKE_COPY_FAILURE_EXIT_CODE,
                        stdout="environment-secret-value\n",
                        stderr=_credential_url(),
                    )
                self._write_linux_bundle(
                    Path(command[-1]), corrupt=self.failure_mode == "digest"
                )
            if verb == "container" and self.failure_mode == "cleanup":
                return subprocess.CompletedProcess(
                    command,
                    55,
                    stdout="",
                    stderr="owned container cleanup failed\n",
                )
            return subprocess.CompletedProcess(command, 0, stdout="", stderr="")
        if len(command) > 1 and Path(command[1]).name == "hostnative.py":
            if self.failure_mode == "host-native":
                return subprocess.CompletedProcess(
                    command,
                    61,
                    stdout="",
                    stderr="native companion failed\n",
                )
            evidence_dir = Path(command[-1])
            self._write_macos_companion(evidence_dir)
            return subprocess.CompletedProcess(command, 0, stdout="{}\n", stderr="")
        raise AssertionError(f"unexpected release-proof command: {command}")

    def _write_linux_bundle(self, directory: Path, *, corrupt: bool) -> None:
        wheel = directory / "spice_harness-0.26.0-py3-none-any.whl"
        sdist = directory / "spice_harness-0.26.0.tar.gz"
        wheel.write_bytes(b"tested wheel\n")
        sdist.write_bytes(b"tested sdist\n")
        wheel_digest = _test_sha256(wheel)
        sdist_digest = _test_sha256(sdist)
        report = {
            "schema_version": 1,
            "claim_boundary": {
                "operating_system": "linux",
                "host_native_companion": "release-proof-macos.json",
                "host_native_checks": [
                    "kqueue-or-fsevents",
                    "appearance",
                    "speech",
                ],
            },
            "source_identity": {
                "schema_version": 1,
                "source": {
                    "commit": self.commit,
                    "tree": self.tree,
                    "commit_epoch": 1,
                },
                "synthetic": {"commit": "c" * 40, "tree": "d" * 40},
            },
            "artifacts": {
                "wheel": {
                    "filename": wheel.name,
                    "bytes": wheel.stat().st_size,
                    "sha256": "0" * 64 if corrupt else wheel_digest,
                },
                "sdist": {
                    "filename": sdist.name,
                    "bytes": sdist.stat().st_size,
                    "sha256": sdist_digest,
                },
                "installed_wheel_sha256": wheel_digest,
                "sdist_rebuilt_from_sha256": sdist_digest,
            },
        }
        (directory / "release-proof.json").write_text(
            json.dumps(report, sort_keys=True) + "\n",
            encoding="utf-8",
        )

    def _write_macos_companion(self, directory: Path) -> None:
        linux = (directory / "release-proof.json").read_bytes()
        report = {
            "schema_version": 1,
            "claim_boundary": {
                "operating_system": "macos",
                "container_operating_system": "linux",
                "container_evidence_unchanged": True,
            },
            "container_evidence": {
                "filename": "release-proof.json",
                "sha256": hashlib.sha256(linux).hexdigest(),
            },
            "source_identity": {
                "agreement": "exact",
                "checkout_head": self.commit,
                "container_source_commit": self.commit,
            },
            "checks": {
                "kqueue-or-fsevents": {"status": "passed"},
                "appearance": {"status": "passed"},
                "speech": {"status": "passed"},
            },
        }
        (directory / "release-proof-macos.json").write_text(
            json.dumps(report, sort_keys=True) + "\n",
            encoding="utf-8",
        )


def _credential_url() -> str:
    return (
        "https://user:pass@example.test/callback?access_token=query-secret"
        "#token=fragment-secret\n"
    )


def _appliance_paths(tmp_path: Path) -> tuple[Path, Path]:
    root = tmp_path / "repository"
    (root / "scripts").mkdir(parents=True)
    (root / "release-proof").mkdir()
    return root, tmp_path / "proof-output"


def _run_appliance(
    root: Path,
    output: Path,
    runner: _ApplianceRunner,
    *,
    system_name: str = "Linux",
    run_id: str = "0123456789abcdef",
) -> dict[str, object]:
    return APPLIANCE.run_release_proof(
        root,
        runner.engine,
        output,
        command_runner=runner,
        which=lambda name: f"/fake/{name}",
        system_name=system_name,
        run_id=run_id,
        clock=lambda: "2026-07-21T00:00:00Z",
    )


@pytest.mark.parametrize("engine", ["docker", "podman"])
def test_release_proof_appliance_uses_the_portable_exact_engine_lifecycle(
    tmp_path, engine
):
    root, output = _appliance_paths(tmp_path)
    runner = _ApplianceRunner(engine)

    result = _run_appliance(root, output, runner)

    engine_commands = [
        command for command, _cwd, _timeout in runner.calls if command[0] == engine
    ]
    build = engine_commands[1]
    context = build[-1]
    image = build[build.index("--tag") + 1]
    container = engine_commands[2][3]
    copy_destination = engine_commands[3][-1]
    assert engine_commands == [
        [engine, "--version"],
        [
            engine,
            "build",
            "--file",
            f"{context}/release-proof/Containerfile",
            "--tag",
            image,
            context,
        ],
        [engine, "create", "--name", container, image, "artifact-carrier"],
        [engine, "cp", f"{container}:/artifacts/.", copy_destination],
        [engine, "container", "rm", container],
        [engine, "image", "rm", image],
    ]
    assert result["status"] == "passed"
    assert result["engine"] == {
        "name": engine,
        "version": f"{engine} version 99.0",
    }
    assert result["source"] == {"commit": runner.commit, "tree": runner.tree}
    assert _file_inventory(output) == [
        "release-proof.json",
        "spice_harness-0.26.0-py3-none-any.whl",
        "spice_harness-0.26.0.tar.gz",
    ]


def test_release_proof_appliance_object_names_are_run_scoped(tmp_path):
    root, first_output = _appliance_paths(tmp_path)
    first = _ApplianceRunner("docker")
    second = _ApplianceRunner("docker")
    second_output = tmp_path / "proof-output-two"

    _run_appliance(root, first_output, first, run_id="1111111111111111")
    _run_appliance(root, second_output, second, run_id="2222222222222222")

    first_create = next(command for command, *_ in first.calls if "create" in command)
    second_create = next(command for command, *_ in second.calls if "create" in command)
    assert (first_create[3], second_create[3]) == (
        "spice-release-proof-aaaaaaaaaaaa-1111111111111111",
        "spice-release-proof-aaaaaaaaaaaa-2222222222222222",
    )


def test_release_proof_appliance_publishes_redacted_failure_and_exact_cleanup(
    tmp_path, monkeypatch
):
    root, output = _appliance_paths(tmp_path)
    runner = _ApplianceRunner("podman", failure_mode="copy")
    monkeypatch.setenv("UV_PUBLISH_TOKEN", "environment-secret-value")

    result = _run_appliance(root, output, runner)
    report = json.loads(
        (output / "release-proof-failure.json").read_text(encoding="utf-8")
    )
    diagnostic_path = output / report["diagnostics"][0]["filename"]
    diagnostic = diagnostic_path.read_text(encoding="utf-8")

    assert result == report
    assert report["status"] == "failed"
    assert report["phase"] == "engine-copy"
    assert report["exit_code"] == FAKE_COPY_FAILURE_EXIT_CODE
    assert report["cleanup"] == {"container": "removed", "image": "removed"}
    assert _file_inventory(output) == [
        "failures/01-engine-copy.log",
        "release-proof-failure.json",
    ]
    assert "<redacted-env:UV_PUBLISH_TOKEN>" in diagnostic
    assert "https://<redacted>@example.test/callback" in diagnostic
    assert "access_token=%3Credacted%3E" in diagnostic
    assert "#token=%3Credacted%3E" in diagnostic
    assert report["diagnostics"] == [
        {
            "filename": "failures/01-engine-copy.log",
            "bytes": diagnostic_path.stat().st_size,
            "sha256": _test_sha256(diagnostic_path),
        }
    ]


@pytest.mark.parametrize(
    ("failure_mode", "phase", "exit_code"),
    [("deadline", "engine-build", 124), ("signal", "signal", 130)],
)
def test_release_proof_appliance_publishes_bounded_interruption_status(
    tmp_path, failure_mode, phase, exit_code
):
    root, output = _appliance_paths(tmp_path)
    runner = _ApplianceRunner("docker", failure_mode=failure_mode)

    result = _run_appliance(root, output, runner)

    assert result["status"] == "failed"
    assert result["phase"] == phase
    assert result["exit_code"] == exit_code
    assert result["output_published"] is True
    cleanup = cast(dict[str, str], result["cleanup"])
    assert tuple(sorted(cleanup.items())) in (
        (("container", "not-created"), ("image", "not-created")),
        (("container", "removed"), ("image", "removed")),
    )


def test_release_proof_appliance_publishes_digest_validation_evidence(tmp_path):
    root, output = _appliance_paths(tmp_path)
    runner = _ApplianceRunner("docker", failure_mode="digest")

    result = _run_appliance(root, output, runner)

    assert result["status"] == "failed"
    assert result["phase"] == "artifact-validation"
    assert result["cleanup"] == {"container": "removed", "image": "removed"}
    assert _file_inventory(output) == [
        "failures/01-artifact-validation.log",
        "release-proof-failure.json",
    ]


@pytest.mark.parametrize(
    ("failure_mode", "system_name", "phase", "cleanup"),
    [
        (
            "cleanup",
            "Linux",
            "cleanup-container",
            {"container": "failed", "image": "removed"},
        ),
        (
            "host-native",
            "Darwin",
            "host-native",
            {"container": "removed", "image": "removed"},
        ),
    ],
)
def test_release_proof_appliance_publishes_cleanup_and_host_failure_status(
    tmp_path, failure_mode, system_name, phase, cleanup
):
    root, output = _appliance_paths(tmp_path)
    runner = _ApplianceRunner("docker", failure_mode=failure_mode)

    result = _run_appliance(root, output, runner, system_name=system_name)

    assert result["status"] == "failed"
    assert result["phase"] == phase
    assert result["cleanup"] == cleanup
    diagnostics = cast(list[dict[str, object]], result["diagnostics"])
    assert diagnostics[0]["filename"] == f"failures/01-{phase}.log"


def test_release_proof_appliance_runs_and_validates_darwin_companion(tmp_path):
    root, output = _appliance_paths(tmp_path)
    runner = _ApplianceRunner("docker")

    result = _run_appliance(root, output, runner, system_name="Darwin")
    host_command = next(
        command for command, *_ in runner.calls if "hostnative.py" in " ".join(command)
    )
    host = json.loads((output / "release-proof-macos.json").read_text(encoding="utf-8"))

    assert result["status"] == "passed"
    assert result["host_native_companion"] == "release-proof-macos.json"
    assert host_command[1:3] == [
        str(root / "release-proof" / "hostnative.py"),
        "--evidence-dir",
    ]
    assert host["source_identity"] == {
        "agreement": "exact",
        "checkout_head": runner.commit,
        "container_source_commit": runner.commit,
    }
    assert _file_inventory(output) == [
        "release-proof-macos.json",
        "release-proof.json",
        "spice_harness-0.26.0-py3-none-any.whl",
        "spice_harness-0.26.0.tar.gz",
    ]


def test_release_proof_appliance_records_clean_source_preflight_failure(tmp_path):
    root, output = _appliance_paths(tmp_path)
    runner = _ApplianceRunner("docker", failure_mode="dirty")

    result = _run_appliance(root, output, runner)

    assert result["status"] == "failed"
    assert result["phase"] == "source-preflight"
    assert result["cleanup"] == {"container": "not-created", "image": "not-created"}
    assert _file_inventory(output) == [
        "failures/01-source-preflight.log",
        "release-proof-failure.json",
    ]


def test_release_proof_appliance_reports_unsafe_output_without_mutation(tmp_path):
    root, _output = _appliance_paths(tmp_path)
    runner = _ApplianceRunner("docker")

    result = _run_appliance(root, root / "proof-output", runner)

    assert result["status"] == "failed"
    assert result["phase"] == "output-preflight"
    assert result["output_published"] is False
    assert result["diagnostics"] == []
    assert runner.calls == []


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
    assert "FROM scratch AS artifact_carrier" in containerfile
    assert "COPY --from=proof /proof/artifacts/ /artifacts/" in containerfile
    assert containerfile.split("FROM scratch AS artifact_carrier\n", 1)[1] == (
        "\nCOPY --from=proof /proof/artifacts/ /artifacts/\n"
    )
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
    assert (PROJECT_ROOT / "scripts" / "release-proof").stat().st_mode & stat.S_IXUSR


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

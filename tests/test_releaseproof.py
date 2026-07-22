"""Hermetic source and toolchain boundary contracts."""

from __future__ import annotations

import hashlib
import json
import os
import stat
import subprocess
import sys
import zipfile
from pathlib import Path
from typing import cast

import pytest

from tests.test_releaseproofhelpers import (
    CONTAINERFILE,
    EVIDENCE,
    HOSTNATIVE,
    PROJECT_ROOT,
    REHEARSAL,
    SOURCE_EXPORTER,
    SOURCE_INITIALIZER,
    _file_inventory,
    _git,
    _source_repository,
    _test_sha256,
    _write_test_wheel,
)


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
        "packaging-toolchain",
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

    def missing_modules(command, **_kwargs):
        return subprocess.CompletedProcess(
            command, 0, stdout="build.__main__ twine\n", stderr=""
        )

    monkeypatch.setattr(REHEARSAL, "_run", missing_modules)

    with pytest.raises(REHEARSAL.RehearsalError) as failure:
        REHEARSAL.verify_packaging_toolchain(root)

    message = str(failure.value)
    assert (
        "build.__main__, twine failed to import" in message,
        "uv lock" in message,
    ) == (True, True)


def test_packaging_preflight_records_the_toolchain_it_proved(tmp_path, monkeypatch):
    root = tmp_path / "checkout"
    probes: list[tuple[str, ...]] = []

    def present_modules(command, **_kwargs):
        probes.append(tuple(command))
        return subprocess.CompletedProcess(command, 0, stdout="\n", stderr="")

    monkeypatch.setattr(REHEARSAL, "_run", present_modules)

    proven = REHEARSAL.verify_packaging_toolchain(root)

    assert (proven, probes[0][: len(proven)]) == (
        ["build.__main__", "setuptools", "twine", "wheel"],
        REHEARSAL.packaging_python_command(root)[: len(proven)],
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


def _release_receipt_fixture(tmp_path: Path) -> tuple[Path, Path]:
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
        receipt["artifact_rehearsal"]["packaging_modules"],
    ) == (False, list(REHEARSAL.CHECKS), list(REHEARSAL.PACKAGING_MODULES))
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

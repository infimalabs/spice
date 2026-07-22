"""Hermetic source and toolchain boundary contracts."""

from __future__ import annotations

import hashlib
import json
import os
import stat
import subprocess
import sys
from pathlib import Path
from typing import cast

PROJECT_ROOT = Path(__file__).resolve().parent.parent
SOURCE_EXPORTER = PROJECT_ROOT / "scripts" / "release-proof-source"
SOURCE_INITIALIZER = PROJECT_ROOT / "release-proof" / "init-source.py"
CONTAINERFILE = PROJECT_ROOT / "release-proof" / "Containerfile"
TOOLCHAIN_DECLARATION = PROJECT_ROOT / "release-proof" / "toolchain.json"
BASE_IMAGE = "mcr.microsoft.com/playwright:v1.61.0-noble"
BASE_DIGEST = "sha256:57b65fdc9ceabe0ef613124c7bbe2babcf9362c4d85e382fe3b03604e84b428a"


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

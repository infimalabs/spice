"""Shared release-proof fixtures: script handles, source repositories, wheels."""

from __future__ import annotations

import hashlib
import importlib.util
import os
import subprocess
import zipfile
from pathlib import Path
from typing import Any

PROJECT_ROOT = Path(__file__).resolve().parent.parent
SOURCE_EXPORTER = PROJECT_ROOT / "scripts" / "release-proof-source"
SOURCE_INITIALIZER = PROJECT_ROOT / "release-proof" / "init-source.py"
CONTAINERFILE = PROJECT_ROOT / "release-proof" / "Containerfile"
TOOLCHAIN_DECLARATION = PROJECT_ROOT / "release-proof" / "toolchain.json"
REHEARSAL_SCRIPT = PROJECT_ROOT / "release-proof" / "rehearse.py"
UPGRADE_SCRIPT = PROJECT_ROOT / "release-proof" / "upgrade.py"
EVIDENCE_SCRIPT = PROJECT_ROOT / "release-proof" / "evidence.py"
HOSTNATIVE_SCRIPT = PROJECT_ROOT / "release-proof" / "hostnative.py"
APPLIANCE_SCRIPT = PROJECT_ROOT / "release-proof" / "appliance.py"
PINNED_SCRIPT = PROJECT_ROOT / "release-proof" / "pinned.py"
BASE_IMAGE = "mcr.microsoft.com/playwright:v1.61.0-noble"
BASE_DIGEST = "sha256:57b65fdc9ceabe0ef613124c7bbe2babcf9362c4d85e382fe3b03604e84b428a"
FAKE_COPY_FAILURE_EXIT_CODE = 41


def _load_script(script: Path) -> Any:
    spec = importlib.util.spec_from_file_location(
        f"spice_release_proof_{script.stem.replace('-', '_')}", script
    )
    if spec is None or spec.loader is None:
        raise RuntimeError(f"could not load release-proof script: {script}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


REHEARSAL = _load_script(REHEARSAL_SCRIPT)
UPGRADE = _load_script(UPGRADE_SCRIPT)
EVIDENCE = _load_script(EVIDENCE_SCRIPT)
HOSTNATIVE = _load_script(HOSTNATIVE_SCRIPT)
APPLIANCE = _load_script(APPLIANCE_SCRIPT)
PINNED = _load_script(PINNED_SCRIPT)


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


def _file_inventory(root: Path) -> list[str]:
    return sorted(
        path.relative_to(root).as_posix() for path in root.rglob("*") if path.is_file()
    )


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

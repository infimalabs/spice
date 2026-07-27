"""Shared release-proof fixtures: script handles, source repositories, wheels."""

from __future__ import annotations

import hashlib
import importlib.util
import os
import subprocess
import tomllib
import zipfile
from pathlib import Path
from typing import Any

PROJECT_ROOT = Path(__file__).resolve().parent.parent
SOURCE_EXPORTER = PROJECT_ROOT / "scripts" / "release-proof-source"
SOURCE_INITIALIZER = PROJECT_ROOT / "release-proof" / "init-source.py"
CONTAINERFILE = PROJECT_ROOT / "release-proof" / "Containerfile"
TOOLCHAIN_DECLARATION = PROJECT_ROOT / "release-proof" / "toolchain.json"
REHEARSAL_SCRIPT = PROJECT_ROOT / "release-proof" / "rehearse.py"
INSTALLED_UPGRADE_SCRIPT = PROJECT_ROOT / "release-proof" / "installed-upgrade.py"
UPGRADE_SCRIPT = PROJECT_ROOT / "release-proof" / "upgrade.py"
EVIDENCE_SCRIPT = PROJECT_ROOT / "release-proof" / "evidence.py"
HOSTNATIVE_SCRIPT = PROJECT_ROOT / "release-proof" / "hostnative.py"
APPLIANCE_SCRIPT = PROJECT_ROOT / "release-proof" / "appliance.py"
PINNED_SCRIPT = PROJECT_ROOT / "release-proof" / "pinned.py"
BASE_IMAGE = "mcr.microsoft.com/playwright:v1.61.0-noble"
BASE_DIGEST = "sha256:57b65fdc9ceabe0ef613124c7bbe2babcf9362c4d85e382fe3b03604e84b428a"
FAKE_COPY_FAILURE_EXIT_CODE = 41
PROJECTION_SOURCE_PATH = "spice/serve/team/projection.py"


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
INSTALLED_UPGRADE = _load_script(INSTALLED_UPGRADE_SCRIPT)
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


def _release_pyproject(version: str) -> str:
    """Render a hermetic distribution declaring ``version``.

    The exporter builds whatever a tag points at, so the tagged commit has to be
    a real distribution. uv's own backend keeps that hermetic: it ships inside
    the uv binary, so no build requirement is fetched.
    """
    return (
        "[build-system]\n"
        'requires = ["uv_build"]\n'
        'build-backend = "uv_build"\n\n'
        "[project]\n"
        'name = "spice-harness"\n'
        f'version = "{version}"\n\n'
        "[tool.uv.build-backend]\n"
        'module-root = ""\n'
        'module-name = "spice"\n'
    )


def _write_release_tree(repository: Path, version: str) -> None:
    """Lay down a distribution declaring ``version`` with the governed schemas.

    Every store the manifest requires has to be readable at the tag, so a tree
    that omits one is recorded absent and refused long before the tag it names
    is the question under test.
    """
    sources = {
        "pyproject.toml": _release_pyproject(version),
        "spice/__init__.py": "",
        "spice/serve/team/schema.py": 'TEAM_SCHEMA = "CREATE TABLE team_old(id)"\n',
        "spice/mail/ackstate.py": (
            'ACK_STATE_TABLE_SQL = "CREATE TABLE ack_old(id)"\n'
            'ACK_STATE_INDEX_SQL = "CREATE INDEX ack_old_idx ON ack_old(id)"\n'
        ),
        "spice/agent/maximmetrics.py": (
            'MAXIM_METRICS_TABLE_SQL = "CREATE TABLE maxim_metric_events(id)"\n'
            "MAXIM_METRICS_EVENT_INDEX_SQL = "
            '"CREATE INDEX maxim_event ON maxim_metric_events(id)"\n'
            "MAXIM_METRICS_RECURRENCE_INDEX_SQL = "
            '"CREATE INDEX maxim_recurrence ON maxim_metric_events(id)"\n'
            "MAXIM_METRICS_FIRE_RECENCY_INDEX_SQL = "
            '"CREATE INDEX maxim_recency ON maxim_metric_events(id)"\n'
        ),
    }
    for relative, source_text in sources.items():
        path = repository / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(source_text, encoding="utf-8")


def _projection_store_state_at(tag: str) -> str:
    """How the release at ``tag`` leaves the projection store classified.

    Read from that tag's own tree rather than from the manifest, so the
    classification is measured against what the release actually shipped
    instead of against the exporter under test. Projection is the one governed
    store a predecessor may legitimately lack, so which of the two states
    applies changes on whichever release first ships the module -- and pinning
    either literal here would come due as a stale expectation exactly then.
    """
    listed = _git(
        PROJECT_ROOT, "ls-tree", "-r", "--name-only", tag, "--", PROJECTION_SOURCE_PATH
    ).split()
    return "source" if PROJECTION_SOURCE_PATH in listed else "absent"


def _release_this_checkout_upgrades_from() -> str:
    """The newest release tag this checkout carries below the version it ships.

    Read from git and the project metadata rather than from the exporter, so
    the rehearsal is measured against the repository instead of against itself.
    """
    declared = tomllib.loads(
        (PROJECT_ROOT / "pyproject.toml").read_text(encoding="utf-8")
    )["project"]["version"]
    ordering = {}
    for tag in _git(
        PROJECT_ROOT, "tag", "--list", "v[0-9]*", "--merged", "HEAD"
    ).split():
        fields = tag[1:].split(".")
        if all(field.isdigit() for field in fields):
            ordering[tag] = tuple(int(field) for field in fields)
    shipping = tuple(int(field) for field in declared.split("."))
    return max(
        (tag for tag, order in ordering.items() if order < shipping),
        key=ordering.__getitem__,
    )

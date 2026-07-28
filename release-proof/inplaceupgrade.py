"""Installed predecessor-to-current upgrade proof for release rehearsal."""

from __future__ import annotations

import json
import re
import sqlite3
import sys
from pathlib import Path
from typing import NamedTuple

from evidence import FailureArtifactStore
from rehearsalcommon import (
    RehearsalError,
    isolated_environment as _isolated_environment,
    run as _run,
    sha256 as _sha256,
)
import upgrade as upgrade_proof

SCHEMA_VERSION = 1
SHA256_HEX_LENGTH = 64
PRIOR_ARTIFACT_DIRECTORY = Path(".release-proof") / "prior-artifact"
PRIOR_ARTIFACT_MANIFEST = PRIOR_ARTIFACT_DIRECTORY / "manifest.json"
UPGRADE_INSTALLED_PROBE = Path("release-proof") / "installed-upgrade.py"
UPGRADE_SEED_PROJECT = "task.upgradeproof"
# Task creation requires provenance. Nothing steered these throwaway rows, so
# they carry the key of the task that introduced this gate.
UPGRADE_SEED_ORIGIN = "ack:1kH7dz6P"
UPGRADE_TEAM_STORE = "spiceteams.sqlite3"
# Every store is tied to the proof appropriate to its storage class. Authority
# facts survive in place; the projection is rebuilt from its native source.
UPGRADE_AUTHORITY_STATE = (
    "spiceacks.sqlite3",
    "spicemaxims.sqlite3",
    "spiceteams.sqlite3",
)
UPGRADE_PROJECTION_STATE = ("spiceprojections.sqlite3",)
UPGRADE_GOVERNED_STATE = UPGRADE_AUTHORITY_STATE + UPGRADE_PROJECTION_STATE
UPGRADE_REHEARSAL_BY_STORE = {
    **dict.fromkeys(UPGRADE_AUTHORITY_STATE, "preserve-authority"),
    **dict.fromkeys(UPGRADE_PROJECTION_STATE, "rebuild-projection"),
}
# state_5.sqlite is an agent driver's own home file and taskchampion.sqlite3 is
# Taskwarrior-owned and opened read-only; both are excluded on the record.
UPGRADE_EXCLUSION_REASONS = {
    "state_5.sqlite": "the Codex driver's foreign home database",
    "taskchampion.sqlite3": "Taskwarrior-owned task authority and operation history",
}
UPGRADE_EXCLUDED_STATE = tuple(UPGRADE_EXCLUSION_REASONS)
# Source names each durable store in a constant beside the code that opens it.
# Reading them back is what makes the inventory above answerable to the tree
# under proof instead of to whichever files happened to appear during a run.
UPGRADE_STATE_DECLARATION = re.compile(
    r'^[A-Z][A-Z0-9_]*(?:_DB|_DATABASE)_FILENAME\s*=\s*"([^"]+\.sqlite3)"',
    re.MULTILINE,
)
GIT_OBJECT_ID = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})")


def _carried_predecessor(root: Path) -> Path:
    """Resolve and authenticate the predecessor wheel carried beside source."""
    manifest_path = root / PRIOR_ARTIFACT_MANIFEST
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise RehearsalError(
            f"could not read the carried predecessor manifest {manifest_path}: {exc}"
        ) from exc
    if not isinstance(manifest, dict):
        raise RehearsalError(
            "the carried predecessor manifest is not an object; "
            f"{manifest_path} holds {type(manifest).__name__}"
        )
    if set(manifest) != {"schema_version", "release", "state", "wheel"}:
        raise RehearsalError(
            f"the carried predecessor manifest has an invalid shape: {manifest_path}"
        )
    if manifest.get("schema_version") != SCHEMA_VERSION:
        raise RehearsalError(
            f"the carried predecessor manifest has an unsupported schema: "
            f"{manifest_path}"
        )
    if manifest.get("state") != "built":
        raise RehearsalError(
            "the in-place upgrade proof needs a built predecessor artifact; "
            f"{manifest_path} records state={manifest.get('state')!r}"
        )
    release = manifest.get("release")
    if (
        not isinstance(release, dict)
        or set(release) != {"tag", "commit"}
        or not isinstance(release.get("tag"), str)
        or not str(release["tag"]).startswith("v")
        or not isinstance(release.get("commit"), str)
        or GIT_OBJECT_ID.fullmatch(str(release["commit"])) is None
    ):
        raise RehearsalError(
            "the carried predecessor manifest names no exact release identity; "
            f"{manifest_path} records release={release!r}"
        )
    entry = manifest.get("wheel")
    name = entry.get("name") if isinstance(entry, dict) else None
    if (
        not isinstance(entry, dict)
        or set(entry) != {"name", "sha256"}
        or not isinstance(name, str)
        or not name
        or name != Path(name).name
    ):
        raise RehearsalError(
            "the carried predecessor manifest claims a built artifact but names "
            f"no wheel file beside itself; {manifest_path} records wheel={entry!r}"
        )
    wheel = root / PRIOR_ARTIFACT_DIRECTORY / name
    if not wheel.is_file():
        raise RehearsalError(f"carried predecessor wheel is missing: {wheel}")
    recorded_sha256 = entry.get("sha256")
    resolved_sha256 = _sha256(wheel)
    if (
        not isinstance(recorded_sha256, str)
        or len(recorded_sha256) != SHA256_HEX_LENGTH
        or resolved_sha256 != recorded_sha256
    ):
        raise RehearsalError(
            "carried predecessor wheel does not match its manifest: "
            f"{wheel} records {recorded_sha256!r}, resolved {resolved_sha256}"
        )
    return wheel


def _resolve_predecessor(root: Path, scratch: Path) -> Path:
    """Use the container carry or derive the same bounded wheel for host proof."""
    manifest_path = root / PRIOR_ARTIFACT_MANIFEST
    if manifest_path.exists() or manifest_path.is_symlink():
        return _carried_predecessor(root)
    derived_root = scratch / "host-predecessor"
    try:
        upgrade_proof.export_prior_artifact(
            root, derived_root / PRIOR_ARTIFACT_DIRECTORY
        )
    except (OSError, RuntimeError, ValueError) as exc:
        raise RehearsalError(
            f"could not derive the host predecessor artifact: {exc}"
        ) from exc
    return _carried_predecessor(derived_root)


def _assert_state_inventory_is_declared(
    root: Path,
    *,
    rehearsal_by_store: dict[str, str] = UPGRADE_REHEARSAL_BY_STORE,
    governed_state: tuple[str, ...] = UPGRADE_GOVERNED_STATE,
    excluded_state: tuple[str, ...] = UPGRADE_EXCLUDED_STATE,
) -> list[str]:
    """Answer the recorded inventory to the stores current source opens."""
    package = root / "spice"
    declared: set[str] = set()
    for module in sorted(package.rglob("*.py")):
        declared.update(
            UPGRADE_STATE_DECLARATION.findall(module.read_text(encoding="utf-8"))
        )
    if not declared:
        raise RehearsalError(
            f"no durable store filename is declared under {package}; the in-place "
            "upgrade inventory has nothing to answer to"
        )
    registered = set(rehearsal_by_store)
    governed = set(governed_state)
    if registered != governed:
        raise RehearsalError(
            "the in-place upgrade rehearsal registry drifted from its governed "
            f"stores: governed={sorted(governed)!r} registered={sorted(registered)!r}"
        )
    missing_governed = sorted(governed.difference(declared))
    if missing_governed:
        raise RehearsalError(
            "the in-place upgrade proof governs stores current source no longer "
            f"declares: {', '.join(missing_governed)}"
        )
    unrecorded = sorted(declared.difference(governed).difference(excluded_state))
    if unrecorded:
        raise RehearsalError(
            "current source opens durable stores the in-place upgrade proof does "
            f"not account for: {', '.join(unrecorded)}; rehearse each one or "
            "record it as an exclusion with its reason"
        )
    return sorted(declared)


def _seed_repository(scratch: Path, failures: FailureArtifactStore | None) -> Path:
    """Create the scratch repository whose Git directory anchors proof state."""
    repository = scratch / "upgrade-state"
    repository.mkdir()
    (repository / "seed.txt").write_text("in-place upgrade proof\n", encoding="utf-8")
    for argv in (
        ["git", "init", "--quiet", "--initial-branch=upgrade-proof"],
        ["git", "config", "user.email", "release-proof@spice.invalid"],
        ["git", "config", "user.name", "Spice Release Proof"],
        ["git", "config", "commit.gpgsign", "false"],
        ["git", "add", "seed.txt"],
        ["git", "commit", "--quiet", "--message", "upgrade proof seed"],
    ):
        _run(argv, cwd=repository, failures=failures, gate="in-place-upgrade")
    return repository


def _team_store_version(path: Path) -> int:
    connection = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    try:
        return int(connection.execute("PRAGMA user_version").fetchone()[0])
    finally:
        connection.close()


def _run_installed_probe(
    python: Path,
    root: Path,
    repository: Path,
    action: str,
    environment: dict[str, str],
    failures: FailureArtifactStore | None,
) -> dict[str, object]:
    """Run the source-independent probe through the wheel's isolated Python."""
    completed = _run(
        [
            str(python),
            "-I",
            str(root / UPGRADE_INSTALLED_PROBE),
            action,
            "--repository",
            str(repository),
        ],
        cwd=repository,
        capture=True,
        env=environment,
        failures=failures,
        gate="in-place-upgrade",
    )
    try:
        payload = json.loads(completed.stdout)
    except json.JSONDecodeError as exc:
        raise RehearsalError(
            f"installed upgrade probe {action} returned invalid JSON: {exc}"
        ) from exc
    if not isinstance(payload, dict):
        raise RehearsalError(
            f"installed upgrade probe {action} returned {type(payload).__name__}"
        )
    return payload


def _validated_installed_paths(
    report: object,
    *,
    expected: set[str],
    root: Path,
    repository: Path,
    venv: Path,
) -> dict[str, Path]:
    """Validate the exact paths and import origin reported by the installed wheel."""
    if not isinstance(report, dict) or set(report) != {"import_origin", "paths"}:
        raise RehearsalError("installed upgrade path report has an invalid shape")
    raw_paths = report["paths"]
    if not isinstance(raw_paths, dict) or set(raw_paths) != expected:
        resolved = sorted(raw_paths) if isinstance(raw_paths, dict) else raw_paths
        raise RehearsalError(
            "installed upgrade path inventory drifted: "
            f"expected={sorted(expected)!r} resolved={resolved!r}"
        )
    source_root = root.resolve()
    installed_root = venv.resolve()
    import_origin = Path(str(report["import_origin"])).resolve()
    if source_root == import_origin or source_root in import_origin.parents:
        raise RehearsalError(
            f"installed upgrade probe imported Spice from source: {import_origin}"
        )
    if installed_root not in import_origin.parents:
        raise RehearsalError(
            f"installed upgrade probe did not import from its venv: {import_origin}"
        )
    repository_root = repository.resolve()
    paths: dict[str, Path] = {}
    for name, value in raw_paths.items():
        resolved = Path(str(value)).resolve()
        if repository_root != resolved and repository_root not in resolved.parents:
            raise RehearsalError(
                f"installed {name} path escaped the scratch repository: {resolved}"
            )
        paths[str(name)] = resolved
    return paths


def _assert_rehearsed_state_files(
    paths: dict[str, Path], *, required: set[str]
) -> list[str]:
    """Require every rehearsed store and reject unregistered SQLite siblings."""
    missing = sorted(
        name for name in required if name not in paths or not paths[name].is_file()
    )
    if missing:
        raise RehearsalError(
            "the installed upgrade proof did not exercise required stores: "
            + ", ".join(missing)
        )
    allowed = set(UPGRADE_GOVERNED_STATE) | set(UPGRADE_EXCLUDED_STATE)
    parents = {paths[name].parent for name in required}
    inventory: set[str] = set()
    for parent in parents:
        if not parent.is_dir():
            continue
        for item in parent.glob("*.sqlite*"):
            name = item.name
            for suffix in ("-journal", "-shm", "-wal"):
                if name.endswith(suffix):
                    name = name[: -len(suffix)]
                    break
            inventory.add(name)
    unrecorded = sorted(inventory.difference(allowed))
    if unrecorded:
        raise RehearsalError(
            "in-place upgrade produced unrecorded state files: " + ", ".join(unrecorded)
        )
    return sorted(inventory)


def _store_identities(
    paths: dict[str, Path], names: tuple[str, ...]
) -> dict[str, tuple[int, int]]:
    identities = {}
    for name in names:
        try:
            status = paths[name].stat()
        except OSError as exc:
            raise RehearsalError(
                f"could not identify authority store {name}: {exc}"
            ) from exc
        identities[name] = (status.st_dev, status.st_ino)
    return identities


def _store_versions(paths: dict[str, Path], names: tuple[str, ...]) -> dict[str, int]:
    return {name: _team_store_version(paths[name]) for name in names}


def _authority_facts(payload: object, *, written: bool) -> dict[str, dict[str, object]]:
    if not isinstance(payload, dict) or set(payload) != set(UPGRADE_AUTHORITY_STATE):
        resolved = sorted(payload) if isinstance(payload, dict) else payload
        raise RehearsalError(
            "installed authority evidence drifted: "
            f"expected={sorted(UPGRADE_AUTHORITY_STATE)!r} resolved={resolved!r}"
        )
    facts: dict[str, dict[str, object]] = {}
    required = {"preserved", "written"} if written else {"preserved"}
    for name, raw in payload.items():
        if not isinstance(raw, dict) or set(raw) != required:
            raise RehearsalError(f"installed authority evidence is invalid for {name}")
        if not isinstance(raw["preserved"], list) or not raw["preserved"]:
            raise RehearsalError(f"installed authority seed is empty for {name}")
        if written and (not isinstance(raw["written"], list) or not raw["written"]):
            raise RehearsalError(f"installed authority write is empty for {name}")
        facts[str(name)] = raw
    return facts


def _assert_authority_preserved(
    seeded: dict[str, dict[str, object]],
    upgraded: dict[str, dict[str, object]],
    before_identity: dict[str, tuple[int, int]],
    after_identity: dict[str, tuple[int, int]],
) -> None:
    for name in UPGRADE_AUTHORITY_STATE:
        if seeded[name]["preserved"] != upgraded[name]["preserved"]:
            raise RehearsalError(f"the upgraded install lost authority facts in {name}")
        if before_identity[name] != after_identity[name]:
            raise RehearsalError(
                f"the upgraded install deleted or reinitialized authority store {name}"
            )


def _projection_evidence(payload: object) -> dict[str, object]:
    if not isinstance(payload, dict) or set(payload) != set(UPGRADE_PROJECTION_STATE):
        raise RehearsalError("installed projection rehearsal evidence drifted")
    evidence = payload[UPGRADE_PROJECTION_STATE[0]]
    if not isinstance(evidence, dict) or set(evidence) != {
        "families",
        "generations",
        "statuses",
    }:
        raise RehearsalError("installed projection rehearsal evidence is invalid")
    families = evidence["families"]
    statuses = evidence["statuses"]
    if not isinstance(families, list) or not families:
        raise RehearsalError("projection rebuild exercised no registered family")
    if statuses != ["ready"] * len(families):
        raise RehearsalError(
            f"projection rebuild did not publish ready families: {statuses!r}"
        )
    return evidence


def _task_handles(text: str) -> list[str]:
    return sorted(set(re.findall(r"\b[A-Z]+-[0-9A-Za-z]{8}\b", text)))


class _UpgradeSeed(NamedTuple):
    venv: Path
    python: Path
    console: Path
    repository: Path
    environment: dict[str, str]
    facts: dict[str, dict[str, object]]
    paths: dict[str, Path]
    identities: dict[str, tuple[int, int]]
    versions: dict[str, int]
    tasks: list[str]


class _UpgradeWrites(NamedTuple):
    paths: dict[str, Path]
    facts: dict[str, dict[str, object]]
    tasks: list[str]


def _install_predecessor_runtime(
    predecessor: Path,
    scratch: Path,
    failures: FailureArtifactStore | None,
) -> tuple[Path, Path, Path, Path, dict[str, str]]:
    venv = scratch / "upgrade-venv"
    python = venv / "bin" / "python"
    console = venv / "bin" / "spice"
    environment = _isolated_environment()
    environment.pop("SPICE_TASK_BACKEND", None)  # env-policy: allow
    _run(
        [sys.executable, "-m", "venv", str(venv)],
        cwd=scratch,
        failures=failures,
        gate="in-place-upgrade",
    )
    _run(
        ["uv", "pip", "install", "--python", str(python), str(predecessor)],
        cwd=scratch,
        failures=failures,
        gate="in-place-upgrade",
    )
    repository = _seed_repository(scratch, failures)
    private_home = repository / ".git" / ".spice" / "proof-home"
    private_home.mkdir(parents=True)
    environment["HOME"] = str(private_home)
    environment["CODEX_HOME"] = str(private_home / "codex")
    return venv, python, console, repository, environment


def _add_upgrade_task(
    console: Path,
    repository: Path,
    environment: dict[str, str],
    description: str,
    failures: FailureArtifactStore | None,
) -> list[str]:
    output = _run(
        [
            str(console),
            "task",
            "add",
            "--project",
            UPGRADE_SEED_PROJECT,
            "--origin",
            UPGRADE_SEED_ORIGIN,
            description,
        ],
        cwd=repository,
        capture=True,
        env=environment,
        failures=failures,
        gate="in-place-upgrade",
    ).stdout
    handles = _task_handles(output)
    if not handles:
        raise RehearsalError(f"the installed package wrote no task for {description}")
    return handles


def _require_upgrade_tasks(
    seed: _UpgradeSeed,
    handles: list[str],
    *,
    phase: str,
    failures: FailureArtifactStore | None,
) -> None:
    visible = _run(
        [str(seed.console), "task", "list"],
        cwd=seed.repository,
        capture=True,
        env=seed.environment,
        failures=failures,
        gate="in-place-upgrade",
    ).stdout
    missing = [handle for handle in handles if handle not in visible]
    if missing:
        raise RehearsalError(
            f"the {phase} install lost task authority in taskchampion.sqlite3: "
            + ", ".join(missing)
        )


def _seed_predecessor_install(
    root: Path,
    predecessor: Path,
    scratch: Path,
    failures: FailureArtifactStore | None,
) -> _UpgradeSeed:
    venv, python, console, repository, environment = _install_predecessor_runtime(
        predecessor, scratch, failures
    )
    seeded_payload = _run_installed_probe(
        python,
        root,
        repository,
        "seed-authority",
        environment,
        failures,
    )
    if set(seeded_payload) != {"facts", "paths"}:
        raise RehearsalError("installed authority seed evidence has an invalid shape")
    seeded_facts = _authority_facts(seeded_payload["facts"], written=False)
    prior_expected = set(UPGRADE_AUTHORITY_STATE) | set(UPGRADE_EXCLUDED_STATE)
    prior_paths = _validated_installed_paths(
        seeded_payload["paths"],
        expected=prior_expected,
        root=root,
        repository=repository,
        venv=venv,
    )
    tasks = _add_upgrade_task(
        console,
        repository,
        environment,
        "Seed state before the in-place upgrade",
        failures,
    )
    before_required = set(UPGRADE_AUTHORITY_STATE) | {"taskchampion.sqlite3"}
    _assert_rehearsed_state_files(prior_paths, required=before_required)
    return _UpgradeSeed(
        venv=venv,
        python=python,
        console=console,
        repository=repository,
        environment=environment,
        facts=seeded_facts,
        paths=prior_paths,
        identities=_store_identities(prior_paths, UPGRADE_AUTHORITY_STATE),
        versions=_store_versions(prior_paths, UPGRADE_AUTHORITY_STATE),
        tasks=tasks,
    )


def _upgrade_installed_package(
    root: Path,
    wheel: Path,
    scratch: Path,
    seed: _UpgradeSeed,
    failures: FailureArtifactStore | None,
    *,
    run=_run,
    run_installed_probe=_run_installed_probe,
    validated_installed_paths=_validated_installed_paths,
) -> _UpgradeWrites:
    run(
        [
            "uv",
            "pip",
            "install",
            "--reinstall",
            "--python",
            str(seed.python),
            str(wheel),
        ],
        cwd=scratch,
        failures=failures,
        gate="in-place-upgrade",
    )
    current_paths_payload = run_installed_probe(
        seed.python,
        root,
        seed.repository,
        "paths",
        seed.environment,
        failures,
    )
    current_expected = set(UPGRADE_GOVERNED_STATE) | set(UPGRADE_EXCLUDED_STATE)
    current_paths = validated_installed_paths(
        current_paths_payload,
        expected=current_expected,
        root=root,
        repository=seed.repository,
        venv=seed.venv,
    )
    for name, prior_path in seed.paths.items():
        if current_paths[name] != prior_path:
            raise RehearsalError(
                f"installed path changed across the upgrade for {name}: "
                f"{prior_path} -> {current_paths[name]}"
            )
    upgraded_facts = _authority_facts(
        run_installed_probe(
            seed.python,
            root,
            seed.repository,
            "verify-authority",
            seed.environment,
            failures,
        ),
        written=True,
    )
    _require_upgrade_tasks(seed, seed.tasks, phase="upgraded", failures=failures)
    tasks = _add_upgrade_task(
        seed.console,
        seed.repository,
        seed.environment,
        "Write state after the in-place upgrade",
        failures,
    )
    return _UpgradeWrites(paths=current_paths, facts=upgraded_facts, tasks=tasks)


def _finalize_in_place_upgrade(
    root: Path,
    seed: _UpgradeSeed,
    writes: _UpgradeWrites,
    failures: FailureArtifactStore | None,
) -> dict[str, object]:
    _run(
        [str(seed.console), "serve", "rebuild-projections"],
        cwd=seed.repository,
        env=seed.environment,
        failures=failures,
        gate="in-place-upgrade",
    )
    projection = _projection_evidence(
        _run_installed_probe(
            seed.python,
            root,
            seed.repository,
            "projection",
            seed.environment,
            failures,
        )
    )
    after_required = set(UPGRADE_GOVERNED_STATE) | {"taskchampion.sqlite3"}
    inventory = _assert_rehearsed_state_files(writes.paths, required=after_required)
    after_identity = _store_identities(writes.paths, UPGRADE_AUTHORITY_STATE)
    _assert_authority_preserved(
        seed.facts,
        writes.facts,
        seed.identities,
        after_identity,
    )
    after_versions = _store_versions(writes.paths, UPGRADE_AUTHORITY_STATE)
    unchanged_versions = [
        name
        for name in UPGRADE_AUTHORITY_STATE
        if seed.versions[name] == after_versions[name]
    ]
    if unchanged_versions:
        raise RehearsalError(
            "the upgraded install ran no forward authority migration for "
            + ", ".join(unchanged_versions)
        )
    _require_upgrade_tasks(
        seed,
        [*seed.tasks, *writes.tasks],
        phase="final upgraded",
        failures=failures,
    )
    return {
        "adopted": {
            "from": seed.versions[UPGRADE_TEAM_STORE],
            "to": after_versions[UPGRADE_TEAM_STORE],
            "store": UPGRADE_TEAM_STORE,
        },
        "authority": writes.facts,
        "authorityVersions": {
            name: {"from": seed.versions[name], "to": after_versions[name]}
            for name in UPGRADE_AUTHORITY_STATE
        },
        "excluded": list(UPGRADE_EXCLUDED_STATE),
        "exclusionReasons": UPGRADE_EXCLUSION_REASONS,
        "paths": {name: str(path) for name, path in writes.paths.items()},
        "preserved": seed.tasks,
        "projection": projection,
        "rehearsals": UPGRADE_REHEARSAL_BY_STORE,
        "state": inventory,
        "written": writes.tasks,
    }

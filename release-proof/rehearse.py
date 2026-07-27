#!/usr/bin/env python3
"""Execute the hermetic release gates and retain the exact proved artifacts."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shlex
import shutil
import sqlite3
import stat
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import zipfile
from pathlib import Path
from typing import Any, NamedTuple

SCRIPT_DIRECTORY = Path(__file__).resolve().parent
if str(SCRIPT_DIRECTORY) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIRECTORY))

from evidence import (  # noqa: E402
    FAILURE_DIRNAME,
    FailureArtifactStore,
    failure_policy_payload,
    parse_pytest_counts,
    redact_text,
)

SCHEMA_VERSION = 1
EXIT_FAILURE = 2
HASH_CHUNK_BYTES = 1024 * 1024
PYTHON_GATE_COMMAND = ("uv", "run", "--locked", "pytest")
RUFF_GATE_COMMAND = ("uv", "run", "--locked", "ruff", "check", ".")
PRIOR_UPGRADE_GATE_COMMAND = (
    "uv",
    "run",
    "--locked",
    "python",
    "release-proof/upgrade.py",
    "rehearse",
    "--root",
    ".",
)
TOOLCHAIN_DECLARATION_PATH = "release-proof/toolchain.json"
BROWSER_GATE_COMMAND = ("node", "tests/browser/run_release_smokes.js")
MUTATION_GATE_COMMAND = (
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
# Import target -> distribution whose pin the toolchain declaration carries.
# `build` keeps its command line in a submodule, so the probe imports
# `build.__main__` while the version is read against the distribution name.
PACKAGING_TOOLCHAIN = {
    "build.__main__": "build",
    "setuptools": "setuptools",
    "twine": "twine",
    "wheel": "wheel",
}
GIT_PRIVATE_RECORD_PRODUCERS = {
    "release-proof-identities.json": "release-proof/init-source.py",
    "release-proof-toolchain.json": "release-proof/toolchain.py",
}
PLAYWRIGHT_CONFIG_ENV = "SPICE_PLAYWRIGHT_MCP_CONFIG"  # env-policy: allow
RECEIPT_NAME = "release-proof.json"
BROWSER_REPORT_NAME = "browser-scenarios.json"
MUTATION_COUNT_FIELDS = ("killed", "mutants", "score", "survived", "timed_out")
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
CHECKS = (
    "packaging-toolchain",
    "python",
    "ruff",
    "prior-store-upgrades",
    "browser-release-manifest",
    "deterministic-mutation-cohort",
    "build-sdist",
    "build-wheel",
    "metadata",
    "isolated-install",
    "installed-imports",
    "installed-console",
    "in-place-upgrade",
    "sdist-rebuild",
    "wheel-member-content",
    "clean-worktree",
)


class RehearsalError(RuntimeError):
    """The release proof could not establish one required invariant."""


def _run(
    command: list[str] | tuple[str, ...],
    *,
    cwd: Path,
    capture: bool = False,
    env: dict[str, str] | None = None,
    failures: FailureArtifactStore | None = None,
    gate: str = "command",
) -> subprocess.CompletedProcess[str]:
    argv = [str(part) for part in command]
    print(f"+ {shlex.join(argv)}", flush=True)
    completed = subprocess.run(
        argv,
        check=False,
        cwd=cwd,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if completed.returncode != 0:
        environment = (
            env if env is not None else dict(os.environ)  # env-policy: allow
        )
        diagnostic = (
            failures.record(
                gate,
                argv,
                completed.returncode,
                completed.stdout,
                completed.stderr,
                environment=environment,
            )
            if failures is not None
            else None
        )
        safe_stdout = redact_text(completed.stdout, environment).strip()
        safe_stderr = redact_text(completed.stderr, environment).strip()
        if safe_stdout:
            print(safe_stdout, file=sys.stderr)
        if safe_stderr:
            print(safe_stderr, file=sys.stderr)
        suffix = f"; diagnostic={diagnostic}" if diagnostic is not None else ""
        raise RehearsalError(
            f"{gate} failed with exit code {completed.returncode}{suffix}"
        )
    if not capture and completed.stdout:
        print(completed.stdout, end="")
    if completed.stderr:
        print(completed.stderr, end="", file=sys.stderr)
    return completed


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(HASH_CHUNK_BYTES), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _project_version(root: Path) -> str:
    project = tomllib.loads((root / "pyproject.toml").read_text(encoding="utf-8"))[
        "project"
    ]
    return str(project["version"])


def _playwright_config(root: Path, failures: FailureArtifactStore) -> Path:
    expression = (
        "from pathlib import Path; "
        "from spice.agent.driver import write_playwright_mcp_config; "
        "print(write_playwright_mcp_config(Path.cwd()))"
    )
    result = _run(
        ["uv", "run", "--locked", "python", "-c", expression],
        cwd=root,
        capture=True,
        failures=failures,
        gate="browser-config",
    )
    path = Path(result.stdout.strip())
    if not path.is_file():
        raise RehearsalError(f"Playwright config was not materialized: {path}")
    return path


def _run_source_gates(
    root: Path,
    scratch: Path,
    failures: FailureArtifactStore,
) -> dict[str, object]:
    python = _run(
        PYTHON_GATE_COMMAND,
        cwd=root,
        capture=True,
        failures=failures,
        gate="python",
    )
    try:
        python_counts = parse_pytest_counts(python.stdout)
    except ValueError as exc:
        raise RehearsalError(str(exc)) from exc
    print(python.stdout, end="")
    _run(RUFF_GATE_COMMAND, cwd=root, failures=failures, gate="ruff")
    upgrades = _run(
        PRIOR_UPGRADE_GATE_COMMAND,
        cwd=root,
        capture=True,
        failures=failures,
        gate="prior-store-upgrades",
    )
    browser_env = dict(os.environ)  # env-policy: allow
    browser_env[PLAYWRIGHT_CONFIG_ENV] = str(_playwright_config(root, failures))
    browser_report_path = scratch / BROWSER_REPORT_NAME
    browser_env["SPICE_RELEASE_BROWSER_REPORT"] = str(  # env-policy: allow
        browser_report_path
    )
    _run(
        BROWSER_GATE_COMMAND,
        cwd=root,
        env=browser_env,
        failures=failures,
        gate="browser",
    )
    browser = _load_browser_report(browser_report_path)
    mutation = _run(
        MUTATION_GATE_COMMAND,
        cwd=root,
        capture=True,
        failures=failures,
        gate="mutation",
    )
    return {
        "python": python_counts,
        "ruff": {"passed": True},
        "upgrades": _load_upgrade_report(upgrades.stdout),
        "browser": browser,
        "mutation": verify_mutation_cohort(root, mutation.stdout),
    }


def _load_upgrade_report(output: str) -> dict[str, object]:
    try:
        payload = json.loads(output)
    except json.JSONDecodeError as exc:
        raise RehearsalError("prior-store upgrade proof returned invalid JSON") from exc
    stores = payload.get("stores") if isinstance(payload, dict) else None
    if (
        not isinstance(payload, dict)
        or payload.get("schema_version") != 1
        or not isinstance(stores, dict)
        or set(stores) != {"team", "ack", "maxim-metrics", "projection"}
    ):
        raise RehearsalError("prior-store upgrade proof returned incomplete evidence")
    return payload


def _load_browser_report(path: Path) -> dict[str, object]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise RehearsalError(
            f"could not read browser scenario report {path}: {exc}"
        ) from exc
    if not isinstance(payload, dict) or payload.get("schemaVersion") != 1:
        raise RehearsalError(f"invalid browser scenario report: {path}")
    counts = payload.get("counts")
    scenarios = payload.get("scenarios")
    external = payload.get("externalState")
    if not isinstance(counts, dict) or not isinstance(scenarios, list):
        raise RehearsalError(f"incomplete browser scenario report: {path}")
    if not isinstance(external, list) or counts.get("failed") != 0:
        raise RehearsalError(f"browser scenario report records failures: {path}")
    return payload


def verify_mutation_cohort(root: Path, output: str) -> dict[str, dict[str, object]]:
    payload = json.loads(output)
    reports = payload.get("reports")
    if not isinstance(reports, list):
        raise RehearsalError("mutation proof did not return a reports list")
    actual: dict[str, dict[str, object]] = {}
    for raw in reports:
        if not isinstance(raw, dict) or not isinstance(raw.get("path"), str):
            raise RehearsalError("mutation proof returned an invalid report")
        actual[str(raw["path"])] = {
            field: raw.get(field) for field in MUTATION_COUNT_FIELDS
        }
    ratchet = json.loads(
        (root / "tests/mutation-ratchet.json").read_text(encoding="utf-8")
    )
    modules = ratchet.get("modules")
    if not isinstance(modules, dict):
        raise RehearsalError("mutation ratchet has no modules map")
    expected = {
        str(path): {field: values.get(field) for field in MUTATION_COUNT_FIELDS}
        for path, values in modules.items()
        if isinstance(values, dict)
    }
    regressions = payload.get("ratchetRegressions")
    if (actual, regressions) != (expected, []):
        raise RehearsalError(
            "deterministic mutation cohort differs from its standing ratchet:\n"
            + json.dumps(
                {
                    "actual": actual,
                    "expected": expected,
                    "ratchetRegressions": regressions,
                },
                indent=2,
                sort_keys=True,
            )
        )
    return actual


def _materialize_committed_source(root: Path, scratch: Path) -> Path:
    archive = scratch / "source.tar"
    source = scratch / "source"
    _run(
        [
            "git",
            "archive",
            "--format=tar",
            "--output",
            str(archive),
            "HEAD",
        ],
        cwd=root,
    )
    source.mkdir()
    with tarfile.open(archive, "r:") as source_archive:
        source_archive.extractall(source, filter="data")
    return source


def packaging_python_command(project_root: Path) -> tuple[str, ...]:
    """Drive packaging tools from the same locked toolchain as every other gate.

    ``-P`` keeps the working directory off ``sys.path`` so a stray ``build``
    directory beside the invocation cannot shadow the real distribution.
    """
    return ("uv", "run", "--locked", "--project", str(project_root), "python", "-P")


def declared_packaging_pins(project_root: Path) -> dict[str, str]:
    """Read the packaging versions the toolchain declaration pins."""
    declaration = json.loads(
        (project_root / TOOLCHAIN_DECLARATION_PATH).read_text(encoding="utf-8")
    )
    pinned = declaration["pinned"]
    return {name: str(pinned[name]) for name in PACKAGING_TOOLCHAIN.values()}


def verify_packaging_toolchain(
    project_root: Path,
    failures: FailureArtifactStore | None = None,
) -> dict[str, str]:
    """Fail before the long gates when the packaging toolchain is unusable.

    Returns the resolved versions so the caller can record what it proved. Each
    module is probed separately and reported by name, because a mid-run
    ``No module named build`` after the suite, browser, and mutation gates
    costs several minutes and hides which dependency is actually absent. The
    resolved versions are then held against the toolchain declaration, so a
    host run and a container run cannot quietly build their artifacts from
    different packaging tools.
    """
    if shutil.which("uv") is None:
        raise RehearsalError(
            "the release rehearsal drives its packaging toolchain through uv, "
            "which is not on PATH; install uv and re-run so the artifact chain "
            "uses the same locked toolchain as every other gate."
        )
    probe = (
        "import importlib, json\n"
        "from importlib.metadata import PackageNotFoundError, version\n"
        f"targets = {dict(PACKAGING_TOOLCHAIN)!r}\n"
        "missing = []\n"
        "resolved = {}\n"
        "for module, distribution in targets.items():\n"
        "    try:\n"
        "        importlib.import_module(module)\n"
        "        resolved[distribution] = version(distribution)\n"
        "    except (ImportError, PackageNotFoundError):\n"
        "        missing.append(module)\n"
        "print(json.dumps({'missing': missing, 'resolved': resolved}))\n"
    )
    completed = _run(
        [*packaging_python_command(project_root), "-c", probe],
        cwd=project_root,
        capture=True,
        failures=failures,
        gate="packaging-toolchain",
    )
    report = json.loads(completed.stdout)
    missing = report["missing"]
    if missing:
        raise RehearsalError(
            "packaging toolchain is unusable from the locked environment: "
            f"{', '.join(missing)} failed to import under "
            f"`{shlex.join(packaging_python_command(project_root))}`. Add each "
            f"one to the project's dev dependency group at the version "
            f"{TOOLCHAIN_DECLARATION_PATH} declares and re-run `uv lock` so the "
            "artifact chain uses the same locked toolchain as every other gate."
        )
    resolved = {name: str(value) for name, value in report["resolved"].items()}
    declared = declared_packaging_pins(project_root)
    if resolved != declared:
        drifted = sorted(
            name for name in declared if resolved.get(name) != declared[name]
        )
        raise RehearsalError(
            "packaging toolchain differs from "
            f"{TOOLCHAIN_DECLARATION_PATH} for {', '.join(drifted)}:\n"
            + json.dumps(
                {"declared": declared, "resolved": resolved}, indent=2, sort_keys=True
            )
        )
    return resolved


def _build_canonical_artifacts(
    root: Path,
    artifact_dir: Path,
    version: str,
    failures: FailureArtifactStore | None = None,
    *,
    project_root: Path | None = None,
) -> tuple[Path, Path]:
    packaging_python = packaging_python_command(project_root or root)
    artifact_dir.mkdir(parents=True, exist_ok=True)
    _run(
        [
            *packaging_python,
            "-m",
            "build",
            "--no-isolation",
            "--sdist",
            "--outdir",
            str(artifact_dir),
            str(root),
        ],
        cwd=artifact_dir,
        failures=failures,
        gate="build-sdist",
    )
    _run(
        [
            *packaging_python,
            "-m",
            "build",
            "--no-isolation",
            "--wheel",
            "--outdir",
            str(artifact_dir),
            str(root),
        ],
        cwd=artifact_dir,
        failures=failures,
        gate="build-wheel",
    )
    sdist = artifact_dir / f"spice_harness-{version}.tar.gz"
    wheel = artifact_dir / f"spice_harness-{version}-py3-none-any.whl"
    resolved = tuple(sorted(path.name for path in artifact_dir.iterdir()))
    expected = tuple(sorted((sdist.name, wheel.name)))
    if resolved != expected:
        raise RehearsalError(
            f"canonical build artifacts differ: expected={expected!r} "
            f"resolved={resolved!r}"
        )
    _run(
        [*packaging_python, "-m", "twine", "check", str(sdist), str(wheel)],
        cwd=artifact_dir,
        failures=failures,
        gate="metadata",
    )
    return sdist, wheel


def _isolated_environment() -> dict[str, str]:
    environment = dict(os.environ)  # env-policy: allow
    for name in ("PYTHONHOME", "PYTHONPATH", "VIRTUAL_ENV"):
        environment.pop(name, None)
    return environment


def _validate_installed_wheel(
    root: Path,
    wheel: Path,
    version: str,
    scratch: Path,
    failures: FailureArtifactStore | None = None,
) -> None:
    venv = scratch / "installed"
    python = venv / "bin" / "python"
    spice = venv / "bin" / "spice"
    _run(
        [sys.executable, "-m", "venv", str(venv)],
        cwd=scratch,
        failures=failures,
        gate="isolated-venv",
    )
    _run(
        ["uv", "pip", "install", "--python", str(python), str(wheel)],
        cwd=scratch,
        failures=failures,
        gate="isolated-install",
    )
    probe = (
        "from importlib.metadata import version; "
        "from importlib.resources import files; "
        "from spice.config import layers; "
        f"assert version('spice-harness') == {version!r}; "
        "assert files('spice').joinpath('spice.toml').is_file(); "
        "print(layers.__file__)"
    )
    environment = _isolated_environment()
    _run(
        [str(python), "-I", "-c", probe],
        cwd=scratch,
        env=environment,
        failures=failures,
        gate="installed-imports",
    )
    for arguments in (
        ("--version",),
        ("task", "--help"),
        ("session", "--help"),
    ):
        _run(
            [str(spice), *arguments],
            cwd=scratch,
            env=environment,
            failures=failures,
            gate="installed-console",
        )
    if (
        root
        in Path(
            _run(
                [str(python), "-I", "-c", "import spice; print(spice.__path__[0])"],
                cwd=scratch,
                capture=True,
                env=environment,
                failures=failures,
                gate="installed-import-origin",
            ).stdout.strip()
        ).parents
    ):
        raise RehearsalError("isolated import resolved back into the source worktree")


def _carried_predecessor(root: Path) -> Path:
    """Resolve the carried predecessor wheel, refusing to proceed without one.

    The synthetic repository has no tags, so there is nothing here to derive a
    predecessor from. An absent artifact means the release under proof has no
    demonstrated upgrade path, which is a failure and never a skip.
    """
    manifest_path = root / PRIOR_ARTIFACT_MANIFEST
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise RehearsalError(
            f"could not read the carried predecessor manifest {manifest_path}: {exc}"
        ) from exc
    if manifest.get("state") != "built":
        raise RehearsalError(
            "the in-place upgrade proof needs a built predecessor artifact; "
            f"{manifest_path} records state={manifest.get('state')!r}"
        )
    wheel = root / PRIOR_ARTIFACT_DIRECTORY / str(manifest["wheel"]["name"])
    if not wheel.is_file():
        raise RehearsalError(f"carried predecessor wheel is missing: {wheel}")
    return wheel


def _assert_state_inventory_is_declared(root: Path) -> list[str]:
    """Answer the recorded inventory to the stores current source opens.

    Observing a run only catches a store the seed happens to create. Reading the
    declarations catches one added lazily too, so a new spice-owned store stops
    the release until it is rehearsed or excluded on the record.
    """
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
    registered = set(UPGRADE_REHEARSAL_BY_STORE)
    governed = set(UPGRADE_GOVERNED_STATE)
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
    unrecorded = sorted(
        declared.difference(UPGRADE_GOVERNED_STATE).difference(UPGRADE_EXCLUDED_STATE)
    )
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
) -> _UpgradeWrites:
    _run(
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
    _require_upgrade_tasks(seed, seed.tasks, phase="upgraded", failures=failures)
    current_paths_payload = _run_installed_probe(
        seed.python,
        root,
        seed.repository,
        "paths",
        seed.environment,
        failures,
    )
    current_expected = set(UPGRADE_GOVERNED_STATE) | set(UPGRADE_EXCLUDED_STATE)
    current_paths = _validated_installed_paths(
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
        _run_installed_probe(
            seed.python,
            root,
            seed.repository,
            "verify-authority",
            seed.environment,
            failures,
        ),
        written=True,
    )
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


def _validate_in_place_upgrade(
    root: Path,
    wheel: Path,
    scratch: Path,
    failures: FailureArtifactStore | None = None,
) -> dict[str, object]:
    """Install the predecessor, preserve its stores, then read and write."""
    declared = _assert_state_inventory_is_declared(root)
    predecessor = _carried_predecessor(root)
    seed = _seed_predecessor_install(root, predecessor, scratch, failures)
    writes = _upgrade_installed_package(root, wheel, scratch, seed, failures)
    return {
        "declared": declared,
        **_finalize_in_place_upgrade(root, seed, writes, failures),
    }


def _extract_sdist(sdist: Path, destination: Path, version: str) -> Path:
    destination.mkdir()
    with tarfile.open(sdist, "r:gz") as archive:
        archive.extractall(destination, filter="data")
    source = destination / f"spice_harness-{version}"
    if not source.is_dir():
        raise RehearsalError(f"sdist did not contain its canonical root: {source}")
    return source


def _rebuild_wheel_from_sdist(
    sdist: Path,
    version: str,
    scratch: Path,
    failures: FailureArtifactStore | None = None,
    *,
    project_root: Path | None = None,
) -> Path:
    source = _extract_sdist(sdist, scratch / "sdist", version)
    rebuilt_dir = scratch / "rebuilt"
    rebuilt_dir.mkdir()
    _run(
        [
            *packaging_python_command(project_root or source),
            "-m",
            "build",
            "--no-isolation",
            "--wheel",
            "--outdir",
            str(rebuilt_dir),
            str(source),
        ],
        cwd=rebuilt_dir,
        failures=failures,
        gate="sdist-rebuild",
    )
    wheel = rebuilt_dir / f"spice_harness-{version}-py3-none-any.whl"
    if not wheel.is_file():
        raise RehearsalError(f"sdist rebuild did not produce {wheel.name}")
    return wheel


def _wheel_member_hashes(path: Path) -> dict[str, str]:
    with zipfile.ZipFile(path) as archive:
        return {
            info.filename: hashlib.sha256(archive.read(info)).hexdigest()
            for info in archive.infolist()
        }


def wheel_member_mismatches(canonical: Path, rebuilt: Path) -> list[dict[str, str]]:
    """Return every exact member-level delta, ignoring only ZIP container bytes."""
    expected = _wheel_member_hashes(canonical)
    resolved = _wheel_member_hashes(rebuilt)
    mismatches: list[dict[str, str]] = []
    for name in sorted(expected.keys() - resolved.keys()):
        mismatches.append(
            {
                "kind": "missing-from-rebuilt",
                "member": name,
                "canonical_sha256": expected[name],
            }
        )
    for name in sorted(resolved.keys() - expected.keys()):
        mismatches.append(
            {
                "kind": "extra-in-rebuilt",
                "member": name,
                "rebuilt_sha256": resolved[name],
            }
        )
    for name in sorted(expected.keys() & resolved.keys()):
        if expected[name] != resolved[name]:
            mismatches.append(
                {
                    "kind": "content-changed",
                    "member": name,
                    "canonical_sha256": expected[name],
                    "rebuilt_sha256": resolved[name],
                }
            )
    return mismatches


def git_private_path(root: Path, name: str) -> Path:
    """Resolve a Git-private record the way init-source.py writes it.

    A linked worktree keeps a ``.git`` file holding a gitdir pointer, so
    joining ``.git`` as a directory raises ``Not a directory`` there. Asking
    Git for the path works for both layouts.
    """
    resolved = Path(
        _run(
            ["git", "rev-parse", "--git-path", name],
            cwd=root,
            capture=True,
            gate="git-private-path",
        ).stdout.strip()
    )
    return resolved if resolved.is_absolute() else root / resolved


def _load_git_private_json(root: Path, name: str) -> dict[str, Any]:
    path = git_private_path(root, name)
    if not _proof_record_exists(path):
        producer = GIT_PRIVATE_RECORD_PRODUCERS[name]
        raise RehearsalError(
            f"missing Git-private proof record {name} at {path}; "
            f"{producer} writes it during the release-proof container build, "
            "so a host checkout that has not run that build cannot emit a "
            "receipt for artifacts it otherwise proved"
        )
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise RehearsalError(f"invalid Git-private proof record JSON: {path}") from exc
    if not isinstance(payload, dict):
        raise RehearsalError(f"invalid Git-private proof record: {path}")
    return payload


def _container_provenance(
    root: Path,
) -> tuple[dict[str, Any], dict[str, Any], dict[str, object]]:
    names = (
        "release-proof-identities.json",
        "release-proof-toolchain.json",
    )
    paths = {name: git_private_path(root, name) for name in names}
    present = {name: _proof_record_exists(path) for name, path in paths.items()}
    if all(present.values()):
        return (
            _load_git_private_json(root, names[0]),
            _load_git_private_json(root, names[1]),
            {
                "operating_system": "linux",
                "host_native_companion": "release-proof-macos.json",
                "host_native_checks": [
                    "kqueue-or-fsevents",
                    "appearance",
                    "speech",
                ],
            },
        )
    if any(present.values()):
        available = sorted(name for name, exists in present.items() if exists)
        missing = sorted(name for name, exists in present.items() if not exists)
        raise RehearsalError(
            "incomplete container-only release provenance before gates: "
            f"present={available} missing={missing}; run release-proof/appliance.py "
            "to regenerate the container evidence boundary"
        )

    def absent(name: str) -> dict[str, str]:
        return {
            "availability": "absent",
            "reason": (
                "container-only proof record is not produced by a direct host "
                "artifact rehearsal"
            ),
            "record": name,
            "scope": "container-only",
        }

    system = platform.system()
    operating_system = "macos" if system == "Darwin" else system.lower()
    return (
        absent(names[0]),
        absent(names[1]),
        {
            "operating_system": operating_system,
            "claim": "host-artifact-rehearsal",
            "container_provenance": "absent",
        },
    )


def _proof_record_exists(path: Path) -> bool:
    try:
        mode = path.stat().st_mode
    except FileNotFoundError:
        return False
    except OSError as exc:
        raise RehearsalError(f"cannot inspect Git-private proof record: {path}: {exc}")
    if not stat.S_ISREG(mode):
        raise RehearsalError(f"Git-private proof record is not a file: {path}")
    return True


def _write_json(path: Path, payload: dict[str, object]) -> None:
    temporary = path.with_name(f".{path.name}.tmp")
    temporary.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    temporary.replace(path)


def rehearse(root: Path, artifact_dir: Path) -> dict[str, object]:
    root = root.resolve(strict=True)
    artifact_dir = artifact_dir.resolve()
    artifact_dir.mkdir(parents=True, exist_ok=False)
    failures = FailureArtifactStore(artifact_dir)
    packaging = verify_packaging_toolchain(root, failures)
    source_identity, toolchain, claim_boundary = _container_provenance(root)
    version = _project_version(root)
    with tempfile.TemporaryDirectory(prefix="spice-release-rehearsal-") as raw:
        scratch = Path(raw)
        gate_evidence = _run_source_gates(root, scratch, failures)
        source = _materialize_committed_source(root, scratch)
        sdist, wheel = _build_canonical_artifacts(
            source, artifact_dir, version, failures, project_root=root
        )
        _validate_installed_wheel(root, wheel, version, scratch, failures)
        in_place_upgrade = _validate_in_place_upgrade(root, wheel, scratch, failures)
        rebuilt = _rebuild_wheel_from_sdist(
            sdist, version, scratch, failures, project_root=root
        )
        mismatches = wheel_member_mismatches(wheel, rebuilt)
        if mismatches:
            raise RehearsalError(
                "sdist-rebuilt wheel member mismatch:\n"
                + json.dumps(mismatches, indent=2, sort_keys=True)
            )
        rebuilt_sha256 = _sha256(rebuilt)
        rebuilt_member_count = len(_wheel_member_hashes(rebuilt))
    status = _run(
        ["git", "status", "--porcelain=v1", "--untracked-files=all"],
        cwd=root,
        capture=True,
    ).stdout.strip()
    if status:
        raise RehearsalError(f"release rehearsal left a dirty worktree:\n{status}")
    sdist_sha256 = _sha256(sdist)
    wheel_sha256 = _sha256(wheel)
    receipt: dict[str, object] = {
        "schema_version": SCHEMA_VERSION,
        "version": version,
        "claim_boundary": claim_boundary,
        "source_identity": source_identity,
        "toolchain": toolchain,
        "tests": {
            "python": gate_evidence["python"],
            "ruff": gate_evidence["ruff"],
        },
        "browser": gate_evidence["browser"],
        "mutation": gate_evidence["mutation"],
        "upgrades": gate_evidence["upgrades"],
        "in_place_upgrade": in_place_upgrade,
        "artifacts": {
            "sdist": {
                "filename": sdist.name,
                "bytes": sdist.stat().st_size,
                "sha256": sdist_sha256,
            },
            "wheel": {
                "filename": wheel.name,
                "bytes": wheel.stat().st_size,
                "sha256": wheel_sha256,
            },
            "installed_wheel_sha256": wheel_sha256,
            "sdist_rebuilt_from_sha256": sdist_sha256,
        },
        "artifact_rehearsal": {
            "checks": list(CHECKS),
            "installed_wheel_sha256": wheel_sha256,
            "packaging_toolchain": packaging,
            "sdist_rebuilt_from_sha256": sdist_sha256,
        },
        "content_comparison": {
            "canonical_members": len(_wheel_member_hashes(wheel)),
            "rebuilt_members": rebuilt_member_count,
            "mismatches": [],
            "rebuilt_wheel_sha256": rebuilt_sha256,
            "outer_archive_reproducibility": "deferred",
        },
        "failure_diagnostics": failure_policy_payload(),
    }
    _write_json(artifact_dir / RECEIPT_NAME, receipt)
    return receipt


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifacts", required=True, type=Path)
    arguments = parser.parse_args()
    try:
        receipt = rehearse(Path(__file__).resolve().parent.parent, arguments.artifacts)
    except (OSError, RehearsalError, ValueError) as exc:
        safe_error = redact_text(str(exc), dict(os.environ))  # env-policy: allow
        failure_dir = arguments.artifacts / FAILURE_DIRNAME
        if not failure_dir.is_dir() or not tuple(failure_dir.glob("*.log")):
            FailureArtifactStore(arguments.artifacts).record(
                "rehearsal",
                [sys.executable, str(Path(__file__).resolve())],
                EXIT_FAILURE,
                "",
                safe_error,
            )
        print(f"release-proof rehearsal: {safe_error}", file=sys.stderr)
        return EXIT_FAILURE
    print(json.dumps(receipt, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

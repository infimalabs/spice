"""Installed-runtime identity proofs for release gates."""

from __future__ import annotations

import hashlib
import json
import re
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from subprocess import CompletedProcess

from spice.cli.mounts import RUNTIME_PYTHON_ENV
from spice.config.pyproject import pyproject_table, read_pyproject
from spice.errors import SpiceError

RegistryFile = tuple[str, str, int]
Runner = Callable[..., CompletedProcess[str]]
SourceIdentity = Callable[[Path], tuple[str, str]]
WorktreeDrift = Callable[[Path], str]


@dataclass(frozen=True)
class InstalledCliSource:
    python: Path
    module: Path
    root: Path
    commit: str
    tree: str


@dataclass(frozen=True)
class InstalledCliRegistry:
    python: Path
    module: Path
    version: str
    artifact: str
    files: tuple[RegistryFile, ...]


InstalledCliIdentity = InstalledCliSource | InstalledCliRegistry

REGISTRY_SOURCE_EXCLUSIONS = frozenset(
    {
        # Setuptools packages the shell-hook payload but not Git's directory
        # preservation markers. Every other tracked path below spice/ is part
        # of the registry artifact and participates in its identity.
        "spice/agent/shellhooks/.gitignore",
        "spice/agent/staticshellhooks/.gitignore",
    }
)
INSTALLED_CLI_PROBE_SCRIPT = """\
import hashlib
import json
from importlib import metadata
from pathlib import Path

import spice.tasks.git.boundaries as boundaries

distribution = metadata.distribution("spice-harness")
raw_direct_url = distribution.read_text("direct_url.json")
direct_url = json.loads(raw_direct_url) if raw_direct_url else None
files = []
if direct_url is None:
    module = Path(boundaries.__file__).resolve()
    site_root = module.parents[3]
    for target in (site_root / "spice").rglob("*"):
        if (
            not target.is_file()
            or "__pycache__" in target.parts
            or target.suffix in {".pyc", ".pyo"}
        ):
            continue
        relative = target.relative_to(site_root).as_posix()
        data = target.read_bytes()
        files.append([relative, hashlib.sha256(data).hexdigest(), len(data)])
    files.sort()
artifact = hashlib.sha256(
    json.dumps(files, separators=(",", ":")).encode()
).hexdigest()
print(json.dumps({
    "artifact": artifact,
    "files": files,
    "module": str(Path(boundaries.__file__).resolve()),
    "registry": direct_url is None,
    "version": distribution.version,
}, sort_keys=True))
"""


def require_installed_cli_matches_release(
    root: Path,
    installed: InstalledCliIdentity,
    candidate_commit: str,
    candidate_tree: str,
    *,
    run: Runner,
) -> InstalledCliIdentity:
    """Prove ordinary fleet commands import the candidate release identity."""
    candidate_root = root.resolve()
    if installed.python.is_relative_to(candidate_root):
        raise SpiceError(
            "release evidence must come from the independently installed CLI, "
            f"not the candidate worktree interpreter {installed.python}"
        )
    if isinstance(installed, InstalledCliRegistry):
        return _require_registry_cli_matches_release(
            root,
            installed,
            candidate_commit,
            candidate_tree,
            run=run,
        )
    if installed.tree != candidate_tree:
        raise SpiceError(
            "deploy the candidate tree through the installed CLI before claiming "
            "release behavior; branch state has no fleet effect by itself: "
            f"candidate HEAD {candidate_commit} tree {candidate_tree}, while "
            f"{installed.python} -P imports {installed.module} from "
            f"{installed.commit} tree {installed.tree}"
        )
    print(
        "installed CLI source gate passed: "
        f"{installed.python} -P imports {installed.module}; "
        f"candidate {candidate_commit} and installed {installed.commit} "
        f"share tree {candidate_tree}"
    )
    return installed


def _require_registry_cli_matches_release(
    root: Path,
    installed: InstalledCliRegistry,
    candidate_commit: str,
    candidate_tree: str,
    *,
    run: Runner,
) -> InstalledCliRegistry:
    candidate_version = _candidate_release_version(root)
    tag = f"v{candidate_version}"
    candidate_artifact, candidate_files = _candidate_registry_artifact(root, run=run)
    candidate_identity = (
        f"candidate tag {tag} HEAD {candidate_commit} tree {candidate_tree} "
        f"artifact sha256:{candidate_artifact}"
    )
    installed_identity = (
        f"installed spice-harness=={installed.version} "
        f"artifact sha256:{installed.artifact}"
    )
    tags = set(
        run(
            ["git", "-C", str(root), "tag", "--points-at", candidate_commit],
            capture=True,
        ).stdout.splitlines()
    )
    if tag not in tags:
        raise SpiceError(
            "registry-installed release evidence requires the checked-out release "
            f"tag; {candidate_identity} is not tagged at HEAD, while "
            f"{installed_identity}"
        )
    mismatches = []
    if installed.version != candidate_version:
        mismatches.append("version")
    candidate_paths = {row[0] for row in candidate_files}
    installed_paths = {row[0] for row in installed.files}
    if installed_paths != candidate_paths:
        mismatches.append("payload paths")
    if installed.artifact != candidate_artifact:
        mismatches.append("artifact")
    if mismatches:
        raise SpiceError(
            "installed CLI release identity does not match the candidate before "
            f"release gates run ({', '.join(mismatches)}): {candidate_identity}, "
            f"while {installed_identity}"
        )
    print(
        "installed CLI registry gate passed: "
        f"{installed.python} -P imports {installed.module}; "
        f"{candidate_identity} matches {installed_identity}"
    )
    return installed


def _candidate_release_version(root: Path) -> str:
    project = pyproject_table(read_pyproject(root), "project")
    version = str(project.get("version") or "").strip()
    if not version:
        raise SpiceError(
            f"candidate release metadata is unusable: {root / 'pyproject.toml'}"
        )
    return version


def _candidate_registry_artifact(
    root: Path,
    *,
    run: Runner,
) -> tuple[str, tuple[RegistryFile, ...]]:
    tracked = run(
        ["git", "-C", str(root), "ls-files", "-z", "--", "spice"],
        capture=True,
    ).stdout.split("\0")
    paths = sorted(
        path for path in tracked if path and path not in REGISTRY_SOURCE_EXCLUSIONS
    )
    files = tuple(_registry_file(root, path) for path in paths)
    return _registry_artifact(files), files


def _registry_file(root: Path, relative: str) -> RegistryFile:
    data = (root / relative).read_bytes()
    return relative, hashlib.sha256(data).hexdigest(), len(data)


def _registry_artifact(files: tuple[RegistryFile, ...]) -> str:
    return hashlib.sha256(json.dumps(files, separators=(",", ":")).encode()).hexdigest()


def installed_cli_identity(
    raw_python: str,
    *,
    environ: Mapping[str, str],
    run: Runner,
    source_identity: SourceIdentity,
    worktree_drift: WorktreeDrift,
) -> InstalledCliIdentity:
    raw_python = raw_python.strip()
    if not raw_python:
        raise SpiceError(
            "run release gates through the repository-mounted `spice release` "
            f"command; {RUNTIME_PYTHON_ENV} did not identify the installed CLI"
        )
    python = Path(raw_python).expanduser().absolute()
    if not python.is_file():
        raise SpiceError(f"installed CLI interpreter does not exist: {python}")
    probe_env = dict(environ)
    probe_env.pop("PYTHONPATH", None)
    result = run(
        [str(python), "-P", "-c", INSTALLED_CLI_PROBE_SCRIPT],
        capture=True,
        cwd=Path("/"),
        env=probe_env,
    )
    try:
        payload = json.loads(result.stdout.strip().splitlines()[-1])
        module = Path(str(payload["module"])).resolve(strict=True)
        root = module.parents[3]
    except (IndexError, KeyError, OSError, TypeError, ValueError) as exc:
        raise SpiceError(
            f"installed CLI probe returned no usable module path: {result.stdout!r}"
        ) from exc
    expected = Path("spice/tasks/git/boundaries.py")
    if module.relative_to(root) != expected:
        raise SpiceError(
            f"installed CLI probe resolved unexpected module path {module}; "
            f"expected <source-root>/{expected}"
        )
    if not (root / "pyproject.toml").is_file():
        return _installed_registry_identity(python, module, payload)
    drift = worktree_drift(root)
    if drift:
        raise SpiceError(
            "the installed CLI runs its source checkout directly, so a dirty "
            "deployment executes code no commit contains and its committed "
            f"identity proves nothing; commit or revert {root} before "
            f"releasing:\n{drift}"
        )
    commit, tree = source_identity(root)
    return InstalledCliSource(python, module, root, commit, tree)


def _installed_registry_identity(
    python: Path,
    module: Path,
    payload: object,
) -> InstalledCliRegistry:
    if not isinstance(payload, dict) or not payload.get("registry"):
        raise SpiceError(
            f"installed CLI module {module} is neither backed by an editable "
            "Spice source checkout nor installed from a package registry"
        )
    version = str(payload.get("version") or "").strip()
    raw_files = payload.get("files")
    if not version or not isinstance(raw_files, list):
        raise SpiceError(
            f"installed CLI registry probe returned unusable identity: {payload!r}"
        )
    try:
        files = tuple(
            (str(path), str(digest), int(size)) for path, digest, size in raw_files
        )
    except (TypeError, ValueError) as exc:
        raise SpiceError(
            f"installed CLI registry probe returned unusable files: {raw_files!r}"
        ) from exc
    if (
        not files
        or any(
            PurePosixPath(path).parts[:1] != ("spice",)
            or ".." in PurePosixPath(path).parts
            or re.fullmatch(r"[0-9a-f]{64}", digest) is None
            or size < 0
            for path, digest, size in files
        )
        or len({path for path, _digest, _size in files}) != len(files)
        or tuple(sorted(files)) != files
    ):
        raise SpiceError(
            f"installed CLI registry probe returned invalid payload: {raw_files!r}"
        )
    artifact = _registry_artifact(files)
    if artifact != str(payload.get("artifact") or ""):
        raise SpiceError(
            "installed CLI registry probe returned a payload whose artifact "
            "digest does not match its files"
        )
    return InstalledCliRegistry(python, module, version, artifact, files)

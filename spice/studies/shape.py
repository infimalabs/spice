"""Repo shape opinions: namespace packages, path names, no generic splits.

These guards bite inside the package roots a repo declares in its tracked
`pyproject.toml` under `[tool.spice.policy] package_roots` (plus `tests/`
when present). Repos without a declaration skip them — the opinions are
Python-package-specific; the rest of the constitution still applies.

The name-cluster guard flags a run of sibling modules sharing a long prefix or
suffix — a package hiding behind an affix. `name_cluster_error` computes it and
is enforced as a blocking gate: `precommit._run_shape_guards` raises on any
offender alongside the namespace and path-shape guards. Repos can tune the
number of sibling modules that trips this guard with
`[tool.spice.policy].name_cluster_threshold`; unset repos use the default.
"""

from __future__ import annotations

import re
from collections.abc import Iterable
from fnmatch import fnmatch, fnmatchcase
from pathlib import Path

from spice.errors import SpiceError
from spice.policy import BOUNDARY_UNDERSCORE_PATTERN
from spice.repocfg import policy_table, read_pyproject, string_list

BOUNDARY_UNDERSCORE_RE = re.compile(BOUNDARY_UNDERSCORE_PATTERN)
# Generic continuation shards: a split must name the seam, not number it.
GENERIC_SPLIT_RES = (
    re.compile(r"\.part\d+\.py$", re.IGNORECASE),
    re.compile(r"part\d+\.py$", re.IGNORECASE),
    re.compile(r"\d+\.py$"),
)
ALLOWED_NON_SHAPE_FILES = frozenset({"__main__.py", "py.typed"})
# Four or more sibling modules sharing a long prefix or suffix are a package
# wearing an affix as a disguise. Repos may tune the threshold, with 3 as the
# minimum allowed value, but there is no flex headroom: unlike file size, a name
# cluster is not a throughput knob, so there is nothing to amortize.
MIN_NAME_CLUSTER_THRESHOLD = 3
DEFAULT_NAME_CLUSTER_THRESHOLD = 4
NAME_CLUSTER_MIN_AFFIX = 4
NAME_CLUSTER_THRESHOLD_KEY = "name_cluster_threshold"
GENERATED_PATHS_KEY = "generated_paths"


def configured_package_roots(repo_root: Path) -> list[Path]:
    # Explicit `[tool.spice.policy] package_roots` wins; otherwise derive the
    # roots from the project's own Python packaging config so a standard project
    # needs no spice-local declaration. Neither present -> no roots (skip).
    names = string_list(policy_table(repo_root).get("package_roots"))
    if not names:
        names = _derived_package_roots(repo_root)
    return [repo_root / name for name in names if (repo_root / name).is_dir()]


def generated_path_patterns(repo_root: Path) -> tuple[str, ...]:
    """Tracked globs whose matches are exempt from every repo-shape guard.

    Generated sources (e.g. a protobuf ``*_pb2.py`` committed inside a package)
    cannot satisfy the naming law, yet the rest of the tree should stay
    enforced. Unlike study ``exclude``, this exemption reaches the shape guards
    themselves, not just the study walk.
    """
    return tuple(string_list(policy_table(repo_root).get(GENERATED_PATHS_KEY)))


def _has_glob_magic(pattern: str) -> bool:
    return any(char in pattern for char in "*?[")


def is_generated_path(rel_posix: str, patterns: tuple[str, ...]) -> bool:
    """True when a repo-relative posix path is a declared generated path.

    A glob pattern matches by ``fnmatch``; a plain pattern matches the path
    itself or any path beneath it, so a directory entry exempts its subtree.
    """
    for raw in patterns:
        pattern = raw.strip().replace("\\", "/").removeprefix("./")
        if not pattern:
            continue
        if _has_glob_magic(pattern):
            if fnmatchcase(rel_posix, pattern):
                return True
            continue
        prefix = pattern.rstrip("/")
        if rel_posix == prefix or rel_posix.startswith(prefix + "/"):
            return True
    return False


def name_cluster_threshold(repo_root: Path) -> int:
    raw = policy_table(repo_root).get(NAME_CLUSTER_THRESHOLD_KEY)
    if raw is None:
        return DEFAULT_NAME_CLUSTER_THRESHOLD
    if (
        isinstance(raw, bool)
        or not isinstance(raw, int)
        or raw < MIN_NAME_CLUSTER_THRESHOLD
    ):
        raise SpiceError(
            "[tool.spice.policy].name_cluster_threshold must be an integer "
            f">= {MIN_NAME_CLUSTER_THRESHOLD}"
        )
    return raw


def _derived_package_roots(repo_root: Path) -> list[str]:
    """Package roots inferred from the project's own packaging metadata.

    Each backend is tried in a fixed precedence; the first one whose table is
    present wins. A backend whose table is absent is skipped; a backend that is
    present but malformed fails loudly (``SpiceError``) rather than silently
    falling through. When no packaging backend declares anything, fall back to
    a ``src/`` layout, then to the ``[project].name`` package directory.
    """
    data = read_pyproject(repo_root)
    tool = data.get("tool")
    tool = tool if isinstance(tool, dict) else {}
    for resolver in (
        _setuptools_roots,
        _poetry_roots,
        _hatch_roots,
        _flit_roots,
        _pdm_roots,
    ):
        roots = resolver(repo_root, tool)
        if roots is not None:
            return roots
    return _layout_fallback_roots(repo_root, data)


def _setuptools_roots(repo_root: Path, tool: dict[str, object]) -> list[str] | None:
    setuptools = tool.get("setuptools")
    if not isinstance(setuptools, dict):
        return None
    packages = setuptools.get("packages")
    if packages is None:
        return None
    if isinstance(packages, list):
        return _explicit_package_roots(packages)
    if isinstance(packages, dict) and isinstance(packages.get("find"), dict):
        return _find_package_roots(repo_root, packages["find"])
    raise SpiceError(
        "[tool.setuptools].packages must be a list or a {find = {...}} table"
    )


def _poetry_roots(repo_root: Path, tool: dict[str, object]) -> list[str] | None:
    poetry = tool.get("poetry")
    if not isinstance(poetry, dict):
        return None
    packages = poetry.get("packages")
    if packages is not None:
        return _poetry_package_roots(packages)
    name = poetry.get("name")
    if isinstance(name, str) and name.strip():
        return _name_dir_roots(repo_root, name)
    return []


def _poetry_package_roots(packages: object) -> list[str]:
    if not isinstance(packages, list):
        raise SpiceError("[tool.poetry].packages must be a list")
    roots: list[str] = []
    for entry in packages:
        if isinstance(entry, str):
            rel = entry
        elif isinstance(entry, dict) and isinstance(entry.get("include"), str):
            base = entry.get("from")
            base = base if isinstance(base, str) else ""
            rel = f"{base}/{entry['include']}".strip("/") if base else entry["include"]
        else:
            raise SpiceError(
                "[tool.poetry].packages entries must be a string or a table with 'include'"
            )
        rel = rel.strip("/")
        if rel and rel not in roots:
            roots.append(rel)
    return roots


def _hatch_roots(repo_root: Path, tool: dict[str, object]) -> list[str] | None:
    hatch = tool.get("hatch")
    build = hatch.get("build") if isinstance(hatch, dict) else None
    if not isinstance(build, dict):
        return None
    packages = build.get("packages")
    if packages is None:
        targets = build.get("targets")
        wheel = targets.get("wheel") if isinstance(targets, dict) else None
        packages = wheel.get("packages") if isinstance(wheel, dict) else None
    if packages is not None:
        if not isinstance(packages, list):
            raise SpiceError("[tool.hatch.build...].packages must be a list")
        return _dedupe(str(entry).strip().strip("/") for entry in packages)
    include = build.get("include")
    if include is None:
        return None
    return _hatch_include_roots(repo_root, include)


def _hatch_include_roots(repo_root: Path, include: object) -> list[str]:
    if not isinstance(include, list):
        raise SpiceError("[tool.hatch.build].include must be a list")
    roots: list[str] = []
    for entry in include:
        if not isinstance(entry, str):
            raise SpiceError("[tool.hatch.build].include entries must be strings")
        rel = entry.strip().strip("/")
        path = repo_root / rel
        if path.is_dir() and next(path.rglob("*.py"), None) is not None:
            roots.append(rel)
    return _dedupe(roots)


def _flit_roots(repo_root: Path, tool: dict[str, object]) -> list[str] | None:
    flit = tool.get("flit")
    if not isinstance(flit, dict):
        return None
    module = flit.get("module")
    name = module.get("name") if isinstance(module, dict) else None
    if name is None:
        metadata = flit.get("metadata")
        name = metadata.get("module") if isinstance(metadata, dict) else None
    if name is None:
        return None
    if not isinstance(name, str) or not name.strip():
        raise SpiceError("[tool.flit.module].name must be a non-empty string")
    return _name_dir_roots(repo_root, name)


def _pdm_roots(repo_root: Path, tool: dict[str, object]) -> list[str] | None:
    pdm = tool.get("pdm")
    build = pdm.get("build") if isinstance(pdm, dict) else None
    if not isinstance(build, dict):
        return None
    includes = build.get("includes")
    if includes is None:
        return None
    if not isinstance(includes, list):
        raise SpiceError("[tool.pdm.build].includes must be a list")
    candidates = (str(entry or "").strip().rstrip("/") for entry in includes)
    return _dedupe(
        entry
        for entry in candidates
        if entry and "*" not in entry and not entry.endswith(".py")
    )


def _layout_fallback_roots(repo_root: Path, data: dict[str, object]) -> list[str]:
    src = repo_root / "src"
    if src.is_dir():
        roots = [
            f"src/{child.name}"
            for child in sorted(src.iterdir())
            if child.is_dir()
            and not child.name.startswith(".")
            and next(child.rglob("*.py"), None) is not None
        ]
        if roots:
            return roots
    project = data.get("project")
    name = project.get("name") if isinstance(project, dict) else None
    if isinstance(name, str) and name.strip():
        return _name_dir_roots(repo_root, name)
    return []


def _name_dir_roots(repo_root: Path, name: str) -> list[str]:
    normalized = name.strip().replace("-", "_")
    for candidate in (normalized, f"src/{normalized}"):
        path = repo_root / candidate
        if path.is_dir() and next(path.rglob("*.py"), None) is not None:
            return [candidate]
    return []


def _dedupe(names: Iterable[str]) -> list[str]:
    return list(dict.fromkeys(name for name in names if name))


def _explicit_package_roots(packages: list[object]) -> list[str]:
    roots = (str(entry or "").split(".", 1)[0].strip() for entry in packages)
    return list(dict.fromkeys(root for root in roots if root))


def _find_package_roots(repo_root: Path, find: dict[str, object]) -> list[str]:
    where_dirs = string_list(find.get("where")) or ["."]
    includes = string_list(find.get("include")) or ["*"]
    excludes = string_list(find.get("exclude"))
    roots = []
    for base in where_dirs:
        base_dir = repo_root / base
        if not base_dir.is_dir():
            continue
        for child in sorted(base_dir.iterdir()):
            if not _is_find_package_root(child, includes, excludes):
                continue
            rel = child.relative_to(repo_root).as_posix()
            if rel not in roots:
                roots.append(rel)
    return roots


def _is_find_package_root(
    child: Path, includes: list[str], excludes: list[str]
) -> bool:
    name = child.name
    return (
        child.is_dir()
        and not name.startswith(".")
        and any(fnmatch(name, pattern) for pattern in includes)
        and not any(fnmatch(name, pattern) for pattern in excludes)
        and next(child.rglob("*.py"), None) is not None
    )


def namespace_policy_error(repo_root: Path) -> str:
    patterns = generated_path_patterns(repo_root)
    offenders: list[str] = []
    for root in configured_package_roots(repo_root):
        for path in root.rglob("__init__.py"):
            relative = path.relative_to(repo_root).as_posix()
            if is_generated_path(relative, patterns):
                continue
            offenders.append(relative)
    offenders.sort()
    if not offenders:
        return ""
    return "\n".join(
        [
            "namespace-package policy violated: __init__.py found under a "
            "declared package root",
            *offenders,
        ]
    )


def path_shape_errors(repo_root: Path) -> list[str]:
    patterns = generated_path_patterns(repo_root)
    offenders: list[str] = []
    scan_roots = configured_package_roots(repo_root)
    tests_root = repo_root / "tests"
    if tests_root.is_dir():
        scan_roots.append(tests_root)
    for root in scan_roots:
        for path in sorted(root.rglob("*")):
            if _is_residue_path(path):
                continue
            relative = path.relative_to(repo_root).as_posix()
            if is_generated_path(relative, patterns):
                continue
            if path.is_dir():
                if not BOUNDARY_UNDERSCORE_RE.fullmatch(path.name):
                    offenders.append(f"{relative}: directory name shape")
                continue
            if path.suffix != ".py":
                continue
            if path.name in ALLOWED_NON_SHAPE_FILES:
                continue
            if not _has_module_shape(path, tests_root):
                offenders.append(f"{relative}: file name shape")
                continue
            if any(pattern.search(path.name) for pattern in GENERIC_SPLIT_RES):
                offenders.append(
                    f"{relative}: generic split name; name the seam instead"
                )
                continue
            if _is_sibling_shard(path):
                offenders.append(
                    f"{relative}: continuation shard of a sibling; "
                    "name the seam instead"
                )
    return offenders


def _has_module_shape(path: Path, tests_root: Path) -> bool:
    # Test modules carry the pytest-mandated `test_` prefix; the boundary
    # shape applies to what the module is actually named after it.
    if tests_root in path.parents:
        return path.stem.startswith("test_") and bool(
            BOUNDARY_UNDERSCORE_RE.fullmatch(path.stem.removeprefix("test_"))
        )
    return bool(BOUNDARY_UNDERSCORE_RE.fullmatch(path.stem))


def _is_residue_path(path: Path) -> bool:
    return "__pycache__" in path.parts or path.suffix == ".pyc"


def _is_sibling_shard(path: Path) -> bool:
    """A numbered or letter-clustered shard of an existing sibling.

    `foo2.py` next to `foo.py` is a numbered shard. `fooa.py`/`foob.py`
    next to `foo.py` is a letter cluster (two or more single-letter
    variations of one base). Ordinary distinct words never trip this: the
    base file must exist and, for letters, the cluster must have company.
    """
    stem = path.stem
    if len(stem) < 2:
        return False
    base, last = stem[:-1], stem[-1]
    if not base or base.endswith("_"):
        return False
    sibling = path.with_name(f"{base}{path.suffix}")
    if not sibling.exists() or sibling == path:
        return False
    if last.isdigit():
        return True
    if not (last.isalpha() and last == last.lower()):
        return False
    cluster = [
        candidate
        for candidate in path.parent.glob(f"{base}?{path.suffix}")
        if candidate.stem[:-1] == base and candidate.stem[-1].isalpha()
    ]
    return len(cluster) >= 2


def path_shape_error(repo_root: Path) -> str:
    errors = path_shape_errors(repo_root)
    if not errors:
        return ""
    return "path-shape policy violation(s):\n" + "\n".join(errors)


def _normalized_module_stem(path: Path) -> str:
    return path.stem.strip("_")


def _common_affix(left: str, right: str, *, suffix: bool) -> str:
    """Longest shared prefix (or suffix, when `suffix`) of two stems."""
    limit = min(len(left), len(right))
    index = 0
    if suffix:
        while index < limit and left[-1 - index] == right[-1 - index]:
            index += 1
        return left[len(left) - index :] if index else ""
    while index < limit and left[index] == right[index]:
        index += 1
    return left[:index]


def _has_affix(stem: str, affix: str, *, suffix: bool) -> bool:
    if len(stem) <= len(affix):
        return False
    return stem.endswith(affix) if suffix else stem.startswith(affix)


def _name_cluster_candidates(
    modules: tuple[Path, ...], *, suffix: bool, threshold: int
) -> list[tuple[str, tuple[Path, ...]]]:
    entries = [
        (_normalized_module_stem(path), path)
        for path in modules
        if _normalized_module_stem(path)
    ]
    affixes: set[str] = set()
    for index, (left_stem, _) in enumerate(entries):
        for right_stem, _ in entries[index + 1 :]:
            affix = _common_affix(left_stem, right_stem, suffix=suffix)
            if len(affix) >= NAME_CLUSTER_MIN_AFFIX and affix.isalpha():
                affixes.add(affix)
    candidates: list[tuple[str, tuple[Path, ...]]] = []
    for affix in sorted(affixes, key=lambda candidate: (len(candidate), candidate)):
        members = tuple(
            path for stem, path in entries if _has_affix(stem, affix, suffix=suffix)
        )
        if len(members) >= threshold:
            candidates.append((affix, members))
    # Keep the broadest cluster: a longer affix whose members are a subset of an
    # already-selected shorter one adds no new offender, only noise.
    selected: list[tuple[str, tuple[Path, ...]]] = []
    selected_member_sets: list[frozenset[Path]] = []
    for affix, members in candidates:
        member_set = frozenset(members)
        if any(member_set <= chosen for chosen in selected_member_sets):
            continue
        selected.append((affix, members))
        selected_member_sets.append(member_set)
    return selected


def name_cluster_errors(repo_root: Path) -> list[str]:
    patterns = generated_path_patterns(repo_root)
    offenders: list[str] = []
    threshold = name_cluster_threshold(repo_root)
    for root in configured_package_roots(repo_root):
        modules_by_directory: dict[Path, list[Path]] = {}
        for path in sorted(root.rglob("*.py")):
            if _is_residue_path(path):
                continue
            if path.name in ALLOWED_NON_SHAPE_FILES:
                continue
            if is_generated_path(path.relative_to(repo_root).as_posix(), patterns):
                continue
            if not BOUNDARY_UNDERSCORE_RE.fullmatch(path.stem):
                continue
            modules_by_directory.setdefault(path.parent, []).append(path)
        for directory, modules in sorted(modules_by_directory.items()):
            for suffix in (False, True):
                for affix, members in _name_cluster_candidates(
                    tuple(modules), suffix=suffix, threshold=threshold
                ):
                    relative = directory.relative_to(repo_root).as_posix()
                    names = ", ".join(path.name for path in members)
                    kind = "suffix" if suffix else "prefix"
                    offenders.append(
                        f"{relative}/: {len(members)} sibling modules share the "
                        f"{kind} '{affix}' ({names}); that shared concept is a "
                        "package wearing a name affix — split them into a namespace "
                        "subpackage"
                    )
    return offenders


def name_cluster_error(repo_root: Path) -> str:
    offenders = name_cluster_errors(repo_root)
    if not offenders:
        return ""
    return "name-cluster policy violation(s):\n" + "\n".join(offenders)

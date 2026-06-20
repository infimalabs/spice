"""Repo shape opinions: namespace packages, path names, no generic splits.

These guards bite inside the package roots a repo declares in its tracked
`pyproject.toml` under `[tool.spice.policy] package_roots` (plus `tests/`
when present). Repos without a declaration skip them — the opinions are
Python-package-specific; the rest of the constitution still applies.
"""

from __future__ import annotations

import re
from fnmatch import fnmatch
from pathlib import Path

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


def configured_package_roots(repo_root: Path) -> list[Path]:
    # Explicit `[tool.spice.policy] package_roots` wins; otherwise derive the
    # roots from the project's own Python packaging config so a standard project
    # needs no spice-local declaration. Neither present -> no roots (skip).
    names = string_list(policy_table(repo_root).get("package_roots"))
    if not names:
        names = _derived_package_roots(repo_root)
    return [repo_root / name for name in names if (repo_root / name).is_dir()]


def _derived_package_roots(repo_root: Path) -> list[str]:
    """Top-level package roots inferred from `[tool.setuptools]` packaging.

    Supports an explicit `packages` list (keep the top-level segments) and the
    `packages.find` auto-discovery form (top-level dirs under `where` matching
    `include`/`exclude` that actually contain Python — so build artifacts like
    `*.egg-info` are not mistaken for packages).
    """
    tool = read_pyproject(repo_root).get("tool")
    setuptools = tool.get("setuptools") if isinstance(tool, dict) else None
    packages = setuptools.get("packages") if isinstance(setuptools, dict) else None
    if isinstance(packages, list):
        return _explicit_package_roots(packages)
    # Only auto-discover when the project explicitly opts into it; a bare
    # [project] table with no setuptools packaging config derives nothing.
    find = packages.get("find") if isinstance(packages, dict) else None
    if not isinstance(find, dict):
        return []
    return _find_package_roots(repo_root, find)


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
    offenders: list[str] = []
    for root in configured_package_roots(repo_root):
        offenders.extend(
            sorted(
                path.relative_to(repo_root).as_posix()
                for path in root.rglob("__init__.py")
            )
        )
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

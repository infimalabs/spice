"""Mutation testing study: changed-file behavioral constraint measurement.

The runner is intentionally small and incremental. It seeds a disposable
scratch checkout from the caller's effective worktree content, mutates selected
Python source files there one mutant at a time, runs pytest inside the scratch
root, and reports per-module scores plus tests that did not kill any selected
mutant. The caller's checkout is never written: Spice imports and operates its
own source concurrently with a self-study, so mutants must never be observable
outside the scratch root.
"""

from __future__ import annotations

import ast
import json
import re
import subprocess
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from types import MappingProxyType
from typing import Any

from spice.errors import SpiceError
from spice.gitprocess import run_git_command
from spice.procs import ProcessDeadlineExceeded, run_bounded_process_group
from spice.studies.scratch import scratch_checkout
from spice.studies.walk import is_excluded_path, is_test_path
from spice.toolprocess import run_tool_command

MUTATION_RATCHET_VERSION = 1
STANDING_MUTATION_RATCHET_PATH = Path("tests/mutation-ratchet.json")
DEFAULT_MAX_MUTANTS_PER_MODULE = 20
DEFAULT_MUTATION_TIMEOUT_SECONDS = 30
_FAILED_NODEID_RE = re.compile(r"(tests/[^\s:]+\.py::[^\s]+)")


@dataclass(frozen=True)
class MutationPoint:
    index: int
    line: int
    description: str


@dataclass(frozen=True)
class MutationResult:
    point: MutationPoint
    status: str
    killed_by: tuple[str, ...] = ()


@dataclass(frozen=True)
class ModuleMutationReport:
    path: str
    mutants: int
    killed: int
    survived: int
    timed_out: int
    results: tuple[MutationResult, ...]
    zero_constraint_tests: tuple[str, ...] = ()

    @property
    def score(self) -> float:
        constrained = self.killed + self.timed_out
        total = constrained + self.survived
        return constrained / total if total else 0.0


@dataclass(frozen=True)
class RatchetRegression:
    path: str
    baseline_score: float
    current_score: float


@dataclass(frozen=True)
class MutationStudy:
    reports: tuple[ModuleMutationReport, ...]
    ratchet_regressions: tuple[RatchetRegression, ...] = ()
    recovered_roots: tuple[str, ...] = ()


@dataclass(frozen=True)
class StandingMutationRatchet:
    targets: tuple[Path, ...]
    test_paths: tuple[Path, ...]
    max_mutants_per_module: int
    timeout_seconds: int
    equivalent_mutant_count: int
    low_information_mutant_count: int
    retained_zero_constraint_tests: Mapping[str, str]


def changed_python_paths(root: Path, *, baseline_ref: str = "HEAD") -> list[Path]:
    seen: set[str] = set()
    paths: list[Path] = []
    for command in (
        ["git", "diff", "--name-only", "--diff-filter=ACMR", baseline_ref],
        ["git", "diff", "--cached", "--name-only", "--diff-filter=ACMR", baseline_ref],
    ):
        result = run_git_command(
            command, cwd=root, capture_output=True, check=True, text=True
        )
        for raw in result.stdout.splitlines():
            rel = Path(raw.strip())
            if _is_mutation_target(rel, root=root) and rel.as_posix() not in seen:
                seen.add(rel.as_posix())
                paths.append(rel)
    return paths


def run_mutation_study(
    paths: list[Path],
    *,
    root: Path,
    test_paths: list[Path],
    max_mutants_per_module: int = DEFAULT_MAX_MUTANTS_PER_MODULE,
    timeout_seconds: int = DEFAULT_MUTATION_TIMEOUT_SECONDS,
    ratchet_path: Path | None = None,
) -> MutationStudy:
    targets = [path for path in paths if _is_mutation_target(path, root=root)]
    if not targets:
        return MutationStudy(reports=())
    # Every mutant executes inside a disposable scratch checkout seeded from
    # the caller's effective content; the caller's files stay byte-identical
    # through success, survivors, timeouts, baseline failure, and interrupts.
    with scratch_checkout(root) as (scratch_root, recovery):
        _ensure_baseline_tests_pass(
            scratch_root, test_paths, timeout_seconds=timeout_seconds
        )
        collected_tests = _collect_test_nodeids(scratch_root, test_paths)
        reports = tuple(
            _run_module_mutations(
                path,
                root=scratch_root,
                test_paths=test_paths,
                collected_tests=collected_tests,
                max_mutants=max_mutants_per_module,
                timeout_seconds=timeout_seconds,
            )
            for path in targets
        )
    regressions = _ratchet_regressions(reports, ratchet_path)
    return MutationStudy(
        reports=reports,
        ratchet_regressions=tuple(regressions),
        recovered_roots=recovery.removed,
    )


def write_ratchet(path: Path, reports: tuple[ModuleMutationReport, ...]) -> Path:
    standing = _existing_standing_ratchet(path)
    payload = {
        "version": MUTATION_RATCHET_VERSION,
        "modules": {
            report.path: {
                "score": report.score,
                "killed": report.killed,
                "survived": report.survived,
                "timed_out": report.timed_out,
                "mutants": report.mutants,
            }
            for report in reports
        },
    }
    if standing is not None:
        payload["standing"] = standing
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")
    return path


def load_standing_mutation_ratchet(path: Path) -> StandingMutationRatchet:
    try:
        loaded = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise SpiceError(f"invalid standing mutation ratchet {path}: {exc}") from exc
    if (
        not isinstance(loaded, dict)
        or loaded.get("version") != MUTATION_RATCHET_VERSION
    ):
        raise SpiceError(
            f"invalid standing mutation ratchet {path}: version must be "
            f"{MUTATION_RATCHET_VERSION}"
        )
    standing = loaded.get("standing")
    if not isinstance(standing, dict):
        raise SpiceError(
            f"invalid standing mutation ratchet {path}: standing policy is required"
        )
    surface = str(standing.get("surface") or "")
    if surface != "spice dev doctor":
        raise SpiceError(
            f"invalid standing mutation ratchet {path}: surface must be "
            "'spice dev doctor'"
        )
    return StandingMutationRatchet(
        targets=_standing_paths(path, standing, "targets"),
        test_paths=_standing_paths(path, standing, "tests"),
        max_mutants_per_module=_standing_positive_int(
            path, standing, "maxMutantsPerModule"
        ),
        timeout_seconds=_standing_positive_int(path, standing, "timeoutSeconds"),
        equivalent_mutant_count=_standing_handling_count(
            path, standing, "equivalentMutants"
        ),
        low_information_mutant_count=_standing_handling_count(
            path, standing, "lowInformationMutants"
        ),
        retained_zero_constraint_tests=MappingProxyType(
            _standing_reason_map(path, standing, "retainedZeroConstraintTests")
        ),
    )


def _existing_standing_ratchet(path: Path) -> dict[str, object] | None:
    if not path.is_file():
        return None
    try:
        loaded = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    standing = loaded.get("standing") if isinstance(loaded, dict) else None
    return dict(standing) if isinstance(standing, dict) else None


def _standing_paths(
    path: Path, standing: dict[str, object], field: str
) -> tuple[Path, ...]:
    raw = standing.get(field)
    values = tuple(Path(str(value)) for value in raw) if isinstance(raw, list) else ()
    if not values or any(
        value.is_absolute() or not value.as_posix() for value in values
    ):
        raise SpiceError(
            f"invalid standing mutation ratchet {path}: {field} must contain "
            "relative paths"
        )
    return values


def _standing_positive_int(path: Path, standing: dict[str, object], field: str) -> int:
    raw = standing.get(field)
    if isinstance(raw, bool) or not isinstance(raw, int) or raw <= 0:
        raise SpiceError(
            f"invalid standing mutation ratchet {path}: {field} must be positive"
        )
    return raw


def _standing_handling_count(
    path: Path, standing: dict[str, object], field: str
) -> int:
    raw = standing.get(field)
    records = raw if isinstance(raw, list) else []
    if not records or any(
        not isinstance(record, dict)
        or not str(record.get("path") or "")
        or isinstance(record.get("mutationIndex"), bool)
        or not isinstance(record.get("mutationIndex"), int)
        or not str(record.get("reason") or "")
        for record in records
    ):
        raise SpiceError(
            f"invalid standing mutation ratchet {path}: {field} must record "
            "path, mutationIndex, and reason"
        )
    return len(records)


def _standing_reason_map(
    path: Path, standing: dict[str, object], field: str
) -> dict[str, str]:
    raw = standing.get(field)
    reasons = (
        {str(key): str(value) for key, value in raw.items()}
        if isinstance(raw, dict)
        else {}
    )
    if not reasons or any(not key or not reason for key, reason in reasons.items()):
        raise SpiceError(
            f"invalid standing mutation ratchet {path}: {field} must map "
            "test node IDs to reasons"
        )
    return reasons


def render_mutation_board(study: MutationStudy) -> str:
    lines = ["mutation score"]
    if not study.reports:
        lines.append("  no Python source modules selected")
        return "\n".join(lines)
    lines.append("module | killed | survived | timeout | score")
    lines.append("--- | ---: | ---: | ---: | ---:")
    for report in study.reports:
        lines.append(
            f"{report.path} | {report.killed}/{report.mutants} | "
            f"{report.survived} | {report.timed_out} | {report.score:.0%}"
        )
    zero_rows = [
        (report.path, test)
        for report in study.reports
        for test in report.zero_constraint_tests
    ]
    if zero_rows:
        lines.append("")
        lines.append("zero-constraint tests")
        for path, test in zero_rows:
            lines.append(f"- {path}: {test}")
    if study.ratchet_regressions:
        lines.append("")
        lines.append("ratchet regressions")
        for regression in study.ratchet_regressions:
            lines.append(
                f"- {regression.path}: {regression.current_score:.0%} "
                f"< {regression.baseline_score:.0%}"
            )
    if study.recovered_roots:
        lines.append("")
        lines.append("recovered abandoned scratch roots")
        for name in study.recovered_roots:
            lines.append(f"- {name}")
    return "\n".join(lines)


def mutation_points_for_text(text: str) -> list[MutationPoint]:
    tree = ast.parse(text)
    collector = _MutationCollector()
    collector.visit(tree)
    return collector.points


def mutated_text(text: str, target_index: int) -> str:
    tree = ast.parse(text)
    transformer = _MutationApplier(target_index)
    mutated = transformer.visit(tree)
    if not transformer.applied:
        raise SpiceError(f"mutation index not found: {target_index}")
    ast.fix_missing_locations(mutated)
    return ast.unparse(mutated) + "\n"


def _run_module_mutations(
    path: Path,
    *,
    root: Path,
    test_paths: list[Path],
    collected_tests: set[str],
    max_mutants: int,
    timeout_seconds: int,
) -> ModuleMutationReport:
    abs_path = root / path
    original = abs_path.read_text(encoding="utf-8")
    points = mutation_points_for_text(original)[: max(0, max_mutants)]
    results: list[MutationResult] = []
    killed_by: set[str] = set()
    try:
        for point in points:
            abs_path.write_text(mutated_text(original, point.index), encoding="utf-8")
            result = _run_pytest(root, test_paths, timeout_seconds=timeout_seconds)
            if result is None:
                results.append(MutationResult(point=point, status="timeout"))
                continue
            if result.returncode == 0:
                results.append(MutationResult(point=point, status="survived"))
                continue
            failed = tuple(sorted(_failed_nodeids(result.stdout + result.stderr)))
            killed_by.update(failed)
            results.append(
                MutationResult(point=point, status="killed", killed_by=failed)
            )
    finally:
        abs_path.write_text(original, encoding="utf-8")
    killed = sum(1 for result in results if result.status == "killed")
    survived = sum(1 for result in results if result.status == "survived")
    timed_out = sum(1 for result in results if result.status == "timeout")
    zero_constraint = tuple(sorted(collected_tests - killed_by))
    return ModuleMutationReport(
        path=path.as_posix(),
        mutants=len(points),
        killed=killed,
        survived=survived,
        timed_out=timed_out,
        results=tuple(results),
        zero_constraint_tests=zero_constraint,
    )


def _ensure_baseline_tests_pass(
    root: Path, test_paths: list[Path], *, timeout_seconds: int
) -> None:
    result = _run_pytest(root, test_paths, timeout_seconds=timeout_seconds)
    if result is None:
        raise SpiceError("baseline pytest timed out before mutation testing")
    if result.returncode != 0:
        detail = (result.stdout + result.stderr).strip()
        raise SpiceError("baseline pytest must pass before mutation testing\n" + detail)


def _run_pytest(
    root: Path, test_paths: list[Path], *, timeout_seconds: int
) -> subprocess.CompletedProcess[str] | None:
    command = ["uv", "run", "pytest", "-q", *[path.as_posix() for path in test_paths]]
    try:
        return run_bounded_process_group(
            command,
            timeout_seconds=timeout_seconds,
            phase="tool.study",
            input_label="mutation baseline pytest",
            cwd=root,
            check=False,
            text=True,
        )
    except ProcessDeadlineExceeded:
        return None


def _collect_test_nodeids(root: Path, test_paths: list[Path]) -> set[str]:
    result = run_tool_command(
        [
            "uv",
            "run",
            "pytest",
            "--collect-only",
            "-q",
            *[path.as_posix() for path in test_paths],
        ],
        cwd=root,
        policy="study",
        operation="collect mutation test nodeids",
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        return set()
    return {
        line.strip()
        for line in result.stdout.splitlines()
        if "::" in line and line.strip().startswith("tests/")
    }


def _failed_nodeids(output: str) -> set[str]:
    return set(_FAILED_NODEID_RE.findall(output))


def _ratchet_regressions(
    reports: tuple[ModuleMutationReport, ...], ratchet_path: Path | None
) -> list[RatchetRegression]:
    if ratchet_path is None or not ratchet_path.is_file():
        return []
    try:
        loaded = json.loads(ratchet_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return []
    modules = loaded.get("modules")
    if not isinstance(modules, dict):
        return []
    regressions: list[RatchetRegression] = []
    for report in reports:
        raw = modules.get(report.path)
        if not isinstance(raw, dict):
            continue
        score_value = raw.get("score")
        if not isinstance(score_value, int | float | str):
            continue
        try:
            baseline_score = float(score_value)
        except ValueError:
            continue
        if report.score < baseline_score:
            regressions.append(
                RatchetRegression(
                    path=report.path,
                    baseline_score=baseline_score,
                    current_score=report.score,
                )
            )
    return regressions


def _is_mutation_target(path: Path, *, root: Path) -> bool:
    return (
        path.suffix == ".py"
        and not is_test_path(path, repo_root=root)
        and not is_excluded_path(path, repo_root=root)
        and (root / path).is_file()
    )


class _MutationCollector(ast.NodeVisitor):
    def __init__(self) -> None:
        self.points: list[MutationPoint] = []

    def visit_BinOp(self, node: ast.BinOp) -> Any:
        if type(node.op) in _BINOP_MUTATIONS:
            self._add(node, _BINOP_MUTATIONS[type(node.op)][1])
        self.generic_visit(node)

    def visit_BoolOp(self, node: ast.BoolOp) -> Any:
        if type(node.op) in _BOOLOP_MUTATIONS:
            self._add(node, _BOOLOP_MUTATIONS[type(node.op)][1])
        self.generic_visit(node)

    def visit_Compare(self, node: ast.Compare) -> Any:
        if node.ops and type(node.ops[0]) in _COMPARE_MUTATIONS:
            self._add(node, _COMPARE_MUTATIONS[type(node.ops[0])][1])
        self.generic_visit(node)

    def visit_Constant(self, node: ast.Constant) -> Any:
        if isinstance(node.value, bool):
            self._add(node, "flip boolean constant")
        elif isinstance(node.value, (int, float)):
            self._add(node, "nudge numeric constant")

    def _add(self, node: ast.AST, description: str) -> None:
        self.points.append(
            MutationPoint(
                index=len(self.points),
                line=getattr(node, "lineno", 0),
                description=description,
            )
        )


class _MutationApplier(ast.NodeTransformer):
    def __init__(self, target_index: int) -> None:
        self.target_index = target_index
        self.current_index = 0
        self.applied = False

    def visit_BinOp(self, node: ast.BinOp) -> ast.AST:
        mutation = _BINOP_MUTATIONS.get(type(node.op))
        if mutation is None:
            self.generic_visit(node)
            return node
        if not self._matches():
            self.generic_visit(node)
            return node
        node.op = mutation[0]()
        self.applied = True
        return node

    def visit_BoolOp(self, node: ast.BoolOp) -> ast.AST:
        mutation = _BOOLOP_MUTATIONS.get(type(node.op))
        if mutation is None:
            self.generic_visit(node)
            return node
        if not self._matches():
            self.generic_visit(node)
            return node
        node.op = mutation[0]()
        self.applied = True
        return node

    def visit_Compare(self, node: ast.Compare) -> ast.AST:
        if not node.ops:
            self.generic_visit(node)
            return node
        mutation = _COMPARE_MUTATIONS.get(type(node.ops[0]))
        if mutation is None:
            self.generic_visit(node)
            return node
        if not self._matches():
            self.generic_visit(node)
            return node
        node.ops[0] = mutation[0]()
        self.applied = True
        return node

    def visit_Constant(self, node: ast.Constant) -> ast.AST:
        if isinstance(node.value, bool):
            if not self._matches():
                return node
            self.applied = True
            return ast.copy_location(ast.Constant(value=not node.value), node)
        if isinstance(node.value, (int, float)):
            if not self._matches():
                return node
            self.applied = True
            delta = 1 if node.value >= 0 else -1
            return ast.copy_location(ast.Constant(value=node.value + delta), node)
        return node

    def _matches(self) -> bool:
        matched = self.current_index == self.target_index
        self.current_index += 1
        return matched


_BINOP_MUTATIONS: dict[type[ast.operator], tuple[type[ast.operator], str]] = {
    ast.Add: (ast.Sub, "replace + with -"),
    ast.Sub: (ast.Add, "replace - with +"),
    ast.Mult: (ast.Div, "replace * with /"),
    ast.Div: (ast.Mult, "replace / with *"),
}
_BOOLOP_MUTATIONS: dict[type[ast.boolop], tuple[type[ast.boolop], str]] = {
    ast.And: (ast.Or, "replace and with or"),
    ast.Or: (ast.And, "replace or with and"),
}
_COMPARE_MUTATIONS: dict[type[ast.cmpop], tuple[type[ast.cmpop], str]] = {
    ast.Eq: (ast.NotEq, "replace == with !="),
    ast.NotEq: (ast.Eq, "replace != with =="),
    ast.Lt: (ast.GtE, "replace < with >="),
    ast.LtE: (ast.Gt, "replace <= with >"),
    ast.Gt: (ast.LtE, "replace > with <="),
    ast.GtE: (ast.Lt, "replace >= with <"),
}

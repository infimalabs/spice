"""Whole-suite gate for a landing that reaches far past the tests it ran.

Per-lane verification is a subset twice over. An agent runs the tests it
believes its change touches, and that belief is a direct-import view of the
codebase -- for a shared module the direct view understates the real reach by
an order of magnitude. It then runs that subset against its own pinned
baseline, which is behind the branch by however much the other lanes landed
meanwhile. Both gaps close only on the integrated tree, and the integrated
tree exists exactly once: after the task merge is materialized and before it
is pushed.

So the gate lives there. A repo names the far-reaching paths under
``[policy.suite_seam]``; a task whose footprint touches one runs
``run`` -- the whole suite -- against the merged tree, and a red suite refuses
the publish. A task that touches nothing declared matches nothing and runs
nothing, so an ordinary landing keeps its own wall clock, and no commit
anywhere pays for this.
"""

from __future__ import annotations

import os
import shlex
import time
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from spice.config.layers import config_string_list, effective_table
from spice.errors import SpiceError
from spice.pathmatch import matches_repo_path_or_ancestor, normalize_repo_path
from spice.process.tool import run_tool_command
from spice.studies.reachabilitypython import (
    _direct_imports,
    _module_to_path,
    _path_to_module,
)
from spice.studies.walk import configured_test_roots

SUITE_SEAM_KEY = "suite_seam"
SUITE_SEAM_PATHS_KEY = "paths"
SUITE_SEAM_RUN_KEY = "run"
SUITE_SEAM_SECONDS_KEY = "seconds"
SUITE_SEAM_PACKAGE = "spice"
SUITE_SEAM_TEST_GLOB = "test_*.py"

UNDECLARED_REASON = "this repository declares no suite seam"
UNTOUCHED_REASON = "this task touches no declared suite seam"


@dataclass(frozen=True)
class SuiteSeamPlan:
    """What the gate decided about one task footprint, before running anything."""

    reason: str
    matches: tuple[str, ...]
    argv: tuple[str, ...]
    declared_seconds: int


@dataclass(frozen=True)
class SuiteSeamOutcome:
    """The measured result of a plan, whether or not it ran the suite."""

    plan: SuiteSeamPlan
    elapsed_seconds: float
    returncode: int
    output: str


@dataclass(frozen=True)
class ModuleReach:
    """How much of the test suite one package module holds."""

    module: str
    path: str
    reached_by: int
    imported_by: int
    declared: bool


@dataclass(frozen=True)
class ReachReport:
    """Every package module ranked by the share of the suite that reaches it."""

    test_modules: int
    ranked: tuple[ModuleReach, ...]

    @property
    def declared(self) -> tuple[ModuleReach, ...]:
        """The ranked entries whose path the seam table already names."""
        return tuple(entry for entry in self.ranked if entry.declared)

    @property
    def declared_floor(self) -> int:
        """The narrowest reach inside the declared band."""
        return min(entry.reached_by for entry in self.declared)

    @property
    def widest_undeclared(self) -> ModuleReach:
        """The module just outside the band -- the next path a maintainer weighs."""
        for entry in self.ranked:
            if not entry.declared:
                return entry
        raise SpiceError("every package module is already a declared suite seam")


@dataclass(frozen=True)
class _ImportGraph:
    """One package tree, with the edges already walked kept for reuse."""

    pkg_root: Path
    package: str
    edges: dict[str, set[str]]


def suite_seam_reach(repo_root: Path, package: str) -> ReachReport:
    """Rank ``package`` modules by how many test modules reach them by import.

    This is the measurement ``paths`` is chosen by, so its terms are fixed
    here. A test module is a collected ``test_*.py`` file under the configured
    test roots. Reach follows imports wherever they appear, including inside
    function bodies, because a deferred import binds the two modules just as
    tightly once the process runs. ``imported_by`` counts only the test modules
    that name the module themselves, which is the view a lane has of its own
    change and the reason the two numbers are worth printing side by side.
    """
    graph = _ImportGraph(repo_root / package, package, {})
    declared, _argv, _seconds = _suite_seam_config(repo_root)
    reached: dict[str, int] = {}
    imported: dict[str, int] = {}
    test_paths = sorted(
        path
        for test_root in configured_test_roots(repo_root)
        for path in test_root.rglob(SUITE_SEAM_TEST_GLOB)
    )
    for path in test_paths:
        direct = set(_direct_imports(path, graph.pkg_root, graph.package))
        for module in direct:
            imported[module] = imported.get(module, 0) + 1
        for module in _reached_modules(direct, graph):
            reached[module] = reached.get(module, 0) + 1
    return ReachReport(
        test_modules=len(test_paths),
        ranked=_ranked_modules(repo_root, graph, declared, reached, imported),
    )


def _ranked_modules(
    repo_root: Path,
    graph: _ImportGraph,
    declared: tuple[str, ...],
    reached: dict[str, int],
    imported: dict[str, int],
) -> tuple[ModuleReach, ...]:
    entries: list[ModuleReach] = []
    for path in sorted(graph.pkg_root.rglob("*.py")):
        module = _path_to_module(path, graph.pkg_root, graph.package)
        if module is None:
            continue
        relative = str(path.relative_to(repo_root))
        entries.append(
            ModuleReach(
                module=module,
                path=relative,
                reached_by=reached.get(module, 0),
                imported_by=imported.get(module, 0),
                # The same predicate the gate matches a footprint with, so a
                # seam declared as a directory marks the files under it here.
                declared=any(
                    matches_repo_path_or_ancestor(relative, seam) for seam in declared
                ),
            )
        )
    return tuple(sorted(entries, key=lambda entry: (-entry.reached_by, entry.module)))


def _reached_modules(direct: set[str], graph: _ImportGraph) -> set[str]:
    seen: set[str] = set()
    pending = list(direct)
    while pending:
        module = pending.pop()
        if module in seen:
            continue
        seen.add(module)
        pending.extend(_module_edges(module, graph))
    return seen


def _module_edges(module: str, graph: _ImportGraph) -> set[str]:
    if module not in graph.edges:
        path = _module_to_path(module, graph.pkg_root, graph.package)
        graph.edges[module] = (
            set(_direct_imports(path, graph.pkg_root, graph.package)) if path else set()
        )
    return graph.edges[module]


def render_suite_seam_reach(report: ReachReport, *, limit: int) -> list[str]:
    """The ranking a maintainer reads before adding a path to ``paths``.

    The header states the band the declaration currently claims: the narrowest
    reach inside it and the widest module left outside it. A declaration is
    defensible while those two are a strict break, so when the second catches
    the first the header says so and the rows below show where it happened.
    """
    declared = report.declared
    if not declared:
        return [
            f"suite-seam-reach: {report.test_modules} test module(s) rank "
            f"{len(report.ranked)} package module(s), none of them declared"
        ]
    next_up = report.widest_undeclared
    floor = report.declared_floor
    verdict = "a strict break" if floor > next_up.reached_by else "no longer a break"
    lines = [
        f"suite-seam-reach: {len(declared)} declared module(s) of "
        f"{len(report.ranked)}, reached by at least {floor} of "
        f"{report.test_modules} test module(s)",
        f"suite-seam-reach: {next_up.path} leads the undeclared rest at "
        f"{next_up.reached_by}, so the band is {verdict}",
    ]
    lines.extend(
        f"  {entry.reached_by:>5} reached {entry.imported_by:>5} imported  "
        f"{entry.path}{' [declared]' if entry.declared else ''}"
        for entry in report.ranked[:limit]
    )
    return lines


def suite_seam_plan(repo_root: Path, footprint: Sequence[Path | str]) -> SuiteSeamPlan:
    """Decide whether this task's footprint reaches the whole suite."""
    seams, argv, seconds = _suite_seam_config(repo_root)
    if not seams:
        return _idle_plan(UNDECLARED_REASON)
    matches = tuple(
        _render_match(normalize_repo_path(path), seam)
        for path in sorted(str(entry) for entry in footprint)
        for seam in seams
        if matches_repo_path_or_ancestor(path, seam)
    )
    if not matches:
        return _idle_plan(UNTOUCHED_REASON)
    return SuiteSeamPlan(
        reason=_reach_reason(len(matches)),
        matches=matches,
        argv=argv,
        declared_seconds=seconds,
    )


def run_suite_seam_gate(
    repo_root: Path, footprint: Sequence[Path | str], *, label: str
) -> SuiteSeamOutcome:
    """Run the whole suite over the integrated tree and refuse a red landing."""
    plan = suite_seam_plan(repo_root, footprint)
    if not plan.argv:
        return SuiteSeamOutcome(plan=plan, elapsed_seconds=0.0, returncode=0, output="")
    print(f"suite seam: {plan.reason}")
    for match in plan.matches:
        print(f"suite seam:   {match}")
    print(f"suite seam: running {shlex.join(plan.argv)} ({_cost_note(plan)})")
    outcome = _measure_suite(repo_root, plan)
    if outcome.returncode != 0:
        raise SpiceError(_red_suite_refusal(label, outcome))
    print(f"suite seam: the integrated tree is green after {_elapsed(outcome)}")
    return outcome


def _render_match(path: str, seam: str) -> str:
    if path == seam:
        return f"{path} is a declared suite seam"
    return f"{path} is under the declared suite seam {seam}"


def _reach_reason(count: int) -> str:
    subject = "1 landing path reaches" if count == 1 else f"{count} landing paths reach"
    pronoun = "it" if count == 1 else "them"
    return (
        f"{subject} the whole suite, so the tests that name {pronoun} leave the "
        "rest of the suite unverified"
    )


def _idle_plan(reason: str) -> SuiteSeamPlan:
    return SuiteSeamPlan(reason=reason, matches=(), argv=(), declared_seconds=0)


def _cost_note(plan: SuiteSeamPlan) -> str:
    if plan.declared_seconds:
        return f"declared cost {plan.declared_seconds}s"
    return "the whole suite, minutes rather than seconds"


def _elapsed(outcome: SuiteSeamOutcome) -> str:
    return f"{outcome.elapsed_seconds:.0f}s"


def _suite_env() -> dict[str, str]:
    """The suite is its own top-level command, not a continuation of this one.

    A gate reached through a re-executed `spice` already carries the marker
    recording that re-exec, and inheriting it would tell the suite command to
    skip its own -- starting it from whatever interpreter owns the ambient
    entry point, which refuses to run at all.
    """
    from spice.cli.entry import SELFEXEC_ENV

    env = dict(os.environ)  # env-policy: allow
    env.pop(SELFEXEC_ENV, None)
    return env


def _measure_suite(repo_root: Path, plan: SuiteSeamPlan) -> SuiteSeamOutcome:
    started = time.monotonic()
    result = run_tool_command(
        list(plan.argv),
        policy="suite",
        operation="run the whole suite over an integrated task landing",
        capture_output=True,
        text=True,
        cwd=repo_root,
        env=_suite_env(),
        check=False,
    )
    output = "\n".join(
        part for part in (result.stdout.strip(), result.stderr.strip()) if part
    )
    return SuiteSeamOutcome(
        plan=plan,
        elapsed_seconds=time.monotonic() - started,
        returncode=result.returncode,
        output=output,
    )


def _red_suite_refusal(label: str, outcome: SuiteSeamOutcome) -> str:
    plan = outcome.plan
    lines = [
        "refusing to publish: this task lands paths the whole suite depends on, "
        "and the whole suite is red on the integrated tree:",
        *(f"  {match}" for match in plan.matches),
        f"{shlex.join(plan.argv)} exited {outcome.returncode} after "
        f"{_elapsed(outcome)} ({_cost_note(plan)}).",
    ]
    if outcome.output:
        lines.append(outcome.output)
    lines += [
        "the merge is already in this tree, so the failures above are what the "
        "branch would have gotten:",
        "next commands:",
        "  fix the failures above and commit the fix",
        f'  spice task done {label} --validation "..."',
    ]
    return "\n".join(lines)


def _suite_seam_config(repo_root: Path) -> tuple[tuple[str, ...], tuple[str, ...], int]:
    raw = effective_table(repo_root, "policy").get(SUITE_SEAM_KEY)
    if raw is None:
        return (), (), 0
    if not isinstance(raw, dict):
        raise SpiceError(
            f"[policy.{SUITE_SEAM_KEY}] must be a table with "
            f"{SUITE_SEAM_PATHS_KEY!r} and {SUITE_SEAM_RUN_KEY!r}"
        )
    seams = tuple(
        normalize_repo_path(pattern)
        for pattern in config_string_list(raw.get(SUITE_SEAM_PATHS_KEY))
    )
    argv = _configured_argv(raw.get(SUITE_SEAM_RUN_KEY))
    if seams and not argv:
        raise SpiceError(
            f"[policy.{SUITE_SEAM_KEY}] declares {SUITE_SEAM_PATHS_KEY} "
            f"but no {SUITE_SEAM_RUN_KEY} command to cover them"
        )
    return seams, argv, _configured_seconds(raw.get(SUITE_SEAM_SECONDS_KEY))


def _configured_argv(raw: Any) -> tuple[str, ...]:
    """Read the suite command, keeping order and repeats a real argv depends on."""
    if raw is None:
        return ()
    if not isinstance(raw, Sequence) or isinstance(raw, (str, bytes)):
        raise SpiceError(
            f"[policy.{SUITE_SEAM_KEY}] {SUITE_SEAM_RUN_KEY} must be an argv list"
        )
    argv = tuple(str(item).strip() for item in raw)
    if any(not item for item in argv):
        raise SpiceError(
            f"[policy.{SUITE_SEAM_KEY}] {SUITE_SEAM_RUN_KEY} entries must "
            "be non-empty strings"
        )
    return argv


def _configured_seconds(raw: Any) -> int:
    if raw is None:
        return 0
    if not isinstance(raw, int) or isinstance(raw, bool) or raw <= 0:
        raise SpiceError(
            f"[policy.{SUITE_SEAM_KEY}] {SUITE_SEAM_SECONDS_KEY} must be a "
            "positive whole number of seconds"
        )
    return raw

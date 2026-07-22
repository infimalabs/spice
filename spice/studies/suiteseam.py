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
``[tool.spice.policy.suite_seam]``; a task whose footprint touches one runs
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

SUITE_SEAM_KEY = "suite_seam"
SUITE_SEAM_PATHS_KEY = "paths"
SUITE_SEAM_RUN_KEY = "run"
SUITE_SEAM_SECONDS_KEY = "seconds"

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
            f"[tool.spice.policy.{SUITE_SEAM_KEY}] must be a table with "
            f"{SUITE_SEAM_PATHS_KEY!r} and {SUITE_SEAM_RUN_KEY!r}"
        )
    seams = tuple(
        normalize_repo_path(pattern)
        for pattern in config_string_list(raw.get(SUITE_SEAM_PATHS_KEY))
    )
    argv = _configured_argv(raw.get(SUITE_SEAM_RUN_KEY))
    if seams and not argv:
        raise SpiceError(
            f"[tool.spice.policy.{SUITE_SEAM_KEY}] declares {SUITE_SEAM_PATHS_KEY} "
            f"but no {SUITE_SEAM_RUN_KEY} command to cover them"
        )
    return seams, argv, _configured_seconds(raw.get(SUITE_SEAM_SECONDS_KEY))


def _configured_argv(raw: Any) -> tuple[str, ...]:
    """Read the suite command, keeping order and repeats a real argv depends on."""
    if raw is None:
        return ()
    if not isinstance(raw, Sequence) or isinstance(raw, (str, bytes)):
        raise SpiceError(
            f"[tool.spice.policy.{SUITE_SEAM_KEY}] {SUITE_SEAM_RUN_KEY} must be an "
            "argv list"
        )
    argv = tuple(str(item).strip() for item in raw)
    if any(not item for item in argv):
        raise SpiceError(
            f"[tool.spice.policy.{SUITE_SEAM_KEY}] {SUITE_SEAM_RUN_KEY} entries must "
            "be non-empty strings"
        )
    return argv


def _configured_seconds(raw: Any) -> int:
    if raw is None:
        return 0
    if not isinstance(raw, int) or isinstance(raw, bool) or raw <= 0:
        raise SpiceError(
            f"[tool.spice.policy.{SUITE_SEAM_KEY}] {SUITE_SEAM_SECONDS_KEY} must be a "
            "positive whole number of seconds"
        )
    return raw

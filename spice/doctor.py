"""`spice doctor` — run every subsystem doctor and roll up one verdict.

The harness keeps two subsystem doctors: the environment/repo doctor behind
`spice dev doctor` (tools, runtime resolution, hooks, code-health gates) and the
task allocator-coherence doctor behind `spice task doctor`. Reaching for a
bare `spice doctor` is the natural instinct; this command answers it by running
both in a fixed order and exiting non-zero if either reports a problem, so one
command settles "is this worktree healthy?".
"""

from __future__ import annotations

import argparse
from pathlib import Path
from typing import Any

from spice.paths import require_repo_root


def configure_doctor_parser(subparsers: Any) -> None:
    doctor = subparsers.add_parser(
        "doctor",
        help="Run every subsystem doctor and roll up one health verdict.",
        description=(
            "Aggregate health check: runs the environment doctor "
            "(`spice dev doctor`) and the task allocator doctor "
            "(`spice task doctor`), then exits non-zero if either reports a "
            "problem."
        ),
    )
    doctor.add_argument(
        "--fix",
        action="store_true",
        help="Apply the environment doctor's safe generated-state repairs.",
    )
    doctor.set_defaults(func=handle_doctor)


def _environment_doctor_result(repo_root: Path, *, fix: bool) -> tuple[str, bool]:
    from spice.hooks.doctor import run_doctor

    report = run_doctor(repo_root, fix=fix)
    return report.render(), report.failed


def _render_task_doctor_report() -> tuple[str, list[str]]:
    from spice.tasks.render import render_doctor_report

    return render_doctor_report()


def handle_doctor(args: argparse.Namespace) -> int:
    repo_root = require_repo_root()

    environment_text, environment_failed = _environment_doctor_result(
        repo_root, fix=bool(args.fix)
    )
    task_text, task_problems = _render_task_doctor_report()

    print(environment_text)
    print()
    print("spice task doctor")
    for line in task_text.splitlines():
        print(f"  {line}")

    failed = environment_failed or bool(task_problems)
    print()
    print(f"doctor {'FAIL' if failed else 'ok'}")
    return 1 if failed else 0

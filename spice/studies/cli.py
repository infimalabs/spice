"""`spice study ...` — run the constitution's scans directly."""

from __future__ import annotations

import argparse
from pathlib import Path
from typing import Any

from spice.errors import SpiceError
from spice.paths import require_repo_root
from spice.policy import (
    COMPLEXITY_MAX_CCN,
    COMPLEXITY_MAX_LENGTH,
    FILE_BYTE_LIMIT,
    FILE_LOC_LIMIT,
    MAGIC_BASELINE_REF,
    MAGIC_EXAMINE_VALUE_THRESHOLD,
)
from spice.studies import (
    complexity,
    envpolicy,
    fileloc,
    magicnums,
    reachability,
    shape,
    testquality,
)
from spice.studies.walk import staged_paths, tracked_paths


def configure_study_parser(subparsers: Any) -> None:
    parser = subparsers.add_parser(
        "study", help="Code-health scans: file shape, complexity, magic numbers."
    )
    actions = parser.add_subparsers(dest="study_action", required=True)
    file_loc = _add_study_action(
        actions, "file-loc", "File line/byte pressure with flex + sticky limits."
    )
    file_loc.add_argument("--limit", type=int, default=FILE_LOC_LIMIT)
    file_loc.add_argument("--flex-limit", type=int, default=None)
    file_loc.add_argument("--byte-limit", type=int, default=FILE_BYTE_LIMIT)
    file_loc.add_argument("--byte-flex-limit", type=int, default=None)

    complexity_parser = _add_study_action(
        actions, "complexity", "Routine CCN/length pressure via lizard."
    )
    complexity_parser.add_argument("--max-ccn", type=int, default=COMPLEXITY_MAX_CCN)
    complexity_parser.add_argument(
        "--max-length", type=int, default=COMPLEXITY_MAX_LENGTH
    )
    complexity_parser.add_argument("--ccn-flex-limit", type=int, default=None)
    complexity_parser.add_argument("--length-flex-limit", type=int, default=None)

    magic = _add_study_action(
        actions, "magic-numbers", "Magic-number regressions vs a git baseline."
    )
    magic.add_argument("--baseline-ref", default=MAGIC_BASELINE_REF)
    magic.add_argument("--threshold", type=int, default=MAGIC_EXAMINE_VALUE_THRESHOLD)

    _add_study_action(
        actions, "env-policy", "Undeclared environment-variable literals."
    )
    _add_study_action(actions, "shape", "Namespace-package and path-shape policy.")
    reach = _add_study_action(
        actions,
        "reachability",
        "Test-only modules: code reachable from tests but not from production roots.",
    )
    reach.add_argument(
        "--allow",
        metavar="MODULE",
        action="append",
        dest="allowlist",
        default=[],
        help="Dotted module path to allow even if test-only (repeatable).",
    )
    reach.add_argument(
        "--create-tasks",
        action="store_true",
        help="Create a task for each test-only module (wire-in or delete-both).",
    )
    _add_study_action(
        actions,
        "assertion-free-tests",
        "Test functions that do not appear to assert behavior.",
    )


def _add_study_action(actions: Any, name: str, helptext: str) -> Any:
    sub = actions.add_parser(name, help=helptext)
    sub.add_argument("paths", nargs="*", type=Path)
    sub.add_argument("--staged", action="store_true", help="Scan staged files only.")
    sub.set_defaults(func=handle_study)
    return sub


def _target_paths(args: argparse.Namespace, root: Path) -> list[Path]:
    if args.staged and args.paths:
        raise SpiceError("pass --staged or explicit paths, not both")
    if args.staged:
        return staged_paths(root)
    if args.paths:
        return [_explicit_target_path(path, root) for path in args.paths]
    return tracked_paths(root)


def _explicit_target_path(path: Path, root: Path) -> Path:
    rel_path = path if not path.is_absolute() else path.relative_to(root)
    if (root / rel_path).is_dir():
        raise SpiceError(
            "explicit study paths must be file paths; "
            f"got directory: {rel_path.as_posix()}"
        )
    return rel_path


def handle_study(args: argparse.Namespace) -> int:
    root = require_repo_root()
    handler = _STUDY_ACTIONS.get(args.study_action)
    if handler is None:
        raise SpiceError(f"unknown study action {args.study_action!r}")
    return handler(args, root)


def _study_shape(args: argparse.Namespace, root: Path) -> int:
    errors = [
        error
        for error in (
            shape.namespace_policy_error(root),
            shape.path_shape_error(root),
            shape.name_cluster_error(root),
        )
        if error
    ]
    if errors:
        print("\n".join(errors))
        return 1
    print("shape: ok")
    return 0


def _study_file_loc(args: argparse.Namespace, root: Path) -> int:
    paths = _target_paths(args, root)
    scan = (
        fileloc.scan_staged_loc_violations
        if args.staged
        else fileloc.scan_loc_violations
    )
    findings = scan(
        paths,
        root=root,
        limit=args.limit,
        flex_limit_value=args.flex_limit,
        byte_limit=args.byte_limit,
        byte_flex_limit_value=args.byte_flex_limit,
    )
    print(
        fileloc.render_loc_board(
            findings,
            limit=args.limit,
            flex_limit_value=args.flex_limit,
            byte_limit=args.byte_limit,
            byte_flex_limit_value=args.byte_flex_limit,
        )
    )
    return 1 if findings else 0


def _study_complexity(args: argparse.Namespace, root: Path) -> int:
    findings = complexity.scan_staged_complexity_violations(
        _target_paths(args, root),
        root=root,
        max_ccn=args.max_ccn,
        max_length=args.max_length,
        ccn_flex_limit_value=args.ccn_flex_limit,
        length_flex_limit_value=args.length_flex_limit,
    )
    print(
        complexity.render_complexity_board(
            findings,
            max_ccn=args.max_ccn,
            max_length=args.max_length,
        )
    )
    return 1 if findings else 0


def _study_magic_numbers(args: argparse.Namespace, root: Path) -> int:
    findings = magicnums.detect_magic_regressions(
        _target_paths(args, root),
        root=root,
        baseline_ref=args.baseline_ref,
        examine_threshold=args.threshold,
    )
    print(magicnums.render_magic_board(findings, baseline_ref=args.baseline_ref))
    return 1 if findings else 0


def _study_env_policy(args: argparse.Namespace, root: Path) -> int:
    findings = envpolicy.scan_env_policy(_target_paths(args, root), root=root)
    print(envpolicy.render_env_policy_board(findings))
    return 1 if findings else 0


def _study_reachability(args: argparse.Namespace, root: Path) -> int:
    findings = reachability.scan_reachability(root, allowlist=args.allowlist)
    print("\n".join(reachability.render_reachability_board(findings)))
    if findings and getattr(args, "create_tasks", False):
        _create_exhaust_tasks(findings)
    return 1 if findings else 0


def _study_assertion_free_tests(args: argparse.Namespace, root: Path) -> int:
    findings = testquality.scan_assertion_free_tests(
        testquality.test_paths(root), root=root
    )
    print(testquality.render_assertion_free_board(findings))
    return 1 if findings else 0


def _create_exhaust_tasks(findings: list[reachability.ReachabilityFinding]) -> None:
    from spice.tasks import create

    for f in findings:
        handle = create.add(
            f"Exhaust: wire in or delete-both {f.module_path}",
            project="tests.exhaust",
            tags=["exhaust"],
            acceptance=[
                f"Module {f.module} is either wired into a production entry point "
                f"or deleted along with every test that imports it. "
                f"Imported only by: {', '.join(f.only_test_imports) or 'unknown'}."
            ],
        )
        print(f"  task created: {handle}")


_STUDY_ACTIONS = {
    "shape": _study_shape,
    "file-loc": _study_file_loc,
    "complexity": _study_complexity,
    "magic-numbers": _study_magic_numbers,
    "env-policy": _study_env_policy,
    "reachability": _study_reachability,
    "assertion-free-tests": _study_assertion_free_tests,
}

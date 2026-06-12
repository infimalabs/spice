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
from spice.studies import complexity, envpolicy, fileloc, magicnums, shape
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
        return [
            path if not path.is_absolute() else path.relative_to(root)
            for path in args.paths
        ]
    return tracked_paths(root)


def handle_study(args: argparse.Namespace) -> int:
    root = require_repo_root()
    action = args.study_action
    if action == "shape":
        errors = [
            error
            for error in (
                shape.namespace_policy_error(root),
                shape.path_shape_error(root),
            )
            if error
        ]
        if errors:
            print("\n".join(errors))
            return 1
        print("shape: ok")
        return 0
    paths = _target_paths(args, root)
    if action == "file-loc":
        findings = (
            fileloc.scan_staged_loc_violations(
                paths,
                root=root,
                limit=args.limit,
                flex_limit_value=args.flex_limit,
                byte_limit=args.byte_limit,
                byte_flex_limit_value=args.byte_flex_limit,
            )
            if args.staged
            else fileloc.scan_loc_violations(
                paths,
                root=root,
                limit=args.limit,
                flex_limit_value=args.flex_limit,
                byte_limit=args.byte_limit,
                byte_flex_limit_value=args.byte_flex_limit,
            )
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
    if action == "complexity":
        complexity_findings = complexity.scan_staged_complexity_violations(
            paths,
            root=root,
            max_ccn=args.max_ccn,
            max_length=args.max_length,
            ccn_flex_limit_value=args.ccn_flex_limit,
            length_flex_limit_value=args.length_flex_limit,
        )
        print(
            complexity.render_complexity_board(
                complexity_findings,
                max_ccn=args.max_ccn,
                max_length=args.max_length,
            )
        )
        return 1 if complexity_findings else 0
    if action == "magic-numbers":
        magic_findings = magicnums.detect_magic_regressions(
            paths,
            root=root,
            baseline_ref=args.baseline_ref,
            examine_threshold=args.threshold,
        )
        print(
            magicnums.render_magic_board(magic_findings, baseline_ref=args.baseline_ref)
        )
        return 1 if magic_findings else 0
    if action == "env-policy":
        env_findings = envpolicy.scan_env_policy(paths, root=root)
        print(envpolicy.render_env_policy_board(env_findings))
        return 1 if env_findings else 0
    raise SpiceError(f"unknown study action {action!r}")

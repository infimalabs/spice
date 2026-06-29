"""`spice study ...` — run the constitution's scans directly."""

from __future__ import annotations

import argparse
import json
from collections.abc import Mapping
from dataclasses import fields, is_dataclass
from pathlib import Path
from typing import Any

from spice.errors import SpiceError
from spice.paths import require_repo_root
from spice.policyconfig import resolve_policy
from spice.policy import (
    COMPLEXITY_MAX_CCN,
    COMPLEXITY_MAX_LENGTH,
    FILE_BYTE_LIMIT,
    FILE_LOC_LIMIT,
)
from spice.studies import (
    complexity,
    csharpmembers,
    csharpunused,
    envpolicy,
    fileloc,
    javascriptunused,
    magicnums,
    mutations,
    reachability,
    repodocs,
    shape,
    subsumption,
    testquality,
)
from spice.studies.walk import changed_paths, staged_paths, tracked_paths


def configure_study_parser(subparsers: Any) -> None:
    parser = subparsers.add_parser(
        "study", help="Code-health scans: file shape, complexity, magic numbers."
    )
    actions = parser.add_subparsers(dest="study_action", required=True)
    file_loc = _add_study_action(
        actions, "file-loc", "File line/byte pressure with flex + sticky limits."
    )
    file_loc.add_argument(
        "--baseline-ref",
        default=None,
        help="Scan files changed against this git ref instead of all tracked files.",
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
    complexity_parser.add_argument(
        "--baseline-ref",
        default=None,
        help="Scan files changed against this git ref instead of all tracked files.",
    )
    complexity_parser.add_argument("--ccn-flex-limit", type=int, default=None)
    complexity_parser.add_argument("--length-flex-limit", type=int, default=None)

    hotspots = _add_study_action(
        actions,
        "complexity-hotspots",
        "Top routine complexity hotspots over existing lizard data.",
    )
    hotspots.add_argument(
        "--limit",
        type=_positive_int_arg,
        default=None,
        help="Number of worst routines to show; defaults to tracked policy config.",
    )

    csharp_members = _add_study_action(
        actions, "csharp-members", "Rank C# class members by parsed source length."
    )
    csharp_members.add_argument(
        "--class-name",
        help="Optional exact class name to isolate when a file contains multiple classes.",
    )
    csharp_members.add_argument(
        "--limit",
        type=_positive_int_arg,
        default=csharpmembers.DEFAULT_MEMBER_LIMIT,
        help="Number of longest/tail members to show per class.",
    )

    csharp_unused = _add_study_action(
        actions,
        "csharp-unused-candidates",
        "Report C# private member and using-alias unused candidates.",
    )
    csharp_unused.add_argument(
        "--limit",
        type=_positive_int_arg,
        default=None,
        help="Number of candidate rows to show.",
    )

    magic = _add_study_action(
        actions, "magic-numbers", "Magic-number regressions vs a git baseline."
    )
    magic.add_argument("--baseline-ref", default=None)
    magic.add_argument("--threshold", type=int, default=None)

    javascript = _add_study_action(
        actions,
        "javascript-unused",
        "Unused top-level JavaScript symbols via tree-sitter.",
    )
    javascript.add_argument(
        "--allow-symbol",
        action="append",
        dest="allow_symbols",
        default=[],
        help="Top-level JavaScript symbol to retain even without references.",
    )
    javascript.add_argument(
        "--limit",
        type=_positive_int_arg,
        default=None,
        help="Number of candidate rows to show.",
    )

    _configure_mutation_parser(actions)

    env_policy = _add_study_action(
        actions, "env-policy", "Undeclared environment-variable literals."
    )
    env_policy.add_argument(
        "--write-baseline",
        type=Path,
        default=None,
        help="Write current env-policy findings to a JSON baseline file.",
    )
    _add_study_action(
        actions,
        "env-name-ledger",
        "Exact environment-variable name manifest accounting.",
    )
    _add_study_action(actions, "shape", "Namespace-package and path-shape policy.")
    _configure_reachability_parser(actions)
    _configure_symbol_reachability_parser(actions)
    _configure_subsumption_parser(actions)
    _configure_assertion_free_parser(actions)
    _configure_private_internals_parser(actions)


def _configure_mutation_parser(actions: Any) -> None:
    mutation = _add_study_action(
        actions,
        "mutations",
        "Incremental Python mutation testing for test effectiveness.",
    )
    mutation.add_argument(
        "--baseline-ref",
        default="HEAD",
        help="Git ref for default changed-file selection.",
    )
    mutation.add_argument(
        "--max-mutants",
        type=int,
        default=mutations.DEFAULT_MAX_MUTANTS_PER_MODULE,
        help="Maximum mutants to run per selected module.",
    )
    mutation.add_argument(
        "--timeout",
        type=int,
        default=mutations.DEFAULT_MUTATION_TIMEOUT_SECONDS,
        help="Per-mutant pytest timeout in seconds.",
    )
    mutation.add_argument(
        "--test",
        action="append",
        type=Path,
        default=[],
        help="Test file/path to run. Repeat for multiple test targets.",
    )
    mutation.add_argument(
        "--ratchet",
        type=Path,
        help="Compare scores against a mutation ratchet JSON file.",
    )
    mutation.add_argument(
        "--write-ratchet",
        type=Path,
        help="Write current scores to a mutation ratchet JSON file.",
    )


def _configure_reachability_parser(actions: Any) -> None:
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
        help="Create tagged decision tasks for each test-only reachability finding.",
    )
    reach.add_argument(
        "--limit",
        type=_positive_int_arg,
        default=None,
        help="Number of findings to show.",
    )


def _configure_symbol_reachability_parser(actions: Any) -> None:
    symbol = _add_study_action(
        actions,
        "symbol-reachability",
        "Test-only symbols inside production-reachable modules.",
    )
    symbol.add_argument(
        "--create-tasks",
        action="store_true",
        help="Create tagged decision tasks for each test-only symbol finding.",
    )
    symbol.add_argument(
        "--limit",
        type=_positive_int_arg,
        default=None,
        help="Number of findings to show.",
    )


def _configure_assertion_free_parser(actions: Any) -> None:
    assertion = _add_study_action(
        actions,
        "assertion-free-tests",
        "Test functions that do not appear to assert behavior.",
    )
    assertion.add_argument(
        "--create-tasks",
        action="store_true",
        help="Create tagged decision tasks for each assertion-free test.",
    )
    assertion.add_argument(
        "--limit",
        type=_positive_int_arg,
        default=None,
        help="Number of findings to show.",
    )


def _configure_private_internals_parser(actions: Any) -> None:
    private = _add_study_action(
        actions,
        "private-internals",
        "Tests coupled to private imports or internal assertion structures.",
    )
    private.add_argument(
        "--create-tasks",
        action="store_true",
        help="Create tagged decision tasks for unmanaged private/internal coupling.",
    )
    private.add_argument(
        "--limit",
        type=_positive_int_arg,
        default=None,
        help="Number of offender findings to show.",
    )


def _configure_subsumption_parser(actions: Any) -> None:
    sub_parser = actions.add_parser(
        "subsumption",
        help=(
            "Subsumed tests: tests covering no unique line vs. another test."
            " Requires a .coverage file recorded with --cov-context=test."
        ),
    )
    sub_parser.add_argument(
        "coverage_file",
        type=Path,
        help=".coverage SQLite file; generate with: pytest --cov=<pkg> --cov-context=test",
    )
    sub_parser.add_argument(
        "--package",
        metavar="PREFIX",
        default=None,
        help="Only consider source files under this package prefix.",
    )
    sub_parser.add_argument("--json", action="store_true", dest="emit_json")
    sub_parser.set_defaults(func=handle_study, study_action="subsumption")


def _add_study_action(actions: Any, name: str, helptext: str) -> Any:
    sub = actions.add_parser(name, help=helptext)
    sub.add_argument("paths", nargs="*", type=Path)
    sub.add_argument("--staged", action="store_true", help="Scan staged files only.")
    sub.add_argument("--json", action="store_true", dest="emit_json")
    sub.set_defaults(func=handle_study)
    return sub


def _positive_int_arg(raw: str) -> int:
    try:
        value = int(raw)
    except ValueError as exc:
        raise argparse.ArgumentTypeError("must be a positive integer") from exc
    if value <= 0:
        raise argparse.ArgumentTypeError("must be a positive integer")
    return value


def _target_paths(args: argparse.Namespace, root: Path) -> list[Path]:
    if args.staged and args.paths:
        raise SpiceError("pass --staged or explicit paths, not both")
    if args.staged:
        return staged_paths(root)
    if args.paths:
        return [_explicit_target_path(path, root) for path in args.paths]
    return tracked_paths(root)


def _changed_or_tracked_paths(args: argparse.Namespace, root: Path) -> list[Path]:
    baseline_ref = getattr(args, "baseline_ref", None)
    selected = sum(bool(value) for value in (args.staged, args.paths, baseline_ref))
    if selected > 1:
        raise SpiceError("pass --staged, --baseline-ref, or explicit paths")
    if args.staged:
        return staged_paths(root)
    if args.paths:
        return [_explicit_target_path(path, root) for path in args.paths]
    if baseline_ref:
        return changed_paths(root, baseline_ref)
    return tracked_paths(root)


def _mutation_target_paths(args: argparse.Namespace, root: Path) -> list[Path]:
    if args.staged and args.paths:
        raise SpiceError("pass --staged or explicit paths, not both")
    if args.paths:
        return [_explicit_target_path(path, root) for path in args.paths]
    if args.staged:
        return staged_paths(root, "*.py")
    return mutations.changed_python_paths(root, baseline_ref=args.baseline_ref)


def _explicit_target_path(path: Path, root: Path) -> Path:
    rel_path = path if not path.is_absolute() else path.relative_to(root)
    if (root / rel_path).is_dir():
        raise SpiceError(
            "explicit study paths must be file paths; "
            f"got directory: {rel_path.as_posix()}"
        )
    return rel_path


def _test_target_path(path: Path, root: Path) -> Path:
    return path if not path.is_absolute() else path.relative_to(root)


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
    if args.emit_json:
        _print_study_json(args.study_action, errors=errors)
        return 1 if errors else 0
    if errors:
        print("\n".join(errors))
        return 1
    print("shape: ok")
    return 0


def _study_file_loc(args: argparse.Namespace, root: Path) -> int:
    resolved = resolve_policy(root)
    paths = _changed_or_tracked_paths(args, root)
    scan = (
        fileloc.scan_staged_loc_violations
        if args.staged
        else fileloc.scan_loc_violations
    )
    generated_patterns = (
        *resolved.file_shape_paths.generated_patterns,
        *shape.generated_path_patterns(root),
    )
    findings = scan(
        paths,
        root=root,
        limit=args.limit,
        flex_limit_value=args.flex_limit,
        byte_limit=args.byte_limit,
        byte_flex_limit_value=args.byte_flex_limit,
        source_suffixes=resolved.file_shape_paths.source_suffixes,
        generated_patterns=generated_patterns,
        repo_doc_paths=set(repodocs.repo_truth_doc_candidate_paths(root, resolved)),
        lockfile_suffixes=resolved.lockfiles.suffixes,
        lockfile_names=resolved.lockfiles.names,
    )
    if args.emit_json:
        _print_study_json(
            args.study_action,
            findings=findings,
            lineLimit=args.limit,
            lineFlexLimit=args.flex_limit,
            byteLimit=args.byte_limit,
            byteFlexLimit=args.byte_flex_limit,
            baselineRef=args.baseline_ref,
            staged=args.staged,
        )
        return 1 if findings else 0
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
    resolved = resolve_policy(root)
    findings = complexity.scan_staged_complexity_violations(
        _changed_or_tracked_paths(args, root),
        root=root,
        max_ccn=args.max_ccn,
        max_length=args.max_length,
        ccn_flex_limit_value=args.ccn_flex_limit,
        length_flex_limit_value=args.length_flex_limit,
        suffixes=resolved.languages.complexity,
    )
    if args.emit_json:
        _print_study_json(
            args.study_action,
            findings=findings,
            maxCcn=args.max_ccn,
            maxLength=args.max_length,
            ccnFlexLimit=args.ccn_flex_limit,
            lengthFlexLimit=args.length_flex_limit,
            baselineRef=args.baseline_ref,
            staged=args.staged,
        )
        return 1 if findings else 0
    print(
        complexity.render_complexity_board(
            findings,
            max_ccn=args.max_ccn,
            max_length=args.max_length,
        )
    )
    return 1 if findings else 0


def _study_complexity_hotspots(args: argparse.Namespace, root: Path) -> int:
    resolved = resolve_policy(root)
    limit = args.limit or resolved.complexity.hotspot_limit
    records = complexity.collect_complexity_records(
        _target_paths(args, root), root=root, suffixes=resolved.languages.complexity
    )
    if args.emit_json:
        _print_study_json(
            args.study_action,
            hotspots=complexity.complexity_hotspot_rows(records, limit=limit),
            totalRecords=len(records),
            limit=limit,
        )
        return 0
    print(complexity.render_complexity_hotspots(records, limit=limit))
    return 0


def _study_csharp_members(args: argparse.Namespace, root: Path) -> int:
    records = csharpmembers.collect_csharp_class_records(
        _target_paths(args, root), root=root, class_name=args.class_name
    )
    if args.emit_json:
        print(csharpmembers.render_csharp_members_json(records))
    else:
        print(csharpmembers.render_csharp_members_board(records, limit=args.limit))
    return 0


def _study_csharp_unused_candidates(args: argparse.Namespace, root: Path) -> int:
    entries = csharpunused.collect_csharp_unused_entries(
        _target_paths(args, root), root=root
    )
    if args.emit_json:
        print(csharpunused.render_csharp_unused_json(entries))
    else:
        print(csharpunused.render_csharp_unused_board(entries, limit=args.limit))
    return 0


def _study_magic_numbers(args: argparse.Namespace, root: Path) -> int:
    resolved = resolve_policy(root)
    baseline_ref = args.baseline_ref or resolved.magic.baseline_ref
    threshold = (
        args.threshold
        if args.threshold is not None
        else resolved.magic.examine_threshold
    )
    threshold_for_path = (
        None
        if args.threshold is not None
        else resolved.magic_examine_threshold_for_path
    )
    findings = magicnums.detect_magic_regressions(
        _target_paths(args, root),
        root=root,
        baseline_ref=baseline_ref,
        examine_threshold=threshold,
        examine_threshold_for_path=threshold_for_path,
        suffixes=resolved.languages.magic,
        c_grammar_suffixes=resolved.languages.c_grammar,
    )
    if args.emit_json:
        _print_study_json(
            args.study_action,
            findings=findings,
            baselineRef=baseline_ref,
            threshold=threshold,
        )
        return 1 if findings else 0
    print(magicnums.render_magic_board(findings, baseline_ref=baseline_ref))
    return 1 if findings else 0


def _study_javascript_unused(args: argparse.Namespace, root: Path) -> int:
    findings = javascriptunused.scan_javascript_unused_symbols(
        _target_paths(args, root),
        root=root,
        allow_symbols=args.allow_symbols,
    )
    if args.emit_json:
        _print_study_json(
            args.study_action,
            findings=findings,
            allowSymbols=args.allow_symbols,
            limit=args.limit,
        )
        return 0
    print(javascriptunused.render_javascript_unused_board(findings, limit=args.limit))
    return 0


def _study_mutations(args: argparse.Namespace, root: Path) -> int:
    test_paths = [_test_target_path(path, root) for path in args.test] or [
        Path("tests")
    ]
    ratchet_path = root / args.ratchet if args.ratchet else None
    study = mutations.run_mutation_study(
        _mutation_target_paths(args, root),
        root=root,
        test_paths=test_paths,
        max_mutants_per_module=args.max_mutants,
        timeout_seconds=args.timeout,
        ratchet_path=ratchet_path,
    )
    if args.write_ratchet:
        mutations.write_ratchet(root / args.write_ratchet, study.reports)
    if args.emit_json:
        _print_study_json(
            args.study_action,
            reports=_mutation_reports_payload(study.reports),
            ratchetRegressions=study.ratchet_regressions,
        )
        return 1 if study.ratchet_regressions else 0
    print(mutations.render_mutation_board(study))
    return 1 if study.ratchet_regressions else 0


def _study_env_policy(args: argparse.Namespace, root: Path) -> int:
    resolved = resolve_policy(root)
    paths = _target_paths(args, root)
    findings = envpolicy.scan_env_policy(
        paths,
        root=root,
        suffixes=resolved.languages.env,
        apply_baseline=args.write_baseline is None,
    )
    if args.write_baseline is not None:
        baseline_path = root / args.write_baseline
        envpolicy.write_env_policy_baseline(baseline_path, findings)
        if args.emit_json:
            _print_study_json(
                args.study_action,
                baselinePath=args.write_baseline.as_posix(),
                findingCount=len(findings),
                findings=findings,
            )
        else:
            print(
                "env-policy: wrote "
                f"{len(findings)} baseline entr"
                f"{'y' if len(findings) == 1 else 'ies'} to "
                f"{args.write_baseline.as_posix()}"
            )
        return 0
    if args.emit_json:
        _print_study_json(args.study_action, findings=findings)
        return 1 if findings else 0
    print(envpolicy.render_env_policy_board(findings))
    return 1 if findings else 0


def _study_env_name_ledger(args: argparse.Namespace, root: Path) -> int:
    resolved = resolve_policy(root)
    findings = envpolicy.scan_env_name_ledger(
        _target_paths(args, root), root=root, suffixes=resolved.languages.env
    )
    if args.emit_json:
        _print_study_json(args.study_action, findings=findings)
        return 1 if findings else 0
    print(envpolicy.render_env_name_ledger_board(findings))
    return 1 if findings else 0


def _study_reachability(args: argparse.Namespace, root: Path) -> int:
    findings = reachability.scan_reachability(root, allowlist=args.allowlist)
    created_tasks: list[str] = []
    if findings and getattr(args, "create_tasks", False):
        created_tasks = _create_exhaust_tasks(
            findings, print_created=not args.emit_json
        )
    if args.emit_json:
        _print_study_json(
            args.study_action,
            findings=findings,
            createdTasks=created_tasks,
            allowlist=args.allowlist,
            limit=args.limit,
        )
        return 1 if findings else 0
    print("\n".join(reachability.render_reachability_board(findings, limit=args.limit)))
    return 1 if findings else 0


def _study_symbol_reachability(args: argparse.Namespace, root: Path) -> int:
    findings = reachability.scan_symbol_reachability(root)
    created_tasks: list[str] = []
    if findings and getattr(args, "create_tasks", False):
        created_tasks = _create_symbol_reachability_tasks(
            findings, print_created=not args.emit_json
        )
    if args.emit_json:
        _print_study_json(
            args.study_action,
            findings=findings,
            createdTasks=created_tasks,
            limit=args.limit,
        )
        return 1 if findings else 0
    print(
        "\n".join(
            reachability.render_symbol_reachability_board(findings, limit=args.limit)
        )
    )
    return 1 if findings else 0


def _study_assertion_free_tests(args: argparse.Namespace, root: Path) -> int:
    findings = testquality.scan_assertion_free_tests(
        testquality.test_paths(root), root=root
    )
    created_tasks: list[str] = []
    if findings and getattr(args, "create_tasks", False):
        created_tasks = _create_assertion_free_tasks(
            findings, print_created=not args.emit_json
        )
    if args.emit_json:
        _print_study_json(
            args.study_action,
            findings=findings,
            createdTasks=created_tasks,
            limit=args.limit,
        )
        return 1 if findings else 0
    print(testquality.render_assertion_free_board(findings, limit=args.limit))
    return 1 if findings else 0


def _study_private_internals(args: argparse.Namespace, root: Path) -> int:
    from spice.policy import LEGITIMATE_INTERNAL_COUPLINGS

    findings = testquality.scan_private_internal_coupling(
        testquality.test_paths(root), root=root
    )
    offenders, stale = testquality.unmanaged_private_internal_couplings(
        findings,
        repo_root=root,
        built_in_couplings=LEGITIMATE_INTERNAL_COUPLINGS,
    )
    created_tasks: list[str] = []
    if (offenders or stale) and getattr(args, "create_tasks", False):
        created_tasks = _create_private_internal_tasks(
            offenders, stale, print_created=not args.emit_json
        )
    if args.emit_json:
        _print_study_json(
            args.study_action,
            offenders=offenders,
            stale=stale,
            createdTasks=created_tasks,
            limit=args.limit,
        )
        return 1 if offenders or stale else 0
    print(
        testquality.render_unmanaged_private_internal_board(
            offenders[: args.limit] if args.limit is not None else offenders,
            stale,
        )
    )
    return 1 if offenders or stale else 0


def _create_exhaust_tasks(
    findings: list[reachability.ReachabilityFinding],
    *,
    print_created: bool = True,
) -> list[str]:
    from spice.tasks import create

    handles: list[str] = []
    for f in findings:
        handle = create.add(
            f"Exhaust decision: wire-in/delete-both {f.path}",
            project="tests.exhaust",
            tags=["exhaust", "decision", "wire_in_delete_both"],
            acceptance=[
                f"Resolve {f.provider} {f.kind} {f.subject} by either wiring it "
                f"into a production entry point or deleting {f.path} along with "
                "every test that imports it.",
                f"Current test-only importers: "
                f"{', '.join(f.only_test_imports) or 'unknown'}.",
            ],
        )
        handles.append(handle)
        if print_created:
            print(f"  task created: {handle}")
    return handles


def _create_symbol_reachability_tasks(
    findings: list[reachability.SymbolReachabilityFinding],
    *,
    print_created: bool = True,
) -> list[str]:
    from spice.tasks import create

    handles: list[str] = []
    for f in findings:
        handle = create.add(
            f"Symbol reachability decision: wire-in/delete {f.module}.{f.symbol}",
            project="tests.exhaust",
            tags=["exhaust", "symbol-reachability", "decision"],
            acceptance=[
                f"Resolve {f.provider} {f.kind} {f.module}.{f.symbol} by wiring it "
                "into production reachability, deleting the symbol and tests that "
                "only import it, or documenting a reviewed allowlist when dynamic "
                "production reachability cannot be made explicit.",
                "Current test-only importers: "
                f"{', '.join(f.only_test_imports) or 'unknown'}.",
            ],
        )
        handles.append(handle)
        if print_created:
            print(f"  task created: {handle}")
    return handles


def _create_assertion_free_tasks(
    findings: list[testquality.AssertionFreeTestFinding],
    *,
    print_created: bool = True,
) -> list[str]:
    from spice.tasks import create

    handles: list[str] = []
    for f in findings:
        handle = create.add(
            f"Assertion decision: constrain/delete {f.path}:{f.test_name}",
            project="tests.quality",
            tags=["test-quality", "assertion-free", "decision"],
            acceptance=[
                f"Resolve assertion-free test {f.path}:{f.line} {f.test_name} by "
                "adding an assertion that constrains behavior or deleting the test "
                "if it carries no useful signal."
            ],
        )
        handles.append(handle)
        if print_created:
            print(f"  task created: {handle}")
    return handles


def _create_private_internal_tasks(
    offenders: list[testquality.PrivateInternalCouplingFinding],
    stale: list[testquality.InternalCouplingKey],
    *,
    print_created: bool = True,
) -> list[str]:
    from spice.tasks import create

    handles: list[str] = []
    for f in offenders:
        handle = create.add(
            f"Private coupling decision: resolve {f.path}:{f.test_name}",
            project="tests.quality",
            tags=["test-quality", "private-internals", "decision"],
            acceptance=[
                f"Resolve private/internal coupling {f.kind} {f.target} in "
                f"{f.path}:{f.line} {f.test_name} by asserting through public "
                "behavior, moving the seam into production API, or documenting a "
                "reviewed policy exception."
            ],
        )
        handles.append(handle)
        if print_created:
            print(f"  task created: {handle}")
    for path, test_name, target in stale:
        handle = create.add(
            f"Private coupling cleanup: remove stale exception {path}:{test_name}",
            project="tests.quality",
            tags=["test-quality", "private-internals", "cleanup"],
            acceptance=[
                f"Remove stale [tool.spice.policy] internal_couplings entry for "
                f"{path} {test_name} {target}, or restore the reviewed coupling if "
                "it is still required."
            ],
        )
        handles.append(handle)
        if print_created:
            print(f"  task created: {handle}")
    return handles


def _study_subsumption(args: argparse.Namespace, root: Path) -> int:
    report = subsumption.scan_subsumption(
        args.coverage_file,
        package_prefix=args.package,
    )
    if args.emit_json:
        _print_study_json(args.study_action, report=report)
        return 1 if report.findings else 0
    print("\n".join(subsumption.render_subsumption_board(report)))
    return 1 if report.findings else 0


def _mutation_reports_payload(
    reports: tuple[mutations.ModuleMutationReport, ...],
) -> list[Mapping[str, object]]:
    payload: list[Mapping[str, object]] = []
    for report in reports:
        item = _json_ready(report)
        if not isinstance(item, dict):
            raise TypeError("mutation report payload must be a JSON object")
        item["score"] = report.score
        payload.append(item)
    return payload


def _print_study_json(study_action: str, **payload: object) -> None:
    print(
        json.dumps(
            _json_ready(
                {
                    "artifactKind": f"spice.study.{study_action}",
                    **payload,
                }
            ),
            indent=2,
            sort_keys=True,
        )
    )


def _json_ready(value: object) -> object:
    if is_dataclass(value) and not isinstance(value, type):
        return {
            field.name: _json_ready(getattr(value, field.name))
            for field in fields(value)
        }
    if isinstance(value, Path):
        return value.as_posix()
    if isinstance(value, Mapping):
        return {str(key): _json_ready(item) for key, item in value.items()}
    if isinstance(value, (list, tuple, set, frozenset)):
        return [_json_ready(item) for item in value]
    if value is None or isinstance(value, (str, int, float, bool)):
        return value
    raise TypeError(f"unsupported JSON payload value: {type(value).__name__}")


_STUDY_ACTIONS = {
    "shape": _study_shape,
    "file-loc": _study_file_loc,
    "complexity": _study_complexity,
    "complexity-hotspots": _study_complexity_hotspots,
    "csharp-members": _study_csharp_members,
    "csharp-unused-candidates": _study_csharp_unused_candidates,
    "magic-numbers": _study_magic_numbers,
    "javascript-unused": _study_javascript_unused,
    "mutations": _study_mutations,
    "env-policy": _study_env_policy,
    "env-name-ledger": _study_env_name_ledger,
    "reachability": _study_reachability,
    "symbol-reachability": _study_symbol_reachability,
    "assertion-free-tests": _study_assertion_free_tests,
    "private-internals": _study_private_internals,
    "subsumption": _study_subsumption,
}

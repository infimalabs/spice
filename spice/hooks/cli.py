"""`spice init` and `spice dev ...` — repo bootstrap and the hook backends."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

from spice import paths as spice_paths
from spice.cli.withdrawn import add_withdrawn_dry_run_argument
from spice.errors import SpiceError
from spice.operatorstate import OPERATOR_STATE_RELOCATION_RELEASE
from spice.paths import require_repo_root

DEINIT_WITHDRAWAL_RELEASE = OPERATOR_STATE_RELOCATION_RELEASE


def repo_root_from_cwd(cwd: Path | None = None) -> Path | None:
    """Resolve through the live paths module so test/config overrides cannot leak."""
    return (
        spice_paths.repo_root_from_cwd()
        if cwd is None
        else spice_paths.repo_root_from_cwd(cwd)
    )


def configure_dev_parser(subparsers: Any) -> None:
    _configure_initialization_parsers(subparsers)

    parser = subparsers.add_parser(
        "dev",
        help="Hook backends and environment plumbing.",
        description=(
            "`pre-commit`, `commit-msg`, and `reference-transaction` are the "
            "gates the generated hook shims call into; commit normally to run "
            "them. `doctor` checks the environment end to end."
        ),
    )
    actions = parser.add_subparsers(dest="dev_command", required=True)

    actions.add_parser(
        "install-hooks",
        help="Install the spice-owned git hook shims.",
        recovery_examples=("spice dev install-hooks",),
    ).set_defaults(func=handle_dev)

    pre_commit = actions.add_parser(
        "pre-commit",
        help="Hook backend for staged commit checks; commit normally to run it.",
        recovery_examples=("git commit", "spice dev pre-commit --help"),
    )
    pre_commit.add_argument(
        "pre_commit_args",
        nargs=argparse.REMAINDER,
        help=argparse.SUPPRESS,
    )
    pre_commit.set_defaults(func=handle_dev)

    dev_pytest = actions.add_parser(
        "pytest",
        help="Run checkout tests under the worktree venv; arguments pass to pytest.",
        recovery_examples=("spice dev pytest -q tests/test_cliversion.py",),
    )
    dev_pytest.add_argument(
        "pytest_args",
        nargs=argparse.REMAINDER,
        help=argparse.SUPPRESS,
    )
    dev_pytest.set_defaults(func=handle_dev)

    actions.add_parser(
        "serve-web-typecheck",
        help="Typecheck the serve static JavaScript with TypeScript checkJs.",
        recovery_examples=("spice dev serve-web-typecheck",),
    ).set_defaults(func=handle_dev)

    serve_web_types = actions.add_parser(
        "serve-web-types",
        help="Check or regenerate the Python-owned serve wire typedefs.",
        recovery_examples=("spice dev serve-web-types --write",),
    )
    serve_web_types.add_argument(
        "--write",
        action="store_true",
        help="Regenerate spice/serve/static/app.types.js from the wire schema.",
    )
    serve_web_types.set_defaults(func=handle_dev)

    actions.add_parser(
        "python-typecheck",
        help="Typecheck the project's Python package roots with pyright.",
        recovery_examples=("spice dev python-typecheck",),
    ).set_defaults(func=handle_dev)

    _configure_commit_parsers(actions)


def _configure_initialization_parsers(subparsers: Any) -> None:
    init = subparsers.add_parser(
        "init",
        help="Set up this repo: install hooks, materialize skill, exclude state.",
    )
    init.add_argument(
        "--gates",
        action="store_true",
        help=(
            "Install constitution gates only; do not materialize the agent skill "
            "or fleet-specific reference guard."
        ),
    )
    init.add_argument(
        "--apply",
        action="store_true",
        help="Apply the ordered initialization plan; bare invocation only previews.",
    )
    add_withdrawn_dry_run_argument(init)
    init.add_argument(
        "--json",
        action="store_true",
        help="Emit the versioned initialization plan as JSON without applying it.",
    )
    _mark_receipt_writing(init)
    init.set_defaults(func=handle_init)

    deinit = subparsers.add_parser(
        "deinit",
        help=(f"Withdrawn in {DEINIT_WITHDRAWAL_RELEASE}; use `spice init --unapply`."),
        recovery_examples=("spice init --unapply",),
    )
    deinit.add_argument(
        "--apply",
        action="store_true",
        help="Apply the ordered reversal plan; bare invocation only previews.",
    )
    deinit.add_argument(
        "--json",
        action="store_true",
        help="Emit the versioned reversal plan as JSON without applying it.",
    )
    deinit.set_defaults(func=handle_deinit)


def _mark_receipt_writing(parser: argparse.ArgumentParser) -> None:
    """Declare receipt ownership and install its mandatory reversal selector."""
    parser.add_argument(
        "--unapply",
        nargs="?",
        const="",
        metavar="RECEIPT_DIGEST",
        help=(
            "Reverse this verb's current receipt; optionally assert its SHA-256 "
            "digest. Bare reversal only previews."
        ),
    )
    parser.set_defaults(writes_receipt=True)


def _configure_commit_parsers(actions: Any) -> None:
    commit_msg = actions.add_parser(
        "commit-msg",
        help="Validate (and auto-fold) a commit message file.",
        recovery_examples=("spice dev commit-msg .git/COMMIT_EDITMSG",),
    )
    commit_msg.add_argument("message_file", help="Path to the commit message file.")
    commit_msg.set_defaults(func=handle_dev)

    reference_transaction = actions.add_parser(
        "reference-transaction",
        help="Hook backend for prepared Git reference transactions.",
        recovery_examples=("spice dev reference-transaction prepared",),
    )
    reference_transaction.add_argument(
        "state", choices=("prepared", "committed", "aborted")
    )
    reference_transaction.set_defaults(func=handle_dev)

    doctor = actions.add_parser(
        "doctor",
        help="Aggregate health check for the harness environment.",
        recovery_examples=("spice dev doctor --fix",),
    )
    doctor.add_argument(
        "--fix", action="store_true", help="Apply safe generated-state repairs."
    )
    doctor.set_defaults(func=handle_dev)


def handle_init(args: argparse.Namespace) -> int:
    if getattr(args, "unapply", None) is not None:
        return _handle_init_unapply(args)

    from spice.hooks.initplan import (
        InitializationMode,
        apply_initialization_plan,
        initialization_detail_rows,
        initialization_plan_payload,
        initialization_preview_rows,
        plan_initialization,
    )

    repo_root = init_repo_root()
    mode = (
        InitializationMode.GATES_ONLY if bool(args.gates) else InitializationMode.FULL
    )
    plan = plan_initialization(repo_root, mode)
    if bool(args.apply) and bool(args.json):
        raise SpiceError("`spice init --apply` cannot be combined with `--json`")
    if not bool(args.apply):
        if bool(args.json):
            print(
                json.dumps(initialization_plan_payload(plan), indent=2, sort_keys=True)
            )
            return 0
        for row in initialization_preview_rows(plan):
            print(row)
        return 0

    apply_initialization_plan(plan, approve_repository_config=True)
    for row in initialization_detail_rows(plan, include_ready=True):
        print(row)
    return 0


def _handle_init_unapply(args: argparse.Namespace) -> int:
    from spice.hooks.deinitplan import (
        apply_deinitialization_plan,
        deinitialization_plan_payload,
        deinitialization_plan_rows,
        deinitialization_report_rows,
        plan_deinitialization,
    )

    if bool(args.gates):
        raise SpiceError(
            "`spice init --gates` cannot be combined with `--unapply`; "
            "the current receipt selects the reversal surface"
        )
    if bool(args.apply) and bool(args.json):
        raise SpiceError(
            "`spice init --unapply --apply` cannot be combined with `--json`"
        )
    plan = plan_deinitialization(init_repo_root())
    asserted_digest = str(args.unapply)
    if asserted_digest and asserted_digest != plan.receipt_digest:
        raise SpiceError(
            "initialization receipt digest mismatch: "
            f"expected {asserted_digest}; "
            f"observed {plan.receipt_digest or '<none>'}"
        )
    if not bool(args.apply):
        if bool(args.json):
            print(
                json.dumps(
                    deinitialization_plan_payload(plan), indent=2, sort_keys=True
                )
            )
            return 0
        for row in deinitialization_plan_rows(plan):
            print(row)
        return 0
    report = apply_deinitialization_plan(plan)
    for row in deinitialization_report_rows(report):
        print(row)
    return 0


def handle_deinit(_args: argparse.Namespace) -> int:
    raise SpiceError(
        f"`spice deinit` was withdrawn in {DEINIT_WITHDRAWAL_RELEASE}; "
        "use `spice init --unapply` to preview the current receipt reversal"
    )


def init_repo_root(cwd: Path | None = None) -> Path:
    root = repo_root_from_cwd(cwd)
    if root is not None:
        return root
    marker_root = _linked_worktree_marker_root(cwd or Path.cwd())
    if marker_root is not None:
        # Git has run and reported no worktree; launch failures and time-outs
        # raise before this explicit fallback. Git versions differ on whether a
        # linked worktree under a bare common dir satisfies ``--show-toplevel``
        # before initialization writes ``core.bare=false``, so its marker must
        # yield the same root and plan as the primary resolver.
        return marker_root
    return require_repo_root(cwd)


def _linked_worktree_marker_root(cwd: Path) -> Path | None:
    try:
        start = cwd.expanduser().resolve()
    except (OSError, RuntimeError):
        start = cwd.expanduser().absolute()
    if start.is_file():
        start = start.parent
    for candidate in (start, *start.parents):
        marker = candidate / ".git"
        if not marker.exists():
            continue
        if not marker.is_file():
            return None
        try:
            content = marker.read_text(encoding="utf-8")
        except OSError:
            return None
        return candidate if content.startswith("gitdir:") else None
    return None


def handle_dev(args: argparse.Namespace) -> int:
    repo_root = require_repo_root()
    command = args.dev_command
    if command == "install-hooks":
        from spice.hooks.install import install_hooks_for_repo

        for row in install_hooks_for_repo(repo_root):
            print(row)
        return 0
    if command == "pre-commit":
        from spice.hooks.precommit import handle_pre_commit

        extra = tuple(getattr(args, "pre_commit_args", ()) or ())
        if extra:
            joined = " ".join(extra)
            raise SpiceError(
                "`spice dev pre-commit` is the repo pre-commit gate and does "
                f"not accept pre-commit framework arguments: {joined}\n"
                "Run `spice dev pre-commit` for the staged gate, or `git commit` "
                "to run it as the hook."
            )
        return handle_pre_commit(repo_root)
    if command == "pytest":
        from spice.hooks.devpytest import run_checkout_pytest

        return run_checkout_pytest(
            repo_root, list(getattr(args, "pytest_args", ()) or ())
        )
    if command == "serve-web-typecheck":
        from spice.serve.typecheck import run_serve_web_typecheck

        run_serve_web_typecheck(repo_root)
        return 0
    if command == "serve-web-types":
        from spice.serve.payload.wire import check_app_types_js, write_app_types_js

        if bool(args.write):
            print(write_app_types_js(repo_root))
        else:
            check_app_types_js(repo_root)
        return 0
    if command == "python-typecheck":
        from spice.studies.typecheck import run_python_typecheck

        run_python_typecheck(repo_root)
        return 0
    if command == "commit-msg":
        from spice.hooks.commitmsg import handle_commit_msg

        return handle_commit_msg(args.message_file, repo_root)
    if command == "reference-transaction":
        from spice.hooks.refguard import handle_reference_transaction

        return handle_reference_transaction(repo_root, args.state)
    if command == "doctor":
        from spice.hooks.doctor import run_doctor

        report = run_doctor(repo_root, fix=bool(args.fix))
        print(report.render())
        return 1 if report.failed else 0
    raise SpiceError(f"unknown dev command {command!r}")

"""`spice init` and `spice dev ...` — repo bootstrap and the hook backends."""

from __future__ import annotations

import argparse
from typing import Any

from spice.errors import SpiceError
from spice.paths import require_repo_root


def configure_dev_parser(subparsers: Any) -> None:
    init = subparsers.add_parser(
        "init",
        help="Adopt this repo: install hooks, materialize skill, exclude state.",
    )
    init.set_defaults(func=handle_init)

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
    pre_commit.set_defaults(func=handle_dev)

    actions.add_parser(
        "serve-web-typecheck",
        help="Typecheck the serve static JavaScript with TypeScript checkJs.",
        recovery_examples=("spice dev serve-web-typecheck",),
    ).set_defaults(func=handle_dev)

    actions.add_parser(
        "python-typecheck",
        help="Typecheck the project's Python package roots with pyright.",
        recovery_examples=("spice dev python-typecheck",),
    ).set_defaults(func=handle_dev)

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
    from spice.hooks.install import init_repo

    repo_root = require_repo_root()
    for row in init_repo(repo_root):
        print(row)
    return 0


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

        return handle_pre_commit(repo_root)
    if command == "serve-web-typecheck":
        from spice.serve.typecheck import run_serve_web_typecheck

        run_serve_web_typecheck(repo_root)
        return 0
    if command == "python-typecheck":
        from spice.studies.typecheck import run_python_typecheck

        run_python_typecheck(repo_root)
        return 0
    if command == "commit-msg":
        from spice.hooks.commitmsg import handle_commit_msg

        return handle_commit_msg(args.message_file)
    if command == "reference-transaction":
        from spice.hooks.refguard import handle_reference_transaction

        return handle_reference_transaction(repo_root, args.state)
    if command == "doctor":
        from spice.hooks.doctor import run_doctor

        report = run_doctor(repo_root, fix=bool(args.fix))
        print(report.render())
        return 1 if report.failed else 0
    raise SpiceError(f"unknown dev command {command!r}")

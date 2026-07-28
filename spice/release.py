"""`spice release ...` — prepare, publish, and summarize releases."""

from __future__ import annotations

import argparse
import configparser
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request
from collections import OrderedDict
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

from spice.cli.effects import (
    AuthoredInputInvocation,
    EffectRead,
    MutationDecision,
    mark_authored_input,
)
from spice.cli.mounts import RUNTIME_PYTHON_ENV
from spice.commandplan import assert_plan_digest, command_plan_payload
from spice.errors import SpiceError
from spice.tasks import config as task_config
from spice.process.tool import run_tool_command

BUMP_CHOICES = ("minor", "patch")
PLAYWRIGHT_MCP_CONFIG_ENV = "SPICE_PLAYWRIGHT_MCP_CONFIG"  # env-policy: allow
PYPI_POLL_ATTEMPTS = 20
PYPI_POLL_SECONDS = 3
PYPI_URL = "https://pypi.org/pypi/spice-harness/json"
PROJECT_HEADINGS = {
    "cli": "CLI",
    "ui": "UI",
}
TASK_PHASE_SUBJECT_PREFIX_RE = re.compile(
    r"^(?:design|plan|todo|verify|review)\([^)]+\):\s*",
    re.IGNORECASE,
)
TASK_PHASE_SUBJECT_SUFFIX_RE = re.compile(
    r"\s+\((?:design|plan|verify|review)\)$",
    re.IGNORECASE,
)


@dataclass(frozen=True)
class ReleaseRecord:
    commit: str
    subject: str
    project: str
    task_key: str = ""


@dataclass(frozen=True)
class InstalledCliSource:
    python: Path
    module: Path
    root: Path
    commit: str
    tree: str


@dataclass(frozen=True)
class ReleasePlanOperation:
    """One ordered release action described without executing it."""

    action: str
    detail: str


@dataclass(frozen=True)
class ReleasePlan:
    """A complete operator-readable plan for one mutating release verb."""

    repository: Path
    action: str
    version: str
    source_commit: str
    notes_sha256: str | None
    release_commit: str | None
    notes_file: Path | None
    operations: tuple[ReleasePlanOperation, ...]
    schema_version: int = 1

    def payload(self) -> dict[str, object]:
        return command_plan_payload(
            command=f"release {self.action}",
            metadata={
                "repository": str(self.repository),
                "action": self.action,
                "version": self.version,
                "source_commit": self.source_commit,
                "notes_sha256": self.notes_sha256,
                "release_commit": self.release_commit,
                "notes_file": (
                    str(self.notes_file) if self.notes_file is not None else None
                ),
            },
            operations=[
                {
                    "kind": operation.action,
                    "target": operation.detail,
                    "scope": "repository",
                    "action": operation.action,
                    "detail": operation.detail,
                    "source_commit": self.source_commit,
                    "notes_sha256": self.notes_sha256,
                    "release_commit": self.release_commit,
                }
                for operation in self.operations
            ],
        )

    def rows(self) -> list[str]:
        digest = str(self.payload()["plan_digest"])
        rows = [
            f"release-plan schema={self.schema_version} action={self.action} "
            f"version={self.version} digest={digest}",
            f"repository={self.repository}",
        ]
        if self.release_commit is not None:
            rows.append(f"release-commit={self.release_commit}")
        if self.notes_file is not None:
            rows.append(f"notes-file={self.notes_file}")
        rows.extend(
            f"{order}. {operation.action} {operation.detail}"
            for order, operation in enumerate(self.operations, start=1)
        )
        rows.append("preview: no changes applied; pass --apply to execute")
        return rows


SIGINT_EXIT_CODE = 130
INSTALLED_CLI_PROBE_SCRIPT = (
    "from pathlib import Path;"
    "import spice.tasks.git.boundaries as boundaries;"
    "print(Path(boundaries.__file__).resolve())"
)


def build_release_parser(prog: str = "spice release") -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog=prog,
        description=(
            "Prepare, publish, and summarize spice releases from a clean "
            "synchronized worktree. check, notes, and range only read the "
            "tree: they never bump, commit, tag, push, or publish. minor, "
            "patch, prepare, publish, and github preview an ordered plan by "
            "default and mutate only with --apply."
        ),
    )
    actions = parser.add_subparsers(dest="release_action", required=True)

    check = actions.add_parser(
        "check",
        help=(
            "Run the release gates against the current version and stop; "
            "mutates nothing."
        ),
    )
    check.set_defaults(func=handle_release, release_mode="check")

    for bump in BUMP_CHOICES:
        one_pass = actions.add_parser(
            bump,
            help=f"Plan a {bump} bump, validation, commit, push, and publish.",
        )
        _add_apply_options(one_pass)
        _mark_authored_release(one_pass)
        one_pass.set_defaults(func=handle_release, release_mode="release", bump=bump)

    prepare = actions.add_parser(
        "prepare", help="Plan a bump, validation, and commit without publishing."
    )
    prepare.add_argument("bump", choices=BUMP_CHOICES)
    _add_apply_options(prepare)
    _mark_authored_release(prepare, sample_suffix=("minor",))
    prepare.set_defaults(func=handle_release, release_mode="prepare")

    notes = actions.add_parser(
        "notes", help="Generate a draft changelog to curate into release highlights."
    )
    notes.add_argument("version", nargs="?")
    notes.add_argument("--output", type=Path, help="Write notes to this path.")
    notes.add_argument(
        "--release-commit",
        help="Commit-ish to use as the release notes target instead of the default.",
    )
    notes.set_defaults(func=handle_release, release_mode="notes")

    preview = actions.add_parser(
        "range",
        help="Preview the prior-tag..release-commit landed-task range.",
    )
    preview.add_argument("version", nargs="?")
    preview.add_argument(
        "--release-commit",
        help=(
            "Commit-ish for the range end instead of the default; accepts full "
            "refs like refs/remotes/origin/main."
        ),
    )
    preview.set_defaults(func=handle_release, release_mode="range")

    publish = actions.add_parser(
        "publish", help="Validate the prepared version, then push and publish."
    )
    publish.add_argument("--notes-file", type=Path)
    publish.add_argument(
        "--release-commit",
        help=(
            "Explicit release commit; must resolve to HEAD because publish "
            "builds artifacts from the current worktree."
        ),
    )
    _add_apply_options(publish)
    _mark_authored_release(publish)
    publish.set_defaults(func=handle_release, release_mode="publish")

    github = actions.add_parser(
        "github", help="Create/push the release tag and GitHub Release."
    )
    github.add_argument("version", nargs="?")
    github.add_argument("--notes-file", type=Path)
    github.add_argument(
        "--release-commit",
        help="Commit-ish to tag and use as the release notes target.",
    )
    _add_apply_options(github)
    _mark_authored_release(github)
    github.set_defaults(func=handle_release, release_mode="github")
    return parser


def _mark_authored_release(
    parser: argparse.ArgumentParser,
    *,
    sample_suffix: tuple[str, ...] = (),
) -> None:
    mark_authored_input(
        parser,
        AuthoredInputInvocation(
            reads=(EffectRead.AUTHORED_REPOSITORY,),
            decision=MutationDecision.PREVIEW_APPLY,
            sample_suffix=sample_suffix,
            mutation_args=("--apply",),
        ),
    )


def _add_apply_options(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--apply",
        nargs="?",
        const=True,
        metavar="PLAN_DIGEST",
        help=(
            "Execute the ordered plan, optionally asserting its digest; "
            "bare invocation only previews."
        ),
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Emit the ordered plan as JSON without applying it.",
    )


def main(argv: list[str] | None = None) -> int:
    parser = build_release_parser()
    try:
        args = parser.parse_args(sys.argv[1:] if argv is None else argv)
        return int(args.func(args))
    except SpiceError as exc:
        print(f"spice release: {exc}", file=sys.stderr)
        return 2
    except subprocess.CalledProcessError as exc:
        return int(exc.returncode)
    except KeyboardInterrupt:
        print("spice release: interrupted", file=sys.stderr)
        return SIGINT_EXIT_CODE


def handle_release(args: argparse.Namespace) -> int:
    previous_cwd = Path.cwd()
    root = repo_root()
    try:
        os.chdir(root)
        return _handle_release_from_root(args, root)
    finally:
        os.chdir(previous_cwd)


def _handle_release_from_root(args: argparse.Namespace, root: Path) -> int:
    mode = str(args.release_mode)
    if mode == "notes":
        if args.version is None and args.release_commit is None:
            version = current_version()
            release_commit = git("rev-parse", "HEAD")
            if git("tag", "--list", f"v{version}"):
                version = "unreleased"
                output = release_notes_for_unreleased(release_commit)
            else:
                output = release_notes_for_version(version, release_commit)
        else:
            version = str(args.version or current_version())
            release_commit = release_commit_for_target(
                version, getattr(args, "release_commit", None)
            )
            output = release_notes_for_version(version, release_commit)
        notes_output = getattr(args, "output", None)
        if notes_output:
            notes_output.write_text(output, encoding="utf-8")
            print(f"wrote release notes draft for {version} to {notes_output}")
        else:
            print(output, end="" if output.endswith("\n") else "\n")
        print(
            f"draft notes for {version} include a collapsed task-level export — "
            "replace the Highlights placeholder with curated highlights and drop "
            "the draft banner before publishing; keep the generated details section",
            file=sys.stderr,
        )
        return 0

    if mode == "range":
        if args.version is None and args.release_commit is None:
            release_commit = git("rev-parse", "HEAD")
            output = release_range_for_unreleased(release_commit)
            print(output, end="" if output.endswith("\n") else "\n")
            return 0
        version = str(args.version or current_version())
        release_commit = release_commit_for_target(
            version, getattr(args, "release_commit", None)
        )
        output = release_range_for_version(version, release_commit)
        print(output, end="" if output.endswith("\n") else "\n")
        return 0

    if mode == "check":
        ensure_clean_worktree(root)
        version = run_release_gates(root, current_version)
        print(
            f"release gates passed for {version}; nothing was bumped, "
            "committed, tagged, pushed, or published"
        )
        return 0

    if mode in {"prepare", "release", "publish", "github"}:
        apply_requested = args.apply is not None
        if apply_requested and bool(args.json):
            raise SpiceError(
                f"`spice release {args.release_action} --apply` cannot be "
                "combined with `--json`"
            )
        plan = plan_release(args, root)
        if not apply_requested:
            if bool(args.json):
                print(json.dumps(plan.payload(), indent=2, sort_keys=True))
            else:
                for row in plan.rows():
                    print(row)
            return 0
        expected_digest = args.apply if isinstance(args.apply, str) else None
        assert_plan_digest(plan.payload(), expected_digest)
        return apply_release_plan(args, root, plan)

    raise SpiceError(f"unknown release action {mode!r}")


def plan_release(args: argparse.Namespace, root: Path) -> ReleasePlan:
    """Build the ordered plan for a mutating release action without mutation."""
    mode = str(args.release_mode)
    ensure_clean_worktree(root)
    if mode in {"release", "publish", "github"}:
        ensure_notes_file(getattr(args, "notes_file", None))

    release_commit: str | None = None
    if mode in {"prepare", "release"}:
        ensure_release_preconditions(root)
        version = preview_bumped_version(str(args.bump))
        operations = [
            ReleasePlanOperation(
                "verify-installed-source",
                "prove the independently installed CLI carries this tree",
            ),
            ReleasePlanOperation(
                "clean-artifacts", "remove stale build and dist trees"
            ),
            ReleasePlanOperation(
                "run-constitution", "run Python, Ruff, and browser release gates"
            ),
            ReleasePlanOperation(
                "bump-version", f"rewrite pyproject.toml and uv.lock to {version}"
            ),
            ReleasePlanOperation(
                "build-and-probe", f"build and verify artifacts for {version}"
            ),
            ReleasePlanOperation("stage-version", "stage pyproject.toml and uv.lock"),
            ReleasePlanOperation(
                "commit-version", f"commit release: bump to {version}"
            ),
        ]
        if mode == "release":
            operations.extend(_publication_operations(version))
    elif mode == "publish":
        version = current_version()
        release_commit = release_commit_for_target(
            version, getattr(args, "release_commit", None)
        )
        ensure_publish_release_commit_is_head(release_commit)
        operations = [
            ReleasePlanOperation(
                "verify-installed-source",
                "prove the independently installed CLI carries this tree",
            ),
            ReleasePlanOperation(
                "clean-artifacts", "remove stale build and dist trees"
            ),
            ReleasePlanOperation(
                "run-constitution", "run Python, Ruff, and browser release gates"
            ),
            ReleasePlanOperation(
                "build-and-probe", f"build and verify artifacts for {version}"
            ),
            *_publication_operations(version),
        ]
    elif mode == "github":
        version = str(args.version or current_version())
        release_commit = release_commit_for_target(
            version, getattr(args, "release_commit", None)
        )
        operations = list(_github_publication_operations(version))
    else:
        raise SpiceError(f"cannot plan non-mutating release action {mode!r}")
    return ReleasePlan(
        repository=root.resolve(),
        action=str(args.release_action),
        version=version,
        source_commit=git("-C", str(root), "rev-parse", "HEAD"),
        notes_sha256=_release_notes_digest(getattr(args, "notes_file", None)),
        release_commit=release_commit,
        notes_file=getattr(args, "notes_file", None),
        operations=tuple(operations),
    )


def _release_notes_digest(path: Path | None) -> str | None:
    if path is None or not path.is_file():
        return None
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _publication_operations(version: str) -> tuple[ReleasePlanOperation, ...]:
    return (
        ReleasePlanOperation(
            "check-publish", f"dry-run upload artifacts for {version}"
        ),
        ReleasePlanOperation("push-main", "push HEAD to origin/main"),
        ReleasePlanOperation("publish-package", f"upload spice-harness {version}"),
        ReleasePlanOperation("wait-for-pypi", f"wait for PyPI to report {version}"),
        *_github_publication_operations(version),
    )


def _github_publication_operations(version: str) -> tuple[ReleasePlanOperation, ...]:
    return (
        ReleasePlanOperation("create-tag", f"create v{version} when absent"),
        ReleasePlanOperation("push-tag", f"push v{version} to origin"),
        ReleasePlanOperation(
            "create-github-release", f"publish release v{version} when absent"
        ),
    )


def apply_release_plan(args: argparse.Namespace, root: Path, plan: ReleasePlan) -> int:
    """Execute a previously rendered release plan through the canonical seams."""
    mode = str(args.release_mode)
    if mode in {"prepare", "release"}:
        version = run_release_gates(root, lambda: bump_version(str(args.bump)))
        if version != plan.version:
            raise SpiceError(
                f"release plan expected version {plan.version}, bump produced {version}"
            )
        run(["git", "add", "pyproject.toml", "uv.lock"])
        run(["git", "commit", "-m", f"release: bump to {version}"])
        if mode == "prepare":
            print_prepare_instructions(version)
            run(["git", "status", "--short", "--branch"])
            return 0
        publish_release(version, getattr(args, "notes_file", None))
        return 0
    if mode == "publish":
        release_commit = plan.release_commit
        if release_commit is None:
            raise SpiceError("publish plan is missing its release commit")
        run_release_gates(root, lambda: plan.version)
        publish_release(
            plan.version,
            getattr(args, "notes_file", None),
            release_commit=release_commit,
        )
        return 0
    if mode == "github":
        release_commit = plan.release_commit
        if release_commit is None:
            raise SpiceError("GitHub plan is missing its release commit")
        publish_github_release(
            plan.version,
            getattr(args, "notes_file", None),
            release_commit=release_commit,
        )
        return 0
    raise SpiceError(f"cannot apply non-mutating release action {mode!r}")


def repo_root() -> Path:
    result = run(["git", "rev-parse", "--show-toplevel"], capture=True)
    return Path(result.stdout.strip()).resolve()


def ensure_clean_worktree(root: Path) -> None:
    # A release runs from whatever clean worktree we happen to be in: there is
    # no dedicated release tree and no local `main` branch. Only a dirty tree
    # blocks it; publish pushes HEAD to origin/main by ref.
    status = git("status", "--porcelain")
    if status:
        raise SpiceError("refusing to release with a dirty worktree")


def ensure_release_preconditions(root: Path) -> None:
    # A bump-and-commit release demands everything a task claim demands, so a
    # stray uncaptured commit can never be folded into the release bump: a task
    # must be claimed, and there can be no local commits the task system has not
    # yet recorded (the dirty-tree case is handled by ensure_clean_worktree).
    from spice.tasks import claimstate
    from spice.tasks.git import boundaries

    if not claimstate.has_active_claim():
        raise SpiceError(
            "claim a release task first (e.g. `spice task add --project "
            "lifecycle.release ...` then `spice task claim <handle>`); "
            "refusing to release with no task claimed"
        )
    ahead = boundaries.commits_ahead_of_baseline(root)
    if ahead > 0:
        raise SpiceError(
            f"refusing to release with {ahead} local commit(s) not captured by a "
            "completed task; complete or capture them into a task before releasing"
        )


def ensure_notes_file(path: Path | None) -> None:
    if path is None:
        return
    if not path.is_file():
        raise SpiceError(f"release notes file not found: {path}")


def run_release_gates(root: Path, choose_version: Callable[[], str]) -> str:
    """Every gate a release passes, and the one decision that varies between modes.

    All four gate-running modes share this body, so a change to what a release
    verifies reaches every one of them in the same edit. A verification path
    maintained alongside the release path would drift until it proved something
    the release does not actually run.

    What differs is only which version the artifact gate builds, so that is the
    single seam. `check` reads the tree, so it passes `current_version` directly.
    `publish` passes the version it already pinned, because it resolved a release
    commit against that version and must build the one it validated, not whatever
    the tree says by the time the gates finish. `prepare` and `release` ship a new
    version and pass the bump, because the artifact that gets uploaded has to be
    the artifact that was twine-checked and import-probed.

    The seam sits here, between the two gates, and not at either end: a bump
    before the constitution gate would leave a rewritten `pyproject.toml` behind
    whenever the suite comes back red, and a bump after the artifact gate would
    ship a version nothing ever built. Returning the chosen version keeps that
    decision readable at the call site.
    """
    require_installed_cli_carries_release_tree(root)
    clean_build_artifacts(root)
    run_constitution_gate()
    version = choose_version()
    run_artifact_gate(version)
    return version


def require_installed_cli_carries_release_tree(root: Path) -> InstalledCliSource:
    """Prove ordinary fleet commands import the exact committed release tree."""
    installed = _installed_cli_source()
    candidate_root = root.resolve()
    if installed.python.is_relative_to(candidate_root):
        raise SpiceError(
            "release evidence must come from the independently installed CLI, "
            f"not the candidate worktree interpreter {installed.python}"
        )
    candidate_commit, candidate_tree = _source_identity(root)
    if installed.tree != candidate_tree:
        raise SpiceError(
            "deploy the candidate tree through the installed CLI before claiming "
            "release behavior; branch state has no fleet effect by itself: "
            f"candidate HEAD {candidate_commit} tree {candidate_tree}, while "
            f"{installed.python} -P imports {installed.module} from "
            f"{installed.commit} tree {installed.tree}"
        )
    print(
        "installed CLI source gate passed: "
        f"{installed.python} -P imports {installed.module}; "
        f"candidate {candidate_commit} and installed {installed.commit} "
        f"share tree {candidate_tree}"
    )
    return installed


def _installed_cli_source() -> InstalledCliSource:
    raw_python = str(
        os.environ.get(RUNTIME_PYTHON_ENV) or ""  # env-policy: allow
    ).strip()
    if not raw_python:
        raise SpiceError(
            "run release gates through the repository-mounted `spice release` "
            f"command; {RUNTIME_PYTHON_ENV} did not identify the installed CLI"
        )
    python = Path(raw_python).expanduser().absolute()
    if not python.is_file():
        raise SpiceError(f"installed CLI interpreter does not exist: {python}")
    probe_env = dict(os.environ)  # env-policy: allow
    probe_env.pop("PYTHONPATH", None)
    result = run(
        [str(python), "-P", "-c", INSTALLED_CLI_PROBE_SCRIPT],
        capture=True,
        cwd=Path("/"),
        env=probe_env,
    )
    try:
        module = Path(result.stdout.strip().splitlines()[-1]).resolve(strict=True)
        root = module.parents[3]
    except (IndexError, OSError) as exc:
        raise SpiceError(
            f"installed CLI probe returned no usable module path: {result.stdout!r}"
        ) from exc
    expected = Path("spice/tasks/git/boundaries.py")
    if module.relative_to(root) != expected:
        raise SpiceError(
            f"installed CLI probe resolved unexpected module path {module}; "
            f"expected <source-root>/{expected}"
        )
    if not (root / "pyproject.toml").is_file():
        raise SpiceError(
            f"installed CLI module {module} is not backed by an editable Spice "
            "source checkout; deploy with `uv tool install -e <main-tree>`"
        )
    drift = _worktree_drift(root)
    if drift:
        raise SpiceError(
            "the installed CLI runs its source checkout directly, so a dirty "
            "deployment executes code no commit contains and its committed "
            f"identity proves nothing; commit or revert {root} before "
            f"releasing:\n{drift}"
        )
    commit, tree = _source_identity(root)
    return InstalledCliSource(python, module, root, commit, tree)


def _worktree_drift(root: Path) -> str:
    """Whatever the deployment carries that its own HEAD does not.

    An editable install imports the working tree, never HEAD, so comparing
    committed identities alone would let uncommitted edits to the very modules
    the probe resolves pass the gate untouched: the tree hash is the same
    before and after them.
    """
    return run(
        ["git", "-C", str(root), "status", "--porcelain"],
        capture=True,
    ).stdout.strip()


def _source_identity(root: Path) -> tuple[str, str]:
    commit = run(
        ["git", "-C", str(root), "rev-parse", "HEAD^{commit}"],
        capture=True,
    ).stdout.strip()
    tree = run(
        ["git", "-C", str(root), "rev-parse", "HEAD^{tree}"],
        capture=True,
    ).stdout.strip()
    return commit, tree


def run_constitution_gate() -> None:
    run(["uv", "run", "pytest"])
    run(["uv", "run", "ruff", "check", "."])
    run_browser_gate()


def run_browser_gate(root: Path | None = None) -> None:
    from spice.agent.driver import write_playwright_mcp_config

    config_path = write_playwright_mcp_config(root or Path.cwd())
    env = dict(os.environ)  # env-policy: allow
    env[PLAYWRIGHT_MCP_CONFIG_ENV] = str(config_path)
    run(["node", "tests/browser/run_release_smokes.js"], env=env)


def clean_build_artifacts(root: Path) -> None:
    for name in ("build", "dist"):
        shutil.rmtree(root / name, ignore_errors=True)


def run_artifact_gate(version: str) -> None:
    sdist = Path("dist") / f"spice_harness-{version}.tar.gz"
    wheel = Path("dist") / f"spice_harness-{version}-py3-none-any.whl"

    clean_build_artifacts(Path.cwd())
    run(["uv", "build", "--python", "3.12"])
    run(["uvx", "twine", "check", str(sdist), str(wheel)])

    with tempfile.TemporaryDirectory() as tmpdir:
        venv = Path(tmpdir) / "venv"
        python = venv / "bin" / "python"
        spice = venv / "bin" / "spice"
        run(["uv", "venv", "--python", "3.12", str(venv)])
        run(["uv", "pip", "install", "--python", str(python), str(wheel)])
        smoke_env = hermetic_wheel_env()
        run(
            [
                str(python),
                "-I",
                "-c",
                "from spice.config import layers; print(layers.__file__)",
            ],
            capture=True,
            env=smoke_env,
        )
        run([str(spice), "--help"], capture=True, env=smoke_env)
        run([str(spice), "task", "--help"], capture=True, env=smoke_env)
        run([str(spice), "session", "--help"], capture=True, env=smoke_env)


def hermetic_wheel_env() -> dict[str, str]:
    return dict(os.environ)  # env-policy: allow


def current_version() -> str:
    return run(["uv", "version", "--short"], capture=True).stdout.strip()


def bump_version(bump: str) -> str:
    return run(
        ["uv", "version", "--bump", bump, "--no-sync", "--short"],
        capture=True,
    ).stdout.strip()


def preview_bumped_version(bump: str) -> str:
    """Resolve the version a bump would write without changing project files."""
    return run(
        [
            "uv",
            "version",
            "--bump",
            bump,
            "--dry-run",
            "--no-sync",
            "--short",
        ],
        capture=True,
    ).stdout.strip()


def release_commit_for_version(version: str) -> str:
    tag = f"v{version}"
    if git("tag", "--list", tag):
        return git("rev-list", "-n", "1", tag)
    if version == current_version():
        return git("rev-parse", "HEAD")
    commit = git(
        "log", "--format=%H", "--grep", f"^release: bump to {version}$", "-n", "1"
    )
    return commit or git("rev-parse", "HEAD")


def release_commit_for_target(version: str, target: str | None) -> str:
    if target is None:
        return release_commit_for_version(version)
    try:
        return git("rev-parse", "--verify", f"{target}^{{commit}}")
    except subprocess.CalledProcessError as exc:
        raise SpiceError(f"release commit not found: {target}") from exc


def ensure_publish_release_commit_is_head(release_commit: str) -> None:
    head = git("rev-parse", "HEAD")
    if release_commit != head:
        raise SpiceError(
            "use `spice release github --release-commit ...` for tag or GitHub "
            "release repair; --release-commit must resolve to HEAD for publish "
            "because publish builds artifacts from the current worktree"
        )


def previous_release_tag(current_tag: str) -> str:
    raw = git("tag", "--list", "v*", "--sort=-v:refname")
    for tag in raw.splitlines():
        if tag and tag != current_tag:
            return tag
    return ""


def release_notes_for_version(version: str, release_commit: str) -> str:
    current_tag = f"v{version}"
    previous_tag = previous_release_tag(current_tag)
    records = commit_records(previous_tag, release_commit)
    return render_release_notes(
        version=version,
        release_commit=release_commit,
        release_short=short_commit(release_commit),
        current_tag=current_tag,
        previous_tag=previous_tag,
        records=records,
    )


def release_notes_for_unreleased(release_commit: str) -> str:
    previous_tag = latest_release_tag_merged_into(release_commit)
    records = commit_records(previous_tag, release_commit)
    return render_release_notes(
        version="unreleased",
        release_commit=release_commit,
        release_short=short_commit(release_commit),
        current_tag="unreleased",
        previous_tag=previous_tag,
        records=records,
    )


def tag_ref(tag: str) -> str:
    # Address the tag by its full ref so a same-named branch or shadow ref can
    # never mask the real tag when computing a release range.
    return f"refs/tags/{tag}"


def release_range_for_version(version: str, release_commit: str) -> str:
    current_tag = f"v{version}"
    previous_tag = previous_release_tag(current_tag)
    records = commit_records(previous_tag, release_commit)
    return render_release_range(
        version=version,
        release_short=short_commit(release_commit),
        current_tag=current_tag,
        previous_tag=previous_tag,
        records=records,
    )


def release_range_for_unreleased(release_commit: str) -> str:
    previous_tag = latest_release_tag_merged_into(release_commit)
    records = commit_records(previous_tag, release_commit)
    return render_release_range(
        version="unreleased",
        release_short=short_commit(release_commit),
        current_tag="unreleased",
        previous_tag=previous_tag,
        records=records,
    )


def latest_release_tag_merged_into(commit: str) -> str:
    raw = git("tag", "--merged", commit, "--list", "v*", "--sort=-v:refname")
    return raw.splitlines()[0] if raw else ""


def render_release_range(
    *,
    version: str,
    release_short: str,
    current_tag: str,
    previous_tag: str,
    records: list[ReleaseRecord],
) -> str:
    if previous_tag:
        span = f"{tag_ref(previous_tag)}..{release_short}"
    else:
        span = f"latest first-parent commits ending at {release_short}"
    lines = [
        f"Release range for {version}",
        f"Range: {span}",
        f"Release tag: {current_tag}",
        f"Landed commits: {len(records)}",
        "",
    ]
    if records:
        width = max(len(release_project_key(record.project)) for record in records)
        for record in records:
            key = release_project_key(record.project)
            lines.append(
                f"{shortish_commit(record.commit)}  {key.ljust(width)}  {record.subject}"
            )
    else:
        lines.append("No non-release commits found.")
    lines.append("")
    return "\n".join(lines)


REVERT_TARGET_RE = re.compile(r"This reverts commit ([0-9a-f]{7,40})\b")


def _is_ancestor(candidate: str, commit: str) -> bool:
    """True iff `candidate` is an ancestor of (or equal to) `commit`."""
    result = run_tool_command(
        ["git", "merge-base", "--is-ancestor", candidate, commit],
        policy="release",
        operation="check release ancestry",
        capture_output=True,
        text=True,
    )
    return result.returncode == 0


def commit_records(previous_tag: str, release_commit: str) -> list[ReleaseRecord]:
    format_arg = (
        "--format=%H%x1f%s%x1f%(trailers:key=Task-Project,valueonly)"
        "%x1f%(trailers:key=Task-Key,valueonly)%x1f%b%x1e"
    )
    if previous_tag:
        args = [
            "log",
            "--first-parent",
            "--reverse",
            format_arg,
            f"{tag_ref(previous_tag)}..{release_commit}",
        ]
    else:
        args = [
            "log",
            "--first-parent",
            "--reverse",
            "-n",
            "5",
            format_arg,
            release_commit,
        ]

    raw = run(["git", *args], capture=True).stdout
    rows: list[tuple[str, str, str, str, str]] = []
    for raw_record in raw.split("\x1e"):
        raw_record = raw_record.strip("\n")
        if not raw_record:
            continue
        commit, subject, project, task_key, body = (
            raw_record.split("\x1f", 4) + ["", "", "", "", ""]
        )[:5]
        if subject.startswith("release: bump to "):
            continue
        rows.append(
            (commit, subject, project.strip() or "general", task_key.strip(), body)
        )

    # A revert commit and the (first-parent) commit that introduced the work
    # it reverts both landing in this same range is a net no-op for this
    # release; suppress the pair rather than claim credit for shipping
    # something that got undone before it shipped. The revert body names the
    # raw commit it undoes, which usually merged in on a side branch, so find
    # the first-parent commit whose history contains it instead of matching
    # commit hashes directly.
    suppressed_commits: set[str] = set()
    for revert_commit, _subject, _project, _task_key, body in rows:
        match = REVERT_TARGET_RE.search(body)
        if not match:
            continue
        target = match.group(1)
        introduced_by = next(
            (
                commit
                for commit, *_rest in rows
                if commit != revert_commit and _is_ancestor(target, commit)
            ),
            None,
        )
        if introduced_by is None:
            continue
        suppressed_commits.add(revert_commit)
        suppressed_commits.add(introduced_by)

    records: list[ReleaseRecord] = []
    latest_index_by_task_key: dict[str, int] = {}
    for commit, subject, project, task_key, _body in rows:
        if commit in suppressed_commits:
            continue
        record = ReleaseRecord(
            commit=commit, subject=subject, project=project, task_key=task_key
        )
        # A task's todo-phase and review-phase merges carry the same
        # Task-Key; keep one highlight per task, at its first position, with
        # the latest (most final) subject.
        if task_key and task_key in latest_index_by_task_key:
            records[latest_index_by_task_key[task_key]] = record
            continue
        if task_key:
            latest_index_by_task_key[task_key] = len(records)
        records.append(record)
    return records


def render_release_notes(
    *,
    version: str,
    release_commit: str,
    release_short: str,
    current_tag: str,
    previous_tag: str,
    records: list[ReleaseRecord],
) -> str:
    groups: OrderedDict[str, OrderedDict[str, list[str]]] = OrderedDict()
    for record in records:
        project_subjects = groups.setdefault(
            release_project_key(record.project), OrderedDict()
        )
        project_subjects.setdefault(
            edited_release_highlight(
                release_note_subject(record.subject, record.task_key, record.project)
            ),
            [],
        ).append(shortish_commit(record.commit))

    lines = [
        "> [!IMPORTANT]",
        "> **Draft release notes — curate Highlights before publishing.** Replace",
        "> the placeholder under _Highlights_ with a short summary, then delete this",
        "> banner. The generated task inventory is already wrapped in the collapsed",
        "> _Task-level changes_ section below; keep that section intact. Omit from",
        "> Highlights any feature that was added and then functionally reverted",
        "> within this same release window — a net-zero change is not a highlight.",
        "",
        "## Highlights",
        "",
        "_Replace this line with a short, curated set of highlights folded from "
        "the changes below._",
        "",
        "<details>",
        "<summary>Task-level changes</summary>",
        "",
        "## Changes by project",
        "",
    ]
    if groups:
        for project, subjects in groups.items():
            lines.extend([f"### {release_project_heading(project)}", ""])
            for highlight, commits in subjects.items():
                # GitHub release pages turn bare repository SHAs into commit links.
                refs = ", ".join(commits)
                lines.append(f"- {highlight} ({refs})")
            lines.append("")
    else:
        lines.extend(["- No non-release commits found.", ""])

    lines.extend(
        [
            "</details>",
            "",
            "## Package Notes",
            "",
            f"- PyPI release: `spice-harness=={version}`",
            f"- Release commit: `{release_short}`",
        ]
    )
    if previous_tag:
        lines.append(f"- Commit range: `{previous_tag}..{release_short}`")
    else:
        lines.append(
            f"- Commit range: latest first-parent commits ending at `{release_short}`"
        )
    lines.append(
        "- Commit source: first-parent history grouped by `Task-Project` metadata"
    )
    if current_tag:
        lines.append(f"- Release tag: `{current_tag}`")
    lines.append("")
    return "\n".join(lines)


def edited_release_highlight(subject: str) -> str:
    raw = " ".join(subject.split()).strip()
    if not raw:
        return "Updated the release."
    replacements = (
        ("fix ", "Fixed "),
        ("prefer ", "Improved "),
        ("add ", "Added "),
        ("expose ", "Added "),
        ("remove ", "Removed "),
        ("update ", "Updated "),
        ("track ", "Tracked "),
        ("document ", "Documented "),
        ("restore ", "Restored "),
        ("clean ", "Cleaned "),
        ("wire ", "Wired "),
        ("make ", "Made "),
    )
    lower = raw.lower()
    for prefix, replacement in replacements:
        if lower.startswith(prefix):
            return punctuate(replacement + raw[len(prefix) :])
    return punctuate(capitalize_first(raw))


def release_note_subject(subject: str, task_key: str = "", project: str = "") -> str:
    trimmed = TASK_PHASE_SUBJECT_PREFIX_RE.sub("", subject, count=1)
    if project:
        project_prefix = f"{task_config.project_stem(project)}: "
        if trimmed.casefold().startswith(project_prefix.casefold()):
            trimmed = trimmed[len(project_prefix) :]
            trimmed = TASK_PHASE_SUBJECT_SUFFIX_RE.sub("", trimmed, count=1)
    if task_key:
        head, sep, last = trimmed.rpartition(" ")
        if sep and head and last.endswith(f"-{task_key}"):
            # Drop the trailing KEY-INCEPTED handle: GitHub already renders each
            # entry's bare short SHA as a commit link, so the handle token is
            # redundant. Keyed on this commit's own Task-Key, never a guess.
            trimmed = head
    return trimmed


def release_project_heading(project: str) -> str:
    if project in PROJECT_HEADINGS:
        return PROJECT_HEADINGS[project]
    parts = [
        segment
        for dotted in project.replace("_", "-").split(".")
        for segment in dotted.split("-")
        if segment
    ]
    if not parts:
        return "General"
    return " ".join(PROJECT_HEADINGS.get(part, part.title()) for part in parts)


def release_project_key(project: str) -> str:
    key = project.strip().lower()
    if not key or key.startswith("agent."):
        return "general"
    return key


def capitalize_first(text: str) -> str:
    first = text[:1]
    return f"{first.upper()}{text[1:]}" if first.islower() else text


def punctuate(text: str) -> str:
    return text if text.endswith((".", "!", "?")) else f"{text}."


def shortish_commit(commit: str) -> str:
    return commit[:7] if len(commit) > 7 else commit


def short_commit(commit: str) -> str:
    return git("rev-parse", "--short", commit)


def publish_release(
    version: str,
    notes_file: Path | None = None,
    *,
    release_commit: str | None = None,
) -> None:
    release_commit = release_commit or release_commit_for_version(version)
    ensure_publish_release_commit_is_head(release_commit)
    sdist = Path("dist") / f"spice_harness-{version}.tar.gz"
    wheel = Path("dist") / f"spice_harness-{version}-py3-none-any.whl"
    token = read_pypi_token()

    env = dict(os.environ)  # env-policy: allow
    env["UV_PUBLISH_TOKEN"] = token
    run(["uv", "publish", "--dry-run", str(sdist), str(wheel)], env=env)
    # Push the release commit (made on a synchronized lane) to origin/main by
    # ref, so the local branch name does not have to be `main`.
    run(["git", "push", "origin", "HEAD:main"])
    run(["uv", "publish", str(sdist), str(wheel)], env=env)
    wait_for_pypi(version)
    publish_github_release(version, notes_file, release_commit=release_commit)
    run(["git", "status", "--short", "--branch"])


def publish_github_release(
    version: str,
    notes_file: Path | None = None,
    *,
    release_commit: str | None = None,
) -> None:
    tag = f"v{version}"
    release_commit = release_commit or release_commit_for_version(version)
    existing_tag = git("tag", "--list", tag)
    if existing_tag:
        tagged_commit = git("rev-list", "-n", "1", tag)
        if tagged_commit != release_commit:
            raise SpiceError(
                f"tag {tag} already exists on {tagged_commit}, not {release_commit}"
            )
    else:
        run(["git", "tag", "-a", tag, release_commit, "-m", f"release: {tag}"])

    run(["git", "push", "origin", tag])
    existing_release_url = github_release_url(tag)
    if existing_release_url:
        print(f"GitHub release exists: {existing_release_url}")
        return

    if notes_file is not None:
        run(
            [
                "gh",
                "release",
                "create",
                tag,
                "--title",
                tag,
                "--notes-file",
                str(notes_file),
            ]
        )
        return

    with tempfile.NamedTemporaryFile("w", encoding="utf-8", delete=False) as handle:
        path = Path(handle.name)
        handle.write(release_notes_for_version(version, release_commit))
    try:
        run(["gh", "release", "create", tag, "--title", tag, "--notes-file", str(path)])
    finally:
        path.unlink(missing_ok=True)


def github_release_url(tag: str) -> str:
    result = run_tool_command(
        ["gh", "release", "view", tag, "--json", "url", "--jq", ".url"],
        policy="release",
        operation="read GitHub release URL",
        capture_output=True,
        text=True,
    )
    output = result.stdout.strip()
    return output if result.returncode == 0 and output.startswith("https://") else ""


def read_pypi_token() -> str:
    path = Path.home() / ".pypirc"
    config = configparser.RawConfigParser()
    if not config.read(path):
        raise SpiceError(f"missing {path}")
    if not config.has_section("pypi"):
        raise SpiceError(f"{path} is missing [pypi]")
    token = config.get("pypi", "password", fallback="").strip()
    if not token.startswith("pypi-"):
        raise SpiceError("expected a PyPI token in ~/.pypirc [pypi].password")
    return token


def wait_for_pypi(target: str) -> None:
    for _ in range(PYPI_POLL_ATTEMPTS):
        with urllib.request.urlopen(PYPI_URL, timeout=20) as response:
            import json

            version = json.load(response)["info"]["version"]
        print(f"PyPI reports {version}")
        if version == target:
            return
        time.sleep(PYPI_POLL_SECONDS)
    raise SpiceError(f"PyPI never reported {target}")


def print_prepare_instructions(version: str) -> None:
    print(
        "prepared release "
        f"{version}; review, then run "
        f"spice release notes > /tmp/spice-release-{version}-notes.md"
    )
    print(
        "the draft already includes collapsed task-level details — replace the "
        "Highlights placeholder, drop the draft banner, keep the details section, "
        "then run "
        f"spice release publish --notes-file /tmp/spice-release-{version}-notes.md "
        "--apply"
    )


def git(*args: str) -> str:
    return run(["git", *args], capture=True).stdout.strip()


def run(
    command: list[str],
    *,
    capture: bool = False,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    return run_tool_command(
        command,
        policy="release",
        operation="run release command",
        capture_output=capture,
        check=True,
        cwd=cwd,
        text=True,
        env=env,
    )


if __name__ == "__main__":
    raise SystemExit(main())

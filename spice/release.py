"""`spice release ...` — prepare, publish, and summarize releases."""

from __future__ import annotations

import argparse
import configparser
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib
import urllib.request
from collections.abc import Callable
from pathlib import Path

from spice.cli.effects import (
    AuthoredInputInvocation,
    EffectRead,
    MutationDecision,
    mark_authored_input,
)
from spice.commandplan import assert_plan_digest
from spice.commandownership import defer_command_owned_apply
from spice.errors import SpiceError
from spice.process.tool import run_tool_command
from spice.releaseidentity import (
    INSTALLED_CLI_PROBE_SCRIPT as INSTALLED_CLI_PROBE_SCRIPT,
    RUNTIME_PYTHON_ENV as RUNTIME_PYTHON_ENV,
    InstalledCliRegistry,
    InstalledCliSource,
    installed_cli_identity as _probe_installed_cli_identity,
    require_installed_cli_matches_release as _require_installed_cli_matches_release,
)
from spice.releasenotes import (
    ReleaseRecord,
    commit_records as _collect_commit_records,
    edited_release_highlight as edited_release_highlight,
    is_ancestor as _release_note_is_ancestor,
    render_release_notes,
    render_release_range,
)
from spice.releaseplan import (
    ReleasePlan,
    ReleasePlanOperation,
    curated_notes_operation as _curated_notes_operation,
    github_publication_operations as _github_publication_operations,
    publication_operations as _publication_operations,
)

BUMP_CHOICES = ("minor", "patch")
PLAYWRIGHT_MCP_CONFIG_ENV = "SPICE_PLAYWRIGHT_MCP_CONFIG"  # env-policy: allow
PYPI_POLL_ATTEMPTS = 20
PYPI_POLL_SECONDS = 3
PYPI_URL = "https://pypi.org/pypi/spice-harness/json"


SIGINT_EXIT_CODE = 130


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
        _add_notes_file(one_pass)
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
    _add_notes_file(publish)
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
    _add_notes_file(github)
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


def _add_notes_file(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--notes-file",
        type=Path,
        help="Curated GitHub release notes; untouched generated drafts are refused.",
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
            requested_version = args.version
            version = str(requested_version or current_version())
            target = getattr(args, "release_commit", None)
            release_commit = release_commit_for_target(
                version,
                target,
            )
            if target is not None:
                version = version_for_release_commit(requested_version, release_commit)
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
        requested_version = args.version
        version = str(requested_version or current_version())
        target = getattr(args, "release_commit", None)
        release_commit = release_commit_for_target(
            version,
            target,
        )
        if target is not None:
            version = version_for_release_commit(requested_version, release_commit)
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
        payload = plan.payload()
        assert_plan_digest(payload, expected_digest)
        if defer_command_owned_apply(
            payload,
            apply_requested=apply_requested,
            environ=os.environ,  # env-policy: allow
        ):
            print(json.dumps(payload, indent=2, sort_keys=True))
            return 0
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
                "verify-installed-runtime",
                "prove the independently installed CLI matches this release",
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
        target = getattr(args, "release_commit", None)
        release_commit = release_commit_for_target(
            version,
            target,
        )
        if target is not None:
            version = version_for_release_commit(version, release_commit)
        ensure_publish_release_commit_is_head(release_commit)
        operations = [
            _curated_notes_operation(),
            ReleasePlanOperation(
                "verify-installed-runtime",
                "prove the independently installed CLI matches this release",
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
            *_publication_operations(version, check_notes=False),
        ]
    elif mode == "github":
        requested_version = args.version
        version = str(requested_version or current_version())
        target = getattr(args, "release_commit", None)
        release_commit = release_commit_for_target(
            version,
            target,
        )
        if target is not None:
            version = version_for_release_commit(requested_version, release_commit)
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
        notes_file = getattr(args, "notes_file", None)
        release_commit = git("rev-parse", "HEAD")
        ensure_curated_release_notes(notes_file, release_commit)
        publish_release(version, notes_file, release_commit=release_commit)
        return 0
    if mode == "publish":
        release_commit = plan.release_commit
        if release_commit is None:
            raise SpiceError("publish plan is missing its release commit")
        notes_file = getattr(args, "notes_file", None)
        ensure_curated_release_notes(notes_file, release_commit)
        run_release_gates(root, lambda: plan.version)
        publish_release(
            plan.version,
            notes_file,
            release_commit=release_commit,
        )
        return 0
    if mode == "github":
        release_commit = plan.release_commit
        if release_commit is None:
            raise SpiceError("GitHub plan is missing its release commit")
        notes_file = getattr(args, "notes_file", None)
        ensure_curated_release_notes(notes_file, release_commit)
        publish_github_release(
            plan.version,
            notes_file,
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
    require_installed_cli_matches_release(root)
    clean_build_artifacts(root)
    run_constitution_gate()
    version = choose_version()
    run_artifact_gate(version)
    return version


def require_installed_cli_matches_release(
    root: Path,
) -> InstalledCliSource | InstalledCliRegistry:
    installed = _installed_cli_identity()
    if installed.python.is_relative_to(root.resolve()):
        raise SpiceError(
            "release evidence must come from the independently installed CLI, "
            f"not the candidate worktree interpreter {installed.python}"
        )
    candidate_commit, candidate_tree = _source_identity(root)
    return _require_installed_cli_matches_release(
        root,
        installed,
        candidate_commit,
        candidate_tree,
        run=run,
    )


def _installed_cli_identity() -> InstalledCliSource | InstalledCliRegistry:
    raw_python = str(
        os.environ.get(RUNTIME_PYTHON_ENV) or ""  # env-policy: allow
    )
    return _probe_installed_cli_identity(
        raw_python,
        environ=os.environ,  # env-policy: allow
        run=run,
        source_identity=_source_identity,
        worktree_drift=_worktree_drift,
    )


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


def previous_release_tag(current_tag: str, release_commit: str) -> str:
    raw = git(
        "tag",
        "--merged",
        release_commit,
        "--list",
        "v*",
        "--sort=-v:refname",
    )
    for tag in raw.splitlines():
        if tag and tag != current_tag:
            return tag
    return ""


def release_notes_for_version(version: str, release_commit: str) -> str:
    current_tag = f"v{version}"
    previous_tag = previous_release_tag(current_tag, release_commit)
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


def release_range_for_version(version: str, release_commit: str) -> str:
    current_tag = f"v{version}"
    previous_tag = previous_release_tag(current_tag, release_commit)
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


def commit_records(previous_tag: str, release_commit: str) -> list[ReleaseRecord]:
    return _collect_commit_records(
        previous_tag,
        release_commit,
        run=run,
        is_ancestor=_is_ancestor,
    )


def _is_ancestor(candidate: str, commit: str) -> bool:
    return _release_note_is_ancestor(candidate, commit)


def short_commit(commit: str) -> str:
    return git("rev-parse", "--short", commit)


def release_version_at_commit(release_commit: str) -> str:
    """Read the package version from the exact tree the release will name."""
    try:
        pyproject = tomllib.loads(git("show", f"{release_commit}:pyproject.toml"))
        version = pyproject["project"]["version"]
    except (KeyError, TypeError, tomllib.TOMLDecodeError) as exc:
        raise SpiceError(
            f"release commit {release_commit} has no valid project.version"
        ) from exc
    if not isinstance(version, str) or not version:
        raise SpiceError(
            f"release commit {release_commit} has no valid project.version"
        )
    return version


def version_for_release_commit(
    requested_version: str | None,
    release_commit: str,
) -> str:
    """Bind an explicit release target to the version stored in that tree."""
    tree_version = release_version_at_commit(release_commit)
    if requested_version is not None and requested_version != tree_version:
        raise SpiceError(
            f"release commit {release_commit} contains version {tree_version}, "
            f"not requested version {requested_version}"
        )
    return tree_version


def canonical_release_notes_for_commit(release_commit: str) -> str:
    """Generate the untouched draft solely from the candidate release tree."""
    return release_notes_for_version(
        release_version_at_commit(release_commit),
        release_commit,
    )


def ensure_curated_release_notes(
    notes_file: Path | None,
    release_commit: str,
) -> None:
    """Refuse the generator's untouched Highlights scaffold before publication."""
    canonical = canonical_release_notes_for_commit(release_commit).encode("utf-8")
    candidate = canonical if notes_file is None else notes_file.read_bytes()
    if candidate == canonical:
        raise SpiceError(
            "refusing to publish untouched generated release notes: the candidate "
            "still exactly matches the canonical draft and retains the Highlights "
            "placeholder; curate Highlights before publication"
        )


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

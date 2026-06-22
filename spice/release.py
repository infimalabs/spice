"""`spice release ...` — prepare, publish, and summarize releases."""

from __future__ import annotations

import argparse
import configparser
import os
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request
from collections import OrderedDict
from dataclasses import dataclass
from pathlib import Path

from spice.agent.gitshadow import scrub_agent_git_shadow_environment
from spice.errors import SpiceError

BUMP_CHOICES = ("minor", "patch")
PYPI_POLL_ATTEMPTS = 20
PYPI_POLL_SECONDS = 3
PYPI_URL = "https://pypi.org/pypi/spice-harness/json"
PROJECT_HEADINGS = {
    "cli": "CLI",
    "ui": "UI",
}


@dataclass(frozen=True)
class ReleaseRecord:
    commit: str
    subject: str
    project: str


SIGINT_EXIT_CODE = 130


def build_release_parser(prog: str = "spice release") -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog=prog,
        description=(
            "Prepare, publish, and summarize spice releases from a clean main worktree."
        ),
    )
    actions = parser.add_subparsers(dest="release_action", required=True)

    for bump in BUMP_CHOICES:
        one_pass = actions.add_parser(
            bump,
            help=f"Bump {bump}, validate, commit, push, and publish.",
        )
        one_pass.set_defaults(func=handle_release, release_mode="release", bump=bump)

    prepare = actions.add_parser(
        "prepare", help="Bump, validate, and commit without publishing."
    )
    prepare.add_argument("bump", choices=BUMP_CHOICES)
    prepare.set_defaults(func=handle_release, release_mode="prepare")

    notes = actions.add_parser("notes", help="Generate edited release-note highlights.")
    notes.add_argument("version", nargs="?")
    notes.add_argument("--output", type=Path, help="Write notes to this path.")
    notes.set_defaults(func=handle_release, release_mode="notes")

    publish = actions.add_parser(
        "publish", help="Validate the prepared version, then push and publish."
    )
    publish.add_argument("--notes-file", type=Path)
    publish.set_defaults(func=handle_release, release_mode="publish")

    github = actions.add_parser(
        "github", help="Create/push the release tag and GitHub Release."
    )
    github.add_argument("version", nargs="?")
    github.add_argument("--notes-file", type=Path)
    github.set_defaults(func=handle_release, release_mode="github")
    return parser


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
    if mode != "notes":
        ensure_release_worktree(root)
    if mode in {"release", "publish", "github"}:
        ensure_notes_file(getattr(args, "notes_file", None))

    if mode in {"prepare", "release"}:
        run_constitution_gate()
        version = bump_version(str(args.bump))
        run_artifact_gate(version)
        run(["git", "add", "pyproject.toml", "uv.lock"])
        run(["git", "commit", "-m", f"release: bump to {version}"])
        if mode == "prepare":
            print_prepare_instructions(version)
            run(["git", "status", "--short", "--branch"])
            return 0
        publish_release(version, getattr(args, "notes_file", None))
        return 0

    if mode == "notes":
        version = str(args.version or current_version())
        release_commit = release_commit_for_version(version)
        output = release_notes_for_version(version, release_commit)
        notes_output = getattr(args, "output", None)
        if notes_output:
            notes_output.write_text(output, encoding="utf-8")
            print(f"wrote release notes draft for {version} to {notes_output}")
        else:
            print(output, end="" if output.endswith("\n") else "\n")
        return 0

    if mode == "publish":
        version = current_version()
        run_constitution_gate()
        run_artifact_gate(version)
        publish_release(version, getattr(args, "notes_file", None))
        return 0

    if mode == "github":
        version = str(args.version or current_version())
        publish_github_release(version, getattr(args, "notes_file", None))
        return 0

    raise SpiceError(f"unknown release action {mode!r}")


def repo_root() -> Path:
    result = run(["git", "rev-parse", "--show-toplevel"], capture=True)
    return Path(result.stdout.strip()).resolve()


def ensure_release_worktree(root: Path) -> None:
    # Lane branches are kept synchronized with origin/main, so a release runs
    # from whichever clean worktree we are in, regardless of the branch name —
    # only a dirty tree blocks it.
    status = git("status", "--porcelain")
    if status:
        raise SpiceError("refusing to release with a dirty worktree")


def ensure_notes_file(path: Path | None) -> None:
    if path is None:
        return
    if not path.is_file():
        raise SpiceError(f"release notes file not found: {path}")


def run_constitution_gate() -> None:
    run(["uv", "run", "pytest"])
    run(["uv", "run", "ruff", "check", "."])


def run_artifact_gate(version: str) -> None:
    sdist = Path("dist") / f"spice_harness-{version}.tar.gz"
    wheel = Path("dist") / f"spice_harness-{version}-py3-none-any.whl"

    shutil.rmtree("dist", ignore_errors=True)
    run(["uv", "build", "--python", "3.12"])
    run(["uvx", "twine", "check", str(sdist), str(wheel)])

    with tempfile.TemporaryDirectory() as tmpdir:
        venv = Path(tmpdir) / "venv"
        python = venv / "bin" / "python"
        spice = venv / "bin" / "spice"
        run(["uv", "venv", "--python", "3.12", str(venv)])
        run(["uv", "pip", "install", "--python", str(python), str(wheel)])
        smoke_env = hermetic_wheel_env()
        run([str(spice), "--help"], capture=True, env=smoke_env)
        run([str(spice), "task", "--help"], capture=True, env=smoke_env)
        run([str(spice), "session", "--help"], capture=True, env=smoke_env)


def hermetic_wheel_env() -> dict[str, str]:
    # The freshly installed wheel must be imported on its own merits. A spice
    # agent shell exports PYTHONPATH=<worktree> (and VIRTUAL_ENV), which would
    # shadow the venv's site-packages and let the smoke pass against worktree
    # source even if the built wheel were broken. Strip both so the gate
    # validates the artifact it just installed.
    env = release_environment()
    env.pop("PYTHONPATH", None)
    env.pop("VIRTUAL_ENV", None)
    return env


def current_version() -> str:
    return run(["uv", "version", "--short"], capture=True).stdout.strip()


def bump_version(bump: str) -> str:
    return run(
        ["uv", "version", "--bump", bump, "--no-sync", "--short"],
        capture=True,
    ).stdout.strip()


def release_commit_for_version(version: str) -> str:
    commit = git(
        "log", "--format=%H", "--grep", f"^release: bump to {version}$", "-n", "1"
    )
    return commit or git("rev-parse", "HEAD")


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


def commit_records(previous_tag: str, release_commit: str) -> list[ReleaseRecord]:
    format_arg = "--format=%H%x1f%s%x1f%(trailers:key=Task-Project,valueonly)%x1e"
    if previous_tag:
        args = [
            "log",
            "--first-parent",
            "--reverse",
            format_arg,
            f"{previous_tag}..{release_commit}",
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
    records: list[ReleaseRecord] = []
    for raw_record in raw.split("\x1e"):
        raw_record = raw_record.strip("\n")
        if not raw_record:
            continue
        commit, subject, project = (raw_record.split("\x1f", 2) + ["", "", ""])[:3]
        if subject.startswith("release: bump to "):
            continue
        records.append(
            ReleaseRecord(
                commit=commit,
                subject=subject,
                project=project.strip() or "general",
            )
        )
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
            edited_release_highlight(record.subject), []
        ).append(shortish_commit(record.commit))

    lines = ["## Highlights", ""]
    if groups:
        for project, subjects in groups.items():
            lines.extend([f"### {release_project_heading(project)}", ""])
            for highlight, commits in subjects.items():
                refs = ", ".join(f"`{commit}`" for commit in commits)
                lines.append(f"- {highlight} ({refs})")
            lines.append("")
    else:
        lines.extend(["- No non-release commits found.", ""])

    lines.extend(
        [
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


def publish_release(version: str, notes_file: Path | None = None) -> None:
    sdist = Path("dist") / f"spice_harness-{version}.tar.gz"
    wheel = Path("dist") / f"spice_harness-{version}-py3-none-any.whl"
    token = read_pypi_token()

    # Push the release commit (made on a synchronized lane) to origin/main by
    # ref, so the local branch name does not have to be `main`.
    run(["git", "push", "origin", "HEAD:main"])
    env = release_environment()
    env["UV_PUBLISH_TOKEN"] = token
    run(["uv", "publish", "--dry-run", str(sdist), str(wheel)], env=env)
    run(["uv", "publish", str(sdist), str(wheel)], env=env)
    wait_for_pypi(version)
    publish_github_release(version, notes_file)
    run(["git", "status", "--short", "--branch"])


def publish_github_release(version: str, notes_file: Path | None = None) -> None:
    tag = f"v{version}"
    release_commit = release_commit_for_version(version)
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
    result = subprocess.run(
        ["gh", "release", "view", tag, "--json", "url", "--jq", ".url"],
        capture_output=True,
        text=True,
        check=False,
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
        "curate the draft notes, then run "
        f"spice release publish --notes-file /tmp/spice-release-{version}-notes.md"
    )


def git(*args: str) -> str:
    return run(["git", *args], capture=True).stdout.strip()


def run(
    command: list[str],
    *,
    capture: bool = False,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        check=True,
        text=True,
        capture_output=capture,
        env=release_environment() if env is None else env,
    )


def release_environment() -> dict[str, str]:
    # Release git ops must see the real upstream, not the agent git-shadow
    # (branch.<name>.remote = . injected via GIT_CONFIG_KEY_n pairs) that makes
    # `git status`/push compare against a stale local ref. Scrubbing the shadow
    # pairs keeps the ahead-count, push, tag, and range queries on real origin.
    return scrub_agent_git_shadow_environment(os.environ)


if __name__ == "__main__":
    raise SystemExit(main())

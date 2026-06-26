"""Release command parsing and release-note highlights."""

import subprocess
from pathlib import Path

import pytest

import spice.release as release
from spice.errors import SpiceError
from spice.cli.mounts import mounted_commands
from spice.release import (
    ReleaseRecord,
    build_release_parser,
    edited_release_highlight,
    render_release_notes,
)


def test_release_parser_accepts_prepare_notes_publish_and_one_pass():
    parser = build_release_parser()

    prepare = parser.parse_args(["prepare", "minor"])
    notes = parser.parse_args(
        ["notes", "0.3.0", "--output", "notes.md", "--release-commit", "HEAD"]
    )
    publish = parser.parse_args(
        ["publish", "--notes-file", "curated.md", "--release-commit", "HEAD"]
    )
    github = parser.parse_args(["github", "0.3.0", "--release-commit", "HEAD"])
    one_pass = parser.parse_args(["minor"])

    assert prepare.release_mode == "prepare"
    assert prepare.bump == "minor"
    assert release.BUMP_CHOICES == ("minor", "patch")
    assert notes.release_mode == "notes"
    assert notes.version == "0.3.0"
    assert notes.output == Path("notes.md")
    assert notes.release_commit == "HEAD"
    assert publish.release_mode == "publish"
    assert publish.notes_file == Path("curated.md")
    assert publish.release_commit == "HEAD"
    assert github.release_mode == "github"
    assert github.version == "0.3.0"
    assert github.release_commit == "HEAD"
    assert one_pass.release_mode == "release"
    assert one_pass.bump == "minor"


def test_release_docs_show_lane_release_workflow():
    readme = Path("README.md").read_text(encoding="utf-8")
    release_section = readme.split("## Release", 1)[1].split("## Status", 1)[0]
    help_text = build_release_parser().format_help()
    normalized_help = " ".join(help_text.split())
    normalized_section = " ".join(release_section.split())
    release_commands = (
        release_section.split("```sh", 1)[1].split("```", 1)[0].strip().splitlines()
    )

    assert "{minor,patch,prepare,notes,publish,github}" in help_text
    assert "clean synchronized worktree" in normalized_help
    assert normalized_section.startswith(
        "Releases are cut from a clean synchronized worktree with this "
        "repository's mounted `spice release` command. Lane branches are "
        "allowed; the release command pushes the prepared release commit to "
        "`origin/main`."
    )
    assert release_commands == [
        "spice release prepare minor   # bump, validate, commit, stop before publish",
        "spice release notes > /tmp/spice-release-notes.md",
        "spice release publish --notes-file /tmp/spice-release-notes.md",
        "spice release minor           # one-pass bump, validate, commit, publish",
    ]
    assert release_section.index("Use a minor release") < release_section.index(
        "Use a patch release"
    )


def test_repo_mounts_release_command(tmp_path):
    (tmp_path / ".git").mkdir()
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.commands]\n"
        'release = ["uv", "run", "python", "-m", "spice.release"]\n',
        encoding="utf-8",
    )

    assert mounted_commands(tmp_path)[("release",)] == (
        "uv",
        "run",
        "python",
        "-m",
        "spice.release",
    )


def test_release_notes_mode_writes_output_without_release_sync(tmp_path, monkeypatch):
    parser = build_release_parser()
    notes_path = tmp_path / "notes.md"
    args = parser.parse_args(["notes", "0.3.0", "--output", str(notes_path)])

    def fail_release_sync(_root):
        raise AssertionError("notes generation is read-only")

    starting_cwd = Path.cwd()
    monkeypatch.setattr(release, "repo_root", lambda: tmp_path)
    monkeypatch.setattr(release, "ensure_release_worktree", fail_release_sync)
    monkeypatch.setattr(
        release,
        "release_commit_for_version",
        lambda version: f"commit-for-{version}",
    )
    monkeypatch.setattr(
        release,
        "release_notes_for_version",
        lambda version, commit: f"notes for {version} at {commit}\n",
    )

    result = release.handle_release(args)

    assert result == 0
    assert Path.cwd() == starting_cwd
    assert notes_path.read_text(encoding="utf-8") == (
        "notes for 0.3.0 at commit-for-0.3.0\n"
    )


def test_release_notes_mode_uses_explicit_release_commit_target(tmp_path, monkeypatch):
    parser = build_release_parser()
    notes_path = tmp_path / "notes.md"
    args = parser.parse_args(
        [
            "notes",
            "0.3.0",
            "--release-commit",
            "main",
            "--output",
            str(notes_path),
        ]
    )

    seen = []
    monkeypatch.setattr(release, "repo_root", lambda: tmp_path)
    monkeypatch.setattr(
        release,
        "release_commit_for_target",
        lambda version, target: seen.append((version, target)) or "resolved-main",
    )
    monkeypatch.setattr(
        release,
        "release_notes_for_version",
        lambda version, commit: f"notes for {version} at {commit}\n",
    )

    result = release.handle_release(args)

    assert result == 0
    assert seen == [("0.3.0", "main")]
    assert notes_path.read_text(encoding="utf-8") == (
        "notes for 0.3.0 at resolved-main\n"
    )


def test_release_commit_for_tagged_version_uses_tagged_commit(monkeypatch):
    def fake_git(*args):
        if args == ("tag", "--list", "v0.9.0"):
            return "v0.9.0"
        if args == ("rev-list", "-n", "1", "v0.9.0"):
            return "tagged-commit"
        raise AssertionError(args)

    monkeypatch.setattr(release, "git", fake_git)

    assert release.release_commit_for_version("0.9.0") == "tagged-commit"


def test_release_commit_for_current_unreleased_version_uses_head(monkeypatch):
    def fake_git(*args):
        if args == ("tag", "--list", "v0.9.0"):
            return ""
        if args == ("rev-parse", "HEAD"):
            return "current-head"
        if args == (
            "log",
            "--format=%H",
            "--grep",
            "^release: bump to 0.9.0$",
            "-n",
            "1",
        ):
            return "old-bump-commit"
        raise AssertionError(args)

    monkeypatch.setattr(release, "git", fake_git)
    monkeypatch.setattr(release, "current_version", lambda: "0.9.0")

    assert release.release_commit_for_version("0.9.0") == "current-head"


def test_release_commit_for_target_resolves_explicit_commitish(monkeypatch):
    def fake_git(*args):
        if args == ("rev-parse", "--verify", "main^{commit}"):
            return "resolved-main"
        raise AssertionError(args)

    monkeypatch.setattr(release, "git", fake_git)

    assert release.release_commit_for_target("0.9.0", "main") == "resolved-main"


def test_publish_mode_with_head_target_runs_gates_before_publish(tmp_path, monkeypatch):
    parser = build_release_parser()
    args = parser.parse_args(["publish", "--release-commit", "HEAD"])
    calls = []

    monkeypatch.setattr(release, "repo_root", lambda: tmp_path)
    monkeypatch.setattr(release, "ensure_release_worktree", lambda root: None)
    monkeypatch.setattr(release, "current_version", lambda: "0.9.0")
    monkeypatch.setattr(
        release,
        "release_commit_for_target",
        lambda version, target: calls.append(("target", version, target)) or "head",
    )
    monkeypatch.setattr(
        release,
        "ensure_publish_release_commit_is_head",
        lambda commit: calls.append(("head", commit)),
    )
    monkeypatch.setattr(
        release, "run_constitution_gate", lambda: calls.append("constitution")
    )
    monkeypatch.setattr(
        release, "run_artifact_gate", lambda version: calls.append(version)
    )
    monkeypatch.setattr(
        release,
        "publish_release",
        lambda version, notes_file, *, release_commit=None: calls.append(
            ("publish", version, notes_file, release_commit)
        ),
    )

    result = release._handle_release_from_root(args, tmp_path)

    assert result == 0
    assert calls == [
        ("target", "0.9.0", "HEAD"),
        ("head", "head"),
        "constitution",
        "0.9.0",
        ("publish", "0.9.0", None, "head"),
    ]


def test_publish_release_with_head_commit_uses_current_artifacts(monkeypatch):
    calls = []

    def fake_git(*args):
        calls.append(("git", args))
        if args == ("rev-parse", "HEAD"):
            return "head-commit"
        raise AssertionError(args)

    def fake_run(command, **kwargs):
        calls.append(("run", command, "UV_PUBLISH_TOKEN" in kwargs.get("env", {})))

    monkeypatch.setattr(release, "git", fake_git)
    monkeypatch.setattr(release, "run", fake_run)
    monkeypatch.setattr(release, "read_pypi_token", lambda: "pypi-token")
    monkeypatch.setattr(
        release, "wait_for_pypi", lambda version: calls.append(("pypi", version))
    )
    monkeypatch.setattr(
        release,
        "publish_github_release",
        lambda version, notes_file, *, release_commit=None: calls.append(
            ("github", version, notes_file, release_commit)
        ),
    )

    release.publish_release("0.9.0", release_commit="head-commit")

    assert calls == [
        ("git", ("rev-parse", "HEAD")),
        (
            "run",
            [
                "uv",
                "publish",
                "--dry-run",
                "dist/spice_harness-0.9.0.tar.gz",
                "dist/spice_harness-0.9.0-py3-none-any.whl",
            ],
            True,
        ),
        ("run", ["git", "push", "origin", "HEAD:main"], False),
        (
            "run",
            [
                "uv",
                "publish",
                "dist/spice_harness-0.9.0.tar.gz",
                "dist/spice_harness-0.9.0-py3-none-any.whl",
            ],
            True,
        ),
        ("pypi", "0.9.0"),
        ("github", "0.9.0", None, "head-commit"),
        ("run", ["git", "status", "--short", "--branch"], False),
    ]


def test_publish_github_release_uses_explicit_release_commit(monkeypatch):
    git_calls = []
    run_calls = []

    def fake_git(*args):
        git_calls.append(args)
        if args == ("tag", "--list", "v0.9.0"):
            return ""
        raise AssertionError(args)

    def fake_run(command, **_kwargs):
        run_calls.append(command)

    monkeypatch.setattr(release, "git", fake_git)
    monkeypatch.setattr(release, "run", fake_run)
    monkeypatch.setattr(
        release, "github_release_url", lambda tag: f"https://example.test/{tag}"
    )

    release.publish_github_release("0.9.0", release_commit="target-commit")

    assert git_calls == [("tag", "--list", "v0.9.0")]
    assert run_calls == [
        ["git", "tag", "-a", "v0.9.0", "target-commit", "-m", "release: v0.9.0"],
        ["git", "push", "origin", "v0.9.0"],
    ]


def test_hermetic_wheel_env_preserves_process_environment(monkeypatch):
    monkeypatch.setenv("PYTHONPATH", "/some/worktree")
    monkeypatch.setenv("VIRTUAL_ENV", "/some/venv")
    monkeypatch.setenv("PATH", "/usr/bin")

    env = release.hermetic_wheel_env()

    assert {name: env.get(name) for name in ("PATH", "PYTHONPATH", "VIRTUAL_ENV")} == {
        "PATH": "/usr/bin",
        "PYTHONPATH": "/some/worktree",
        "VIRTUAL_ENV": "/some/venv",
    }


def test_release_highlight_rewrites_commit_subjects_into_sentences():
    assert (
        edited_release_highlight("Fix speech excerpts for final ACK messages")
        == "Fixed speech excerpts for final ACK messages."
    )
    assert (
        edited_release_highlight("Expose release tooling as spice command")
        == "Added release tooling as spice command."
    )


def test_release_notes_group_edited_highlights_by_project():
    notes = render_release_notes(
        version="0.3.0",
        release_commit="abcdef1234567890",
        release_short="abcdef1",
        current_tag="v0.3.0",
        previous_tag="v0.2.1",
        records=[
            ReleaseRecord(
                commit="1111111aaaa",
                subject="Fix speech excerpts for final ACK messages",
                project="serve",
            ),
            ReleaseRecord(
                commit="2222222bbbb",
                subject="Expose release tooling as spice command",
                project="cli",
            ),
            ReleaseRecord(
                commit="3333333cccc",
                subject="Fix speech excerpts for final ACK messages",
                project="serve",
            ),
            ReleaseRecord(
                commit="4444444dddd",
                subject="Fix narration media session retention",
                project="serve.ui",
            ),
            ReleaseRecord(
                commit="5555555eeee",
                subject="Implement dynamic agent shell-hook surfaces",
                project="task.cli",
            ),
            ReleaseRecord(
                commit="6666666ffff",
                subject="Show agent stem in active header pills",
                project="agent.019ec753620c7cf2b18c06707ac93cbb.task",
            ),
        ],
    )

    assert notes == (
        "## Highlights\n"
        "\n"
        "### Serve\n"
        "\n"
        "- Fixed speech excerpts for final ACK messages. (`1111111`, `3333333`)\n"
        "\n"
        "### CLI\n"
        "\n"
        "- Added release tooling as spice command. (`2222222`)\n"
        "\n"
        "### Serve UI\n"
        "\n"
        "- Fixed narration media session retention. (`4444444`)\n"
        "\n"
        "### Task CLI\n"
        "\n"
        "- Implement dynamic agent shell-hook surfaces. (`5555555`)\n"
        "\n"
        "### General\n"
        "\n"
        "- Show agent stem in active header pills. (`6666666`)\n"
        "\n"
        "## Package Notes\n"
        "\n"
        "- PyPI release: `spice-harness==0.3.0`\n"
        "- Release commit: `abcdef1`\n"
        "- Commit range: `v0.2.1..abcdef1`\n"
        "- Commit source: first-parent history grouped by `Task-Project` metadata\n"
        "- Release tag: `v0.3.0`\n"
    )


def _git(repo: Path, *args: str) -> None:
    subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True)


def test_release_worktree_guard_allows_clean_lane_blocks_dirty(tmp_path, monkeypatch):
    repo = tmp_path / "repo"
    subprocess.run(["git", "init", "-q", "-b", "main", str(repo)], check=True)
    _git(repo, "config", "user.email", "r@example.test")
    _git(repo, "config", "user.name", "Release Tester")
    (repo / "f.txt").write_text("x\n", encoding="utf-8")
    _git(repo, "add", ".")
    _git(repo, "commit", "-qm", "init")
    # A lane branch whose name is not "main".
    _git(repo, "checkout", "-qb", "main-d")
    monkeypatch.chdir(repo)

    # Clean lane (non-main name) is accepted — synchronization is assumed.
    release.ensure_release_worktree(repo)

    # A dirty tree still blocks the release.
    (repo / "g.txt").write_text("y\n", encoding="utf-8")
    with pytest.raises(SpiceError, match="dirty worktree"):
        release.ensure_release_worktree(repo)

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
    notes = parser.parse_args(["notes", "0.3.0", "--output", "notes.md"])
    publish = parser.parse_args(["publish", "--notes-file", "curated.md"])
    one_pass = parser.parse_args(["minor"])

    assert prepare.release_mode == "prepare"
    assert prepare.bump == "minor"
    assert release.BUMP_CHOICES == ("minor", "patch")
    assert notes.release_mode == "notes"
    assert notes.version == "0.3.0"
    assert notes.output == Path("notes.md")
    assert publish.release_mode == "publish"
    assert publish.notes_file == Path("curated.md")
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


def test_hermetic_wheel_env_drops_source_shadowing_entries(monkeypatch):
    monkeypatch.setenv("PYTHONPATH", "/some/worktree")
    monkeypatch.setenv("VIRTUAL_ENV", "/some/venv")
    monkeypatch.setenv("PATH", "/usr/bin")

    env = release.hermetic_wheel_env()

    assert {name: env.get(name) for name in ("PATH", "PYTHONPATH", "VIRTUAL_ENV")} == {
        "PATH": "/usr/bin",
        "PYTHONPATH": None,
        "VIRTUAL_ENV": None,
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

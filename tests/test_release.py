"""Release command parsing and release-note highlights."""

from pathlib import Path

import spice.release as release
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
    one_pass = parser.parse_args(["patch"])

    assert prepare.release_mode == "prepare"
    assert prepare.bump == "minor"
    assert notes.release_mode == "notes"
    assert notes.version == "0.3.0"
    assert notes.output == Path("notes.md")
    assert publish.release_mode == "publish"
    assert publish.notes_file == Path("curated.md")
    assert one_pass.release_mode == "release"
    assert one_pass.bump == "patch"


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

    assert "### Serve" in notes
    assert (
        "- Fixed speech excerpts for final ACK messages. (`1111111`, `3333333`)"
        in notes
    )
    assert "### CLI" in notes
    assert "- Added release tooling as spice command. (`2222222`)" in notes
    assert "### Serve UI" in notes
    assert "- Fixed narration media session retention. (`4444444`)" in notes
    assert "### Task CLI" in notes
    assert "- Implement dynamic agent shell-hook surfaces. (`5555555`)" in notes
    assert "### General" in notes
    assert "- Show agent stem in active header pills. (`6666666`)" in notes
    assert "- PyPI release: `spice-harness==0.3.0`" in notes
    assert "- Commit range: `v0.2.1..abcdef1`" in notes
    assert "- fix speech excerpts" not in notes
    assert "Serve.Ui" not in notes
    assert "Task.Cli" not in notes
    assert "Agent." not in notes

"""Release repository-state guards, ranges, and record rendering."""

import subprocess
from pathlib import Path

import pytest

import spice.release as release
from spice.errors import SpiceError
from spice.release import ReleaseRecord, build_release_parser, render_release_range
from spice.tasks import claimstate


def _git(repo: Path, *args: str) -> None:
    subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True)


def test_release_clean_worktree_guard_allows_any_branch_blocks_dirty(
    tmp_path, monkeypatch
):
    repo = tmp_path / "repo"
    subprocess.run(["git", "init", "-q", "-b", "main", str(repo)], check=True)
    _git(repo, "config", "user.email", "r@example.test")
    _git(repo, "config", "user.name", "Release Tester")
    (repo / "f.txt").write_text("x\n", encoding="utf-8")
    _git(repo, "add", ".")
    _git(repo, "commit", "-qm", "init")
    # Any branch name is fine — there is no dedicated release tree or local main.
    _git(repo, "checkout", "-qb", "main-d")
    monkeypatch.chdir(repo)

    # A clean worktree on any branch is accepted.
    release.ensure_clean_worktree(repo)

    # A dirty tree still blocks the release.
    (repo / "g.txt").write_text("y\n", encoding="utf-8")
    with pytest.raises(SpiceError, match="dirty worktree"):
        release.ensure_clean_worktree(repo)


def _release_repo_state(repo: Path) -> tuple[str, str, str]:
    """The three things a release changes: version, commit graph, tag list."""

    def read(*args: str) -> str:
        return subprocess.run(
            ["git", "-C", str(repo), *args],
            check=True,
            capture_output=True,
            text=True,
        ).stdout

    return (
        (repo / "pyproject.toml").read_text(encoding="utf-8"),
        read("log", "--format=%H"),
        read("tag", "--list"),
    )


def test_release_check_runs_the_gates_and_leaves_the_tree_where_it_found_it(
    tmp_path, monkeypatch, capsys
):
    repo = tmp_path / "repo"
    subprocess.run(["git", "init", "-q", "-b", "main", str(repo)], check=True)
    _git(repo, "config", "user.email", "r@example.test")
    _git(repo, "config", "user.name", "Release Tester")
    (repo / "pyproject.toml").write_text(
        '[project]\nname = "probe-package"\nversion = "1.2.3"\n', encoding="utf-8"
    )
    _git(repo, "add", ".")
    _git(repo, "commit", "-qm", "init")
    _git(repo, "tag", "v1.2.3")
    monkeypatch.chdir(repo)

    # Only the two leaf gates are replaced, because they run the full pytest
    # suite and a real `uv build`. Everything that could mutate -- bump_version,
    # the git add/commit/tag/push calls, publish_release -- stays real and
    # reachable, so a mutating call added to this branch would land on the repo
    # below and the before/after comparison would see it.
    ran: list[object] = []
    monkeypatch.setattr(
        release, "run_constitution_gate", lambda: ran.append("constitution")
    )
    monkeypatch.setattr(
        release, "run_artifact_gate", lambda version: ran.append(("artifact", version))
    )
    monkeypatch.setattr(
        release,
        "require_installed_cli_carries_release_tree",
        lambda root: ran.append(("installed", root)),
    )

    before = _release_repo_state(repo)
    args = build_release_parser().parse_args(["check"])

    assert release.handle_release(args) == 0

    # The gates ran for real, against the version already in the tree.
    assert ran == [
        ("installed", repo),
        "constitution",
        ("artifact", "1.2.3"),
    ]
    assert _release_repo_state(repo) == before
    assert "nothing was bumped" in capsys.readouterr().out


def test_release_preconditions_refuse_without_claim(tmp_path, monkeypatch):
    from spice.tasks.git import boundaries

    monkeypatch.setattr(claimstate, "has_active_claim", lambda: False)
    monkeypatch.setattr(boundaries, "commits_ahead_of_baseline", lambda root: 0)

    with pytest.raises(SpiceError, match="no task claimed"):
        release.ensure_release_preconditions(tmp_path)


def test_release_preconditions_refuse_uncaptured_commits(tmp_path, monkeypatch):
    from spice.tasks.git import boundaries

    monkeypatch.setattr(claimstate, "has_active_claim", lambda: True)
    monkeypatch.setattr(boundaries, "commits_ahead_of_baseline", lambda root: 2)

    with pytest.raises(SpiceError, match="2 local commit"):
        release.ensure_release_preconditions(tmp_path)


def test_release_preconditions_pass_when_claimed_and_baseline_clean(
    tmp_path, monkeypatch
):
    from spice.tasks.git import boundaries

    monkeypatch.setattr(claimstate, "has_active_claim", lambda: True)
    monkeypatch.setattr(boundaries, "commits_ahead_of_baseline", lambda root: 0)

    assert release.ensure_release_preconditions(tmp_path) is None


def test_release_range_mode_is_read_only_and_prints_listing(
    tmp_path, monkeypatch, capsys
):
    parser = build_release_parser()
    args = parser.parse_args(["range", "0.3.0", "--release-commit", "main"])

    def fail_release_sync(_root):
        raise AssertionError("range preview is read-only")

    seen = []
    starting_cwd = Path.cwd()
    monkeypatch.setattr(release, "repo_root", lambda: tmp_path)
    monkeypatch.setattr(release, "ensure_clean_worktree", fail_release_sync)
    monkeypatch.setattr(
        release,
        "release_commit_for_target",
        lambda version, target: seen.append((version, target)) or "resolved-main",
    )
    monkeypatch.setattr(
        release,
        "release_range_for_version",
        lambda version, commit: f"range for {version} at {commit}\n",
    )

    result = release.handle_release(args)

    assert result == 0
    assert Path.cwd() == starting_cwd
    assert seen == [("0.3.0", "main")]
    assert capsys.readouterr().out == "range for 0.3.0 at resolved-main\n"


def test_bare_release_range_mode_uses_head_and_unreleased_renderer(
    tmp_path, monkeypatch, capsys
):
    parser = build_release_parser()
    args = parser.parse_args(["range"])
    seen = []

    monkeypatch.setattr(release, "repo_root", lambda: tmp_path)
    monkeypatch.setattr(
        release,
        "git",
        lambda *args: seen.append(("git", args)) or "head-commit",
    )
    monkeypatch.setattr(
        release,
        "release_range_for_unreleased",
        lambda commit: seen.append(("unreleased", commit)) or "unreleased range\n",
    )

    result = release.handle_release(args)

    assert result == 0
    assert seen == [
        ("git", ("rev-parse", "HEAD")),
        ("unreleased", "head-commit"),
    ]
    assert capsys.readouterr().out == "unreleased range\n"


def test_range_with_explicit_commit_keeps_versioned_resolver(
    tmp_path, monkeypatch, capsys
):
    parser = build_release_parser()
    args = parser.parse_args(["range", "--release-commit", "main"])
    seen = []

    monkeypatch.setattr(release, "repo_root", lambda: tmp_path)
    monkeypatch.setattr(release, "current_version", lambda: "0.20.0")
    monkeypatch.setattr(
        release,
        "release_commit_for_target",
        lambda version, target: (
            seen.append(("target", version, target)) or "resolved-main"
        ),
    )
    monkeypatch.setattr(
        release,
        "release_range_for_version",
        lambda version, commit: (
            seen.append(("range", version, commit)) or "versioned range\n"
        ),
    )

    result = release.handle_release(args)

    assert result == 0
    assert seen == [
        ("target", "0.20.0", "main"),
        ("range", "0.20.0", "resolved-main"),
    ]
    assert capsys.readouterr().out == "versioned range\n"


def test_unreleased_range_uses_latest_merged_tag_and_lists_records(monkeypatch):
    calls = []

    def fake_git(*args):
        calls.append(args)
        if args == (
            "tag",
            "--merged",
            "head-commit",
            "--list",
            "v*",
            "--sort=-v:refname",
        ):
            return "v0.20.0\nv0.19.0"
        if args == ("rev-parse", "--short", "head-commit"):
            return "head123"
        raise AssertionError(args)

    monkeypatch.setattr(release, "git", fake_git)
    monkeypatch.setattr(
        release,
        "commit_records",
        lambda previous, commit: (
            calls.append(("records", previous, commit))
            or [
                ReleaseRecord(
                    commit="abcdef123456",
                    subject="Ship unreleased work",
                    project="lifecycle.release",
                )
            ]
        ),
    )

    output = release.release_range_for_unreleased("head-commit")

    assert calls == [
        (
            "tag",
            "--merged",
            "head-commit",
            "--list",
            "v*",
            "--sort=-v:refname",
        ),
        ("records", "v0.20.0", "head-commit"),
        ("rev-parse", "--short", "head-commit"),
    ]
    assert output == (
        "Release range for unreleased\n"
        "Range: refs/tags/v0.20.0..head123\n"
        "Release tag: unreleased\n"
        "Landed commits: 1\n"
        "\n"
        "abcdef1  lifecycle.release  Ship unreleased work\n"
    )


def test_unreleased_range_without_tags_renders_empty_head_span(monkeypatch):
    seen = []

    def fake_git(*args):
        if args == (
            "tag",
            "--merged",
            "head-commit",
            "--list",
            "v*",
            "--sort=-v:refname",
        ):
            return ""
        if args == ("rev-parse", "--short", "head-commit"):
            return "head123"
        raise AssertionError(args)

    monkeypatch.setattr(release, "git", fake_git)
    monkeypatch.setattr(
        release,
        "commit_records",
        lambda previous, commit: seen.append((previous, commit)) or [],
    )

    output = release.release_range_for_unreleased("head-commit")

    assert seen == [("", "head-commit")]
    assert output == (
        "Release range for unreleased\n"
        "Range: latest first-parent commits ending at head123\n"
        "Release tag: unreleased\n"
        "Landed commits: 0\n"
        "\n"
        "No non-release commits found.\n"
    )


def test_bare_release_notes_mode_uses_unreleased_renderer_for_tagged_version(
    tmp_path, monkeypatch, capsys
):
    parser = build_release_parser()
    args = parser.parse_args(["notes"])
    seen = []

    monkeypatch.setattr(release, "repo_root", lambda: tmp_path)
    monkeypatch.setattr(release, "current_version", lambda: "0.20.0")

    def fake_git(*args):
        seen.append(("git", args))
        if args == ("rev-parse", "HEAD"):
            return "head-commit"
        if args == ("tag", "--list", "v0.20.0"):
            return "v0.20.0"
        raise AssertionError(args)

    monkeypatch.setattr(release, "git", fake_git)
    monkeypatch.setattr(
        release,
        "release_notes_for_unreleased",
        lambda commit: seen.append(("unreleased", commit)) or "unreleased notes\n",
    )

    result = release.handle_release(args)

    assert result == 0
    assert seen == [
        ("git", ("rev-parse", "HEAD")),
        ("git", ("tag", "--list", "v0.20.0")),
        ("unreleased", "head-commit"),
    ]
    captured = capsys.readouterr()
    assert captured.out == "unreleased notes\n"
    assert "draft notes for unreleased" in captured.err


def test_bare_release_notes_mode_uses_versioned_renderer_for_prepared_version(
    tmp_path, monkeypatch, capsys
):
    parser = build_release_parser()
    args = parser.parse_args(["notes"])
    seen = []

    monkeypatch.setattr(release, "repo_root", lambda: tmp_path)
    monkeypatch.setattr(release, "current_version", lambda: "0.21.0")

    def fake_git(*args):
        seen.append(("git", args))
        if args == ("rev-parse", "HEAD"):
            return "prepared-commit"
        if args == ("tag", "--list", "v0.21.0"):
            return ""
        raise AssertionError(args)

    monkeypatch.setattr(release, "git", fake_git)
    monkeypatch.setattr(
        release,
        "release_notes_for_version",
        lambda version, commit: (
            seen.append(("versioned", version, commit)) or "prepared notes\n"
        ),
    )

    result = release.handle_release(args)

    assert result == 0
    assert seen == [
        ("git", ("rev-parse", "HEAD")),
        ("git", ("tag", "--list", "v0.21.0")),
        ("versioned", "0.21.0", "prepared-commit"),
    ]
    captured = capsys.readouterr()
    assert captured.out == "prepared notes\n"
    assert "draft notes for 0.21.0" in captured.err


def test_unreleased_notes_preview_markers_differ_from_versioned_notes(monkeypatch):
    def fake_git(*args):
        if args == (
            "tag",
            "--merged",
            "head-commit",
            "--list",
            "v*",
            "--sort=-v:refname",
        ):
            return "v0.20.0\nv0.19.0"
        if args == ("tag", "--list", "v*", "--sort=-v:refname"):
            return "v0.21.0\nv0.20.0\nv0.19.0"
        if args == ("rev-parse", "--short", "head-commit"):
            return "head123"
        raise AssertionError(args)

    monkeypatch.setattr(release, "git", fake_git)
    monkeypatch.setattr(release, "current_version", lambda: "0.21.0")
    monkeypatch.setattr(
        release,
        "commit_records",
        lambda previous, commit: [
            ReleaseRecord(
                commit="abcdef123456",
                subject="Ship unreleased work",
                project="lifecycle.release",
            )
        ],
    )

    unreleased = release.release_notes_for_unreleased("head-commit")
    versioned = release.release_notes_for_version(
        release.current_version(), "head-commit"
    )

    assert "- PyPI release: `spice-harness==unreleased`" in unreleased
    assert "- Release tag: `unreleased`" in unreleased
    assert "- Commit range: `v0.20.0..head123`" in unreleased
    assert "- PyPI release: `spice-harness==0.21.0`" in versioned
    assert unreleased != versioned


def test_commit_records_addresses_previous_tag_by_full_ref(monkeypatch):
    seen = []

    class FakeResult:
        stdout = ""

    def fake_run(command, **_kwargs):
        seen.append(command)
        return FakeResult()

    monkeypatch.setattr(release, "run", fake_run)

    release.commit_records("v0.2.1", "release-commit-sha")

    assert seen == [
        [
            "git",
            "log",
            "--first-parent",
            "--reverse",
            "--format=%H%x1f%s%x1f%(trailers:key=Task-Project,valueonly)"
            "%x1f%(trailers:key=Task-Key,valueonly)%x1f%b%x1e",
            "refs/tags/v0.2.1..release-commit-sha",
        ]
    ]


def test_commit_records_dedupes_todo_and_review_merges_by_task_key(monkeypatch):
    stdout = (
        "\x1e".join(
            [
                "1111111aaaa\x1ftodo(serve.ui): Fix menu MODEL-abc\x1fserve.ui\x1fabc",
                "2222222bbbb\x1freview(serve.ui): Fix menu MODEL-abc\x1fserve.ui\x1fabc",
                "3333333cccc\x1fFix an unrelated one-off\x1fserve.ui\x1f",
            ]
        )
        + "\x1e"
    )

    class FakeResult:
        pass

    result = FakeResult()
    result.stdout = stdout
    monkeypatch.setattr(release, "run", lambda *_args, **_kwargs: result)

    records = release.commit_records("v0.2.1", "release-commit-sha")

    assert records == [
        ReleaseRecord(
            commit="2222222bbbb",
            subject="review(serve.ui): Fix menu MODEL-abc",
            project="serve.ui",
            task_key="abc",
        ),
        ReleaseRecord(
            commit="3333333cccc",
            subject="Fix an unrelated one-off",
            project="serve.ui",
        ),
    ]


def test_commit_records_suppresses_implement_and_revert_pair_in_same_range(
    monkeypatch,
):
    rows = [
        ("1111111aaaa", "Implement emoji control markers", "mail.acks", "", ""),
        (
            "2222222bbbb",
            'Revert "Implement emoji control markers"',
            "mail.acks",
            "",
            "This reverts commit 1111111aaaa.",
        ),
        ("3333333cccc", "Fix an unrelated one-off", "serve.ui", "", ""),
    ]
    stdout = "\x1e".join("\x1f".join(row) for row in rows) + "\x1e"

    class FakeResult:
        pass

    result = FakeResult()
    result.stdout = stdout
    monkeypatch.setattr(release, "run", lambda *_args, **_kwargs: result)
    monkeypatch.setattr(
        release, "_is_ancestor", lambda candidate, commit: candidate == commit
    )

    records = release.commit_records("v0.2.1", "release-commit-sha")

    assert records == [
        ReleaseRecord(
            commit="3333333cccc",
            subject="Fix an unrelated one-off",
            project="serve.ui",
        ),
    ]


def test_commit_records_keeps_revert_whose_target_shipped_earlier(monkeypatch):
    rows = [
        (
            "1111111aaaa",
            'Revert "Old feature from a prior release"',
            "mail.acks",
            "",
            "This reverts commit 9999999999999999999999999999999999999999.",
        ),
    ]
    stdout = "\x1e".join("\x1f".join(row) for row in rows) + "\x1e"

    class FakeResult:
        pass

    result = FakeResult()
    result.stdout = stdout
    monkeypatch.setattr(release, "run", lambda *_args, **_kwargs: result)
    monkeypatch.setattr(
        release, "_is_ancestor", lambda candidate, commit: candidate == commit
    )

    records = release.commit_records("v0.2.1", "release-commit-sha")

    assert records == [
        ReleaseRecord(
            commit="1111111aaaa",
            subject='Revert "Old feature from a prior release"',
            project="mail.acks",
        ),
    ]


def test_render_release_range_lists_commits_with_project_and_subject():
    output = render_release_range(
        version="0.3.0",
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
                project="agent.019ec753620c7cf2b18c06707ac93cbb.task",
            ),
        ],
    )

    assert output == (
        "Release range for 0.3.0\n"
        "Range: refs/tags/v0.2.1..abcdef1\n"
        "Release tag: v0.3.0\n"
        "Landed commits: 2\n"
        "\n"
        "1111111  serve    Fix speech excerpts for final ACK messages\n"
        "2222222  general  Expose release tooling as spice command\n"
    )

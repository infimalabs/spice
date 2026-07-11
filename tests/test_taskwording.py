"""Task creation suspect wording detector."""

import subprocess
from pathlib import Path

import pytest

from spice.tasks import create


@pytest.fixture
def repo(tmp_path):
    return _init_repo(tmp_path / "repo")


def test_taste_word_match_records_source_word_and_reason(repo):
    matches = create.detect_suspect_wording(
        title="Adopting the plan",
        repo_root=repo,
    )

    assert matches == (
        create.TaskWordingMatch(
            source="title",
            matched="adopting",
            trigger_family="taste",
            reason="consider 'capture'",
        ),
    )


def test_maxim_trigger_match_records_source_family_and_reason(repo):
    _write_pyproject(
        repo,
        """
        [tool.spice.maxims.routes]
        words = ["quiet route"]
        message = "Do not take the quiet route."
        """,
    )

    matches = create.detect_suspect_wording(
        title="Clean title",
        description="This quiet-route hides the real problem.",
        repo_root=repo,
    )

    assert matches == (
        create.TaskWordingMatch(
            source="description",
            matched="quiet route",
            trigger_family="routes",
            reason="Do not take the quiet route.",
        ),
    )


def test_no_match_returns_empty_tuple(repo):
    assert (
        create.detect_suspect_wording(
            title="Clear task",
            description="Specific implementation detail.",
            acceptance=["Focused coverage exists."],
            repo_root=repo,
        )
        == ()
    )


def test_positive_occurrence_after_negated_occurrence_still_matches(repo):
    _write_pyproject(
        repo,
        """
        [tool.spice.maxims.routes]
        words = ["quiet route"]
        message = "Do not take the quiet route."
        """,
    )

    matches = create.detect_suspect_wording(
        title="Clear task",
        description=(
            "Do not use the master label or take the quiet route. "
            "The master label and quiet route remain elsewhere."
        ),
        repo_root=repo,
    )

    assert {(match.matched, match.trigger_family) for match in matches} == {
        ("master", "taste"),
        ("quiet route", "routes"),
    }


def test_hypothetical_trigger_word_remains_suspect(repo):
    _write_pyproject(
        repo,
        """
        [tool.spice.maxims.routes]
        words = ["quiet route"]
        message = "Do not take the quiet route."
        """,
    )

    matches = create.detect_suspect_wording(
        title="Clear task",
        description="Could this take the quiet route?",
        repo_root=repo,
    )

    assert [(match.matched, match.trigger_family) for match in matches] == [
        ("quiet route", "routes")
    ]


def test_title_only_match_does_not_scan_clean_body_or_acceptance(repo):
    matches = create.detect_suspect_wording(
        title="Recover orphaned notes",
        description="Specific implementation detail.",
        acceptance=["Focused coverage exists."],
        repo_root=repo,
    )

    assert {match.source for match in matches} == {"title"}
    assert matches[0].matched == "orphaned"
    assert matches[0].reason == "consider 'loose'"


def test_body_and_acceptance_only_matches_keep_source_fields(repo):
    matches = create.detect_suspect_wording(
        title="Clear task",
        description="The master plan is too vague.",
        acceptance=["Avoid hallucinating about task state."],
        repo_root=repo,
    )

    assert {
        (match.source, match.matched, match.trigger_family) for match in matches
    } == {
        ("description", "master", "taste"),
        ("acceptance", "hallucinating", "taste"),
    }


def test_project_and_routing_metadata_are_not_scanned(repo):
    _write_pyproject(
        repo,
        """
        [tool.spice.maxims.routes]
        words = ["quiet route"]
        message = "Do not take the quiet route."
        """,
    )

    matches = create.detect_suspect_wording(
        title="Clear task",
        description="Specific implementation detail.",
        acceptance=["Focused coverage exists."],
        project="task.master",
        flow=["quiet route"],
        repo_root=repo,
    )

    assert matches == ()


def test_config_overrides_taste_words_and_maxim_triggers(repo):
    _write_pyproject(
        repo,
        """
        [tool.spice.policy.taste.words]
        bespoke = "specific"

        [tool.spice.maxims.routes]
        words = ["quiet route"]
        message = "Do not take the quiet route."
        """,
    )

    matches = create.detect_suspect_wording(
        title="Bespoke task",
        acceptance=["This quiet route hides the real problem."],
        repo_root=repo,
    )

    assert matches == (
        create.TaskWordingMatch(
            source="title",
            matched="bespoke",
            trigger_family="taste",
            reason="consider 'specific'",
        ),
        create.TaskWordingMatch(
            source="acceptance",
            matched="quiet route",
            trigger_family="routes",
            reason="Do not take the quiet route.",
        ),
    )


def _write_pyproject(path: Path, text: str) -> None:
    path.joinpath("pyproject.toml").write_text(text, encoding="utf-8")


def _init_repo(path: Path) -> Path:
    path.mkdir()
    subprocess.run(
        ["git", "init", "-b", "main"],
        cwd=path,
        check=True,
        capture_output=True,
        text=True,
    )
    return path

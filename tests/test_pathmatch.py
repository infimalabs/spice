from pathlib import Path

import pytest

from spice.pathmatch import (
    PathSpecificity,
    has_glob_magic,
    matches_repo_path,
    matches_repo_path_or_ancestor,
    normalize_repo_path,
    path_specificity,
)


@pytest.mark.parametrize(
    ("path", "pattern"),
    (
        ("src/app.py", "src/app.py"),
        ("src/pkg/app.py", "src"),
        ("src/app.py", "src/*.py"),
        ("src/pkg/app.py", "src/*/app.py"),
        ("app.py", "**/*.py"),
        ("pkg/app.py", "**/*.py"),
        (r".\src\app.py", r".\src\*.py"),
    ),
)
def test_repo_path_matcher_positive_truth_table(path, pattern):
    assert matches_repo_path(path, pattern)


def test_repo_path_or_ancestor_includes_glob_selected_tree():
    assert matches_repo_path_or_ancestor(
        "Assets/Game/Tests/TestThing.cs", "Assets/**/Tests"
    )


def test_repo_path_normalization_and_glob_detection_share_one_contract():
    assert normalize_repo_path(Path(r".\src\app.py")) == "src/app.py"
    assert normalize_repo_path(r".\src\*.py") == "src/*.py"
    assert has_glob_magic(r".\src\*.py")


def test_path_specificity_preserves_named_lexicographic_precedence():
    literal = path_specificity("./src/app.py", priority=1)
    glob = path_specificity(r"src\*.py", priority=1)

    assert literal == PathSpecificity(1, True, 10, 2, 10)
    assert glob == PathSpecificity(1, False, 7, 2, 8)
    assert literal > glob

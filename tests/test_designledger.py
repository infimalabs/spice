"""Design records declare canonical maturity families with dated statuses."""

from __future__ import annotations

import re
from pathlib import Path

import pytest

PROJECT_ROOT = Path(__file__).resolve().parents[1]
DESIGN_ROOT = PROJECT_ROOT / "docs" / "design"
RECORD_DIRECTORIES = ("accepted", "experimental")

# The maturity ladder published by docs/design/README.md under "Statuses".
CANONICAL_FAMILIES = frozenset(
    {
        "draft",
        "research",
        "recommendation",
        "decision",
        "implemented contract",
        "prototype result",
        "superseded",
    }
)

# Named per-record exceptions: design-relative path -> the exact non-canonical
# family that one record is allowed to declare. Every entry must carry a
# reviewed reason at the line that adds it; an empty map means every record on
# the ledger is canonical.
ALLOWED_EXCEPTIONS: dict[str, str] = {}

STATUS_LINE = re.compile(
    r"^Status: (?P<family>[a-z][a-z ]*[a-z]), (?P<date>\d{4}-\d{2}-\d{2})\.(?: |$)"
)


def _record_paths() -> list[Path]:
    return sorted(
        path
        for directory in RECORD_DIRECTORIES
        for path in (DESIGN_ROOT / directory).glob("*.md")
    )


def _relative(path: Path) -> str:
    return path.relative_to(DESIGN_ROOT).as_posix()


def test_record_scan_covers_both_maturity_directories():
    relatives = {_relative(path) for path in _record_paths()}
    assert "accepted/task-documents.md" in relatives
    assert "accepted/unbounded-wait-audit.md" in relatives
    assert "experimental/top-level-non-code-phases.md" in relatives


def test_readme_publishes_every_canonical_family():
    readme = (DESIGN_ROOT / "README.md").read_text(encoding="utf-8")
    assert {family: f"`{family}`" in readme for family in CANONICAL_FAMILIES} == {
        family: True for family in CANONICAL_FAMILIES
    }


@pytest.mark.parametrize("path", _record_paths(), ids=_relative)
def test_record_declares_dated_canonical_status(path: Path):
    lines = path.read_text(encoding="utf-8").splitlines()
    assert lines[0].startswith("# "), "a record opens with its title heading"
    assert lines[1] == "", "one blank line separates the title from the status"
    match = STATUS_LINE.match(lines[2])
    assert match, (
        f"{_relative(path)} line 3 must read 'Status: <family>, YYYY-MM-DD.'; "
        f"got: {lines[2]!r}"
    )
    family = match.group("family")
    sanctioned = ALLOWED_EXCEPTIONS.get(_relative(path))
    if sanctioned is None:
        assert family in CANONICAL_FAMILIES, (
            f"{_relative(path)} declares non-canonical family {family!r}; use one "
            f"of {sorted(CANONICAL_FAMILIES)} or add a named ALLOWED_EXCEPTIONS "
            "entry with its reviewed reason"
        )
    else:
        assert family == sanctioned

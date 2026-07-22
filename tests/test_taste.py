"""Taste study: flag low-value/poor-taste words with suggestions."""

from pathlib import Path

import pytest

from spice.studies import taste

INCLUSIVE_TASTE_SUGGESTIONS = (
    ("whitelist", "allowlist"),
    ("whitelists", "allowlists"),
    ("whitelisted", "allowlisted"),
    ("whitelisting", "allowlisting"),
    ("blacklist", "blocklist"),
    ("blacklists", "blocklists"),
    ("blacklisted", "blocklisted"),
    ("blacklisting", "blocklisting"),
)


def test_scan_matches_whole_word_case_insensitively(tmp_path):
    doc = tmp_path / "notes.md"
    doc.write_text(
        "This Just works; the design has a smell.\nadjust nothing here.\n",
        encoding="utf-8",
    )

    findings = taste.scan_taste(
        [Path("notes.md")], root=tmp_path, words={"just": "", "smell": ""}
    )

    hits = {(finding.word, finding.line) for finding in findings}
    assert ("just", 1) in hits
    assert ("smell", 1) in hits
    # 'adjust' on line 2 must not match the whole word 'just'.
    assert all(finding.line != 2 for finding in findings)


def test_suggestions_render_alternative_or_rephrase(tmp_path):
    doc = tmp_path / "notes.md"
    doc.write_text("do not hallucinate\n", encoding="utf-8")

    findings = taste.scan_taste([Path("notes.md")], root=tmp_path)

    assert findings[0].suggestion == "confabulate"
    assert "consider 'confabulate'" in taste.render_taste_board(findings)

    empty = taste.TasteFinding(path="x.md", line=1, word="just", suggestion="")
    assert "consider rephrasing" in taste.render_taste_board([empty])


def test_only_text_files_scanned_and_clean_passes(tmp_path):
    (tmp_path / "code.py").write_text("just = 1  # smell\n", encoding="utf-8")
    (tmp_path / "clean.md").write_text("Well phrased prose here.\n", encoding="utf-8")

    assert taste.scan_taste([Path("code.py")], root=tmp_path) == []
    assert taste.scan_taste([Path("clean.md")], root=tmp_path) == []
    assert taste.render_taste_board([]) == "taste: ok"


def test_stem_star_key_matches_every_inflection(tmp_path):
    doc = tmp_path / "notes.md"
    doc.write_text(
        "we migrated it; still migrating; an orphaned note; the migration.\n",
        encoding="utf-8",
    )

    findings = taste.scan_taste(
        [Path("notes.md")],
        root=tmp_path,
        words={"migrat*": "move", "orphan*": "loose"},
    )

    hits = {(finding.word, finding.suggestion) for finding in findings}
    assert ("migrated", "move") in hits
    assert ("migrating", "move") in hits
    assert ("migration", "move") in hits
    assert ("orphaned", "loose") in hits


def test_whole_word_key_never_stem_matches(tmp_path):
    doc = tmp_path / "notes.md"
    doc.write_text("a masterpiece of mastery, and the master plan\n", encoding="utf-8")

    findings = taste.scan_taste(
        [Path("notes.md")], root=tmp_path, words={"master": "main"}
    )

    # Only the standalone word 'master' matches; 'masterpiece'/'mastery' do not.
    assert [(finding.word, finding.line) for finding in findings] == [("master", 1)]


def test_default_hallucinate_stem_catches_variations(tmp_path):
    doc = tmp_path / "notes.md"
    doc.write_text("no hallucination here; stop hallucinating now\n", encoding="utf-8")

    findings = taste.scan_taste([Path("notes.md")], root=tmp_path)

    assert {finding.word for finding in findings} == {
        "hallucination",
        "hallucinating",
    }
    assert all(finding.suggestion == "confabulate" for finding in findings)


@pytest.mark.parametrize(("word", "suggestion"), INCLUSIVE_TASTE_SUGGESTIONS)
def test_default_inclusive_terms_match_case_insensitively_with_exact_suggestion(
    tmp_path, word, suggestion
):
    doc = tmp_path / "notes.md"
    doc.write_text(f"The term {word.upper()} appears here.\n", encoding="utf-8")

    findings = taste.scan_taste([Path("notes.md")], root=tmp_path)

    assert [(finding.word, finding.suggestion) for finding in findings] == [
        (word, suggestion)
    ]


def test_custom_word_map_overrides_default(tmp_path):
    doc = tmp_path / "notes.md"
    doc.write_text("this is verbose\n", encoding="utf-8")

    findings = taste.scan_taste(
        [Path("notes.md")], root=tmp_path, words={"verbose": "terse"}
    )

    assert [(finding.word, finding.suggestion) for finding in findings] == [
        ("verbose", "terse")
    ]

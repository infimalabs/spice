"""Taste study: flag low-value/poor-taste words with suggestions."""

from pathlib import Path

from spice.studies import taste


def test_scan_matches_whole_word_case_insensitively(tmp_path):
    doc = tmp_path / "notes.md"
    doc.write_text(
        "This Just works; the design has a smell.\nadjust nothing here.\n",
        encoding="utf-8",
    )

    findings = taste.scan_taste([Path("notes.md")], root=tmp_path)

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
    assert "use 'confabulate'" in taste.render_taste_board(findings)

    empty = taste.TasteFinding(path="x.md", line=1, word="just", suggestion="")
    assert "adds no value" in taste.render_taste_board([empty])


def test_only_text_files_scanned_and_clean_passes(tmp_path):
    (tmp_path / "code.py").write_text("just = 1  # smell\n", encoding="utf-8")
    (tmp_path / "clean.md").write_text("Well phrased prose here.\n", encoding="utf-8")

    assert taste.scan_taste([Path("code.py")], root=tmp_path) == []
    assert taste.scan_taste([Path("clean.md")], root=tmp_path) == []
    assert taste.render_taste_board([]) == "taste: ok"


def test_custom_word_map_overrides_default(tmp_path):
    doc = tmp_path / "notes.md"
    doc.write_text("this is verbose\n", encoding="utf-8")

    findings = taste.scan_taste(
        [Path("notes.md")], root=tmp_path, words={"verbose": "terse"}
    )

    assert [(finding.word, finding.suggestion) for finding in findings] == [
        ("verbose", "terse")
    ]

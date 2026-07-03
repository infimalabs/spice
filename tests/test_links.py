"""Tracked markdown link resolver study."""

from __future__ import annotations

import subprocess
from pathlib import Path

from spice.studies.links import (
    MarkdownLinkCaseFinding,
    markdown_link_case_findings,
    render_markdown_link_case_board,
)


def test_markdown_link_case_findings_compare_against_tracked_index_case(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    docs = repo / "docs" / "design"
    images = docs / "images"
    images.mkdir(parents=True)
    (docs / "architecture.md").write_text("Architecture\n", encoding="utf-8")
    (images / "diagram.png").write_bytes(b"diagram")
    (docs / "index.md").write_text(
        "\n".join(
            [
                "[Architecture](ARCHITECTURE.md#overview)",
                "![Diagram](images/DIAGRAM.png)",
                "[Exact](architecture.md)",
                "[External](https://example.test/ARCHITECTURE.md)",
                "[Mail](mailto:docs@example.test)",
                "[Fragment](#architecture)",
                "[Absolute](/docs/design/ARCHITECTURE.md)",
            ]
        )
        + "\n",
        encoding="utf-8",
    )
    _run(repo, "git", "add", "docs")

    findings = markdown_link_case_findings(repo)

    assert findings == [
        MarkdownLinkCaseFinding(
            source_path=Path("docs/design/index.md"),
            line=1,
            raw_target="ARCHITECTURE.md#overview",
            resolved_path=Path("docs/design/ARCHITECTURE.md"),
            expected_path=Path("docs/design/architecture.md"),
        ),
        MarkdownLinkCaseFinding(
            source_path=Path("docs/design/index.md"),
            line=2,
            raw_target="images/DIAGRAM.png",
            resolved_path=Path("docs/design/images/DIAGRAM.png"),
            expected_path=Path("docs/design/images/diagram.png"),
        ),
    ]
    assert (
        markdown_link_case_findings(repo, paths=[Path("docs/design/index.md")])
        == findings
    )
    assert render_markdown_link_case_board(findings) == "\n".join(
        [
            "markdown-links: 2 case-mismatched tracked markdown link target(s)",
            "  FAIL  docs/design/index.md:1 "
            "ARCHITECTURE.md#overview -> docs/design/architecture.md",
            "  FAIL  docs/design/index.md:2 "
            "images/DIAGRAM.png -> docs/design/images/diagram.png",
        ]
    )
    assert render_markdown_link_case_board([]) == "markdown-links: ok"


def test_markdown_link_case_findings_apply_staged_renames_to_tracked_map(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    docs = repo / "docs"
    docs.mkdir()
    (docs / "Guide.md").write_text("Guide\n", encoding="utf-8")
    (docs / "Obsolete.md").write_text("Obsolete\n", encoding="utf-8")
    (docs / "index.md").write_text(
        "[Guide](GUIDE.md)\n[Obsolete](OBSOLETE.md)\n",
        encoding="utf-8",
    )
    _run(repo, "git", "add", "docs")
    _run(repo, "git", "commit", "-m", "base")

    _run(repo, "git", "mv", "docs/Guide.md", "docs/guide.md")
    _run(repo, "git", "rm", "docs/Obsolete.md")

    findings = markdown_link_case_findings(
        repo,
        tracked_paths=[
            Path("docs/index.md"),
            Path("docs/Guide.md"),
            Path("docs/Obsolete.md"),
        ],
    )

    assert findings == [
        MarkdownLinkCaseFinding(
            source_path=Path("docs/index.md"),
            line=1,
            raw_target="GUIDE.md",
            resolved_path=Path("docs/GUIDE.md"),
            expected_path=Path("docs/guide.md"),
        )
    ]


def _init_repo(path: Path) -> Path:
    path.mkdir()
    _run(path, "git", "init", "-q", "-b", "main")
    _run(path, "git", "config", "user.email", "spice@example.test")
    _run(path, "git", "config", "user.name", "Spice Tests")
    return path


def _run(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True)

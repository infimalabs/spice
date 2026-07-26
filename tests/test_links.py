"""Tracked markdown link resolver study."""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import pytest

from spice.errors import SpiceError
from spice.hooks import precommit
from spice.studies.links import (
    MarkdownLinkCaseFinding,
    markdown_link_case_findings,
    render_markdown_link_case_board,
)
from tests.test_reposcaffolding import run as _run

PROJECT_ROOT = Path(__file__).resolve().parents[1]


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


def test_markdown_links_gate_rejects_then_accepts_case_exact_targets(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    docs = repo / "docs" / "design"
    images = docs / "images"
    images.mkdir(parents=True)
    (docs / "ARCHITECTURE.md").write_text("# Architecture\n", encoding="utf-8")
    (images / "Diagram.PNG").write_bytes(b"image")
    readme = repo / "README.md"
    readme.write_text(_mixed_case_docs_readme("architecture.md", "diagram.png"))
    _run(repo, "git", "add", ".")

    dirty = _run_spice(repo, "study", "markdown-links", check=False)
    findings = markdown_link_case_findings(repo)

    assert dirty.returncode == 1
    assert dirty.stdout == "\n".join(
        [
            "markdown-links: 2 case-mismatched tracked markdown link target(s)",
            "  FAIL  README.md:1 docs/design/architecture.md#overview -> "
            "docs/design/ARCHITECTURE.md",
            "  FAIL  README.md:2 docs/design/images/diagram.png -> "
            "docs/design/images/Diagram.PNG",
            "",
        ]
    )
    assert findings == [
        MarkdownLinkCaseFinding(
            source_path=Path("README.md"),
            line=1,
            raw_target="docs/design/architecture.md#overview",
            resolved_path=Path("docs/design/architecture.md"),
            expected_path=Path("docs/design/ARCHITECTURE.md"),
        ),
        MarkdownLinkCaseFinding(
            source_path=Path("README.md"),
            line=2,
            raw_target="docs/design/images/diagram.png",
            resolved_path=Path("docs/design/images/diagram.png"),
            expected_path=Path("docs/design/images/Diagram.PNG"),
        ),
    ]
    with pytest.raises(SpiceError) as exc_info:
        precommit._run_markdown_links_guard(repo)
    assert str(exc_info.value) == dirty.stdout.strip()

    readme.write_text(_mixed_case_docs_readme("ARCHITECTURE.md", "Diagram.PNG"))
    _run(repo, "git", "add", "README.md")

    clean = _run_spice(repo, "study", "markdown-links", check=False)

    assert clean.returncode == 0
    assert clean.stdout == "markdown-links: ok\n"
    assert markdown_link_case_findings(repo) == []
    precommit._run_markdown_links_guard(repo)


def _init_repo(path: Path) -> Path:
    path.mkdir()
    _run(path, "git", "init", "-q", "-b", "main")
    _run(path, "git", "config", "user.email", "spice@example.test")
    _run(path, "git", "config", "user.name", "Spice Tests")
    return path


def _run_spice(cwd: Path, *args: str, check: bool) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()  # env-policy: allow
    env["PYTHONPATH"] = os.pathsep.join(
        entry for entry in (str(PROJECT_ROOT), env.get("PYTHONPATH", "")) if entry
    )
    return subprocess.run(
        [sys.executable, "-m", "spice", *args],
        cwd=cwd,
        env=env,
        check=check,
        capture_output=True,
        text=True,
    )


def _mixed_case_docs_readme(architecture: str, diagram: str) -> str:
    return "\n".join(
        [
            f"[Architecture](docs/design/{architecture}#overview)",
            f"![Diagram](docs/design/images/{diagram})",
            "[External](https://example.test/docs/design/architecture.md)",
            "[Fragment](#architecture)",
            "",
        ]
    )

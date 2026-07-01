"""Flag low-value or poor-taste words in prose and suggest better phrasing.

The study nudges writing toward better taste: each configured word maps to a
suggestion. An empty suggestion means "remove or rephrase; it adds no value";
a non-empty one is the preferred alternative. Matching is whole-word and
case-insensitive over tracked text files.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path

from spice.studies.walk import is_excluded_path

# Seed map; extend as more tics surface. Keys are matched case-insensitively.
DEFAULT_TASTE_WORDS: dict[str, str] = {
    "smell": "",
    "just": "",
    "hallucinate": "confabulate",
}

TEXT_SUFFIXES = frozenset({".md", ".txt", ".rst"})


@dataclass(frozen=True)
class TasteFinding:
    path: str
    line: int
    word: str
    suggestion: str


def _word_pattern(words: dict[str, str]) -> re.Pattern[str]:
    alternation = "|".join(re.escape(word) for word in words)
    return re.compile(rf"\b({alternation})\b", re.IGNORECASE)


def scan_taste(
    paths: list[Path],
    *,
    root: Path,
    words: dict[str, str] | None = None,
) -> list[TasteFinding]:
    resolved = {
        key.lower(): value for key, value in (words or DEFAULT_TASTE_WORDS).items()
    }
    if not resolved:
        return []
    pattern = _word_pattern(resolved)
    findings: list[TasteFinding] = []
    for rel_path in paths:
        if rel_path.suffix.lower() not in TEXT_SUFFIXES:
            continue
        if is_excluded_path(rel_path, repo_root=root):
            continue
        abs_path = root / rel_path
        if not abs_path.is_file():
            continue
        text = abs_path.read_text(encoding="utf-8", errors="replace")
        for line_number, line in enumerate(text.splitlines(), start=1):
            for match in pattern.finditer(line):
                word = match.group(1).lower()
                findings.append(
                    TasteFinding(
                        path=rel_path.as_posix(),
                        line=line_number,
                        word=word,
                        suggestion=resolved[word],
                    )
                )
    return findings


def _finding_hint(finding: TasteFinding) -> str:
    if finding.suggestion:
        return f"use '{finding.suggestion}'"
    return "remove or rephrase; adds no value"


def render_taste_board(findings: list[TasteFinding]) -> str:
    if not findings:
        return "taste: ok"
    lines = [
        f"taste: {len(findings)} low-value or poor-taste word(s); "
        "rephrase for better taste"
    ]
    for finding in findings:
        lines.append(
            f"  FAIL  {finding.path}:{finding.line}  "
            f"'{finding.word}' -> {_finding_hint(finding)}"
        )
    return "\n".join(lines)

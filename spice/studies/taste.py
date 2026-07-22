"""Flag low-value or poor-taste words in prose and suggest better phrasing.

The study nudges writing toward better taste: each configured word maps to a
suggestion. An empty suggestion means "remove or rephrase; it adds no value";
a non-empty one is the preferred alternative. Matching is case-insensitive
over tracked text files; a key is whole-word, or a stem covering every
inflection when it ends with ``*`` (``migrat*`` catches migrate/migrated/migration).
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Sequence

from spice import policy
from spice.studies.walk import is_excluded_path

TEXT_SUFFIXES = frozenset({".md", ".txt", ".rst"})


@dataclass(frozen=True)
class TasteFinding:
    path: str
    line: int
    word: str
    suggestion: str


@dataclass(frozen=True)
class TasteTextFinding:
    source: str
    word: str
    suggestion: str


def _compile_words(
    words: dict[str, str],
) -> tuple[re.Pattern[str], list[tuple[str, bool, str]]]:
    """Compile the scan pattern and the ordered token->suggestion rules.

    A key ending in ``*`` is a stem: its fragment matches the stem plus any
    trailing word characters, so every inflection is caught. Any other key is
    whole-word. The whole scan is one compiled alternation walked once per line;
    a matched token is attributed to a suggestion by the first rule it matches,
    in insertion order (attribution runs only per finding, which is rare).
    """
    fragments: list[str] = []
    rules: list[tuple[str, bool, str]] = []
    for key, suggestion in words.items():
        low = key.lower()
        if low.endswith("*"):
            stem = low[:-1]
            fragments.append(re.escape(stem) + r"\w*")
            rules.append((stem, True, suggestion))
        else:
            fragments.append(re.escape(low))
            rules.append((low, False, suggestion))
    pattern = re.compile(rf"\b(?:{'|'.join(fragments)})\b", re.IGNORECASE)
    return pattern, rules


def _suggestion_for(token: str, rules: list[tuple[str, bool, str]]) -> str:
    """The suggestion of the first rule the token satisfies; "" if none does.

    The token always originates from the compiled pattern, so a rule matches;
    an empty suggestion is a legitimate "rephrase" hint, not a miss.
    """
    for needle, is_stem, suggestion in rules:
        if token.startswith(needle) if is_stem else token == needle:
            return suggestion
    return ""


def scan_taste(
    paths: list[Path],
    *,
    root: Path,
    words: dict[str, str] | None = None,
) -> list[TasteFinding]:
    source = policy.TASTE_WORD_SUGGESTIONS if words is None else words
    if not source:
        return []
    pattern, rules = _compile_words(source)
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
                word = match.group(0).lower()
                findings.append(
                    TasteFinding(
                        path=rel_path.as_posix(),
                        line=line_number,
                        word=word,
                        suggestion=_suggestion_for(word, rules),
                    )
                )
    return findings


def scan_taste_texts(
    items: Sequence[tuple[str, str]],
    *,
    words: dict[str, str] | None = None,
    match_filter: Callable[[str, int], bool] | None = None,
) -> list[TasteTextFinding]:
    source = policy.TASTE_WORD_SUGGESTIONS if words is None else words
    if not source:
        return []
    pattern, rules = _compile_words(source)
    findings: list[TasteTextFinding] = []
    for source_name, text in items:
        for match in pattern.finditer(text):
            if match_filter is not None and not match_filter(text, match.start()):
                continue
            word = match.group(0).lower()
            findings.append(
                TasteTextFinding(
                    source=source_name,
                    word=word,
                    suggestion=_suggestion_for(word, rules),
                )
            )
    return findings


def _finding_hint(finding: TasteFinding) -> str:
    if finding.suggestion:
        return f"consider '{finding.suggestion}'"
    return "consider rephrasing; it adds no value"


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

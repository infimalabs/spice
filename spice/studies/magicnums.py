"""Magic-number regressions: staged literals diffed against a HEAD baseline.

The scan finds bare integer literals buried in comparisons, slices, and
default arguments — positions where behaviour silently pivots on an unnamed
threshold. Literals in assignments, call arguments, and arithmetic are the
constant *being defined or passed*, not a hidden pivot, so they pass. The
gate is a ratchet, not an amnesty: only literals absent from the same file at
the baseline ref fail, so existing debt does not block unrelated commits
while new debt cannot land.

Library seam: target-repo tools may import the public finding dataclass,
scan/detect helpers, and `render_magic_board`; underscored names remain
private.
"""

from __future__ import annotations

import ast
import re
from dataclasses import dataclass
from pathlib import Path

from spice.policy import (
    MAGIC_BASELINE_REF,
    MAGIC_EXAMINE_VALUE_THRESHOLD,
    MAGIC_SUFFIXES,
)
from spice.studies.walk import git_blob_text, is_excluded_path

EXAMINE_PARENT_KINDS = frozenset({"default_arg", "compare", "slice"})
SCANNED_SUFFIXES = MAGIC_SUFFIXES

_ALL_CAPS_RE = re.compile(r"^[A-Z][A-Z0-9_]*$")
_C_COMMENT_RE = re.compile(r"//.*$|/\*.*?\*/", re.DOTALL)
_C_NUMBER = r"-?\d[\d_]*(?:\.\d+)?"
# A number on either side of a comparison operator. `=>` (arrow), `>>`/`<<`
# (shifts), and `>=`-style compounds are carved out by the look-arounds.
_C_COMPARE_RE = re.compile(
    r"(?:(?<![=<>!])(?:<=|>=|===|!==|==|!=|<|>)\s*(" + _C_NUMBER + r")(?![\w.])"
    r"|(?<![\w.])(" + _C_NUMBER + r")\s*(?:<=|>=|===|!==|==|!=|<(?![<=])|>(?![>=])))"
)


@dataclass(frozen=True)
class MagicFinding:
    path: str
    line: int
    literal: str


def scan_paths_magic_numbers(
    paths: list[Path],
    *,
    root: Path,
    examine_threshold: int = MAGIC_EXAMINE_VALUE_THRESHOLD,
) -> list[MagicFinding]:
    findings: list[MagicFinding] = []
    for rel_path in paths:
        if rel_path.suffix not in SCANNED_SUFFIXES or is_excluded_path(
            rel_path, repo_root=root
        ):
            continue
        abs_path = root / rel_path
        if not abs_path.exists():
            continue
        text = abs_path.read_text(encoding="utf-8", errors="replace")
        findings.extend(
            scan_text_magic_numbers(rel_path, text, examine_threshold=examine_threshold)
        )
    return findings


def scan_text_magic_numbers(
    rel_path: Path,
    text: str,
    *,
    examine_threshold: int = MAGIC_EXAMINE_VALUE_THRESHOLD,
) -> list[MagicFinding]:
    if rel_path.suffix == ".py":
        return _scan_python(rel_path, text, examine_threshold=examine_threshold)
    return _scan_c_grammar(rel_path, text, examine_threshold=examine_threshold)


def _examine_value(value: float, *, threshold: int) -> bool:
    return abs(value) >= threshold


# ---- Python: ast parents classify the literal's position --------------------

_PY_PARENT_KIND_MAP: tuple[tuple[type, str], ...] = (
    (ast.arguments, "default_arg"),
    (ast.Compare, "compare"),
    (ast.Slice, "slice"),
)


def _scan_python(
    rel_path: Path,
    text: str,
    *,
    examine_threshold: int,
) -> list[MagicFinding]:
    try:
        tree = ast.parse(text)
    except SyntaxError:
        return []
    exempt_ids = _python_exempt_constant_ids(tree)
    parent_map: dict[int, ast.AST] = {}
    for node in ast.walk(tree):
        for child in ast.iter_child_nodes(node):
            parent_map[id(child)] = node
    findings: list[MagicFinding] = []
    for node in ast.walk(tree):
        if not isinstance(node, ast.Constant) or id(node) in exempt_ids:
            continue
        value = node.value
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            continue
        if not _examine_value(value, threshold=examine_threshold):
            continue
        if _python_parent_kind(node, parent_map) not in EXAMINE_PARENT_KINDS:
            continue
        findings.append(
            MagicFinding(
                path=rel_path.as_posix(), line=node.lineno, literal=repr(value)
            )
        )
    return findings


def _python_parent_kind(node: ast.AST, parent_map: dict[int, ast.AST]) -> str:
    parent = parent_map.get(id(node))
    if isinstance(parent, ast.UnaryOp):
        parent = parent_map.get(id(parent))
    if parent is None:
        return "other"
    for parent_type, kind in _PY_PARENT_KIND_MAP:
        if isinstance(parent, parent_type):
            return kind
    return "other"


def _python_exempt_constant_ids(tree: ast.Module) -> set[int]:
    """Constants inside module-level ALL_CAPS assignments — the named home."""
    exempt: set[int] = set()
    for stmt in tree.body:
        if isinstance(stmt, ast.Assign):
            targets: list[ast.expr] = list(stmt.targets)
            value: ast.expr = stmt.value
        elif isinstance(stmt, ast.AnnAssign) and stmt.value is not None:
            targets = [stmt.target]
            value = stmt.value
        else:
            continue
        if not all(
            isinstance(target, ast.Name) and _ALL_CAPS_RE.fullmatch(target.id)
            for target in targets
        ):
            continue
        for node in ast.walk(value):
            if isinstance(node, ast.Constant):
                exempt.add(id(node))
    return exempt


# ---- C-grammar family: comparison-adjacent literals by regex ----------------
# Every non-Python language in MAGIC_SUFFIXES shares `//`/`/* */` comments and
# C comparison syntax. Without a parser, comparisons are the one
# examine-position a regex can hold reliably; default args and slices pass
# here, comparisons do not.


def _scan_c_grammar(
    rel_path: Path,
    text: str,
    *,
    examine_threshold: int,
) -> list[MagicFinding]:
    findings: list[MagicFinding] = []
    in_template = False
    for line_number, raw_line in enumerate(text.splitlines(), start=1):
        line, in_template = _strip_template_literals(raw_line, in_template)
        line = _C_COMMENT_RE.sub("", line)
        for match in _C_COMPARE_RE.finditer(line):
            literal = (match.group(1) or match.group(2)).replace("_", "")
            try:
                value = float(literal) if "." in literal else int(literal)
            except ValueError:
                continue
            if not _examine_value(value, threshold=examine_threshold):
                continue
            findings.append(
                MagicFinding(
                    path=rel_path.as_posix(), line=line_number, literal=literal
                )
            )
    return findings


def _strip_template_literals(line: str, in_template: bool) -> tuple[str, bool]:
    """Blank out template-literal content; markup strings are not numbers."""
    kept: list[str] = []
    for char in line:
        if char == "`":
            in_template = not in_template
            continue
        if not in_template:
            kept.append(char)
    return "".join(kept), in_template


def detect_magic_regressions(
    paths: list[Path],
    *,
    root: Path,
    baseline_ref: str = MAGIC_BASELINE_REF,
    examine_threshold: int = MAGIC_EXAMINE_VALUE_THRESHOLD,
) -> list[MagicFinding]:
    """Findings in the working copies that are absent from the baseline blobs.

    Baseline membership is per (path, literal): moving an existing literal
    around a file is not a regression; introducing a new one is.
    """
    regressions: list[MagicFinding] = []
    for rel_path in paths:
        if rel_path.suffix not in SCANNED_SUFFIXES or is_excluded_path(
            rel_path, repo_root=root
        ):
            continue
        abs_path = root / rel_path
        if not abs_path.exists():
            continue
        current = scan_text_magic_numbers(
            rel_path,
            abs_path.read_text(encoding="utf-8", errors="replace"),
            examine_threshold=examine_threshold,
        )
        if not current:
            continue
        baseline_text = git_blob_text(root, baseline_ref, rel_path)
        baseline_literals = (
            {
                finding.literal
                for finding in scan_text_magic_numbers(
                    rel_path,
                    baseline_text,
                    examine_threshold=examine_threshold,
                )
            }
            if baseline_text is not None
            else set()
        )
        regressions.extend(
            finding for finding in current if finding.literal not in baseline_literals
        )
    return regressions


def render_magic_board(
    findings: list[MagicFinding], *, baseline_ref: str = MAGIC_BASELINE_REF
) -> str:
    if not findings:
        return f"magic-numbers: ok (baseline {baseline_ref})"
    lines = [
        f"magic-numbers: {len(findings)} regression(s) vs {baseline_ref}; "
        "name each value as a module-level constant"
    ]
    for finding in findings:
        lines.append(f"  FAIL  {finding.path}:{finding.line}: {finding.literal}")
    return "\n".join(lines)

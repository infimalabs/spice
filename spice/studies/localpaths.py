"""Committed local-machine path literals that must not enter source."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from spice.studies.walk import is_excluded_path

FORBIDDEN_MACOS_USER_PATH_MARKER = "/" + "Users" + "/"


@dataclass(frozen=True)
class LocalPathFinding:
    path: str
    line: int


def scan_local_path_literals(
    paths: list[Path], *, root: Path
) -> list[LocalPathFinding]:
    findings: list[LocalPathFinding] = []
    marker = FORBIDDEN_MACOS_USER_PATH_MARKER
    for rel_path in paths:
        if is_excluded_path(rel_path, repo_root=root):
            continue
        abs_path = root / rel_path
        if not abs_path.is_file():
            continue
        text = abs_path.read_text(encoding="utf-8", errors="replace")
        for line_number, line in enumerate(text.splitlines(), start=1):
            if marker in line:
                findings.append(
                    LocalPathFinding(path=rel_path.as_posix(), line=line_number)
                )
    return findings


def render_local_path_board(findings: list[LocalPathFinding]) -> str:
    if not findings:
        return "local-paths: ok"
    lines = [
        f"local-paths: {len(findings)} absolute macOS user path literal(s); "
        "use repo-relative paths or construct test fixtures indirectly"
    ]
    for finding in findings:
        lines.append(f"  FAIL  {finding.path}:{finding.line}")
    return "\n".join(lines)

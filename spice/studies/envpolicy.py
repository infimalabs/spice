"""Environment-variable literal policy: every read is declared or waived.

Harness-owned env names (`SPICE_*`, plus shipped driver thread variables) may
appear in source only in statements carrying, or immediately following a
standalone, `env-policy: allow` waiver comment. The point is an auditable
inventory: grep the waiver to see every place the environment shapes behavior.

The watchlist only sees env reads whose literal name matches a declared
pattern. The presence reverse-gate closes that gap: the inventory covers
*every* env read — including reads under non-watchlisted or dynamic names — by
requiring a waiver on every `os.environ` / `os.getenv` access site too. It is
on by default (the strongest audit with no configuration); a repo opts *out*
with `[tool.spice.policy] env_presence_gate = false`.

Library seam: target-repo tools may import the public finding dataclass,
pattern/matcher helpers, scan helper, and `render_env_policy_board`;
underscored names remain private.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path

from spice.errors import SpiceError
from spice.policy import (
    ENV_POLICY_ALLOW_MARKER,
    ENV_POLICY_DEFAULT_NAME_PATTERNS,
    ENV_POLICY_SELF_PATH_SUFFIX,
    ENV_SUFFIXES,
)
from spice.repocfg import policy_table, string_list
from spice.studies.walk import is_excluded_path

SCANNED_SUFFIXES = ENV_SUFFIXES
# This module necessarily names the patterns it polices; it is self-waived.
SELF_PATH_SUFFIX = ENV_POLICY_SELF_PATH_SUFFIX

# Presence reverse-gate: the name-pattern watchlist only sees env reads whose
# literal name matches a declared pattern, so a read under any other name
# (`os.getenv("HOME")`) or a dynamic name (`os.environ[var]`) escapes the
# inventory entirely. When the gate is enabled, any env-access *site* must also
# carry the waiver, making the audit cover every place the environment is read.
ENV_ACCESS_RE = re.compile(r"\bos\.(?:environ|getenv)\b")
ENV_ACCESS_FINDING_NAME = "os env access"


@dataclass(frozen=True)
class EnvPolicyFinding:
    path: str
    line: int
    name: str


def scan_env_policy(paths: list[Path], *, root: Path) -> list[EnvPolicyFinding]:
    findings: list[EnvPolicyFinding] = []
    matchers = env_name_matchers(root)
    presence_gate = env_presence_gate_enabled(root)
    for rel_path in paths:
        if rel_path.suffix not in SCANNED_SUFFIXES or is_excluded_path(
            rel_path, repo_root=root
        ):
            continue
        if rel_path.as_posix().endswith(SELF_PATH_SUFFIX):
            continue
        abs_path = root / rel_path
        if not abs_path.exists():
            continue
        text = abs_path.read_text(encoding="utf-8", errors="replace")
        waived_lines = _waived_line_numbers(text)
        for line_number, line in enumerate(text.splitlines(), start=1):
            if line_number in waived_lines:
                continue
            line_findings = [
                EnvPolicyFinding(
                    path=rel_path.as_posix(),
                    line=line_number,
                    name=match.group("name"),
                )
                for matcher in matchers
                for match in matcher.finditer(line)
            ]
            if not line_findings and presence_gate and ENV_ACCESS_RE.search(line):
                line_findings.append(
                    EnvPolicyFinding(
                        path=rel_path.as_posix(),
                        line=line_number,
                        name=ENV_ACCESS_FINDING_NAME,
                    )
                )
            findings.extend(line_findings)
    return findings


def _waived_line_numbers(text: str) -> set[int]:
    lines = text.splitlines()
    marker_lines = {
        line_number
        for line_number, line in enumerate(lines, start=1)
        if ENV_POLICY_ALLOW_MARKER in line
    }
    standalone_marker_lines = {
        line_number
        for line_number, line in enumerate(lines, start=1)
        if _standalone_waiver_line(line)
    }
    waived = set(marker_lines)
    for start, end in _statement_spans(lines):
        if start - 1 in standalone_marker_lines or any(
            line_number in marker_lines for line_number in range(start, end + 1)
        ):
            waived.update(range(start, end + 1))
    return waived


def _standalone_waiver_line(line: str) -> bool:
    stripped = line.strip()
    return ENV_POLICY_ALLOW_MARKER in stripped and (
        stripped.startswith("#") or stripped.startswith("//")
    )


def _statement_spans(lines: list[str]) -> list[tuple[int, int]]:
    spans: list[tuple[int, int]] = []
    start: int | None = None
    depth = 0
    for line_number, raw_line in enumerate(lines, start=1):
        if start is None and not raw_line.strip():
            continue
        if start is None:
            start = line_number
        structural = _structural_line(raw_line)
        depth = max(0, depth + _delimiter_delta(structural))
        if depth == 0 and not structural.rstrip().endswith("\\"):
            spans.append((start, line_number))
            start = None
    if start is not None:
        spans.append((start, len(lines)))
    return spans


def _structural_line(line: str) -> str:
    result: list[str] = []
    in_single = False
    in_double = False
    escape = False
    index = 0
    while index < len(line):
        char = line[index]
        nxt = line[index + 1] if index + 1 < len(line) else ""
        if in_single:
            if escape:
                escape = False
            elif char == "\\":
                escape = True
            elif char == "'":
                in_single = False
            index += 1
            continue
        if in_double:
            if escape:
                escape = False
            elif char == "\\":
                escape = True
            elif char == '"':
                in_double = False
            index += 1
            continue
        if char == "#":
            break
        if char == "/" and nxt == "/":
            break
        if char == "'":
            in_single = True
            index += 1
            continue
        if char == '"':
            in_double = True
            index += 1
            continue
        result.append(char)
        index += 1
    return "".join(result)


def _delimiter_delta(line: str) -> int:
    return sum(1 for char in line if char in "([{") - sum(
        1 for char in line if char in ")]}"
    )


def env_presence_gate_enabled(repo_root: Path) -> bool:
    """Whether the env-access presence reverse-gate is on for this repo.

    On by default — the strongest audit is the default, with no configuration.
    A repo opts *out* with `[tool.spice.policy] env_presence_gate = false` if it
    is not ready to waive every `os.environ`/`os.getenv` access site.
    """
    value = policy_table(repo_root).get("env_presence_gate", True)
    if not isinstance(value, bool):
        raise SpiceError(
            "[tool.spice.policy] env_presence_gate must be a boolean (true/false)"
        )
    return value


def env_name_patterns(repo_root: Path) -> list[str]:
    declared = string_list(policy_table(repo_root).get("env_name_patterns"))
    patterns: list[str] = list(ENV_POLICY_DEFAULT_NAME_PATTERNS)
    patterns.extend(pattern for pattern in declared if pattern not in patterns)
    return patterns


def env_name_matchers(repo_root: Path) -> list[re.Pattern[str]]:
    matchers: list[re.Pattern[str]] = []
    for pattern in env_name_patterns(repo_root):
        try:
            matchers.append(re.compile(_quoted_name_pattern(pattern)))
        except re.error as exc:
            raise SpiceError(
                "[tool.spice.policy] env_name_patterns contains invalid regex "
                f"{pattern!r}: {exc}"
            ) from exc
    return matchers


def _quoted_name_pattern(name_pattern: str) -> str:
    return rf"""["'](?P<name>{name_pattern})["']"""


def render_env_policy_board(findings: list[EnvPolicyFinding]) -> str:
    if not findings:
        return "env-policy: ok"
    lines = [
        f"env-policy: {len(findings)} undeclared environment literal(s); "
        f"add `# {ENV_POLICY_ALLOW_MARKER}` (or move the read behind a "
        "declared seam)"
    ]
    for finding in findings:
        lines.append(f"  FAIL  {finding.path}:{finding.line}: {finding.name}")
    return "\n".join(lines)

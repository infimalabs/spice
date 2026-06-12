"""Environment-variable literal policy: every read is declared or waived.

Harness-owned env names (`SPICE_*`, plus the driver's thread variable) may
appear in source only on lines carrying the `env-policy: allow` waiver
comment. The point is an auditable inventory: grep the waiver to see every
place the environment shapes behavior.

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


@dataclass(frozen=True)
class EnvPolicyFinding:
    path: str
    line: int
    name: str


def scan_env_policy(paths: list[Path], *, root: Path) -> list[EnvPolicyFinding]:
    findings: list[EnvPolicyFinding] = []
    matchers = env_name_matchers(root)
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
        for line_number, line in enumerate(text.splitlines(), start=1):
            if ENV_POLICY_ALLOW_MARKER in line:
                continue
            for matcher in matchers:
                for match in matcher.finditer(line):
                    findings.append(
                        EnvPolicyFinding(
                            path=rel_path.as_posix(),
                            line=line_number,
                            name=match.group("name"),
                        )
                    )
    return findings


def env_name_patterns(repo_root: Path) -> list[str]:
    declared = string_list(policy_table(repo_root).get("env_name_patterns"))
    patterns = list(ENV_POLICY_DEFAULT_NAME_PATTERNS)
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

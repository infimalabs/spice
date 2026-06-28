"""Environment-variable literal policy: every read is declared or waived.

Harness-owned env names (`SPICE_*`, plus shipped driver thread variables) may
appear in source only in statements carrying, or immediately following a
standalone, `env-policy: allow` waiver comment. The point is an auditable
inventory: grep the waiver to see every place the environment shapes behavior.

The watchlist only sees env reads whose literal name matches a declared
pattern. The access gate closes that gap: the inventory covers
*every* env access — including reads under non-watchlisted or dynamic names —
by requiring a waiver on every known env-access site too. It is on by default
(the strongest audit with no configuration); a repo opts *out* with
`[tool.spice.policy] env_access_gate = false`.

Library seam: target-repo tools may import the public finding dataclass,
pattern/matcher helpers, scan helper, and `render_env_policy_board`;
underscored names remain private.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable

from spice.errors import SpiceError
from spice.policy import (
    ENV_ACCESS_DEFAULT_PATTERNS,
    ENV_ACCESS_FAMILY_SUFFIXES,
    ENV_ACCESS_FINDING_NAMES,
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
ENV_NAME_LEDGER_UNACCOUNTED = "unaccounted"
ENV_NAME_LEDGER_STALE = "stale"
_ENV_NAME_LITERAL_PATTERN = r"[A-Z_][A-Z0-9_]*"
_QUOTED_ENV_NAME_PATTERN = (
    rf"(?P<quote>[\"'])(?P<name>{_ENV_NAME_LITERAL_PATTERN})(?P=quote)"
)
_PYTHON_ACCESS_NAME_PATTERNS = (
    rf"\bos\.(?:getenv|putenv|unsetenv)\(\s*{_QUOTED_ENV_NAME_PATTERN}",
    rf"\bos\.environ\s*\[\s*{_QUOTED_ENV_NAME_PATTERN}",
    rf"\bos\.environ\.(?:get|pop|setdefault)\(\s*{_QUOTED_ENV_NAME_PATTERN}",
)
_ENV_DECLARATION_NAME_PATTERNS = (
    rf"\b[A-Z][A-Z0-9_]*_ENV\s*=\s*(?:\(\s*)?{_QUOTED_ENV_NAME_PATTERN}",
    rf"\b(?:bin_env|thread_id_env)\s*=\s*{_QUOTED_ENV_NAME_PATTERN}",
)
_CSHARP_ACCESS_NAME_PATTERNS = (
    rf"\b(?:System\.)?Environment\."
    rf"(?:GetEnvironmentVariable|SetEnvironmentVariable)\(\s*"
    rf"{_QUOTED_ENV_NAME_PATTERN}",
)
_LUA_ACCESS_NAME_PATTERNS = (rf"\bos\.getenv\(\s*{_QUOTED_ENV_NAME_PATTERN}",)
_JS_BRACKET_ENV_NAME_PATTERN = re.compile(
    rf"\bprocess\.env\s*\[\s*{_QUOTED_ENV_NAME_PATTERN}\s*\]"
)
_JS_DOT_ENV_NAME_PATTERN = re.compile(
    rf"\bprocess\.env\.(?P<name>{_ENV_NAME_LITERAL_PATTERN})\b"
)
_JS_DESTRUCTURED_ENV_PATTERN = re.compile(r"\{(?P<body>[^}]+)\}\s*=\s*process\.env\b")
_ENV_CONTEXT_PATTERN = re.compile(
    r"\bbase_env\b|\bos\.environ\b|\benviron\b|"
    r"\b(?:setenv|delenv|putenv|unsetenv)\b|"
    r"\benv\s*(?:\[|\.get\(|\.pop\(|\.setdefault\()|process\.env"
)
_ENV_CONTEXT_NAME_PATTERN = re.compile(_QUOTED_ENV_NAME_PATTERN)
_COMPILED_ENV_DECLARATION_NAME_PATTERNS = tuple(
    re.compile(pattern) for pattern in _ENV_DECLARATION_NAME_PATTERNS
)
_SHELL_EXPORT_ENV_NAME_PATTERN = re.compile(
    rf"\bexport\s+(?P<name>{_ENV_NAME_LITERAL_PATTERN})="
)
_SHELL_BRACED_ENV_NAME_PATTERN = re.compile(
    rf"(?<!\\)\$\{{(?P<name>{_ENV_NAME_LITERAL_PATTERN})(?::[^}}]*)?\}}"
)
_SHELL_BARE_ENV_NAME_PATTERN = re.compile(
    rf"(?<!\\)\$(?P<name>{_ENV_NAME_LITERAL_PATTERN})\b"
)
_COMPILED_ACCESS_NAME_PATTERNS = {
    "python": tuple(re.compile(pattern) for pattern in _PYTHON_ACCESS_NAME_PATTERNS),
    "csharp": tuple(re.compile(pattern) for pattern in _CSHARP_ACCESS_NAME_PATTERNS),
    "lua": tuple(re.compile(pattern) for pattern in _LUA_ACCESS_NAME_PATTERNS),
}

# Access gate: the name-pattern watchlist only sees env reads whose
# literal name matches a declared pattern, so a read under any other name
# (`os.getenv("HOME")`) or a dynamic name (`os.environ[var]`) escapes the
# inventory entirely. When the gate is enabled, any env-access *site* must also
# carry the waiver, making the audit cover every place the environment is read.
# Access idioms differ per language, so matchers are scoped by suffix family
# (`env_access_matchers`); a shell `$VAR` pattern never runs against `.cs`.


@dataclass(frozen=True)
class EnvAccessMatcher:
    pattern: re.Pattern[str]
    name: str


@dataclass(frozen=True)
class EnvPolicyFinding:
    path: str
    line: int
    name: str


@dataclass(frozen=True)
class EnvNameReference:
    path: str
    line: int
    name: str


@dataclass(frozen=True)
class EnvNameLedgerFinding:
    kind: str
    name: str
    references: tuple[EnvNameReference, ...] = ()


def scan_env_policy(paths: list[Path], *, root: Path) -> list[EnvPolicyFinding]:
    findings: list[EnvPolicyFinding] = []
    matchers = env_name_matchers(root)
    access_gate = env_access_gate_enabled(root)
    access_matchers = env_access_matchers(root) if access_gate else {}
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
        access = access_matchers.get(rel_path.suffix, ())
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
            if not line_findings:
                access_name = _first_access_name(access, line)
                if access_name is not None:
                    line_findings.append(
                        EnvPolicyFinding(
                            path=rel_path.as_posix(),
                            line=line_number,
                            name=access_name,
                        )
                    )
            findings.extend(line_findings)
    return findings


def scan_env_name_ledger(
    paths: list[Path], *, root: Path
) -> list[EnvNameLedgerFinding]:
    declared = set(env_names(root))
    references = collect_env_name_references(paths, root=root)
    by_name: dict[str, list[EnvNameReference]] = {}
    for reference in references:
        by_name.setdefault(reference.name, []).append(reference)

    findings: list[EnvNameLedgerFinding] = []
    for name in sorted(set(by_name) - declared):
        findings.append(
            EnvNameLedgerFinding(
                kind=ENV_NAME_LEDGER_UNACCOUNTED,
                name=name,
                references=tuple(by_name[name]),
            )
        )
    for name in sorted(declared - set(by_name)):
        findings.append(EnvNameLedgerFinding(kind=ENV_NAME_LEDGER_STALE, name=name))
    return findings


def collect_env_name_references(
    paths: list[Path], *, root: Path
) -> list[EnvNameReference]:
    references: dict[tuple[str, int, str], EnvNameReference] = {}
    watchlist_matchers = env_name_matchers(root)
    exact_matchers = env_name_exact_matchers(root)
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
        language = _env_family_for_suffix(rel_path.suffix)
        text = abs_path.read_text(encoding="utf-8", errors="replace")
        for line_number, line in enumerate(text.splitlines(), start=1):
            names = set(_literal_env_names_from_access_line(language, line))
            names.update(_literal_env_names_from_declaration_line(line))
            names.update(_literal_env_names_from_context_line(line))
            for matcher in (*watchlist_matchers, *exact_matchers):
                names.update(match.group("name") for match in matcher.finditer(line))
            for name in sorted(name for name in names if _is_ledger_env_name(name)):
                key = (rel_path.as_posix(), line_number, name)
                references.setdefault(
                    key,
                    EnvNameReference(
                        path=rel_path.as_posix(),
                        line=line_number,
                        name=name,
                    ),
                )
    return sorted(references.values(), key=lambda ref: (ref.name, ref.path, ref.line))


def _env_family_for_suffix(suffix: str) -> str | None:
    for family, suffixes in ENV_ACCESS_FAMILY_SUFFIXES.items():
        if suffix in suffixes:
            return family
    return None


def _is_ledger_env_name(name: str) -> bool:
    return bool(re.fullmatch(_ENV_NAME_LITERAL_PATTERN, name)) and not name.endswith(
        "_"
    )


def _literal_env_names_from_access_line(family: str | None, line: str) -> set[str]:
    if family in _COMPILED_ACCESS_NAME_PATTERNS:
        return {
            match.group("name")
            for pattern in _COMPILED_ACCESS_NAME_PATTERNS[family]
            for match in pattern.finditer(line)
        }
    if family == "javascript":
        return _javascript_env_names_from_access_line(line)
    if family == "shell":
        return _shell_env_names_from_access_line(line)
    return set()


def _literal_env_names_from_context_line(line: str) -> set[str]:
    if not (_ENV_CONTEXT_PATTERN.search(line) or ENV_POLICY_ALLOW_MARKER in line):
        return set()
    return {match.group("name") for match in _ENV_CONTEXT_NAME_PATTERN.finditer(line)}


def _literal_env_names_from_declaration_line(line: str) -> set[str]:
    return {
        match.group("name")
        for pattern in _COMPILED_ENV_DECLARATION_NAME_PATTERNS
        for match in pattern.finditer(line)
    }


def _javascript_env_names_from_access_line(line: str) -> set[str]:
    names = {
        match.group("name") for match in _JS_BRACKET_ENV_NAME_PATTERN.finditer(line)
    }
    names.update(
        match.group("name") for match in _JS_DOT_ENV_NAME_PATTERN.finditer(line)
    )
    for match in _JS_DESTRUCTURED_ENV_PATTERN.finditer(line):
        names.update(_javascript_destructured_env_names(match.group("body")))
    return names


def _javascript_destructured_env_names(body: str) -> set[str]:
    names: set[str] = set()
    for raw_part in body.split(","):
        part = raw_part.strip()
        if not part or part.startswith("..."):
            continue
        name = part.split(":", maxsplit=1)[0].split("=", maxsplit=1)[0].strip()
        if re.fullmatch(_ENV_NAME_LITERAL_PATTERN, name):
            names.add(name)
    return names


def _shell_env_names_from_access_line(line: str) -> set[str]:
    names = {
        match.group("name") for match in _SHELL_EXPORT_ENV_NAME_PATTERN.finditer(line)
    }
    names.update(
        match.group("name") for match in _SHELL_BRACED_ENV_NAME_PATTERN.finditer(line)
    )
    names.update(
        match.group("name") for match in _SHELL_BARE_ENV_NAME_PATTERN.finditer(line)
    )
    return names


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


def env_access_gate_enabled(repo_root: Path) -> bool:
    """Whether the env access gate is on for this repo.

    On by default — the strongest audit is the default, with no configuration.
    A repo opts *out* with `[tool.spice.policy] env_access_gate = false` if it
    is not ready to waive every `os.environ`/`os.getenv` access site.
    """
    value = policy_table(repo_root).get("env_access_gate", True)
    if not isinstance(value, bool):
        raise SpiceError(
            "[tool.spice.policy] env_access_gate must be a boolean (true/false)"
        )
    return value


def env_name_patterns(repo_root: Path) -> list[str]:
    declared = string_list(policy_table(repo_root).get("env_name_patterns"))
    patterns: list[str] = list(ENV_POLICY_DEFAULT_NAME_PATTERNS)
    patterns.extend(pattern for pattern in declared if pattern not in patterns)
    return patterns


def env_names(repo_root: Path) -> list[str]:
    return string_list(policy_table(repo_root).get("env_names"))


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


def env_name_exact_matchers(repo_root: Path) -> list[re.Pattern[str]]:
    return [
        re.compile(_quoted_name_pattern(re.escape(name)))
        for name in env_names(repo_root)
    ]


def _quoted_name_pattern(name_pattern: str) -> str:
    return rf"""["'](?P<name>{name_pattern})["']"""


def env_access_patterns(repo_root: Path) -> dict[str, list[str]]:
    """Per-family env-access idiom regexes: built-in defaults plus repo additions.

    The defaults carry the standard idiom for each language family; a repo adds
    its own or additional idioms via `[tool.spice.policy] env_access_patterns`,
    a table keyed by family name (`python`, `csharp`, `lua`, `shell`).
    """
    patterns: dict[str, list[str]] = {
        family: list(defaults)
        for family, defaults in ENV_ACCESS_DEFAULT_PATTERNS.items()
    }
    for family, declared in _declared_env_access_patterns(repo_root).items():
        family_patterns = patterns.setdefault(family, [])
        family_patterns.extend(
            pattern for pattern in declared if pattern not in family_patterns
        )
    return patterns


def _declared_env_access_patterns(repo_root: Path) -> dict[str, list[str]]:
    raw = policy_table(repo_root).get("env_access_patterns")
    if raw is None:
        return {}
    known = ", ".join(sorted(ENV_ACCESS_FAMILY_SUFFIXES))
    if not isinstance(raw, dict):
        raise SpiceError(
            "[tool.spice.policy] env_access_patterns must be a table mapping a "
            f"language family to a list of access-idiom regexes; families: {known}"
        )
    declared: dict[str, list[str]] = {}
    for family, value in raw.items():
        if family not in ENV_ACCESS_FAMILY_SUFFIXES:
            raise SpiceError(
                f"[tool.spice.policy] env_access_patterns has unknown family "
                f"{family!r}; families: {known}"
            )
        declared[family] = string_list(value)
    return declared


def env_access_matchers(repo_root: Path) -> dict[str, list[EnvAccessMatcher]]:
    """Suffix -> compiled env-access matchers, scoped by language family.

    A file is audited only by the matchers of its own family, so a shell `$VAR`
    idiom never fires against a `.cs` or `.js` source.
    """
    by_suffix: dict[str, list[EnvAccessMatcher]] = {}
    for family, patterns in env_access_patterns(repo_root).items():
        name = ENV_ACCESS_FINDING_NAMES.get(family, f"{family} env access")
        family_matchers = [
            EnvAccessMatcher(
                pattern=_compile_access_pattern(family, pattern), name=name
            )
            for pattern in patterns
        ]
        if not family_matchers:
            continue
        for suffix in ENV_ACCESS_FAMILY_SUFFIXES[family]:
            by_suffix.setdefault(suffix, []).extend(family_matchers)
    return by_suffix


def _compile_access_pattern(family: str, pattern: str) -> re.Pattern[str]:
    try:
        return re.compile(pattern)
    except re.error as exc:
        raise SpiceError(
            "[tool.spice.policy] env_access_patterns contains invalid regex for "
            f"family {family!r}: {pattern!r}: {exc}"
        ) from exc


def _first_access_name(access: Iterable[EnvAccessMatcher], line: str) -> str | None:
    for matcher in access:
        if matcher.pattern.search(line):
            return matcher.name
    return None


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


def render_env_name_ledger_board(findings: list[EnvNameLedgerFinding]) -> str:
    if not findings:
        return "env-name-ledger: ok"
    lines = [
        f"env-name-ledger: {len(findings)} manifest mismatch(es); "
        "update `[tool.spice.policy] env_names`"
    ]
    for finding in findings:
        lines.append(f"  FAIL  {finding.kind}: {finding.name}")
        for reference in finding.references:
            lines.append(f"        used at {reference.path}:{reference.line}")
    return "\n".join(lines)

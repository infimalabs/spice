"""Bounded and redacted diagnostics shared by release-proof runners."""

from __future__ import annotations

import os
import re
import shlex
from pathlib import Path
from urllib.parse import parse_qsl, urlencode, urlsplit, urlunsplit

MAX_FAILURE_ARTIFACTS = 8
MAX_FAILURE_BYTES = 64 * 1024
MIN_SECRET_LENGTH = 6
FAILURE_DIRNAME = "failures"
SENSITIVE_ENV_MARKERS = (
    "AUTH",
    "CREDENTIAL",
    "KEY",
    "PASSWORD",
    "SECRET",
    "TOKEN",
)
SENSITIVE_URL_MARKERS = (
    "access_token",
    "api_key",
    "auth",
    "credential",
    "key",
    "password",
    "secret",
    "sig",
    "signature",
    "token",
)
URL_PATTERN = re.compile(r"https?://[^\s\"'<>]+")
SLUG_PATTERN = re.compile(r"[^a-z0-9]+")
TRUNCATION_MARKER = "\n... release-proof diagnostic truncated ...\n"


class FailureArtifactStore:
    """Write deterministic failure logs under strict count and byte bounds."""

    def __init__(self, artifact_dir: Path) -> None:
        self.directory = artifact_dir / FAILURE_DIRNAME
        self._count = 0

    def record(
        self,
        gate: str,
        command: list[str] | tuple[str, ...],
        returncode: int,
        stdout: str,
        stderr: str,
        *,
        environment: dict[str, str] | None = None,
    ) -> Path:
        self._count += 1
        slot = min(self._count, MAX_FAILURE_ARTIFACTS)
        label = _slug(gate) if slot < MAX_FAILURE_ARTIFACTS else "overflow"
        path = self.directory / f"{slot:02d}-{label}.log"
        payload = "\n".join(
            (
                f"gate={gate}",
                f"command={shlex.join([str(part) for part in command])}",
                f"exit_code={returncode}",
                "stdout:",
                stdout,
                "stderr:",
                stderr,
            )
        )
        effective_environment = (
            environment
            if environment is not None
            else dict(os.environ)  # env-policy: allow
        )
        redacted = redact_text(payload, effective_environment)
        _atomic_write_text(path, _bounded_utf8(redacted))
        return path


def redact_text(text: str, environment: dict[str, str]) -> str:
    """Redact token-bearing URLs and values from sensitive environment names."""
    redacted = URL_PATTERN.sub(_redact_url_match, text)
    sensitive = sorted(
        (
            (name, value)
            for name, value in environment.items()
            if len(value) >= MIN_SECRET_LENGTH and _sensitive_env_name(name)
        ),
        key=lambda item: len(item[1]),
        reverse=True,
    )
    for name, value in sensitive:
        redacted = redacted.replace(value, f"<redacted-env:{name}>")
    return redacted


def parse_pytest_counts(output: str) -> dict[str, int]:
    """Extract the final pytest outcome counts from terminal output."""
    summary_lines = [line for line in output.splitlines() if " in " in line]
    summary = summary_lines[-1] if summary_lines else output
    pairs = re.findall(
        r"(\d+) (passed|failed|skipped|xfailed|xpassed|deselected|errors?)",
        summary,
    )
    counts: dict[str, int] = {}
    for raw_count, raw_name in pairs:
        name = "errors" if raw_name in {"error", "errors"} else raw_name
        counts[name] = int(raw_count)
    if "passed" not in counts:
        raise ValueError(f"pytest output has no passed count: {summary!r}")
    counts["total"] = sum(
        counts.get(name, 0)
        for name in ("passed", "failed", "skipped", "xfailed", "xpassed", "errors")
    )
    return counts


def failure_policy_payload() -> dict[str, object]:
    return {
        "directory": FAILURE_DIRNAME,
        "max_artifacts": MAX_FAILURE_ARTIFACTS,
        "max_bytes_per_artifact": MAX_FAILURE_BYTES,
        "redactions": ["sensitive-environment-values", "token-bearing-urls"],
    }


def _sensitive_env_name(name: str) -> bool:
    upper = name.upper()
    return any(marker in upper for marker in SENSITIVE_ENV_MARKERS)


def _redact_url_match(match: re.Match[str]) -> str:
    raw = match.group(0)
    trailing = ""
    while raw.endswith((".", ",", ")", "]")):
        trailing = raw[-1] + trailing
        raw = raw[:-1]
    try:
        parsed = urlsplit(raw)
    except ValueError:
        return "<redacted-url>" + trailing
    netloc = (
        f"<redacted>@{parsed.netloc.rsplit('@', 1)[1]}"
        if "@" in parsed.netloc
        else parsed.netloc
    )
    return (
        urlunsplit(
            (
                parsed.scheme,
                netloc,
                parsed.path,
                _redact_url_pairs(parsed.query),
                _redact_url_fragment(parsed.fragment),
            )
        )
        + trailing
    )


def _redact_url_pairs(value: str) -> str:
    return urlencode(
        [
            (
                name,
                "<redacted>" if _sensitive_url_name(name) else item_value,
            )
            for name, item_value in parse_qsl(value, keep_blank_values=True)
        ]
    )


def _redact_url_fragment(fragment: str) -> str:
    if "?" in fragment:
        route, separator, query = fragment.partition("?")
        return route + separator + _redact_url_pairs(query)
    if "=" in fragment:
        return _redact_url_pairs(fragment)
    return fragment


def _sensitive_url_name(name: str) -> bool:
    lowered = name.casefold()
    return any(marker in lowered for marker in SENSITIVE_URL_MARKERS)


def _slug(value: str) -> str:
    return SLUG_PATTERN.sub("-", value.casefold()).strip("-") or "failure"


def _bounded_utf8(text: str) -> str:
    encoded = text.encode("utf-8")
    if len(encoded) <= MAX_FAILURE_BYTES:
        return text
    marker = TRUNCATION_MARKER.encode("utf-8")
    remaining = MAX_FAILURE_BYTES - len(marker)
    prefix_size = remaining // 2
    suffix_size = remaining - prefix_size
    prefix = encoded[:prefix_size].decode("utf-8", errors="ignore")
    suffix = encoded[-suffix_size:].decode("utf-8", errors="ignore")
    return prefix + TRUNCATION_MARKER + suffix


def _atomic_write_text(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.tmp")
    temporary.write_text(content, encoding="utf-8")
    temporary.replace(path)

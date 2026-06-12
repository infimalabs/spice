"""TTS rendering for the UI: macOS `say` output as browser-playable M4A."""

from __future__ import annotations

import os
import re
import subprocess
import tempfile
from pathlib import Path

from spice.config import say_command_args

SAY_AUDIO_CONTENT_TYPE = "audio/mp4"
SAY_AUDIO_SUFFIX = ".m4a"
DEFAULT_SAY_RATE_MULTIPLIER = 1.0
MIN_SAY_RATE_MULTIPLIER = 0.5
MAX_SAY_RATE_MULTIPLIER = 2.0
_SAY_WORDISH_SLASH_TOKEN_RE = re.compile(r"\S+")
_SAY_FULL_GIT_HASH_RE = re.compile(r"(?<![0-9A-Fa-f])([0-9A-Fa-f]{40})(?![0-9A-Fa-f])")
_SAY_UTC_DATETIME_RE = re.compile(
    r"(?<![0-9A-Za-z])("
    r"[0-9]{8}T[0-9]{6,12}Z|"
    r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}"
    r"(?:\.[0-9]+)?Z"
    r")(?![0-9A-Za-z])"
)
_SAY_MARKDOWN_LINK_RE = re.compile(r"\[([^\]]+)\]\([^)\s]+\)")
_SAY_MARKDOWN_IMAGE_RE = re.compile(r"!\[([^\]]*)\]\([^)\s]+\)")
_SAY_IDENTIFIER_SPOKEN_LENGTH = 8


def prepare_say_text(text: str) -> str:
    """Massage text on its way to the TTS engine.

    Markdown links and images collapse to their labels; full git hashes and
    UTC stamps collapse to their last eight characters; a word-like token
    with exactly one internal slash has that slash replaced with a space
    (macOS `say` pronounces slash-heavy agent text too literally).
    """
    prepared = _SAY_MARKDOWN_IMAGE_RE.sub(lambda match: match.group(1) or "image", text)
    prepared = _SAY_MARKDOWN_LINK_RE.sub(lambda match: match.group(1), prepared)
    prepared = _SAY_FULL_GIT_HASH_RE.sub(
        lambda match: match.group(1)[-_SAY_IDENTIFIER_SPOKEN_LENGTH:], prepared
    )
    prepared = _SAY_UTC_DATETIME_RE.sub(
        lambda match: re.sub(r"[-:.]", "", match.group(1))[
            -_SAY_IDENTIFIER_SPOKEN_LENGTH:
        ],
        prepared,
    )
    return _SAY_WORDISH_SLASH_TOKEN_RE.sub(_prepare_say_token, prepared)


def _prepare_say_token(match: re.Match[str]) -> str:
    token = match.group(0)
    if token.count("/") != 1:
        return token
    slash_index = token.index("/")
    if slash_index == 0 or slash_index == len(token) - 1:
        return token
    return token.replace("/", " ")


def normalize_say_rate_multiplier(value: str | int | float | None) -> float:
    if value is None:
        return DEFAULT_SAY_RATE_MULTIPLIER
    try:
        rate = float(value)
    except (TypeError, ValueError):
        return DEFAULT_SAY_RATE_MULTIPLIER
    if rate != rate:  # NaN
        return DEFAULT_SAY_RATE_MULTIPLIER
    return max(MIN_SAY_RATE_MULTIPLIER, min(rate, MAX_SAY_RATE_MULTIPLIER))


def render_say_audio(
    text: str,
    *,
    repo_root: Path | None = None,
    rate_multiplier: float = DEFAULT_SAY_RATE_MULTIPLIER,
) -> bytes:
    """Render macOS `say` output into browser-playable M4A bytes."""
    handle, raw_path = tempfile.mkstemp(prefix="spice-say-", suffix=SAY_AUDIO_SUFFIX)
    audio_path = Path(raw_path)
    try:
        os.close(handle)
        subprocess.run(
            [
                *say_command_args(
                    repo_root,
                    rate_multiplier=normalize_say_rate_multiplier(rate_multiplier),
                ),
                "-o",
                str(audio_path),
                "--file-format=m4af",
                "--data-format=aac",
                "-f",
                "-",
            ],
            input=prepare_say_text(text),
            text=True,
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        return audio_path.read_bytes()
    finally:
        audio_path.unlink(missing_ok=True)

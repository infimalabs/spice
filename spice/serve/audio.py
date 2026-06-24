"""TTS rendering for the UI through configurable speech backends."""

from __future__ import annotations

import os
import re
import shlex
import subprocess
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Protocol

from spice import config

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


@dataclass(frozen=True)
class SpeechAudio:
    data: bytes
    content_type: str


class SpeechBackend(Protocol):
    def render(
        self,
        text: str,
        *,
        rate_multiplier: float = DEFAULT_SAY_RATE_MULTIPLIER,
    ) -> SpeechAudio:
        """Render text into browser-playable audio bytes."""
        ...


@dataclass(frozen=True)
class MacOSSayBackend:
    repo_root: Path | None = None

    def render(
        self,
        text: str,
        *,
        rate_multiplier: float = DEFAULT_SAY_RATE_MULTIPLIER,
    ) -> SpeechAudio:
        return SpeechAudio(
            data=_render_macos_say_audio(
                text,
                repo_root=self.repo_root,
                rate_multiplier=rate_multiplier,
            ),
            content_type=SAY_AUDIO_CONTENT_TYPE,
        )


@dataclass(frozen=True)
class ExternalCommandSpeechBackend:
    command: tuple[str, ...]
    content_type: str = config.DEFAULT_EXTERNAL_SAY_CONTENT_TYPE

    def render(
        self,
        text: str,
        *,
        rate_multiplier: float = DEFAULT_SAY_RATE_MULTIPLIER,
    ) -> SpeechAudio:
        if not self.command:
            raise RuntimeError("external speech backend requires a command")
        result = subprocess.run(
            list(self.command),
            input=prepare_say_text(text).encode("utf-8"),
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        if result.returncode != 0:
            detail = result.stderr.decode("utf-8", "replace").strip()
            suffix = f": {detail}" if detail else ""
            raise RuntimeError(
                f"external speech backend exited {result.returncode}{suffix}"
            )
        if not result.stdout:
            raise RuntimeError("external speech backend produced no audio")
        return SpeechAudio(result.stdout, self.content_type)


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
    """Render configured speech audio bytes, preserving the historical API."""
    return render_speech_audio(
        text,
        repo_root=repo_root,
        rate_multiplier=rate_multiplier,
    ).data


def render_speech_audio(
    text: str,
    *,
    repo_root: Path | None = None,
    rate_multiplier: float = DEFAULT_SAY_RATE_MULTIPLIER,
) -> SpeechAudio:
    backend = speech_backend(repo_root)
    return backend.render(text, rate_multiplier=rate_multiplier)


def speech_backend(repo_root: Path | None = None) -> SpeechBackend:
    backend = config.configured_say_backend(repo_root)
    if backend == "external":
        command = _external_speech_command(repo_root)
        return ExternalCommandSpeechBackend(
            command=command,
            content_type=config.configured_say_content_type(repo_root),
        )
    return MacOSSayBackend(repo_root)


def _external_speech_command(repo_root: Path | None) -> tuple[str, ...]:
    raw = config.configured_say_command(repo_root)
    if not raw:
        raise RuntimeError("external speech backend requires a configured command")
    try:
        command = tuple(shlex.split(raw))
    except ValueError as exc:
        raise RuntimeError(f"invalid external speech command: {exc}") from exc
    if not command:
        raise RuntimeError("external speech backend requires a configured command")
    return command


def _render_macos_say_audio(
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
                *config.say_command_args(
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

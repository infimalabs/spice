"""Typed configuration values resolved from the effective layered mapping."""

from __future__ import annotations

import math
import sys
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from spice import defaults
from spice.config.layers import (
    SYSTEM_SOURCE,
    contextualize_config_error,
    effective_mapping,
    load_config,
)
from spice.errors import SpiceError
from spice.paths import repo_root_from_cwd

SAY_KEY = "say"
SAY_BACKEND_KEY = "backend"
SAY_BACKEND_CHOICES = defaults.strings("say", "backend_choices")
DEFAULT_SAY_BACKEND = defaults.string("say", "backend")
SAY_COMMAND_KEY = "command"
SAY_CONTENT_TYPE_KEY = "content_type"
DEFAULT_EXTERNAL_SAY_CONTENT_TYPE = defaults.string("say", SAY_CONTENT_TYPE_KEY)
SAY_VOICE_KEY = "voice"
SAY_WORDS_PER_MINUTE_KEY = "words_per_minute"
DEFAULT_SAY_WORDS_PER_MINUTE = defaults.integer("say", "words_per_minute")
SAY_TIMEOUT_SECONDS_KEY = "timeout_seconds"
# Generous ceiling: comfortably covers well over a minute of spoken content
# (plus render overhead) so legitimately-long messages are never clipped, while
# still bounding a wedged speech process instead of blocking forever. A repo or
# worktree ``say.timeout_seconds`` override tunes it through the accessor below.
DEFAULT_SAY_TIMEOUT_SECONDS = defaults.number("say", SAY_TIMEOUT_SECONDS_KEY)
SAY_MUTABLE_KEYS = (
    SAY_BACKEND_KEY,
    SAY_COMMAND_KEY,
    SAY_CONTENT_TYPE_KEY,
    SAY_VOICE_KEY,
    SAY_WORDS_PER_MINUTE_KEY,
    SAY_TIMEOUT_SECONDS_KEY,
)

AGENT_KEY = "agent"
AGENT_PERSONALITY_KEY = "personality"
AGENT_PERSONALITY_CHOICES = defaults.strings("agent", "personality_choices")
DEFAULT_AGENT_PERSONALITY = defaults.string("agent", "personality")
AGENT_MODEL_KEY = "model"
AGENT_EFFORT_KEY = "effort"
AGENT_DRIVER_KEY = "driver"
AGENT_LAUNCH_KEYS = (AGENT_MODEL_KEY, AGENT_EFFORT_KEY, AGENT_DRIVER_KEY)

JUDGE_KEY = "judge"
JUDGE_BIN_KEY = "bin"
JUDGE_ENABLED_KEY = "enabled"
DEFAULT_JUDGE_BIN = defaults.string("judge", "bin")
PORTABLE_JUDGE_BIN = defaults.string("judge", "portable_bin")
RTK_KEY = "rtk"
RTK_EXECUTABLE_KEY = "executable"
DEFAULT_RTK_EXECUTABLE = defaults.string(RTK_KEY, RTK_EXECUTABLE_KEY)
_CONFIG_FLAG_TRUE = frozenset({"true", "1", "yes", "on"})
_PACKAGED_VALUES = defaults.packaged_values()


@dataclass(frozen=True)
class ScalarPolicy:
    """Declared coercion policy for one effective scalar configuration key."""

    coerce: Callable[[Any, "ScalarPolicy", tuple[str, str]], Any]
    default: Any = None
    choices: tuple[str, ...] = ()
    requires_root: bool = False


def _coerce_text(raw: Any, policy: ScalarPolicy, _path: tuple[str, str]) -> Any:
    return str(raw or "").strip() or policy.default


def _coerce_optional_text(
    raw: Any, _policy: ScalarPolicy, _path: tuple[str, str]
) -> str | None:
    return str(raw).strip() or None if raw else None


def _coerce_choice(raw: Any, policy: ScalarPolicy, path: tuple[str, str]) -> Any:
    if raw is None:
        return policy.default
    value = str(raw).strip()
    if value in policy.choices:
        return value
    section, key = path
    choices = ", ".join(repr(choice) for choice in policy.choices)
    raise SpiceError(
        f"[{section}] {key} has invalid value {raw!r}; expected one of {choices}"
    )


def _coerce_positive_int(
    raw: Any, policy: ScalarPolicy, _path: tuple[str, str]
) -> int | None:
    if raw is None:
        return policy.default
    try:
        value = int(raw)
    except (TypeError, ValueError):
        return policy.default
    return value if value > 0 else policy.default


def _coerce_positive_seconds(
    raw: Any, policy: ScalarPolicy, _path: tuple[str, str]
) -> float:
    if raw is None:
        return policy.default
    try:
        value = float(raw)
    except (TypeError, ValueError):
        return policy.default
    if not math.isfinite(value):
        return policy.default
    return value if value > 0 else policy.default


def _coerce_flag(raw: Any, _policy: ScalarPolicy, _path: tuple[str, str]) -> bool:
    if isinstance(raw, bool):
        return raw
    return str(raw or "").strip().casefold() in _CONFIG_FLAG_TRUE


SCALAR_SCHEMA: dict[tuple[str, str], ScalarPolicy] = {
    (SAY_KEY, SAY_VOICE_KEY): ScalarPolicy(_coerce_optional_text),
    (SAY_KEY, SAY_BACKEND_KEY): ScalarPolicy(
        _coerce_choice, DEFAULT_SAY_BACKEND, SAY_BACKEND_CHOICES
    ),
    (SAY_KEY, SAY_COMMAND_KEY): ScalarPolicy(_coerce_text, ""),
    (SAY_KEY, SAY_CONTENT_TYPE_KEY): ScalarPolicy(
        _coerce_text, DEFAULT_EXTERNAL_SAY_CONTENT_TYPE
    ),
    (SAY_KEY, SAY_WORDS_PER_MINUTE_KEY): ScalarPolicy(
        _coerce_positive_int, DEFAULT_SAY_WORDS_PER_MINUTE
    ),
    (SAY_KEY, SAY_TIMEOUT_SECONDS_KEY): ScalarPolicy(
        _coerce_positive_seconds, DEFAULT_SAY_TIMEOUT_SECONDS
    ),
    (AGENT_KEY, AGENT_PERSONALITY_KEY): ScalarPolicy(
        _coerce_choice, DEFAULT_AGENT_PERSONALITY, AGENT_PERSONALITY_CHOICES
    ),
    (AGENT_KEY, AGENT_MODEL_KEY): ScalarPolicy(_coerce_text, "", requires_root=True),
    (AGENT_KEY, AGENT_EFFORT_KEY): ScalarPolicy(_coerce_text, "", requires_root=True),
    (AGENT_KEY, AGENT_DRIVER_KEY): ScalarPolicy(_coerce_text, "", requires_root=True),
    (JUDGE_KEY, JUDGE_ENABLED_KEY): ScalarPolicy(
        _coerce_flag, False, requires_root=True
    ),
}


def _scalar(section: str, key: str, repo_root: Path | None) -> Any:
    policy = SCALAR_SCHEMA[(section, key)]
    root = _root_or_current(repo_root)
    if root is None and policy.requires_root:
        return policy.default
    path = (section, key)
    try:
        return policy.coerce(_configured_value(root, section, key), policy, path)
    except SpiceError as exc:
        if root is None:
            raise
        raise contextualize_config_error(root, exc, *path) from exc


def _root_or_current(repo_root: Path | None) -> Path | None:
    return repo_root if repo_root is not None else repo_root_from_cwd()


def _effective_section(root: Path | None, key: str) -> dict[str, Any]:
    raw = layered_mapping(root).get(key)
    return raw if isinstance(raw, dict) else {}


def _configured_value(root: Path | None, section: str, key: str) -> Any:
    return _effective_section(root, section).get(key)


def _configured_choice(
    repo_root: Path | None,
    section: str,
    key: str,
    *,
    default: str,
    choices: tuple[str, ...],
) -> str:
    root = _root_or_current(repo_root)
    policy = ScalarPolicy(_coerce_choice, default, choices)
    path = (section, key)
    try:
        return policy.coerce(
            _configured_value(root, section, key),
            policy,
            path,
        )
    except SpiceError as exc:
        if root is None:
            raise
        raise contextualize_config_error(root, exc, *path) from exc


def layered_value(repo_root: Path | None, *path: str) -> Any:
    """Resolve one required packaged key through the effective layer chain.

    The immutable installed snapshot remains the base even when a caller
    redirects the system layer to a deliberately partial compatibility
    fixture. Effective configuration replaces that base wherever it supplies
    a value.
    """
    current: Any = layered_mapping(repo_root)
    for part in path:
        if not isinstance(current, Mapping) or part not in current:
            error = SpiceError(f"configuration is missing {'.'.join(path)}")
            root = _root_or_current(repo_root)
            if root is None:
                raise error
            raise contextualize_config_error(root, error, *path) from error
        current = current[part]
    return current


def layered_mapping(repo_root: Path | None = None) -> dict[str, Any]:
    """Return packaged defaults recursively overlaid by effective config."""
    root = _root_or_current(repo_root)
    return _overlay_mapping(_PACKAGED_VALUES, effective_mapping(root))


def layered_table(repo_root: Path | None, *path: str) -> dict[str, Any]:
    raw = layered_value(repo_root, *path)
    if isinstance(raw, dict):
        return raw
    raise _layered_type_error(repo_root, path, "a table")


def _overlay_mapping(
    base: Mapping[str, Any], override: Mapping[str, Any]
) -> dict[str, Any]:
    merged = {str(key): _mutable_config_value(value) for key, value in base.items()}
    for key, value in override.items():
        base_value = base.get(key)
        merged[str(key)] = (
            _overlay_mapping(base_value, value)
            if isinstance(base_value, Mapping) and isinstance(value, Mapping)
            else _mutable_config_value(value)
        )
    return merged


def _mutable_config_value(value: Any) -> Any:
    if isinstance(value, Mapping):
        return {str(key): _mutable_config_value(child) for key, child in value.items()}
    if isinstance(value, Sequence) and not isinstance(value, (str, bytes)):
        return [_mutable_config_value(child) for child in value]
    return value


def layered_string(repo_root: Path | None, *path: str) -> str:
    raw = layered_value(repo_root, *path)
    if isinstance(raw, str):
        return raw
    raise _layered_type_error(repo_root, path, "a string")


def layered_integer(repo_root: Path | None, *path: str) -> int:
    raw = layered_value(repo_root, *path)
    if isinstance(raw, int) and not isinstance(raw, bool):
        return raw
    raise _layered_type_error(repo_root, path, "an integer")


def layered_number(repo_root: Path | None, *path: str) -> float:
    raw = layered_value(repo_root, *path)
    if isinstance(raw, (int, float)) and not isinstance(raw, bool):
        return float(raw)
    raise _layered_type_error(repo_root, path, "numeric")


def layered_strings(repo_root: Path | None, *path: str) -> tuple[str, ...]:
    raw = layered_value(repo_root, *path)
    if (
        isinstance(raw, Sequence)
        and not isinstance(raw, (str, bytes))
        and all(isinstance(item, str) for item in raw)
    ):
        return tuple(raw)
    raise _layered_type_error(repo_root, path, "a list of strings")


def _layered_type_error(
    repo_root: Path | None, path: tuple[str, ...], expected: str
) -> SpiceError:
    root = _root_or_current(repo_root)
    error = SpiceError(f"configuration {'.'.join(path)} must be {expected}")
    return error if root is None else contextualize_config_error(root, error, *path)


def configured_say_voice(repo_root: Path | None = None) -> str | None:
    return _scalar(SAY_KEY, SAY_VOICE_KEY, repo_root)


def configured_say_backend(repo_root: Path | None = None) -> str:
    return _configured_choice(
        repo_root,
        SAY_KEY,
        SAY_BACKEND_KEY,
        default=DEFAULT_SAY_BACKEND,
        choices=layered_strings(repo_root, SAY_KEY, "backend_choices"),
    )


def configured_say_command(repo_root: Path | None = None) -> str:
    return _scalar(SAY_KEY, SAY_COMMAND_KEY, repo_root)


def configured_say_content_type(repo_root: Path | None = None) -> str:
    return _scalar(SAY_KEY, SAY_CONTENT_TYPE_KEY, repo_root)


def configured_say_words_per_minute(repo_root: Path | None = None) -> int:
    return int(_scalar(SAY_KEY, SAY_WORDS_PER_MINUTE_KEY, repo_root))


def configured_say_timeout(repo_root: Path | None = None) -> float:
    """Seconds a speech subprocess may run before it is bounded and reported.

    Falls back to the generous default when unset or non-positive so a valid
    long message is never clipped; a positive override lets operators tune it.
    """
    return _scalar(SAY_KEY, SAY_TIMEOUT_SECONDS_KEY, repo_root)


def configured_agent_personality(repo_root: Path | None = None) -> str:
    return _configured_choice(
        repo_root,
        AGENT_KEY,
        AGENT_PERSONALITY_KEY,
        default=DEFAULT_AGENT_PERSONALITY,
        choices=layered_strings(repo_root, AGENT_KEY, "personality_choices"),
    )


def configured_agent_model(repo_root: Path | None = None) -> str:
    """Agent launch model from the canonical layered configuration."""
    return _scalar(AGENT_KEY, AGENT_MODEL_KEY, repo_root)


def configured_agent_model_for_driver(repo_root: Path | None, driver_name: str) -> str:
    """Resolve a launch model, including a driver's packaged model key."""
    configured = configured_agent_model(repo_root)
    if not configured and driver_name == "claude":
        return layered_string(repo_root, AGENT_KEY, "claude", "default_model")
    return configured


def configured_agent_effort(repo_root: Path | None = None) -> str:
    """Codex reasoning effort from the configured spice effort setting."""
    return _scalar(AGENT_KEY, AGENT_EFFORT_KEY, repo_root)


def configured_agent_driver(repo_root: Path | None = None) -> str:
    """Which agent driver this worktree binds: worktree state, then project.

    Selects the agent CLI (`codex` | `claude`) when `SPICE_AGENT_DRIVER` is
    unset. Worktree-local state wins so one clone can run a different driver
    than the tracked project default without editing tracked history.
    """
    return _scalar(AGENT_KEY, AGENT_DRIVER_KEY, repo_root)


def maxim_adjudication_enabled(repo_root: Path | None = None) -> bool:
    """Return whether the opt-in maxim judge adjudicates trigger hits.

    Judge-free is the deterministic default: a matched trigger bag publishes
    its ``[MAXIM]`` reminder directly, with no judge subprocess. An
    installation opts into local YES/NO adjudication by setting
    ``[judge] enabled = true`` in any effective configuration layer -- the
    tracked ``spice.toml`` or the worktree-local config -- so a repository can
    turn adjudication on for itself without changing the packaged default that
    every other install inherits; any other value
    (including an absent one) resolves to the judge-free default.
    """
    return _scalar(JUDGE_KEY, JUDGE_ENABLED_KEY, repo_root)


def effective_agent_config(repo_root: Path) -> dict[str, str]:
    from spice.agent.driver import driver_for

    driver = driver_for(repo_root)
    configured_model = configured_agent_model_for_driver(repo_root, driver.name)
    return {
        AGENT_DRIVER_KEY: driver.name,
        AGENT_MODEL_KEY: driver.resolve_model(configured_model),
        AGENT_EFFORT_KEY: (
            configured_agent_effort(repo_root) or driver.default_reasoning_effort
        ),
    }


def default_judge_bin() -> str:
    """Return the built-in judge bin for this platform.

    macOS keeps the Apple Foundation Models ``afm-cli`` default; every other
    platform, where ``afm-cli`` does not exist, defaults to the portable
    ``spice-judge`` adapter so the conscience works out of the box off macOS.
    """
    return DEFAULT_JUDGE_BIN if sys.platform == "darwin" else PORTABLE_JUDGE_BIN


def configured_judge_bin(repo_root: Path | None = None) -> str:
    root = _root_or_current(repo_root)
    raw = str(_configured_value(root, JUDGE_KEY, JUDGE_BIN_KEY) or "").strip()
    portable = layered_string(repo_root, JUDGE_KEY, "portable_bin")
    source = load_config(root).source_for((JUDGE_KEY, JUDGE_BIN_KEY)) if root else None
    if sys.platform != "darwin" and (source is None or source.name == SYSTEM_SOURCE):
        return portable
    return raw or default_judge_bin()


def configured_judge_model(repo_root: Path | None = None) -> str:
    return layered_string(repo_root, JUDGE_KEY, "model")


def configured_judge_model_command(repo_root: Path | None = None) -> tuple[str, ...]:
    return layered_strings(repo_root, JUDGE_KEY, "model_command")


def configured_judge_timeout(repo_root: Path | None = None) -> float:
    return layered_number(repo_root, JUDGE_KEY, "timeout_seconds")


def configured_playwright_mcp(
    repo_root: Path | None = None,
) -> tuple[str, str, tuple[str, ...]]:
    return (
        layered_string(repo_root, AGENT_KEY, "playwright_mcp", "server_name"),
        layered_string(repo_root, AGENT_KEY, "playwright_mcp", "command"),
        layered_strings(repo_root, AGENT_KEY, "playwright_mcp", "args"),
    )


def configured_claude_auto_compact_window(repo_root: Path | None = None) -> int:
    return layered_integer(repo_root, AGENT_KEY, "claude", "auto_compact_window_tokens")


def configured_serve_host(repo_root: Path | None = None) -> str:
    return layered_string(repo_root, "serve", "host")


def configured_serve_port(repo_root: Path | None = None) -> int:
    return layered_integer(repo_root, "serve", "port")


def configured_rtk_executable(repo_root: Path | None = None) -> str:
    """Return the exact layered RTK executable identity without probing it."""
    root = _root_or_current(repo_root)
    section = layered_mapping(root).get(RTK_KEY)
    if not isinstance(section, Mapping):
        raise _rtk_config_error(
            root,
            SpiceError("[rtk] must be a table"),
            RTK_KEY,
        )
    raw = section.get(RTK_EXECUTABLE_KEY)
    if not isinstance(raw, str) or not _is_rtk_executable_identity(raw):
        raise _rtk_config_error(
            root,
            SpiceError(
                "[rtk] executable must be one non-empty executable "
                "basename or absolute path"
            ),
            RTK_KEY,
            RTK_EXECUTABLE_KEY,
        )
    return raw


def _is_rtk_executable_identity(value: str) -> bool:
    if not value or "\0" in value:
        return False
    path = Path(value)
    if path.is_absolute():
        return True
    return path.name == value and not any(character.isspace() for character in value)


def _rtk_config_error(
    repo_root: Path | None, error: SpiceError, *path: str
) -> SpiceError:
    if repo_root is None:
        return error
    return contextualize_config_error(repo_root, error, *path)


def scale_say_words_per_minute(base: int, rate_multiplier: float) -> int:
    """The words-per-minute one rate multiplier resolves to, for any backend.

    Both speech backends scale the same configured base, so a listener changing
    the rate hears the same proportion whichever engine renders the clip. The
    rate a listener sends is coerced once, where it enters, by the serve handler
    that reads it off the request; whatever reaches here is already a proportion.
    So a value that is not one is a broken caller rather than input to absorb,
    and it says so instead of resolving to a silently plausible word count.
    """
    if not math.isfinite(rate_multiplier) or rate_multiplier <= 0:
        raise SpiceError(
            "say rate multiplier must be a positive finite number, "
            f"got {rate_multiplier!r}"
        )
    return max(1, int(base * rate_multiplier + 0.5))


def say_command_args(
    repo_root: Path | None = None, *, rate_multiplier: float = 1.0
) -> list[str]:
    """Build the macOS `say` argv from repo-local config.

    Unset config emits only `["say"]` so the system voice and rate apply.
    """
    args = ["say"]
    voice = configured_say_voice(repo_root)
    if voice:
        args.extend(["-v", voice])
    words_per_minute = configured_say_words_per_minute(repo_root)
    effective = scale_say_words_per_minute(words_per_minute, rate_multiplier)
    args.extend(["-r", str(effective)])
    return args


def config_overview(repo_root: Path) -> dict[str, Any]:
    loaded = load_config(repo_root)
    configured_rtk_executable(repo_root)
    return {
        "layers": {
            layer.name: {
                "path": str(layer.path) if layer.path is not None else None,
                "present": layer.present,
                "values": _json_value(layer.values),
            }
            for layer in loaded.layers
        },
        "effective": _json_value(layered_mapping(repo_root)),
        "provenance": {
            ".".join(path): {
                "scope": layer.name,
                "path": str(layer.path) if layer.path is not None else None,
            }
            for path, layer in sorted(loaded.sources.items())
        },
    }


def agent_config_overview(repo_root: Path) -> dict[str, Any]:
    overview = config_overview(repo_root)
    provenance = overview["provenance"]
    return {
        "effective": effective_agent_config(repo_root),
        "provenance": {
            key: value
            for key, value in provenance.items()
            if key.startswith(f"{AGENT_KEY}.")
        },
    }


def _json_value(value: Any) -> Any:
    if isinstance(value, Mapping):
        return {str(key): _json_value(item) for key, item in value.items()}
    if isinstance(value, tuple):
        return [_json_value(item) for item in value]
    return value


def default_classifications() -> dict[str, str]:
    """Classify exported defaults for configuration diagnostics."""
    return defaults.export_classifications()

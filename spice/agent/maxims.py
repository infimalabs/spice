"""Judge whether a statement agrees with a maxim using a local LLM.

The primitive is deliberately small: render a YES/NO adjudication prompt from
a ``maxim`` and a ``statement``, ask a local model (the configured judge
binary, ``afm-cli`` by default), and collapse the reply to a single boolean.
The prompt is a ``str.format`` template exposing two fields, ``{maxim}`` and
``{statement}``, so callers can supply a different framing without touching
the parsing or backend wiring.
"""

from __future__ import annotations

import random
import re
import string
import subprocess
from collections.abc import Callable, Sequence
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass

from spice.config import configured_judge_bin
from spice.errors import SpiceError

DEFAULT_MAX_ATTEMPTS = 2
PARALLEL_MAXIM_JUDGES = 2
ANSWER_CHARACTERS = frozenset("YESNO ")
TRAILING_NOISE = string.punctuation + string.whitespace
ALL_MAXIM = "all"
ANY_MAXIM = "any"
META_MAXIMS = frozenset({ALL_MAXIM, ANY_MAXIM})
DEFAULT_PROMPT_LINES = (
    'IFF "{maxim}" AGREES WITH "{statement}": ANSWER ONLY "YES".',
    'IFF "{maxim}" DISAGREES WITH "{statement}": ANSWER ONLY "NO".',
    'IFF "{statement}" AGREES WITH "{maxim}": ANSWER ONLY "YES".',
    'IFF "{statement}" DISAGREES WITH "{maxim}": ANSWER ONLY "NO".',
)
DEFAULT_PROMPT_TEMPLATE = "\n".join(DEFAULT_PROMPT_LINES) + "\n"

JudgeBackend = Callable[[str], str]
SubprocessRunner = Callable[..., "subprocess.CompletedProcess[str]"]

# Built-in maxims keyed by a *bag* of high-signal words that should each
# trigger the maxim. Bags declare every supported spelling explicitly; the hot
# path only tokenizes prose and intersects observed words with the registered
# key set. Each value is a standalone maxim meant to be fed verbatim into a
# verdict, e.g. ``spice maxim agree "$(spice maxim show fallback)" "<text>"``.
BUILTIN_MAXIMS: dict[frozenset[str], str] = {
    frozenset(
        {
            "delay",
            "delayed",
            "delaying",
            "delays",
            "poll",
            "polled",
            "polling",
            "polls",
            "sleep",
            "sleeping",
            "sleeps",
            "slept",
        }
    ): (
        "DO NOT add polling, busy-waits, or retry loops to paper over timing; "
        "react to the real signal or restructure the flow so the wait is "
        "unnecessary."
    ),
    frozenset({"fallback", "fallbacks", "option", "optional", "options"}): (
        "DO NOT add fallbacks or defensive secondary paths; commit to the one "
        "correct path and let it fail loudly when its assumptions break."
    ),
    frozenset({"mode", "modes"}): (
        "DO NOT add modes that split behavior into broad parallel paths; model "
        "the concrete state, capability, or intent and update callers to one "
        "explicit contract."
    ),
    frozenset({"compatibilities", "compatibility", "compatible"}): (
        "DO NOT preserve backwards compatibility; this is unreleased software, "
        "so move to the latest thinking and update every caller instead of "
        "carrying the old contract forward."
    ),
    frozenset(
        {
            "shim",
            "shimmed",
            "shimming",
            "shims",
        }
    ): (
        "DO NOT add shims, adapters, or bridges between an old shape and a new "
        "one; replace the old shape outright and delete it."
    ),
    frozenset({"alias", "aliased", "aliases", "aliasing"}): (
        "DO NOT add aliases that keep an old name alongside a new one; rename "
        "in place and update every reference so only one name survives."
    ),
    frozenset({"legacy", "legacies"}): (
        "DO NOT retain legacy code, dead branches, or commented-out history; "
        "delete it and update to the latest thinking."
    ),
}


def _flatten_bag_keys(bags: dict[frozenset[str], str]) -> dict[str, frozenset[str]]:
    return {key.lower(): bag for bag in bags for key in bag}


_KEY_TO_BAG: dict[str, frozenset[str]] = _flatten_bag_keys(BUILTIN_MAXIMS)
_MAXIM_KEYS = frozenset(_KEY_TO_BAG)
_BAG_ORDER = {bag: index for index, bag in enumerate(BUILTIN_MAXIMS)}
_WORD_REGEX = re.compile(r"(?<![A-Za-z0-9_])[A-Za-z]+(?![A-Za-z0-9_])")


@dataclass(frozen=True)
class MaximVerdict:
    """One resolved adjudication of a statement against a maxim."""

    maxim: str
    statement: str
    prompt: str
    answer: str
    attempts: tuple[str, ...]

    @property
    def agrees(self) -> bool:
        return self.answer == "YES"


def normalize_field(value: str) -> str:
    """Flatten whitespace and drop trailing punctuation so ``value`` reads
    cleanly inside the prompt's double quotes, whatever the source message
    happened to contain."""
    collapsed = " ".join(value.split())
    return collapsed.rstrip(TRAILING_NOISE)


def render_maxim_prompt(
    maxim: str, statement: str, *, template: str = DEFAULT_PROMPT_TEMPLATE
) -> str:
    """Inject ``maxim`` and ``statement`` into the prompt template."""
    normalized_maxim = normalize_field(maxim)
    normalized_statement = normalize_field(statement)
    if template == DEFAULT_PROMPT_TEMPLATE:
        # Shuffle the four equivalent framings so a judge that latches onto
        # line order cannot bias the verdict.
        lines = [
            line.format(maxim=normalized_maxim, statement=normalized_statement)
            for line in DEFAULT_PROMPT_LINES
        ]
        random.shuffle(lines)
        return "\n".join(lines) + "\n"
    try:
        return template.format(maxim=normalized_maxim, statement=normalized_statement)
    except (KeyError, IndexError) as exc:
        raise SpiceError(
            "maxim prompt template may only reference the {maxim} and "
            f"{{statement}} fields; offending placeholder {exc}"
        ) from exc


def parse_yes_no(raw: str) -> str | None:
    """Collapse a raw model reply to ``"YES"``, ``"NO"``, or ``None``.

    Uppercase the reply, drop every character outside ``[YESNO ]``, split on
    spaces, and dedupe the tokens into a set. A clean reply leaves exactly one
    recognized token; anything else is ambiguous and returns ``None``.
    """
    kept = "".join(
        character for character in raw.upper() if character in ANSWER_CHARACTERS
    )
    tokens = {token for token in kept.split() if token}
    if tokens == {"YES"}:
        return "YES"
    if tokens == {"NO"}:
        return "NO"
    return None


def judge_cli_backend(
    prompt: str,
    *,
    judge_bin: str | None = None,
    run: SubprocessRunner = subprocess.run,
) -> str:
    """Send ``prompt`` to the judge binary over stdin and return its stdout."""
    binary = judge_bin or configured_judge_bin()
    try:
        completed = run(
            [binary],
            input=prompt,
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError as exc:
        raise SpiceError(f"could not launch {binary!r}: {exc}") from exc
    if completed.returncode != 0:
        detail = (completed.stderr or "").strip()
        suffix = f": {detail}" if detail else ""
        raise SpiceError(f"{binary} exited with code {completed.returncode}{suffix}")
    return completed.stdout


def evaluate_maxim(
    maxim: str,
    statement: str,
    *,
    template: str = DEFAULT_PROMPT_TEMPLATE,
    backend: JudgeBackend = judge_cli_backend,
    max_attempts: int = DEFAULT_MAX_ATTEMPTS,
) -> MaximVerdict:
    """Adjudicate ``statement`` against ``maxim`` and return the verdict.

    A reply that does not collapse to a single YES/NO triggers a retry, up to
    ``max_attempts`` total invocations of ``backend``.
    """
    attempts: list[str] = []
    prompt = ""
    for _attempt in range(max(1, max_attempts)):
        prompt = render_maxim_prompt(maxim, statement, template=template)
        raw = backend(prompt)
        attempts.append(raw)
        answer = parse_yes_no(raw)
        if answer is not None:
            return MaximVerdict(
                maxim=maxim,
                statement=statement,
                prompt=prompt,
                answer=answer,
                attempts=tuple(attempts),
            )
    raise SpiceError(
        f"judge did not return a single YES/NO after {len(attempts)} "
        f"attempt(s); replies={attempts!r}"
    )


def maxim_agrees(
    maxim: str,
    statement: str,
    *,
    template: str = DEFAULT_PROMPT_TEMPLATE,
    backend: JudgeBackend = judge_cli_backend,
    max_attempts: int = DEFAULT_MAX_ATTEMPTS,
) -> bool:
    """Return whether ``statement`` agrees with ``maxim``."""
    return evaluate_maxim(
        maxim,
        statement,
        template=template,
        backend=backend,
        max_attempts=max_attempts,
    ).agrees


def evaluate_maxim_any_violation(
    maxim: str,
    statement: str,
    *,
    template: str = DEFAULT_PROMPT_TEMPLATE,
    backend: JudgeBackend = judge_cli_backend,
    max_attempts: int = DEFAULT_MAX_ATTEMPTS,
) -> MaximVerdict:
    """Adjudicate with two parallel judges and fail if either disagrees."""
    with ThreadPoolExecutor(max_workers=PARALLEL_MAXIM_JUDGES) as executor:
        futures = [
            executor.submit(
                evaluate_maxim,
                maxim,
                statement,
                template=template,
                backend=backend,
                max_attempts=max_attempts,
            )
            for _ in range(PARALLEL_MAXIM_JUDGES)
        ]
        verdicts = [future.result() for future in futures]
    attempts = [attempt for verdict in verdicts for attempt in verdict.attempts]
    answer = "NO" if any(not verdict.agrees for verdict in verdicts) else "YES"
    return MaximVerdict(
        maxim=maxim,
        statement=statement,
        prompt=verdicts[0].prompt,
        answer=answer,
        attempts=tuple(attempts),
    )


def builtin_maxim_names() -> list[str]:
    """Return every name (across every variation bag) that resolves a maxim."""
    return sorted(_KEY_TO_BAG)


def builtin_maxim(name: str) -> str:
    """Resolve a built-in maxim by short name, case-insensitively.

    Any key in the variation bag works, so ``compatibility`` and
    ``compatible`` both resolve to the same maxim.
    """
    bag = _KEY_TO_BAG.get(name.strip().lower())
    if bag is None:
        known = ", ".join(builtin_maxim_names())
        raise SpiceError(f"unknown maxim {name!r}; built-in maxims are: {known}")
    return BUILTIN_MAXIMS[bag]


def triggered_maxims(statements: Sequence[str]) -> list[frozenset[str]]:
    """Return matched maxim bags, in declared order.

    The scan tokenizes the prose once, then intersects the observed whole
    words with the explicitly registered key set. Variation support belongs
    in the maxim's frozenset bag, not in match-time word mutation.
    """
    words = {
        match.group(0).casefold()
        for statement in statements
        for match in _WORD_REGEX.finditer(statement)
    }
    seen = {_KEY_TO_BAG[key] for key in words & _MAXIM_KEYS}
    return sorted(seen, key=_BAG_ORDER.__getitem__)


def resolve_maxim(maxim: str) -> str:
    """Expand a built-in short name to its maxim text.

    Any key in the variation bag matches (case-insensitive). Any other
    single-word value is rejected, since a real maxim is never one word;
    multi-word values pass through unchanged.
    """
    bag = _KEY_TO_BAG.get(maxim.strip().lower())
    if bag is not None:
        return BUILTIN_MAXIMS[bag]
    if len(maxim.split()) <= 1:
        known = ", ".join(builtin_maxim_names())
        raise SpiceError(
            f"maxim {maxim!r} is a single word but not a known short name; "
            f"pass a full maxim or one of: {known}"
        )
    return maxim

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
from collections.abc import Callable, Mapping, Sequence
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from spice.config import configured_judge_bin
from spice.errors import SpiceError
from spice.paths import repo_root_from_cwd
from spice.repocfg import maxims_table, string_list

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


@dataclass(frozen=True)
class MaximBag:
    name: str
    words: frozenset[str]
    message: str


# Built-in maxims keyed by a stable bag name. Bags declare every supported
# spelling explicitly; the hot path tokenizes prose and matches whole trigger
# keys. Each message is fed verbatim into a verdict, e.g. ``spice maxim agree
# "$(spice maxim show fallback)" "<text>"``.
BUILTIN_MAXIM_BAGS: dict[str, MaximBag] = {
    "polling": MaximBag(
        name="polling",
        words=frozenset(
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
        ),
        message=(
            "😊 Respond to the real event, signal, or completion "
            "notification through a blocking call, watcher, or callback, "
            "or else restructure the flow so nothing remains to be "
            "awaited at all!"
        ),
    ),
    "fallbacks": MaximBag(
        name="fallbacks",
        words=frozenset(
            {
                "fall back",
                "fall backs",
                "fallback",
                "fallbacks",
                "falls back",
                "option",
                "optional",
                "options",
            }
        ),
        message=(
            "😊 Commit to a single deterministic path and let violated "
            "assumptions fail loudly and immediately, reserving an "
            "explicit default or documented resolver order strictly for "
            "what the contract names outright!"
        ),
    ),
    "backwards-compat": MaximBag(
        name="backwards-compat",
        words=frozenset({"compatibilities", "compatibility", "compatible"}),
        message=(
            "😊 Migrate every caller directly to the current shape, "
            "delete the prior one outright, and hold every new addition "
            "to that same shape going forward!"
        ),
    ),
    "shims": MaximBag(
        name="shims",
        words=frozenset(
            {
                "shim",
                "shimmed",
                "shimming",
                "shims",
            }
        ),
        message=(
            "😊 Replace the old shape outright, delete it completely, "
            "and route every caller straight to the new shape's real "
            "interface!"
        ),
    ),
    "aliases": MaximBag(
        name="aliases",
        words=frozenset({"alias", "aliased", "aliases", "aliasing"}),
        message=(
            "😊 Rename in place, update every reference immediately, and "
            "let the changeover happen in one clean move so exactly one "
            "name survives!"
        ),
    ),
    "legacy": MaximBag(
        name="legacy",
        words=frozenset({"legacy", "legacies"}),
        message=(
            "😊 Delete superseded code, dead branches, and commented-out "
            "history outright, trust the current thinking as the "
            "complete record, and bring everything forward to match it!"
        ),
    ),
}

BUILTIN_MAXIMS: dict[frozenset[str], str] = {
    bag.words: bag.message for bag in BUILTIN_MAXIM_BAGS.values()
}


def _flatten_bag_keys(bags: Mapping[str, MaximBag]) -> dict[str, str]:
    lookup: dict[str, str] = {}
    for name, bag in bags.items():
        for key in bag.words:
            owner = lookup.setdefault(key, name)
            if owner != name:
                raise SpiceError(
                    f"maxim trigger key {key!r} appears in both {owner!r} and {name!r}"
                )
    return lookup


_WORD_REGEX = re.compile(r"(?<![A-Za-z0-9_])[A-Za-z]+(?![A-Za-z0-9_])")
_MAXIM_KEY_RE = re.compile(r"^[a-z]+(?: [a-z]+)*$")


def resolved_maxim_bags(repo_root: Path | None = None) -> dict[str, MaximBag]:
    """Return built-in maxim bags merged with tracked repo configuration."""
    root = repo_root if repo_root is not None else repo_root_from_cwd()
    bags = dict(BUILTIN_MAXIM_BAGS)
    if root is None:
        return bags
    for raw_name, raw_config in maxims_table(root).items():
        name = _normalize_bag_name(raw_name)
        if not isinstance(raw_config, dict):
            raise SpiceError(f"[tool.spice.maxims.{name}] must be a table")
        base = bags.get(name)
        bags[name] = MaximBag(
            name=name,
            words=_configured_words(raw_config, base, name),
            message=_configured_message(raw_config, base, name),
        )
    _flatten_bag_keys(bags)
    return bags


def _normalize_bag_name(raw: Any) -> str:
    name = str(raw or "").strip().casefold()
    if not name:
        raise SpiceError("[tool.spice.maxims] bag names must be non-empty")
    return name


def _configured_words(
    raw_config: Mapping[str, Any], base: MaximBag | None, name: str
) -> frozenset[str]:
    if "words" not in raw_config:
        if base is None:
            raise SpiceError(f"[tool.spice.maxims.{name}] requires words")
        return base.words
    words = []
    for word in string_list(raw_config.get("words")):
        normalized = _normalize_trigger_key(word)
        if not _MAXIM_KEY_RE.fullmatch(normalized):
            raise SpiceError(
                f"[tool.spice.maxims.{name}] words must be alphabetic phrases; "
                f"got {word!r}"
            )
        if normalized not in words:
            words.append(normalized)
    if not words:
        raise SpiceError(f"[tool.spice.maxims.{name}] words must be non-empty")
    return frozenset(words)


def _normalize_trigger_key(raw: Any) -> str:
    return " ".join(str(raw or "").casefold().split())


def _normalize_trigger_selector(raw: str) -> str:
    normalized = _normalize_trigger_key(raw)
    return normalized if _MAXIM_KEY_RE.fullmatch(normalized) else raw.strip().casefold()


def _configured_message(
    raw_config: Mapping[str, Any], base: MaximBag | None, name: str
) -> str:
    raw = raw_config.get("message")
    if raw is None:
        if base is None:
            raise SpiceError(f"[tool.spice.maxims.{name}] requires message")
        return base.message
    message = str(raw or "").strip()
    if not message:
        raise SpiceError(f"[tool.spice.maxims.{name}] message must be non-empty")
    return message


def _resolved_lookup(
    repo_root: Path | None = None,
) -> tuple[dict[str, MaximBag], dict[str, str], dict[str, int]]:
    bags = resolved_maxim_bags(repo_root)
    key_to_name = _flatten_bag_keys(bags)
    bag_order = {name: index for index, name in enumerate(bags)}
    return bags, key_to_name, bag_order


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


def maxim_names(repo_root: Path | None = None) -> list[str]:
    """Return every stable name and trigger word that resolves a maxim."""
    bags, key_to_name, _bag_order = _resolved_lookup(repo_root)
    return sorted(set(bags) | set(key_to_name))


def configured_maxim(name: str, *, repo_root: Path | None = None) -> str:
    """Resolve a configured maxim by stable name or trigger word.

    Any trigger word in the variation bag works, so ``compatibility`` and
    ``compatible`` both resolve to the same built-in maxim by default.
    """
    bags, key_to_name, _bag_order = _resolved_lookup(repo_root)
    selector = name.strip().casefold()
    bag = bags.get(selector)
    if bag is not None:
        return bag.message
    bag_name = key_to_name.get(_normalize_trigger_selector(name))
    if bag_name is None:
        known = ", ".join(maxim_names(repo_root))
        raise SpiceError(f"unknown maxim {name!r}; configured maxims are: {known}")
    return bags[bag_name].message


def builtin_maxim_names() -> list[str]:
    """Return every built-in/configured name that resolves a maxim."""
    return maxim_names()


def builtin_maxim(name: str) -> str:
    """Resolve a built-in/configured maxim by short name."""
    return configured_maxim(name)


def triggered_maxims(
    statements: Sequence[str], *, repo_root: Path | None = None
) -> list[MaximBag]:
    """Return matched maxim bags, in declared order.

    The scan tokenizes prose into alphabetic words, then matches explicitly
    registered single-word or phrase keys. Variation support belongs in the
    maxim's frozenset bag, not in match-time word mutation.
    """
    bags, key_to_name, bag_order = _resolved_lookup(repo_root)
    seen: set[str] = set()
    trigger_parts = {key: tuple(key.split()) for key in key_to_name}
    for statement in statements:
        words = [match.group(0).casefold() for match in _WORD_REGEX.finditer(statement)]
        if not words:
            continue
        word_set = set(words)
        for key, parts in trigger_parts.items():
            if len(parts) == 1:
                if parts[0] in word_set:
                    seen.add(key_to_name[key])
                continue
            if _contains_word_phrase(words, parts):
                seen.add(key_to_name[key])
    return [bags[name] for name in sorted(seen, key=bag_order.__getitem__)]


def _contains_word_phrase(words: Sequence[str], phrase: tuple[str, ...]) -> bool:
    size = len(phrase)
    if size > len(words):
        return False
    return any(
        tuple(words[index : index + size]) == phrase
        for index in range(len(words) - size + 1)
    )


def resolve_maxim(maxim: str, *, repo_root: Path | None = None) -> str:
    """Expand a configured short name to its maxim text.

    Any key in the variation bag matches (case-insensitive). Any other
    single-word value is rejected, since a real maxim is never one word;
    multi-word values pass through unchanged.
    """
    bags, key_to_name, _bag_order = _resolved_lookup(repo_root)
    selector = maxim.strip().casefold()
    bag = bags.get(selector)
    if bag is not None:
        return bag.message
    bag_name = key_to_name.get(_normalize_trigger_selector(maxim))
    if bag_name is not None:
        return bags[bag_name].message
    if len(maxim.split()) <= 1:
        known = ", ".join(maxim_names(repo_root))
        raise SpiceError(
            f"maxim {maxim!r} is a single word but not a known short name; "
            f"pass a full maxim or one of: {known}"
        )
    return maxim

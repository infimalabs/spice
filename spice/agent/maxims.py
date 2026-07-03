"""Judge whether a statement agrees with a maxim using a local LLM.

The primitive is deliberately small: render a YES/NO adjudication prompt from
a ``maxim`` and a ``statement``, ask a local model (the configured judge
binary, ``afm-cli`` by default), and collapse the reply to a single boolean.
The prompt is a ``str.format`` template exposing two fields, ``{maxim}`` and
``{statement}``, so callers can supply a different framing without touching
the parsing or backend wiring.
"""

from __future__ import annotations

import json
import random
import re
import string
import subprocess
from collections import Counter
from collections.abc import Callable, Mapping, Sequence
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from spice.agent.driver import driver_choices
from spice.config import configured_judge_bin
from spice.errors import SpiceError
from spice.flexstate import load_sticky_items, save_sticky_items
from spice.mail.ackstate import AckStateRecord, ack_state_records
from spice.mail.inbox import parse_inbox_payload
from spice.paths import repo_root_from_cwd
from spice.repocfg import maxims_table, string_list

DEFAULT_MAX_ATTEMPTS = 2
PARALLEL_MAXIM_JUDGES = 2
ANSWER_CHARACTERS = frozenset("YESNO ")
TRAILING_NOISE = string.punctuation + string.whitespace
ALL_MAXIM = "all"
ANY_MAXIM = "any"
META_MAXIMS = frozenset({ALL_MAXIM, ANY_MAXIM})
DISABLED_MAXIM_BAGS_GIT_PATH = "spice/disabled-maxim-bags.json"
DISABLED_MAXIM_BAGS_KEY = "disabled_bags"
MAXIM_PROPOSAL_MIN_RECURRENCE = 2
MAXIM_PROPOSAL_DRAFT_MAX_WORDS = 8
MAXIM_PROPOSAL_TASK_CREATION_SURFACE = "maxim_proposal"
MAXIM_PROPOSAL_TASK_TAG = "maxim_proposal"
_TOML_BARE_KEY_RE = re.compile(r"^[A-Za-z0-9_-]+$")
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
    drivers: frozenset[str] = field(default_factory=lambda: frozenset(driver_choices()))


@dataclass(frozen=True)
class MaximProposalEvidence:
    field: str
    text: str


@dataclass(frozen=True)
class MaximProposalSourceRecord:
    key: str
    inbox_name: str
    steering_text: str
    ack_text: str
    ack_content: str
    disposition: str
    archived_at: float
    evidence: tuple[MaximProposalEvidence, ...]


@dataclass(frozen=True)
class MaximProposalDispositionCount:
    disposition: str
    count: int


@dataclass(frozen=True)
class MaximProposalTheme:
    name: str
    recurring_terms: tuple[str, ...]
    evidence_count: int
    source_keys: tuple[str, ...]
    dispositions: tuple[MaximProposalDispositionCount, ...]
    evidence: tuple[MaximProposalEvidence, ...]


@dataclass(frozen=True)
class MaximProposalDraft:
    bag_name: str
    words: tuple[str, ...]
    message: str
    theme_name: str
    recurring_terms: tuple[str, ...]
    evidence_count: int
    source_keys: tuple[str, ...]
    dispositions: tuple[MaximProposalDispositionCount, ...]
    evidence: tuple[MaximProposalEvidence, ...]


@dataclass(frozen=True)
class FiledMaximProposalTask:
    handle: str
    bag_name: str
    project: str


@dataclass(frozen=True)
class _PreparedProposalSource:
    record: MaximProposalSourceRecord
    terms: frozenset[str]


def maxim_proposal_source_records(
    repo_root: str | Path,
) -> tuple[MaximProposalSourceRecord, ...]:
    """Return normalized ACK ledger records useful for maxim proposal mining."""
    records: list[MaximProposalSourceRecord] = []
    for record in ack_state_records(repo_root):
        source = _maxim_proposal_source_record(record)
        if source is not None:
            records.append(source)
    return tuple(records)


def maxim_proposal_themes(
    records: Sequence[MaximProposalSourceRecord],
    *,
    min_recurrence: int = MAXIM_PROPOSAL_MIN_RECURRENCE,
) -> tuple[MaximProposalTheme, ...]:
    """Cluster recurring ACK correction sources into human-reviewed themes."""
    threshold = max(2, int(min_recurrence))
    prepared = [
        _PreparedProposalSource(record=record, terms=terms)
        for record in records
        if (terms := _maxim_proposal_terms(record))
    ]
    clusters = _maxim_proposal_clusters(prepared)
    themes = [
        theme
        for cluster in clusters
        if (theme := _maxim_proposal_theme(cluster, threshold)) is not None
    ]
    return tuple(
        sorted(
            themes,
            key=lambda theme: (
                -theme.evidence_count,
                theme.name,
                theme.source_keys,
            ),
        )
    )


def maxim_proposal_drafts(
    themes: Sequence[MaximProposalTheme],
    *,
    existing_bags: Mapping[str, MaximBag] | None = None,
) -> tuple[MaximProposalDraft, ...]:
    """Return mergeable TOML draft data for human-reviewed maxim proposals."""
    trigger_owners = _flatten_bag_keys(existing_bags or BUILTIN_MAXIM_BAGS)
    drafts: list[MaximProposalDraft] = []
    for theme in themes:
        candidate_words = _maxim_proposal_draft_words(theme.recurring_terms)
        if not candidate_words:
            continue
        bag_name = _maxim_proposal_draft_bag_name(candidate_words, trigger_owners)
        words = tuple(
            word
            for word in candidate_words
            if (owner := trigger_owners.get(word)) is None or owner == bag_name
        )
        if not words:
            continue
        drafts.append(
            MaximProposalDraft(
                bag_name=bag_name,
                words=words,
                message=_maxim_proposal_draft_message(theme, words),
                theme_name=theme.name,
                recurring_terms=theme.recurring_terms,
                evidence_count=theme.evidence_count,
                source_keys=theme.source_keys,
                dispositions=theme.dispositions,
                evidence=theme.evidence,
            )
        )
    return tuple(drafts)


def file_maxim_proposal_tasks(
    drafts: Sequence[MaximProposalDraft],
    *,
    actor_override: str | None = None,
) -> tuple[FiledMaximProposalTask, ...]:
    """File draft maxims as deferred hidden triage tasks, never as config edits."""
    from spice.tasks import config as task_config
    from spice.tasks import create

    filed: list[FiledMaximProposalTask] = []
    existing_incepted: set[str] = set()
    for draft in drafts:
        handle = create.add_one(
            title=_maxim_proposal_task_title(draft, limit=create.TASK_TITLE_LIMIT),
            description=maxim_proposal_task_description(draft),
            project=task_config.MAXIM_PROPOSAL_PROJECT,
            priority="medium",
            flow=None,
            tags=[MAXIM_PROPOSAL_TASK_TAG],
            after=[],
            acceptance=[
                (
                    "Human triage decides whether to merge, revise, or reject "
                    "this proposed maxim; filing this task must not modify "
                    "pyproject.toml or install maxim config."
                )
            ],
            wait=None,
            claim=False,
            deferred=True,
            existing=existing_incepted,
            system_project=True,
            actor_override=actor_override,
            creation_surface=MAXIM_PROPOSAL_TASK_CREATION_SURFACE,
        )
        filed.append(
            FiledMaximProposalTask(
                handle=handle,
                bag_name=draft.bag_name,
                project=task_config.MAXIM_PROPOSAL_PROJECT,
            )
        )
    return tuple(filed)


def maxim_proposal_task_description(draft: MaximProposalDraft) -> str:
    evidence_rows = [
        f"- {item.field}: {_compact_proposal_description_text(item.text)}"
        for item in draft.evidence
    ]
    if not evidence_rows:
        evidence_rows = ["- none"]
    return "\n".join(
        [
            "Mergeable maxim stanza:",
            "",
            "```toml",
            render_maxim_proposal_draft_stanza(draft),
            "```",
            "",
            "Evidence:",
            f"- theme: {draft.theme_name}",
            f"- evidence_count: {draft.evidence_count}",
            f"- source_keys: {', '.join(draft.source_keys) or '-'}",
            f"- dispositions: {_format_proposal_dispositions(draft)}",
            *evidence_rows,
        ]
    )


def render_maxim_proposal_draft_stanza(draft: MaximProposalDraft) -> str:
    return "\n".join(
        [
            f"[tool.spice.maxims.{_render_toml_key(draft.bag_name)}]",
            f"words = {_render_toml_string_array(draft.words)}",
            f"message = {_render_toml_string(draft.message)}",
        ]
    )


def _maxim_proposal_source_record(
    record: AckStateRecord,
) -> MaximProposalSourceRecord | None:
    steering_text = _normalize_proposal_text(parse_inbox_payload(record.text).body)
    ack_text = _normalize_proposal_text(record.ack_text)
    ack_content = _normalize_proposal_text(record.ack_content)
    evidence = tuple(
        item
        for item in (
            _maxim_proposal_evidence("steering_text", steering_text),
            _maxim_proposal_evidence("ack_text", ack_text),
            _maxim_proposal_evidence("ack_content", ack_content),
        )
        if item is not None
    )
    if not evidence:
        return None
    return MaximProposalSourceRecord(
        key=record.key,
        inbox_name=record.inbox_name,
        steering_text=steering_text,
        ack_text=ack_text,
        ack_content=ack_content,
        disposition=record.disposition,
        archived_at=record.archived_at,
        evidence=evidence,
    )


def _maxim_proposal_evidence(field: str, text: str) -> MaximProposalEvidence | None:
    if not text:
        return None
    return MaximProposalEvidence(field=field, text=text)


def _normalize_proposal_text(value: str) -> str:
    return " ".join(str(value or "").split())


_MAXIM_PROPOSAL_TOKEN_RE = re.compile(r"[a-z][a-z0-9]*")
_MAXIM_PROPOSAL_STOP_WORDS = frozenset(
    {
        "about",
        "again",
        "because",
        "being",
        "cannot",
        "capture",
        "captured",
        "correction",
        "could",
        "done",
        "from",
        "have",
        "into",
        "must",
        "nack",
        "need",
        "needs",
        "operator",
        "please",
        "should",
        "that",
        "their",
        "there",
        "these",
        "this",
        "those",
        "through",
        "with",
        "would",
    }
)
_MAXIM_PROPOSAL_DRAFT_STOP_WORDS = _MAXIM_PROPOSAL_STOP_WORDS | frozenset(
    {
        "acceptance",
        "accepted",
        "acked",
        "agent",
        "agents",
        "allocator",
        "briefing",
        "codex",
        "command",
        "commands",
        "evidence",
        "guidance",
        "inbox",
        "maxim",
        "message",
        "project",
        "refused",
        "session",
        "source",
        "spice",
        "status",
        "task",
        "tests",
        "then",
        "validation",
        "worktree",
    }
)
_MAXIM_PROPOSAL_DRAFT_WORD_RE = re.compile(r"[a-z]+")
_ACK_MESSAGE_PREFIX_RE = re.compile(r"^(?:ACK|NACK)\s+\S+:\s*", re.IGNORECASE)
_MAXIM_PROPOSAL_IMPERATIVE_STARTS = (
    "avoid ",
    "cite ",
    "commit ",
    "delete ",
    "do not ",
    "don't ",
    "drive ",
    "fail ",
    "hold ",
    "keep ",
    "let ",
    "migrate ",
    "prefer ",
    "preserve ",
    "react ",
    "remove ",
    "rename ",
    "replace ",
    "require ",
    "respond ",
    "route ",
    "run ",
    "treat ",
    "update ",
    "use ",
)


def _maxim_proposal_terms(record: MaximProposalSourceRecord) -> frozenset[str]:
    text = " ".join(item.text for item in record.evidence)
    tokens = []
    for raw in _MAXIM_PROPOSAL_TOKEN_RE.findall(text.casefold().replace("-", " ")):
        token = raw.strip()
        if len(token) < 4:
            continue
        if token in _MAXIM_PROPOSAL_STOP_WORDS:
            continue
        if _looks_like_ack_key_fragment(token):
            continue
        tokens.append(token)
    return frozenset(tokens)


def _looks_like_ack_key_fragment(token: str) -> bool:
    return token.startswith("t") and token.endswith("z") and token[1:-1].isdigit()


def _maxim_proposal_draft_words(candidates: Sequence[str]) -> tuple[str, ...]:
    words: list[str] = []
    for candidate in candidates:
        word = _normalize_proposal_draft_trigger(candidate)
        if word is None:
            continue
        if word in _MAXIM_PROPOSAL_DRAFT_STOP_WORDS:
            continue
        if word not in words:
            words.append(word)
        if len(words) >= MAXIM_PROPOSAL_DRAFT_MAX_WORDS:
            break
    return tuple(words)


def _normalize_proposal_draft_trigger(raw: Any) -> str | None:
    text = str(raw or "").casefold()
    if any(character.isdigit() for character in text):
        return None
    normalized = _normalize_trigger_key(
        " ".join(_MAXIM_PROPOSAL_DRAFT_WORD_RE.findall(text))
    )
    if not normalized:
        return None
    if not _MAXIM_KEY_RE.fullmatch(normalized):
        return None
    return normalized


def _maxim_proposal_draft_bag_name(
    words: Sequence[str], trigger_owners: Mapping[str, str]
) -> str:
    owner_counts = Counter(
        owner for word in words if (owner := trigger_owners.get(word)) is not None
    )
    if owner_counts:
        return min(owner_counts, key=lambda name: (-owner_counts[name], name))
    name_terms: list[str] = []
    for word in words:
        name_terms.extend(word.split())
        if len(name_terms) >= 4:
            break
    return "proposal-" + "-".join(name_terms[:4])


def _maxim_proposal_draft_message(
    theme: MaximProposalTheme, words: Sequence[str]
) -> str:
    preferred = tuple(
        item for item in theme.evidence if item.field == "steering_text"
    ) + tuple(item for item in theme.evidence if item.field != "steering_text")
    for item in preferred:
        message = _clean_proposal_draft_message(item.text)
        if message and _looks_imperative(message):
            return _ensure_terminal_punctuation(message)
    return (
        "Keep "
        + _format_proposal_word_list(words)
        + " guidance broad, portable, and immediately actionable across contexts."
    )


def _clean_proposal_draft_message(raw: str) -> str:
    message = _normalize_proposal_text(raw)
    message = _ACK_MESSAGE_PREFIX_RE.sub("", message)
    return message.removeprefix("[MAXIM] ").strip()


def _looks_imperative(message: str) -> bool:
    normalized = re.sub(r"^[^A-Za-z]+", "", message).casefold()
    return normalized.startswith(_MAXIM_PROPOSAL_IMPERATIVE_STARTS)


def _ensure_terminal_punctuation(message: str) -> str:
    if message[-1:] in {".", "!", "?"}:
        return message
    return message + "."


def _format_proposal_word_list(words: Sequence[str]) -> str:
    if len(words) == 1:
        return words[0]
    if len(words) == 2:
        return f"{words[0]} and {words[1]}"
    return ", ".join(words[:-1]) + f", and {words[-1]}"


def _maxim_proposal_task_title(draft: MaximProposalDraft, *, limit: int) -> str:
    title = f"Triage maxim proposal: {draft.bag_name}"
    if len(title) <= limit:
        return title
    return title[:limit].rstrip()


def _format_proposal_dispositions(draft: MaximProposalDraft) -> str:
    return ",".join(f"{item.disposition}={item.count}" for item in draft.dispositions)


def _compact_proposal_description_text(value: str) -> str:
    return " ".join(str(value or "").split())


def _render_toml_key(key: str) -> str:
    return key if _TOML_BARE_KEY_RE.fullmatch(key) else _render_toml_string(key)


def _render_toml_string_array(values: tuple[str, ...]) -> str:
    return "[" + ", ".join(_render_toml_string(value) for value in values) + "]"


def _render_toml_string(value: str) -> str:
    return json.dumps(value, ensure_ascii=False)


def _maxim_proposal_clusters(
    sources: Sequence[_PreparedProposalSource],
) -> list[list[_PreparedProposalSource]]:
    clusters: list[list[_PreparedProposalSource]] = []
    for source in sources:
        matches = [
            index
            for index, cluster in enumerate(clusters)
            if any(
                _maxim_proposal_terms_close(source.terms, item.terms)
                for item in cluster
            )
        ]
        if not matches:
            clusters.append([source])
            continue
        first = matches[0]
        clusters[first].append(source)
        for index in reversed(matches[1:]):
            clusters[first].extend(clusters.pop(index))
    return clusters


def _maxim_proposal_terms_close(left: frozenset[str], right: frozenset[str]) -> bool:
    shared = left & right
    if len(shared) < 2:
        return False
    return len(shared) / max(1, min(len(left), len(right))) >= 0.5


def _maxim_proposal_theme(
    cluster: Sequence[_PreparedProposalSource], threshold: int
) -> MaximProposalTheme | None:
    if len(cluster) < threshold:
        return None
    counts = Counter(term for source in cluster for term in source.terms)
    recurring_terms = tuple(
        sorted(term for term, count in counts.items() if count >= threshold)
    )
    if not recurring_terms:
        return None
    records = tuple(source.record for source in cluster)
    disposition_counts = Counter(record.disposition for record in records)
    return MaximProposalTheme(
        name="/".join(recurring_terms[:4]),
        recurring_terms=recurring_terms,
        evidence_count=len(records),
        source_keys=tuple(record.key for record in records),
        dispositions=tuple(
            MaximProposalDispositionCount(disposition=disposition, count=count)
            for disposition, count in sorted(disposition_counts.items())
        ),
        evidence=tuple(item for record in records for item in record.evidence),
    )


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
    """Return active maxim bags after config, driver scopes, and local disables."""
    root = repo_root if repo_root is not None else repo_root_from_cwd()
    bags = _configured_maxim_bags(root)
    if root is None:
        return bags
    disabled = _load_disabled_maxim_bag_names(root)
    _validate_disabled_maxim_bag_names(disabled, bags)
    return {name: bag for name, bag in bags.items() if name not in disabled}


def disabled_maxim_bag_names(repo_root: Path | None = None) -> frozenset[str]:
    root = _require_maxim_repo_root(repo_root)
    names = _load_disabled_maxim_bag_names(root)
    _validate_disabled_maxim_bag_names(names, _configured_maxim_bags(root))
    return frozenset(names)


def set_maxim_bag_disabled(
    name: str, *, disabled: bool, repo_root: Path | None = None
) -> frozenset[str]:
    root = _require_maxim_repo_root(repo_root)
    configured = _configured_maxim_bags(root)
    normalized = _normalize_bag_name(name)
    if normalized not in configured:
        expected = ", ".join(sorted(configured))
        raise SpiceError(
            f"unknown maxim bag {name!r}; configured maxim bags are: {expected}"
        )
    names = set(_load_disabled_maxim_bag_names(root))
    if disabled:
        names.add(normalized)
    else:
        names.discard(normalized)
    _save_disabled_maxim_bag_names(root, names)
    return frozenset(names)


def _configured_maxim_bags(root: Path | None) -> dict[str, MaximBag]:
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
            drivers=_configured_drivers(raw_config, name),
        )
    _flatten_bag_keys(bags)
    return bags


def _require_maxim_repo_root(repo_root: Path | None = None) -> Path:
    root = repo_root if repo_root is not None else repo_root_from_cwd()
    if root is None:
        raise SpiceError("not inside a git worktree")
    return root


def _load_disabled_maxim_bag_names(root: Path) -> set[str]:
    return load_sticky_items(
        root=root,
        state_path=None,
        git_path=DISABLED_MAXIM_BAGS_GIT_PATH,
        entries_key=DISABLED_MAXIM_BAGS_KEY,
        decode=_decode_disabled_maxim_bag_name,
    )


def _save_disabled_maxim_bag_names(root: Path, names: set[str]) -> None:
    save_sticky_items(
        names,
        root=root,
        state_path=None,
        git_path=DISABLED_MAXIM_BAGS_GIT_PATH,
        entries_key=DISABLED_MAXIM_BAGS_KEY,
        encode=str,
    )


def _decode_disabled_maxim_bag_name(raw: Any) -> str | None:
    if not isinstance(raw, str):
        return None
    name = _normalize_bag_name(raw)
    return name or None


def _validate_disabled_maxim_bag_names(
    names: set[str], configured: Mapping[str, MaximBag]
) -> None:
    unknown = sorted(name for name in names if name not in configured)
    if unknown:
        expected = ", ".join(sorted(configured))
        raise SpiceError(
            "disabled maxim bag state references unknown bag(s) "
            f"{', '.join(repr(name) for name in unknown)}; "
            f"configured maxim bags are: {expected}"
        )


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


def _configured_drivers(raw_config: Mapping[str, Any], name: str) -> frozenset[str]:
    if "drivers" not in raw_config:
        return _known_driver_names()
    known = _known_driver_names()
    configured: list[str] = []
    for raw_driver in string_list(raw_config.get("drivers")):
        driver = raw_driver.casefold()
        if driver not in known:
            expected = ", ".join(sorted(known))
            raise SpiceError(
                f"[tool.spice.maxims.{name}] drivers must be known agent "
                f"drivers; got {raw_driver!r}; expected one of: {expected}"
            )
        if driver not in configured:
            configured.append(driver)
    if not configured:
        raise SpiceError(f"[tool.spice.maxims.{name}] drivers must be non-empty")
    return frozenset(configured)


def _known_driver_names() -> frozenset[str]:
    return frozenset(driver_choices())


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
    statements: Sequence[str],
    *,
    repo_root: Path | None = None,
    driver_name: str | None = None,
) -> list[MaximBag]:
    """Return matched maxim bags, in declared order.

    The scan tokenizes prose into alphabetic words, then matches explicitly
    registered single-word or phrase keys. Variation support belongs in the
    maxim's frozenset bag, not in match-time word mutation.
    """
    bags, key_to_name, bag_order = _resolved_lookup(repo_root)
    driver_scope = _normalized_driver_scope_name(driver_name)
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
    return [
        bag
        for bag in (bags[name] for name in sorted(seen, key=bag_order.__getitem__))
        if _maxim_bag_matches_driver(bag, driver_scope)
    ]


def _normalized_driver_scope_name(driver_name: str | None) -> str:
    if driver_name is None:
        return ""
    driver = str(driver_name or "").strip().casefold()
    if not driver:
        return ""
    known = _known_driver_names()
    if driver not in known:
        expected = ", ".join(sorted(known))
        raise SpiceError(
            f"maxim driver scope {driver_name!r} must be one of: {expected}"
        )
    return driver


def _maxim_bag_matches_driver(bag: MaximBag, driver_name: str) -> bool:
    return not driver_name or driver_name in bag.drivers


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

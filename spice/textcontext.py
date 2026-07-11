"""Small clause-local prose context helpers shared by text scanners."""

from __future__ import annotations

CONTEXT_BREAK_CHARS = "\r\n.!?;"
CONTEXT_WORD_EXTRA_CHARS = frozenset({"'", "-"})
CONTEXT_WINDOW = 6
NEGATION_WORDS = frozenset(
    {
        "can't",
        "cannot",
        "cant",
        "not",
        "refuse",
        "refused",
        "refuses",
        "refusing",
        "will-not",
        "won't",
        "wont",
    }
)
NEGATION_PHRASES = (
    ("instead", "of"),
    ("instead-of",),
    ("refuse", "to"),
    ("refused", "to"),
    ("refuses", "to"),
    ("refusing", "to"),
)
TURNING_WORDS = frozenset({"but", "hence", "so", "therefore", "thus"})


def has_explicit_negation_before(text: str, token_pos: int) -> bool:
    """Return whether the token is explicitly negated in its recent clause."""
    words = clause_prefix_words(text, token_pos)
    recent = words_after_last_turn(words)[-CONTEXT_WINDOW:]
    return bool(NEGATION_WORDS & set(recent)) or contains_phrase(
        recent, NEGATION_PHRASES
    )


def clause_prefix_words(text: str, token_pos: int) -> tuple[str, ...]:
    start = token_pos
    while start > 0 and text[start - 1] not in CONTEXT_BREAK_CHARS:
        start -= 1
    words: list[str] = []
    cursor = start
    while cursor < token_pos:
        char = text[cursor]
        if char.isalnum():
            word_start = cursor
            cursor += 1
            while cursor < token_pos and (
                text[cursor].isalnum() or text[cursor] in CONTEXT_WORD_EXTRA_CHARS
            ):
                cursor += 1
            words.append(text[word_start:cursor].lower())
            continue
        cursor += 1
    return tuple(words)


def words_after_last_turn(words: tuple[str, ...]) -> tuple[str, ...]:
    for index in range(len(words) - 1, -1, -1):
        if words[index] in TURNING_WORDS:
            return words[index + 1 :]
    return words


def contains_phrase(
    words: tuple[str, ...], phrases: tuple[tuple[str, ...], ...]
) -> bool:
    for phrase in phrases:
        size = len(phrase)
        if size > len(words):
            continue
        for index in range(0, len(words) - size + 1):
            if words[index : index + size] == phrase:
                return True
    return False

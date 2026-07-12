"""Fuzz and robustness laws: determinism, never-raise, deep structure, encodings.

Mirrors the gauntlet's fuzz and robustness suites. Seeded documents assembled
from hazardous line shapes parse deterministically and never raise; every
accepted document is one weakly-connected acyclic graph that upholds the
round-trip law; and deep dependency chains, deep nesting, hostile lines, a byte
order mark, and CRLF newlines all parse to their expected shapes.
"""

from __future__ import annotations

import random
import time

from spice.tasks.markdown.classifier import parse
from spice.tasks.markdown.dialect import Doc, graph_signature
from spice.tasks.markdown.ledger import export_document

# Seeded documents per fuzz run; every seed replays byte-for-byte from its index.
FUZZ_ROUNDS = 2000
# A dependency chain this deep must parse and export without a recursion error.
DEPTH_FLOOR = 3000
# A containment stack this deep must nest as bullets without losing a rung.
NEST_DEPTH = 1200
# A single label line padded this wide must still match in linear time.
LABEL_PAD_WIDTH = 50000
LINEAR_TIME_BUDGET_SECONDS = 1.0

# The hazardous line shapes a fuzz document draws from: headings at every level,
# horizontal rules and setext underlines, bullet and ordered markers, checkbox
# chores, field labels in every synonym and emphasis, dependency lines, prose
# that apes structure, links, quotes, tables, fences, comments, escapes, and the
# reserved synthetic-root title. ``{n}`` expands to a small digit per draw.
FUZZ_LINES = [
    "# Alpha section",
    "## Beta section",
    "### Acceptance criteria",
    "#### Delta section",
    "##### Epsilon section",
    "###### Zeta section",
    "### Dependencies",
    "Setext title",
    "====",
    "----",
    "- - -",
    "  - - -",
    "---",
    "...",
    "- bullet {n}",
    "* star bullet {n}",
    "1. first step {n}",
    "1) paren step {n}",
    "7. seventh step {n}",
    "1985. year line",
    "  - nested bullet {n}",
    "    - deep bullet {n}",
    "      code: not a task",
    "\t- tab bullet {n}",
    "- [ ] unchecked chore {n}",
    "- [x] checked chore {n}",
    "- [x] Priority: high",
    "- [ ] Acceptance: checkbox criterion {n}",
    "Acceptance: criterion {n}",
    "Acceptance:",
    "**Acceptance:**",
    "*Acceptance:*",
    "Done when: it works {n}",
    "- Acceptance: bullet criterion {n}",
    "After: alpha-section",
    "After: Alpha Section",
    "After: missing-target-{n}",
    "After: that sentence wraps, badly.",
    "Priority: high",
    "Priority: ASAP",
    "**Priority:** high",
    "__Tags__: em, phasis",
    "Flow: design, todo",
    "Due: 2026-09-01",
    "Deadline: 2026-09-0{n}",
    "Labels: fuzzy gauntlet",
    "Tags: fuzz, gauntlet",
    "plain prose line {n}",
    "prose with [a link](https://example.com/{n}) inside",
    "[a bare link](https://example.com/{n})",
    "\\[escaped link](https://example.com/{n})",
    "[ref]: https://example.com/ref{n}",
    "> a quoted operator note {n}",
    "| col | val {n} |",
    "```",
    "````",
    "    ```",
    "~~~",
    "<!-- a comment line {n} -->",
    "<!-- inline comment --> with trailing prose {n}",
    "<!--",
    "-->",
    "**Phase {n}**",
    "\\- escaped dash",
    "12\\. escaped year",
    "After\\: escaped field",
    "- Document root",
    "",
    "",
]

# A document whose body is nothing but hazardous shapes yet still parses clean:
# a rollup over a leaf that carries every field, prose lines escaped so a dash,
# a number, and a colon stay prose, an inline link, a following leaf that
# depends on the first, a quoted note, a fenced pseudo-bullet, and a comment.
HOSTILE_DOCUMENT = (
    "# Hostile intake\n\n"
    "- Survive the hazardous lines\n"
    "  Priority: high\n"
    "  Flow: todo, verify\n"
    "  \\- this dash is prose, not a bullet\n"
    "  12\\. this number is prose, not an ordered item\n"
    "  After\\: this colon is prose, not a field\n"
    "  see [a link](https://example.com/x) inline\n\n"
    "- Depend on the first via an After line\n"
    "  After: survive-the-hazardous-lines\n\n"
    "> a quoted operator note survives as an annotation\n\n"
    "```\n- a fenced bullet is not a task\n```\n"
    "<!-- a comment line is dropped -->\n"
)


def fuzz_doc(rng: random.Random) -> str:
    """Assemble one seeded document from between one and thirty-nine lines."""
    lines = [
        rng.choice(FUZZ_LINES).replace("{n}", str(rng.randrange(0, 5)))
        for _ in range(rng.randrange(1, 40))
    ]
    return "\n".join(lines) + "\n"


def _fingerprint(document: Doc) -> object:
    """A crash-safe determinism key over every field a re-parse must reproduce.

    Unlike ``graph_signature`` this never sorts nodes by slug, so it stays
    defined on refused documents -- whose duplicate titles share one slug -- and
    still proves two parses of one text agree down to refusals and warnings.
    """
    return (
        tuple(
            (
                node.slug,
                node.title,
                node.kind,
                node.parent,
                node.level,
                node.priority,
                tuple(node.flow or ()),
                node.due,
                tuple(node.tags),
                tuple(node.acceptance),
                node.description(),
                tuple(block.rstrip() for block in node.annotations),
            )
            for node in document.nodes
        ),
        tuple(document.edges),
        tuple(document.refusals),
        tuple(document.warnings),
    )


def _assert_one_acyclic_graph(document: Doc, seed: int) -> None:
    """Absent a refusal the graph is one weakly-connected component, acyclic.

    Connectivity runs over every edge because a synthetic ``document`` root
    reaches its parentless top-level nodes by ``after`` edges, not containment,
    so a parent-chain walk alone would strand them. A Kahn topological pass then
    proves the whole directed graph -- containment nesting and dependency edges
    together -- carries no cycle.
    """
    node_count = len(document.nodes)
    assert 0 <= document.root < node_count, f"seed {seed}: exactly one root"
    edges = [(source, target) for source, target, _kind in document.edges]

    undirected: dict[int, list[int]] = {index: [] for index in range(node_count)}
    for source, target in edges:
        undirected[source].append(target)
        undirected[target].append(source)
    reached = {document.root}
    frontier = [document.root]
    while frontier:
        for neighbour in undirected[frontier.pop()]:
            if neighbour not in reached:
                reached.add(neighbour)
                frontier.append(neighbour)
    assert len(reached) == node_count, f"seed {seed}: one weakly connected component"

    indegree = {index: 0 for index in range(node_count)}
    outgoing: dict[int, list[int]] = {index: [] for index in range(node_count)}
    for source, target in edges:
        outgoing[source].append(target)
        indegree[target] += 1
    ready = [index for index in range(node_count) if indegree[index] == 0]
    settled = 0
    while ready:
        for target in outgoing[ready.pop()]:
            indegree[target] -= 1
            if indegree[target] == 0:
                ready.append(target)
        settled += 1
    assert settled == node_count, f"seed {seed}: acyclic"


def test_seeded_fuzz_documents_stay_deterministic_acyclic_and_round_trip() -> None:
    for seed in range(FUZZ_ROUNDS):
        text = fuzz_doc(random.Random(seed))
        assert fuzz_doc(random.Random(seed)) == text  # the seed replays byte-for-byte
        first = parse(text)
        second = parse(text)
        assert isinstance(first, Doc)  # hostile lines yield a document, never a raise
        assert _fingerprint(first) == _fingerprint(second)  # the same one every parse
        if first.refusals or first.root < 0:
            continue
        _assert_one_acyclic_graph(first, seed)
        reparsed = parse(export_document(first))
        assert reparsed.refusals == [], f"seed {seed}: export re-parses cleanly"
        assert graph_signature(reparsed) == graph_signature(first), (
            f"seed {seed}: round trip"
        )


def test_deep_dependency_chain_parses_and_round_trips() -> None:
    steps = "\n".join(f"{index}. step {index}" for index in range(1, DEPTH_FLOOR + 1))
    document = parse(f"# Chain\n\n{steps}\n")

    assert len(document.nodes) == DEPTH_FLOOR + 1  # the goal plus one node per step
    assert graph_signature(parse(export_document(document))) == graph_signature(
        document
    )


def test_deeply_nested_list_parses_and_round_trips() -> None:
    rungs = "\n".join("  " * depth + f"- n{depth}" for depth in range(NEST_DEPTH))
    document = parse(f"# Deep\n\n{rungs}\n")

    assert len(document.nodes) == NEST_DEPTH + 1  # the goal plus one node per rung
    assert graph_signature(parse(export_document(document))) == graph_signature(
        document
    )


def test_hostile_line_document_parses_to_its_graph_and_round_trips() -> None:
    document = parse(HOSTILE_DOCUMENT)

    assert document.refusals == []
    assert [node.slug for node in document.nodes] == [
        "hostile-intake",
        "survive-the-hazardous-lines",
        "depend-on-the-first-via-an-after-line",
    ]
    once = export_document(document)
    assert graph_signature(parse(once)) == graph_signature(document)  # round trip
    assert export_document(parse(once)) == once  # fixed point after one pass


def test_label_matching_stays_linear_time() -> None:
    padded = "# T\n\nA" + " " * LABEL_PAD_WIDTH + "x\n"

    started = time.monotonic()
    parse(padded)

    assert time.monotonic() - started < LINEAR_TIME_BUDGET_SECONDS


def test_byte_order_mark_strips_before_the_first_heading() -> None:
    document = parse("﻿# T\n\n- a\n")

    assert document.nodes[document.root].slug == "t"


def test_crlf_document_parses_identically_to_its_lf_twin() -> None:
    crlf = parse("# T\r\n\r\n- a\r\n")
    lf = parse("# T\n\n- a\n")
    leaf = next(node for node in crlf.nodes if node.slug == "a")

    assert {node.slug for node in crlf.nodes} == {"t", "a"}
    assert leaf.parent == crlf.root
    assert graph_signature(crlf) == graph_signature(lf)  # CRLF reads exactly like LF

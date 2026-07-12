"""Every documented warning fires without changing the expected graph."""

from __future__ import annotations

from dataclasses import dataclass

import pytest

from spice.tasks.markdown.classifier import parse
from spice.tasks.markdown.dialect import GraphSignature, NodeSignature, graph_signature


@dataclass(frozen=True)
class WarningCase:
    source: str
    line: int
    code: str
    message: str
    graph: GraphSignature

    def report_line(self) -> str:
        return f"warn {self.line} {self.code} {self.message}"


def _node(
    slug: str,
    title: str,
    *,
    parent: str | None = None,
    priority: str | None = None,
    flow: tuple[str, ...] = (),
    due: str | None = None,
    tags: tuple[str, ...] = (),
    acceptance: tuple[str, ...] = (),
    description: str = "",
    annotations: tuple[str, ...] = (),
) -> tuple[str, NodeSignature]:
    return (
        slug,
        (
            title,
            parent,
            priority,
            flow,
            due,
            tags,
            acceptance,
            description,
            annotations,
        ),
    )


def _graph(*nodes: tuple[str, NodeSignature]) -> GraphSignature:
    return tuple(sorted(nodes)), frozenset()


_LONG_TITLE = "A" * 101
WARNING_CASES = (
    WarningCase(
        "# Root\n\n**Child**\n",
        3,
        "bold-heading",
        "sole bold span promoted to level 2 section",
        _graph(_node("root", "Root"), _node("child", "Child", parent="root")),
    ),
    WarningCase(
        "# Root\n## Notes\n- detail\n",
        2,
        "field-section",
        "Notes heading feeds Root",
        _graph(_node("root", "Root", annotations=("> detail",))),
    ),
    WarningCase(
        "# Root\n## Phase 1\n- Same\n## Phase 2\n- Same\n",
        3,
        "dup-qualified",
        "duplicated title took qualified slug phase-1--same",
        _graph(
            _node("root", "Root"),
            _node("phase-1", "Phase 1", parent="root"),
            _node("phase-1--same", "Same", parent="phase-1"),
            _node("phase-2", "Phase 2", parent="root"),
            _node("phase-2--same", "Same", parent="phase-2"),
        ),
    ),
    WarningCase(
        "# Root\n- [x] Done\n",
        2,
        "checked-discarded",
        "checked marker stripped; the board owns status",
        _graph(_node("root", "Root"), _node("done", "Done", parent="root")),
    ),
    WarningCase(
        "# Root\n1985. Year\n",
        2,
        "ordered-start",
        "numbered line did not start at 1 or continue an ordered run; kept as prose",
        _graph(_node("root", "Root", description="1985. Year")),
    ),
    WarningCase(
        "# Root\n- Child\n      - code\n",
        3,
        "indent-code",
        "indented-code line kept as content",
        _graph(
            _node("root", "Root"),
            _node("child", "Child", parent="root", description="    - code"),
        ),
    ),
    WarningCase(
        "# Root\nPriority: high\nPriority: low\n",
        3,
        "field-repeat",
        "Priority repeated; last value won",
        _graph(_node("root", "Root", priority="low")),
    ),
    WarningCase(
        "# Root\nOwner: release\n",
        2,
        "fieldish-prose",
        "Owner is not a task-document field; kept as prose",
        _graph(_node("root", "Root", description="Owner: release")),
    ),
    WarningCase(
        "# Root\nAfter: when ready, proceed.\n",
        2,
        "after-prose",
        "After-shaped line kept as prose because targets were not slug-shaped",
        _graph(_node("root", "Root", description="After: when ready, proceed.")),
    ),
    WarningCase(
        "# Root\nAcceptance:\n***\n",
        2,
        "empty-acceptance",
        "Acceptance intro captured no criteria",
        _graph(_node("root", "Root")),
    ),
    WarningCase(
        "# [Root](https://example.com/root)\n",
        1,
        "url-title",
        "title contains a URL; slug uses visible link text only",
        _graph(_node("root", "[Root](https://example.com/root)")),
    ),
    WarningCase(
        f"# {_LONG_TITLE}\n",
        1,
        "long-title",
        "title is 101 characters; consider decomposing it",
        _graph(_node(_LONG_TITLE.lower(), _LONG_TITLE)),
    ),
    WarningCase(
        "# Root\n```text\ncode\n",
        3,
        "unclosed-fence",
        "code fence never closed",
        _graph(_node("root", "Root", annotations=("```text\ncode\n```",))),
    ),
    WarningCase(
        "# Root\n<!-- open\nhidden\n",
        2,
        "unclosed-comment",
        "HTML comment never closed",
        _graph(_node("root", "Root")),
    ),
    WarningCase(
        "---\ncontext prose\n",
        2,
        "unclosed-frontmatter",
        "frontmatter never closed; its lines replay as content",
        _graph(),
    ),
    WarningCase(
        "---\ncontext prose\n\n# Root\n",
        3,
        "frontmatter-abort",
        "leading '---' is not frontmatter (blank or heading inside); "
        "its lines replay as content",
        _graph(_node("root", "Root", description="context prose")),
    ),
)


def _warning_report(case: WarningCase) -> tuple[str, GraphSignature]:
    document = parse(case.source)
    report = "\n".join(
        f"warn {line} {code} {message}" for line, code, message in document.warnings
    )
    return report, graph_signature(document)


@pytest.mark.parametrize("case", WARNING_CASES, ids=lambda case: case.code)
def test_warning_fires_without_changing_graph(case: WarningCase) -> None:
    report, actual_graph = _warning_report(case)

    assert case.report_line() in report
    assert actual_graph == case.graph


def test_warning_matrix_covers_the_documented_codes() -> None:
    assert {case.code for case in WARNING_CASES} == {
        "after-prose",
        "bold-heading",
        "checked-discarded",
        "dup-qualified",
        "empty-acceptance",
        "field-repeat",
        "field-section",
        "fieldish-prose",
        "frontmatter-abort",
        "indent-code",
        "long-title",
        "ordered-start",
        "unclosed-comment",
        "unclosed-fence",
        "unclosed-frontmatter",
        "url-title",
    }

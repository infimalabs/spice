"""Named board-diagram registry, palette, and layout contract."""

from __future__ import annotations

import json
import re
from dataclasses import dataclass
from typing import Callable

from spice.errors import SpiceError
from spice.tasks.graphs import flow, magnitude, chronology, topology
from spice.tasks.graphs.derive import TaskRow

Builder = Callable[[list[TaskRow]], tuple[str, str, str]]

PALETTE = (
    "#2a78d6",
    "#eb6834",
    "#1baf7a",
    "#eda100",
    "#e87ba4",
    "#008300",
    "#4a3aa7",
    "#e34948",
)
SURFACE = "#fcfcfb"
INK = "#0b0b0b"
MUTED = "#898781"
MIN_ASPECT_RATIO = 0.25
MAX_ASPECT_RATIO = 5.0


@dataclass(frozen=True)
class Cut:
    name: str
    family: str
    builder: Builder


_SEED = (
    ("29-integration-gitgraph", "time"),
    ("23-hour-of-day-xy", "magnitude"),
    ("15-reviewer-strictness-xy", "magnitude"),
    ("05-phase-flow-sankey", "flow"),
    ("26-era-timeline", "time"),
    ("13b-review-matrix-sankey", "flow"),
    ("17-stem-quadrant", "topology"),
    ("20-daily-throughput-xy", "magnitude"),
    ("30-origin-kind-sankey", "flow"),
    ("07-origin-family-1", "topology"),
    ("01-project-stems-xy", "magnitude"),
    ("06-lifecycle-state", "topology"),
    ("24b-final-phase-turnaround-xy", "magnitude"),
    ("12-lineage-handoff-sankey", "flow"),
    ("18-friction-oops-board", "topology"),
    ("02-agent-worktrees-xy", "magnitude"),
    ("03-project-tree-mindmap", "topology"),
    ("04-agent-to-stem-sankey", "flow"),
    ("07-origin-family-2", "topology"),
    ("07-origin-family-3", "topology"),
    ("08-origin-deepest-spines", "topology"),
    ("09-ack-seeding-fanout", "magnitude"),
    ("10-dependency-component-1", "topology"),
    ("10-dependency-component-2", "topology"),
    ("11-taskdoc-families", "topology"),
    ("13-review-network", "topology"),
    ("14-review-findings-pie", "magnitude"),
    ("16-stem-difficulty-xy", "magnitude"),
    ("19-priority-to-stem-sankey", "flow"),
    ("21-cumulative-burnup-xy", "magnitude"),
    ("22-lane-concurrency-xy", "magnitude"),
    ("24-cycle-time-xy", "magnitude"),
    ("25-campaign-gantt", "time"),
    ("27-task-verbs-xy", "magnitude"),
    ("28-record-schema-er", "topology"),
    ("31-title-length-xy", "magnitude"),
    ("32-board-at-a-glance", "topology"),
)

_BUILDERS = {
    **magnitude.BUILDERS,
    **flow.BUILDERS,
    **topology.BUILDERS,
    **chronology.BUILDERS,
}
if set(_BUILDERS) != {name for name, _family in _SEED}:
    raise RuntimeError("board graph builders do not match the handout registry")

CUTS = tuple(Cut(name, family, _BUILDERS[name]) for name, family in _SEED)
NAMES = tuple(cut.name for cut in CUTS)
_INDEX = {cut.name: cut for cut in CUTS}


def _validate_palette() -> None:
    if len(PALETTE) != 8 or len(set(PALETTE)) != 8:
        raise RuntimeError("board graph palette must contain eight unique slots")
    if not all(re.fullmatch(r"#[0-9a-f]{6}", color) for color in PALETTE):
        raise RuntimeError("board graph palette slots must be lowercase hex colors")


_validate_palette()


def _kind(body: str) -> str:
    head = body.strip().splitlines()[0].strip()
    if head.startswith("xychart"):
        return "xychart"
    if head.startswith("pie"):
        return "pie"
    if head.startswith("gitGraph"):
        return "git"
    if head.startswith("sankey"):
        return "sankey"
    return "other"


def _settings(kind: str) -> dict[str, object]:
    variables: dict[str, object] = {
        "fontFamily": "system-ui, -apple-system, Segoe UI, Helvetica, sans-serif",
        "textColor": INK,
        "lineColor": MUTED,
        "primaryColor": "#e8f0fb",
        "primaryTextColor": INK,
        "primaryBorderColor": PALETTE[0],
        "secondaryColor": SURFACE,
        "tertiaryColor": SURFACE,
    }
    if kind == "xychart":
        variables["xyChart"] = {
            "backgroundColor": SURFACE,
            "titleColor": INK,
            "xAxisLabelColor": "#52514e",
            "yAxisLabelColor": "#52514e",
            "plotColorPalette": ",".join(PALETTE),
        }
    elif kind == "pie":
        variables.update(
            {f"pie{index + 1}": color for index, color in enumerate(PALETTE)}
        )
        variables["pieSectionTextColor"] = "#ffffff"
    elif kind == "git":
        variables.update({f"git{index}": color for index, color in enumerate(PALETTE)})
        variables.update({f"gitBranchLabel{index}": "#ffffff" for index in range(8)})
    else:
        variables.update(
            {f"cScale{index}": color for index, color in enumerate(PALETTE)}
        )
        variables.update({f"cScaleLabel{index}": "#ffffff" for index in range(8)})
    settings: dict[str, object] = {"theme": "base", "themeVariables": variables}
    if kind == "xychart":
        settings["xyChart"] = {"width": 1100, "height": 560}
    elif kind == "sankey":
        settings["sankey"] = {
            "width": 1200,
            "height": 660,
            "linkColor": PALETTE[0],
            "nodeAlignment": "justify",
            "showValues": True,
            "useMaxWidth": False,
        }
    return settings


def render(name: str, rows: list[TaskRow]) -> str:
    cut = _INDEX.get(name)
    if cut is None:
        raise SpiceError(
            f"unknown canned graph {name!r}; choose from {', '.join(NAMES)}"
        )
    title, note, body = cut.builder(rows)
    init = json.dumps(_settings(_kind(body)), separators=(",", ":"))
    comments = "\n".join(f"%% {part.strip().rstrip('.')}." for part in note.split(". "))
    return f"%% {title}\n{comments}\n%%{{init: {init}}}%%\n{body.strip()}"


def validate_aspect(name: str, width: float, height: float) -> float:
    if name not in _INDEX:
        raise SpiceError(f"unknown canned graph {name!r}")
    if width <= 0 or height <= 0:
        raise SpiceError(f"{name} rendered non-positive dimensions {width}x{height}")
    ratio = width / height
    if not MIN_ASPECT_RATIO <= ratio <= MAX_ASPECT_RATIO:
        raise SpiceError(
            f"{name} rendered at {ratio:.2f}:1; allowed range is "
            f"{MIN_ASPECT_RATIO}:1..{MAX_ASPECT_RATIO}:1"
        )
    return ratio


def parse_aspect(value: str) -> tuple[float, float]:
    match = re.fullmatch(r"\s*(\d+(?:\.\d+)?)\s*[xX]\s*(\d+(?:\.\d+)?)\s*", value)
    if match is None:
        raise SpiceError("--check-aspect must be WIDTHxHEIGHT, for example 1200x660")
    return float(match.group(1)), float(match.group(2))

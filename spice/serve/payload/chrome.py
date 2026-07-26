"""Project observed lane chrome onto one target's facets.

Every facet has exactly one authority and its own counter, so this boundary
never blends two versions of a facet and never mints a lane-wide revision.
It reads nothing: callers observe their own authority and hand the value in
beside the token that orders it, which is what lets one assembler answer
identically for the HTTP snapshot, the live bus, and any future producer.
"""

from __future__ import annotations

import re
from collections.abc import Iterable, Mapping
from dataclasses import dataclass
from typing import Any

from spice.errors import SpiceError
from spice.serve.payload.wire import (
    LANE_CHROME_FACET_AUTHORITIES,
    validate_emitter_payload,
)

# Mirrors LANE_CHROME_EPOCH_RUNS in serve/static/app.lane-store.js. The server
# decides what to send under the same natural order the browser decides what to
# keep, so a facet this side considers newer is never refused as a redelivery.
_EPOCH_RUNS = re.compile(r"\d+|\D+")


@dataclass(frozen=True)
class LaneChromeOrder:
    """One authority's place in its own counter.

    The epoch names the generation of that counter and only ever advances, so an
    authority that restarted and resumed from a lower revision still supersedes.
    Orders are compared within a facet only: two authorities counting past each
    other means nothing.
    """

    epoch: str = ""
    revision: int = 0

    def __post_init__(self) -> None:
        if self.revision < 0:
            raise SpiceError(
                f"lane chrome revision cannot count backwards: {self.revision}"
            )

    def supersedes(self, previous: LaneChromeOrder | None) -> bool:
        """Say whether this observation is newer than one already standing."""
        if previous is None:
            return True
        if self.epoch != previous.epoch:
            return _compare_epoch(self.epoch, previous.epoch) > 0
        return self.revision > previous.revision

    def as_payload(self) -> dict[str, Any]:
        return {"epoch": self.epoch, "revision": self.revision}


@dataclass(frozen=True)
class LaneChromeObservation:
    """What one authority saw of one facet, ordered in that authority's counter.

    A ``value`` of ``None`` is the authority stating the facet is now empty,
    which the browser applies as a clear. A facet nobody observed is simply
    never named, and the browser keeps whatever it already holds.
    """

    facet: str
    order: LaneChromeOrder
    value: dict[str, Any] | None = None


@dataclass(frozen=True)
class LaneChromeProjection:
    """What to send for one target, and where each facet now stands."""

    target_id: str
    payload: dict[str, Any]
    orders: Mapping[str, LaneChromeOrder]
    changed: tuple[str, ...]


def assemble_lane_chrome(
    target_id: str,
    observations: Iterable[LaneChromeObservation],
    *,
    published: Mapping[str, LaneChromeOrder] | None = None,
) -> LaneChromeProjection:
    """Project ``observations`` onto one target's lane chrome.

    Every fact arrives in ``observations``: two callers holding the same
    observations produce the same payload no matter which is asking or in what
    order they collected. ``published`` carries what that caller's client
    already holds, so a facet that has not moved since is left out entirely
    rather than resent as an update the browser would discard as stale.

    Facet values are passed through untouched, so a value shared by several
    targets -- one task board across every lane -- is the same object in each
    payload rather than rebuilt per lane.
    """
    target = str(target_id).strip()
    if not target:
        raise SpiceError("lane chrome requires a target id")
    latest = _latest_observations(observations)
    standing = dict(published or {})
    payload: dict[str, Any] = {"targetId": target}
    changed: list[str] = []
    # Walk the contract's own facet order so the payload reads the same for
    # every caller, whatever order that caller happened to observe in.
    for facet, authority in LANE_CHROME_FACET_AUTHORITIES.items():
        observation = latest.get(facet)
        if observation is None or not observation.order.supersedes(standing.get(facet)):
            continue
        payload[facet] = {
            "authority": authority,
            "order": observation.order.as_payload(),
            "value": observation.value,
        }
        standing[facet] = observation.order
        changed.append(facet)
    validate_emitter_payload("payload.chrome.assemble_lane_chrome", payload)
    return LaneChromeProjection(target, payload, standing, tuple(changed))


def _latest_observations(
    observations: Iterable[LaneChromeObservation],
) -> dict[str, LaneChromeObservation]:
    """Keep the newest observation of each facet, replacing it whole.

    Fields are never taken from two versions at once: chrome assembled that way
    describes a lane that existed at no single instant. Two observations that
    claim the same order and disagree are a broken authority rather than a
    choice to make, because whichever the assembler kept would depend on the
    order they arrived in.
    """
    latest: dict[str, LaneChromeObservation] = {}
    for observation in observations:
        if observation.facet not in LANE_CHROME_FACET_AUTHORITIES:
            raise SpiceError(f"unknown lane chrome facet: {observation.facet}")
        standing = latest.get(observation.facet)
        if standing is None or observation.order.supersedes(standing.order):
            latest[observation.facet] = observation
        elif not standing.order.supersedes(observation.order):
            _require_settled_value(standing, observation)
    return latest


def _require_settled_value(
    standing: LaneChromeObservation, observation: LaneChromeObservation
) -> None:
    if standing.value == observation.value:
        return
    raise SpiceError(
        f"lane chrome facet {observation.facet} was observed twice at epoch "
        f"{observation.order.epoch!r} revision {observation.order.revision} "
        "with conflicting values"
    )


def _compare_epoch(epoch: str, other: str) -> int:
    """Order epochs naturally: digit runs as numbers, the text between as text.

    Generation 10 supersedes generation 9, and the text around them still
    groups. Zero-padded fields -- an ISO instant, say -- order identically under
    both rules, so this only ever rescues encodings plain collation inverts.
    """
    runs = _EPOCH_RUNS.findall(epoch)
    other_runs = _EPOCH_RUNS.findall(other)
    for run, other_run in zip(runs, other_runs):
        if run.isdigit() and other_run.isdigit():
            if int(run) != int(other_run):
                return -1 if int(run) < int(other_run) else 1
        elif run != other_run:
            return -1 if run < other_run else 1
    if len(runs) != len(other_runs):
        return -1 if len(runs) < len(other_runs) else 1
    return 0

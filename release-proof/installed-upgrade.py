#!/usr/bin/env python3
"""Seed and inspect upgrade facts through the installed Spice package.

This file is deliberately executed by the throwaway virtualenv with ``-I``.
The release-proof source supplies only the orchestration; every ``spice``
import therefore resolves from the wheel installed in that environment.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

BEFORE_TEAM = "team-upgrade-before"
AFTER_TEAM = "team-upgrade-after"
BEFORE_ACK = "20260727T020000Z-upgrade-before"
AFTER_ACK = "20260727T020001Z-upgrade-after"
BEFORE_MAXIM_BODY = "upgrade proof maxim before"
AFTER_MAXIM_BODY = "upgrade proof maxim after"
MAXIM_BAG = "upgrade-proof"
MAXIM_DRIVER = "codex"


def _authority_seed(repository: Path) -> dict[str, object]:
    from spice.agent.maximmetrics import (
        MAXIM_EVENT_PUBLISHED,
        MaximMetricEventWrite,
        record_maxim_metric_events,
    )
    from spice.mail.ackstate import AckStateWrite, record_acked_inbox_items
    from spice.serve.team.store import ServeTeamStore

    try:
        team = ServeTeamStore().create_team(team_id=BEFORE_TEAM)
    except Exception as exc:
        raise SystemExit(f"spiceteams.sqlite3: {exc}") from exc
    try:
        ack_keys = record_acked_inbox_items(
            repository,
            [
                AckStateWrite(
                    key=BEFORE_ACK,
                    inbox_name="upgrade-proof",
                    text="Seed ACK authority before upgrade",
                    ack_text="ACK upgrade proof",
                    ack_content="upgrade proof",
                )
            ],
            now=1_700_000_000.0,
        )
    except Exception as exc:
        raise SystemExit(f"spiceacks.sqlite3: {exc}") from exc
    try:
        record_maxim_metric_events(
            repository,
            [
                MaximMetricEventWrite(
                    MAXIM_EVENT_PUBLISHED,
                    bag_name=MAXIM_BAG,
                    driver_name=MAXIM_DRIVER,
                    reminder_body=BEFORE_MAXIM_BODY,
                )
            ],
            now=1_700_000_001.0,
        )
    except Exception as exc:
        raise SystemExit(f"spicemaxims.sqlite3: {exc}") from exc
    return {
        "facts": {
            "spiceacks.sqlite3": {"preserved": ack_keys},
            "spicemaxims.sqlite3": {"preserved": [BEFORE_MAXIM_BODY]},
            "spiceteams.sqlite3": {"preserved": [team.team_id]},
        },
        "paths": _authority_paths(repository),
    }


def _authority_verify_and_write(repository: Path) -> dict[str, object]:
    from spice.agent.maximmetrics import (
        MAXIM_EVENT_PUBLISHED,
        MaximMetricEventWrite,
        maxim_metric_records,
        record_maxim_metric_events,
    )
    from spice.mail.ackstate import (
        AckStateWrite,
        ack_state_records_for_keys,
        record_acked_inbox_items,
    )
    from spice.serve.team.store import ServeTeamStore

    try:
        teams = ServeTeamStore()
        preserved_team = teams.team_state(BEFORE_TEAM)
        written_team = teams.create_team(team_id=AFTER_TEAM)
    except Exception as exc:
        raise SystemExit(f"spiceteams.sqlite3: {exc}") from exc
    try:
        written_ack = record_acked_inbox_items(
            repository,
            [
                AckStateWrite(
                    key=AFTER_ACK,
                    inbox_name="upgrade-proof",
                    text="Write ACK authority after upgrade",
                    ack_text="ACK upgraded proof",
                    ack_content="upgraded proof",
                )
            ],
            now=1_700_000_002.0,
        )
        ack_records = ack_state_records_for_keys(repository, (BEFORE_ACK, AFTER_ACK))
        ack_keys = sorted(record.key for record in ack_records)
        _require_equal("ACK authority keys", ack_keys, sorted((BEFORE_ACK, AFTER_ACK)))
    except Exception as exc:
        raise SystemExit(f"spiceacks.sqlite3: {exc}") from exc
    try:
        written_maxim = record_maxim_metric_events(
            repository,
            [
                MaximMetricEventWrite(
                    MAXIM_EVENT_PUBLISHED,
                    bag_name=MAXIM_BAG,
                    driver_name=MAXIM_DRIVER,
                    reminder_body=AFTER_MAXIM_BODY,
                )
            ],
            now=1_700_000_003.0,
        )
        maxim_records = [
            record
            for record in maxim_metric_records(repository)
            if record.bag_name == MAXIM_BAG and record.driver_name == MAXIM_DRIVER
        ]
        maxim_bodies = sorted(record.reminder_body for record in maxim_records)
        _require_equal(
            "maxim authority bodies",
            maxim_bodies,
            sorted((BEFORE_MAXIM_BODY, AFTER_MAXIM_BODY)),
        )
    except Exception as exc:
        raise SystemExit(f"spicemaxims.sqlite3: {exc}") from exc
    return {
        "spiceacks.sqlite3": {
            "preserved": [BEFORE_ACK],
            "written": written_ack,
        },
        "spicemaxims.sqlite3": {
            "preserved": [BEFORE_MAXIM_BODY],
            "written": written_maxim,
        },
        "spiceteams.sqlite3": {
            "preserved": [preserved_team.team_id],
            "written": [written_team.team_id],
        },
    }


def _resolved_paths(repository: Path) -> dict[str, object]:
    from spice.agent.driver import CODEX_DRIVER
    from spice.agent import maximmetrics
    from spice.mail import ackstate
    from spice.serve.team import projection, store
    from spice.tasks import opslog

    return {
        "import_origin": str(Path(store.__file__).resolve()),
        "paths": {
            "spiceacks.sqlite3": str(
                ackstate.ack_state_database_path(repository).resolve()
            ),
            "spicemaxims.sqlite3": str(
                maximmetrics.maxim_metrics_database_path(repository).resolve()
            ),
            "spiceprojections.sqlite3": str(
                projection.projection_database_path().resolve()
            ),
            "spiceteams.sqlite3": str(store.team_database_path().resolve()),
            "state_5.sqlite": str(CODEX_DRIVER.state_db_path().resolve()),
            "taskchampion.sqlite3": str(opslog.operations_db_path().resolve()),
        },
    }


def _authority_paths(repository: Path) -> dict[str, object]:
    from spice.agent.driver import CODEX_DRIVER
    from spice.agent import maximmetrics
    from spice.mail import ackstate
    from spice.serve.team import store
    from spice.tasks import opslog

    return {
        "import_origin": str(Path(store.__file__).resolve()),
        "paths": {
            "spiceacks.sqlite3": str(
                ackstate.ack_state_database_path(repository).resolve()
            ),
            "spicemaxims.sqlite3": str(
                maximmetrics.maxim_metrics_database_path(repository).resolve()
            ),
            "spiceteams.sqlite3": str(store.team_database_path().resolve()),
            "state_5.sqlite": str(CODEX_DRIVER.state_db_path().resolve()),
            "taskchampion.sqlite3": str(opslog.operations_db_path().resolve()),
        },
    }


def _projection_evidence() -> dict[str, object]:
    from spice.serve.team.projection import ServeProjectionStore

    states = ServeProjectionStore().family_states()
    if not states:
        raise SystemExit("projection rebuild published no registered family")
    return {
        "spiceprojections.sqlite3": {
            "families": [state.family.name for state in states],
            "generations": [state.generation for state in states],
            "statuses": [state.status for state in states],
        }
    }


def _require_equal(label: str, actual: Any, expected: Any) -> None:
    if actual != expected:
        raise RuntimeError(
            f"{label} changed: expected {expected!r}, resolved {actual!r}"
        )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "action",
        choices=("seed-authority", "verify-authority", "paths", "projection"),
    )
    parser.add_argument("--repository", required=True, type=Path)
    arguments = parser.parse_args()
    repository = arguments.repository.resolve(strict=True)
    if arguments.action == "seed-authority":
        evidence = _authority_seed(repository)
    elif arguments.action == "verify-authority":
        evidence = _authority_verify_and_write(repository)
    elif arguments.action == "paths":
        evidence = _resolved_paths(repository)
    else:
        evidence = _projection_evidence()
    print(json.dumps(evidence, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

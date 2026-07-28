"""Conditional authority-version contracts for the installed upgrade proof."""

from __future__ import annotations

import json
import subprocess
import sys

import pytest

from tests.test_releaseproofhelpers import INSTALLED_UPGRADE_SCRIPT, REHEARSAL


def _authority_versions() -> dict[str, int]:
    return {
        name: index
        for index, name in enumerate(REHEARSAL.UPGRADE_AUTHORITY_STATE, start=1)
    }


def test_in_place_upgrade_accepts_unchanged_authority_versions():
    versions = _authority_versions()

    evidence = REHEARSAL._authority_version_evidence(
        versions,
        versions,
        versions,
    )

    assert evidence == {
        name: {"from": version, "to": version} for name, version in versions.items()
    }


def test_in_place_upgrade_accepts_an_authority_schema_advance():
    predecessor = _authority_versions()
    candidate = dict(predecessor)
    candidate[REHEARSAL.UPGRADE_TEAM_STORE] += 1

    evidence = REHEARSAL._authority_version_evidence(
        predecessor,
        candidate,
        candidate,
    )

    assert evidence[REHEARSAL.UPGRADE_TEAM_STORE] == {
        "from": predecessor[REHEARSAL.UPGRADE_TEAM_STORE],
        "to": candidate[REHEARSAL.UPGRADE_TEAM_STORE],
    }


def test_in_place_upgrade_refuses_an_unperformed_authority_schema_advance():
    predecessor = _authority_versions()
    candidate = dict(predecessor)
    candidate[REHEARSAL.UPGRADE_TEAM_STORE] += 1

    with pytest.raises(
        REHEARSAL.RehearsalError,
        match=(
            rf"{REHEARSAL.UPGRADE_TEAM_STORE} "
            rf"predecessor={predecessor[REHEARSAL.UPGRADE_TEAM_STORE]} "
            rf"candidate={candidate[REHEARSAL.UPGRADE_TEAM_STORE]} "
            rf"installed={predecessor[REHEARSAL.UPGRADE_TEAM_STORE]}"
        ),
    ):
        REHEARSAL._authority_version_evidence(
            predecessor,
            candidate,
            predecessor,
        )


def test_installed_upgrade_action_reports_declared_authority_versions(tmp_path):
    from spice.agent.maximmetrics import MAXIM_METRICS_SCHEMA_VERSION
    from spice.mail.ackschema import ACK_STATE_SCHEMA_VERSION
    from spice.serve.team.schema import TEAM_AUTHORITY_SCHEMA_VERSION

    completed = subprocess.run(
        [
            sys.executable,
            str(INSTALLED_UPGRADE_SCRIPT),
            "authority-versions",
            "--repository",
            str(tmp_path),
        ],
        check=True,
        capture_output=True,
        text=True,
    )

    assert json.loads(completed.stdout) == {
        "spiceacks.sqlite3": ACK_STATE_SCHEMA_VERSION,
        "spicemaxims.sqlite3": MAXIM_METRICS_SCHEMA_VERSION,
        "spiceteams.sqlite3": TEAM_AUTHORITY_SCHEMA_VERSION,
    }

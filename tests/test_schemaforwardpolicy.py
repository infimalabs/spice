"""Release policy for bounded durable-store migration surfaces."""

from spice.mail.ackschema import ACK_STATE_MIGRATION_SOURCES
from spice.serve.team.schema import (
    TEAM_AUTHORITY_MIGRATIONS,
    TEAM_AUTHORITY_MONOTONIC_VERSION_MAX,
    TEAM_AUTHORITY_SCHEMAS,
    TEAM_AUTHORITY_SCHEMA_VERSION,
)


def test_durable_stores_support_at_most_one_prior_source_shape():
    supported_prior_sources = {
        "ACK state": tuple(ACK_STATE_MIGRATION_SOURCES),
        "team authority": tuple(
            version
            for version in TEAM_AUTHORITY_SCHEMAS
            if version < TEAM_AUTHORITY_SCHEMA_VERSION
        ),
    }

    assert {
        store: sources
        for store, sources in supported_prior_sources.items()
        if len(sources) > 1
    } == {}


def test_team_authority_keeps_only_the_current_forward_migration():
    assert 0 < TEAM_AUTHORITY_SCHEMA_VERSION <= TEAM_AUTHORITY_MONOTONIC_VERSION_MAX
    assert set(TEAM_AUTHORITY_MIGRATIONS) == {TEAM_AUTHORITY_SCHEMA_VERSION}
    assert set(TEAM_AUTHORITY_SCHEMAS) <= {
        TEAM_AUTHORITY_SCHEMA_VERSION - 1,
        TEAM_AUTHORITY_SCHEMA_VERSION,
    }

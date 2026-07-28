"""Refusals shared by manual and automatic agent starts."""

from pathlib import Path

from spice.errors import SpiceError


def require_no_pending_authority_migration(repo_root: Path) -> None:
    """Keep every launch surface out of a store's pending migration window."""
    from spice.serve.team.store import (
        LANE_SCHEMA_RECORD_HORIZON_HOURS,
        pending_authority_migration,
        team_database_path,
    )

    pending = pending_authority_migration(team_database_path(repo_root))
    if pending is None:
        return
    raise SpiceError(
        "refusing to start an agent while team authority schema migration "
        f"{pending.source_version} -> {pending.target_version} is pending; "
        "the migration clears this signal once the older lanes drain, and an "
        f"abandoned signal expires after {LANE_SCHEMA_RECORD_HORIZON_HOURS} hours"
    )

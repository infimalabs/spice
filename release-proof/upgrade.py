#!/usr/bin/env python3
"""Generate prior-release stores from tagged source and open current writers."""

from __future__ import annotations

import argparse
import ast
from collections import Counter
import hashlib
import json
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import tomllib
from typing import NamedTuple
import zlib

SCHEMA_VERSION = 1
MANIFEST_RELATIVE_PATH = Path(".release-proof") / "prior-stores.json"
ARTIFACT_RELATIVE_PATH = Path(".release-proof") / "prior-artifact"
ARTIFACT_MANIFEST_NAME = "manifest.json"
REQUIRED_STORES = ("team", "ack", "maxim-metrics", "projection")
STORE_SOURCE_PATHS = {
    "team": ("spice/serve/team/schema.py",),
    "ack": ("spice/mail/ackstate.py", "spice/mail/ackschema.py"),
    "maxim-metrics": ("spice/agent/maximmetrics.py",),
    "projection": ("spice/serve/team/projection.py",),
}


class UpgradeProofError(RuntimeError):
    """The tagged-store rehearsal could not establish its contract."""


class TableState(NamedTuple):
    columns: tuple[str, ...]
    rows: tuple[tuple[object, ...], ...]


class CurrentContracts(NamedTuple):
    team_version: int
    ack_version: int
    projection_version: int
    projection_tables: tuple[str, ...]
    maxim_table_sql: str


def _git(
    repository: Path, *arguments: str, check: bool = True
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", "-C", str(repository), *arguments],
        check=check,
        capture_output=True,
        text=True,
    )


def _release_version(tag: str) -> tuple[int, ...] | None:
    """Order a ``vMAJOR.MINOR.PATCH`` tag as integers, or decline to rank it."""
    fields = tag[1:].split(".") if tag.startswith("v") else []
    if not fields or not all(field.isdigit() for field in fields):
        return None
    return tuple(int(field) for field in fields)


def _declared_release_version(repository: Path) -> tuple[int, ...] | None:
    """Read the version this tree will ship as from its own project metadata."""
    try:
        payload = tomllib.loads(
            (repository / "pyproject.toml").read_text(encoding="utf-8")
        )
    except (OSError, tomllib.TOMLDecodeError):
        return None
    project = payload.get("project")
    declared = project.get("version") if isinstance(project, dict) else None
    return _release_version(f"v{declared}") if isinstance(declared, str) else None


def _prior_release_tag(repository: Path) -> str | None:
    """Name the release this tree upgrades from, ordered by declared version.

    Ancestry alone cannot answer this. Every commit made after a release still
    describes back to that release's tag, so a rule reading ``HEAD^`` starts
    naming the tree's own release the moment one is cut, and the rehearsal then
    builds its fixtures from the very schema it opens them with -- passing while
    comparing a release against itself. The tree already declares the version it
    is going to ship as, so the release it upgrades from is the newest tag
    ordered strictly below that. This holds for every commit of a cycle and
    re-points itself when the next release lands, with nobody to remember it.

    Declining when the tree names no version keeps that closed: an unrankable
    tree yields no predecessor, and ``rehearse_prior_stores`` refuses a manifest
    that carries none rather than rehearsing against whatever git described.
    """
    declared = _declared_release_version(repository)
    if declared is None:
        return None
    listed = _git(
        repository,
        "tag",
        "--list",
        "v[0-9]*",
        "--merged",
        "HEAD",
        "--sort=-v:refname",
        check=False,
    )
    if listed.returncode != 0:
        return None
    for tag in listed.stdout.split():
        version = _release_version(tag)
        if version is not None and version < declared:
            return tag
    return None


def prior_store_manifest(repository: Path) -> dict[str, object]:
    """Read exported provenance or derive it from the repository's release tag."""
    repository = repository.resolve()
    exported = repository / MANIFEST_RELATIVE_PATH
    if exported.is_file():
        try:
            payload = json.loads(exported.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as exc:
            raise UpgradeProofError(
                f"could not read prior-store manifest {exported}: {exc}"
            ) from exc
        return _validate_manifest(payload)

    tag = _prior_release_tag(repository)
    releases: list[dict[str, object]] = []
    if tag is not None:
        commit = _git(repository, "rev-parse", f"{tag}^{{commit}}").stdout.strip()
        stores: dict[str, dict[str, object]] = {}
        for store in REQUIRED_STORES:
            sources: dict[str, str] = {}
            for relative in STORE_SOURCE_PATHS[store]:
                shown = _git(repository, "show", f"{tag}:{relative}", check=False)
                if shown.returncode == 0:
                    sources[relative] = shown.stdout
            stores[store] = {
                "state": "source" if sources else "absent",
                "sources": sources,
            }
        releases.append({"tag": tag, "commit": commit, "stores": stores})
    return _validate_manifest(
        {"schema_version": SCHEMA_VERSION, "releases": releases},
        require_release=False,
    )


def export_prior_store_manifest(repository: Path, output: Path) -> dict[str, object]:
    """Write tagged schema text, never a generated SQLite artifact."""
    payload = prior_store_manifest(repository)
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_name(f".{output.name}.tmp")
    temporary.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    temporary.replace(output)
    return payload


def _require(command: list[str], *, purpose: str) -> None:
    """Run a build step, surfacing its own stderr instead of a bare exit code."""
    completed = subprocess.run(command, check=False, capture_output=True, text=True)
    if completed.returncode != 0:
        raise UpgradeProofError(
            f"{purpose} failed ({completed.returncode}): {completed.stderr.strip()}"
        )


def _build_prior_wheel(repository: Path, tag: str, output: Path) -> dict[str, str]:
    """Build the tagged predecessor into ``output`` and describe its one wheel."""
    with tempfile.TemporaryDirectory() as scratch:
        source = Path(scratch) / "source"
        source.mkdir()
        archive = Path(scratch) / "source.tar"
        _git(repository, "archive", "--format=tar", f"--output={archive}", tag)
        _require(
            ["tar", "-xf", str(archive), "-C", str(source)],
            purpose=f"unpacking {tag}",
        )
        _require(
            ["uv", "build", "--wheel", "--out-dir", str(output), str(source)],
            purpose=f"building the {tag} wheel",
        )
    # ``uv build`` seeds its output directory with a catch-all ignore rule. The
    # carried artifact is tracked on purpose, so that rule must not travel.
    (output / ".gitignore").unlink(missing_ok=True)
    wheels = sorted(output.glob("*.whl"))
    if len(wheels) != 1:
        raise UpgradeProofError(
            f"building {tag} produced {len(wheels)} wheels under {output}; "
            "the in-place upgrade proof needs exactly one predecessor"
        )
    return {
        "name": wheels[0].name,
        "sha256": hashlib.sha256(wheels[0].read_bytes()).hexdigest(),
    }


def export_prior_artifact(repository: Path, output: Path) -> dict[str, object]:
    """Build the predecessor wheel here, where its release tag is reachable.

    The synthetic release-proof repository is a single tagless commit, so it can
    never build its own predecessor, and deriving one from ``HEAD^`` there would
    silently attest the wrong bytes. The carried manifest is the only source.

    ``state`` names what was carried, mirroring the ``source``/``absent`` states
    ``_validate_store_entry`` already accepts, and the sole thing that decides
    it is whether a release tag exists. A repository with none has nothing to be
    upgraded from, which ``prior_store_manifest`` already treats as a legitimate
    shape; refusing that absent predecessor belongs to the gate that consumes
    this manifest, not to the exporter that describes it. A tag that will not
    build is the opposite case -- a broken release, not an absent one -- and it
    raises here, where the failing build command can still say why.
    """
    repository = repository.resolve()
    output.mkdir(parents=True, exist_ok=True)
    tag = _prior_release_tag(repository)
    release: dict[str, str] | None = None
    wheel: dict[str, str] | None = None
    if tag is not None:
        commit = _git(repository, "rev-parse", f"{tag}^{{commit}}").stdout.strip()
        release = {"tag": tag, "commit": commit}
        wheel = _build_prior_wheel(repository, tag, output)
    payload: dict[str, object] = {
        "schema_version": SCHEMA_VERSION,
        "release": release,
        "state": "built" if wheel is not None else "absent",
        "wheel": wheel,
    }
    (output / ARTIFACT_MANIFEST_NAME).write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return payload


def _validate_manifest(
    raw: object, *, require_release: bool = True
) -> dict[str, object]:
    if not isinstance(raw, dict) or set(raw) != {"schema_version", "releases"}:
        raise UpgradeProofError("prior-store manifest has an invalid top-level shape")
    if raw.get("schema_version") != SCHEMA_VERSION:
        raise UpgradeProofError(
            "prior-store manifest has an unsupported schema version"
        )
    releases = raw.get("releases")
    if not isinstance(releases, list):
        raise UpgradeProofError("prior-store manifest releases must be a list")
    if require_release and len(releases) != 1:
        raise UpgradeProofError(
            "prior-store manifest must carry exactly one bounded predecessor release"
        )
    for release in releases:
        if not isinstance(release, dict) or set(release) != {
            "tag",
            "commit",
            "stores",
        }:
            raise UpgradeProofError("prior-store release entry is incomplete")
        if not isinstance(release["tag"], str) or not release["tag"].startswith("v"):
            raise UpgradeProofError("prior-store release tag is invalid")
        commit = release["commit"]
        if not isinstance(commit, str) or len(commit) not in {40, 64}:
            raise UpgradeProofError("prior-store release commit is invalid")
        stores = release["stores"]
        if not isinstance(stores, dict) or set(stores) != set(REQUIRED_STORES):
            raise UpgradeProofError(
                "prior-store manifest inventory must be exactly "
                + ", ".join(REQUIRED_STORES)
            )
        for name, entry in stores.items():
            _validate_store_entry(str(name), entry)
    return raw


def _validate_store_entry(name: str, raw: object) -> None:
    if not isinstance(raw, dict) or set(raw) != {"state", "sources"}:
        raise UpgradeProofError(f"prior-store entry {name} is incomplete")
    state = raw["state"]
    sources = raw["sources"]
    if state not in {"source", "absent"} or not isinstance(sources, dict):
        raise UpgradeProofError(f"prior-store entry {name} has invalid state")
    allowed = set(STORE_SOURCE_PATHS[name])
    if any(
        path not in allowed or not isinstance(source, str)
        for path, source in sources.items()
    ):
        raise UpgradeProofError(f"prior-store entry {name} has unapproved source")
    if (state == "source") != bool(sources):
        raise UpgradeProofError(
            f"prior-store entry {name} does not agree with its source inventory"
        )
    if name != "projection" and state == "absent":
        raise UpgradeProofError(f"required prior store source is absent: {name}")


def _expression_value(node: ast.expr, values: dict[str, object]) -> object:
    if isinstance(node, ast.Constant):
        return node.value
    if isinstance(node, ast.Name):
        return values[node.id]
    if isinstance(node, ast.Tuple):
        return tuple(_expression_value(item, values) for item in node.elts)
    if isinstance(node, ast.List):
        return [_expression_value(item, values) for item in node.elts]
    if isinstance(node, ast.Dict):
        return {
            _expression_value(key, values): _expression_value(value, values)
            for key, value in zip(node.keys, node.values, strict=True)
            if key is not None
        }
    if isinstance(node, ast.UnaryOp) and isinstance(node.op, ast.USub):
        return -int(_expression_value(node.operand, values))
    if isinstance(node, ast.BinOp):
        left = _expression_value(node.left, values)
        right = _expression_value(node.right, values)
        if isinstance(node.op, ast.Add):
            if isinstance(left, str) and isinstance(right, str):
                return left + right
            if isinstance(left, int) and isinstance(right, int):
                return left + right
        if isinstance(node.op, ast.Mult):
            if isinstance(left, int) and isinstance(right, int):
                return left * right
            if isinstance(left, str) and isinstance(right, int):
                return left * right
        if isinstance(node.op, ast.BitOr):
            return int(left) | int(right)
        if isinstance(node.op, ast.BitAnd):
            return int(left) & int(right)
    raise ValueError("assignment is not a safe constant expression")


def _assigned_constants(source: str) -> dict[str, object]:
    values: dict[str, object] = {}
    for statement in ast.parse(source).body:
        name: str | None = None
        value: ast.expr | None = None
        if (
            isinstance(statement, ast.Assign)
            and len(statement.targets) == 1
            and isinstance(statement.targets[0], ast.Name)
        ):
            name = statement.targets[0].id
            value = statement.value
        elif isinstance(statement, ast.AnnAssign) and isinstance(
            statement.target, ast.Name
        ):
            name = statement.target.id
            value = statement.value
        if name is None or value is None:
            continue
        try:
            values[name] = _expression_value(value, values)
        except (KeyError, TypeError, ValueError):
            continue
    return values


def _store_constants(name: str, entry: dict[str, object]) -> dict[str, object]:
    sources = entry["sources"]
    assert isinstance(sources, dict)
    values: dict[str, object] = {}
    for relative in STORE_SOURCE_PATHS[name]:
        source = sources.get(relative)
        if isinstance(source, str):
            values.update(_assigned_constants(source))
    return values


def _required_string(values: dict[str, object], name: str) -> str:
    value = values.get(name)
    if not isinstance(value, str) or not value.strip():
        raise UpgradeProofError(f"tagged schema source is missing {name}")
    return value


def _team_schema(values: dict[str, object]) -> tuple[str, int]:
    schemas = values.get("TEAM_AUTHORITY_SCHEMAS")
    version = values.get("TEAM_AUTHORITY_SCHEMA_VERSION")
    if isinstance(schemas, dict) and isinstance(version, int):
        schema = schemas.get(version)
        if isinstance(schema, str):
            return schema, version
    schema = _required_string(values, "TEAM_SCHEMA")
    fingerprint = (zlib.crc32(schema.encode("utf-8")) & 0x7FFFFFFF) | 1
    return schema, fingerprint


def _fixture_spec(
    name: str, entry: dict[str, object]
) -> tuple[tuple[str, ...], int] | None:
    if entry["state"] == "absent":
        return None
    values = _store_constants(name, entry)
    if name == "team":
        schema, version = _team_schema(values)
        return (schema,), version
    if name == "ack":
        return (
            (
                _required_string(values, "ACK_STATE_TABLE_SQL"),
                _required_string(values, "ACK_STATE_INDEX_SQL"),
            ),
            int(values.get("ACK_STATE_SCHEMA_VERSION", 0)),
        )
    if name == "maxim-metrics":
        return (
            tuple(
                _required_string(values, constant)
                for constant in (
                    "MAXIM_METRICS_TABLE_SQL",
                    "MAXIM_METRICS_EVENT_INDEX_SQL",
                    "MAXIM_METRICS_RECURRENCE_INDEX_SQL",
                    "MAXIM_METRICS_FIRE_RECENCY_INDEX_SQL",
                )
            ),
            0,
        )
    if name == "projection":
        return (
            (_required_string(values, "PROJECTION_SCHEMA"),),
            int(values.get("PROJECTION_SCHEMA_VERSION", 0)),
        )
    raise UpgradeProofError(f"unknown prior store: {name}")


_TEXT_VALUES = {
    "kind": "claim",
    "event_type": "fire",
    "status": "open",
    "state": "started",
    "lifetime": "Drive",
    "disposition": "acked",
    "provenance": "archiveOnly",
    "attachments_json": "[]",
    "agent_ids": "[]",
    "task_filters": "[]",
    "lineage_json": "{}",
    "shell_settings": "{}",
    "predecessor_identity": "{}",
    "successor_identity": "{}",
    "payload": "{}",
    "source_path": "/release-proof/source.jsonl",
}


def _fixture_value(table: str, name: str, declared_type: str) -> object:
    if name == "status" and table == "projection_status":
        return "ready"
    if name == "family":
        return "agentActivity"
    if name in _TEXT_VALUES:
        return _TEXT_VALUES[name]
    upper = declared_type.upper()
    if "INT" in upper:
        return 1
    if any(token in upper for token in ("REAL", "FLOA", "DOUB")):
        return 1.25
    if "BLOB" in upper:
        return b"release-proof"
    return f"{table}:{name}"


def _quote_identifier(value: str) -> str:
    return '"' + value.replace('"', '""') + '"'


def _seed_all_tables(connection: sqlite3.Connection) -> None:
    tables = [
        str(row[0])
        for row in connection.execute(
            "SELECT name FROM sqlite_master "
            "WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
        )
    ]
    for table in tables:
        columns = [
            row
            for row in connection.execute(
                f"PRAGMA table_xinfo({_quote_identifier(table)})"
            )
            if int(row[6]) == 0
        ]
        if not columns:
            continue
        names = [str(row[1]) for row in columns]
        values = [_fixture_value(table, str(row[1]), str(row[2])) for row in columns]
        connection.execute(
            f"INSERT INTO {_quote_identifier(table)} "
            f"({', '.join(_quote_identifier(name) for name in names)}) "
            f"VALUES ({', '.join('?' for _ in values)})",
            values,
        )


def _create_fixture(
    path: Path, scripts: tuple[str, ...], version: int, *, seed: bool = True
) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    connection = sqlite3.connect(path)
    try:
        for script in scripts:
            connection.executescript(script)
        if seed:
            _seed_all_tables(connection)
        connection.execute(f"PRAGMA user_version = {version}")
        connection.commit()
    finally:
        connection.close()


def _snapshot(path: Path) -> dict[str, TableState]:
    connection = sqlite3.connect(path)
    try:
        tables = [
            str(row[0])
            for row in connection.execute(
                "SELECT name FROM sqlite_master "
                "WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
            )
        ]
        snapshot: dict[str, TableState] = {}
        for table in tables:
            columns = tuple(
                str(row[1])
                for row in connection.execute(
                    f"PRAGMA table_xinfo({_quote_identifier(table)})"
                )
                if int(row[6]) == 0
            )
            rows = tuple(
                tuple(row)
                for row in connection.execute(
                    f"SELECT * FROM {_quote_identifier(table)} ORDER BY rowid"
                )
            )
            snapshot[table] = TableState(columns, rows)
        return snapshot
    finally:
        connection.close()


def _version(path: Path) -> int:
    connection = sqlite3.connect(path)
    try:
        return int(connection.execute("PRAGMA user_version").fetchone()[0])
    finally:
        connection.close()


def _assert_rows_preserved(
    name: str,
    before: dict[str, TableState],
    after: dict[str, TableState],
    *,
    allow_additional: bool = False,
) -> int:
    preserved = 0
    for table, source in before.items():
        destination = after.get(table)
        if destination is None:
            raise UpgradeProofError(f"{name} dropped prior table {table}")
        common = tuple(
            column for column in source.columns if column in destination.columns
        )
        if not common:
            raise UpgradeProofError(f"{name}.{table} has no shared columns")
        source_indexes = tuple(source.columns.index(column) for column in common)
        destination_indexes = tuple(
            destination.columns.index(column) for column in common
        )
        source_rows = Counter(
            tuple(row[index] for index in source_indexes) for row in source.rows
        )
        destination_rows = Counter(
            tuple(row[index] for index in destination_indexes)
            for row in destination.rows
        )
        if allow_additional:
            missing = source_rows - destination_rows
            if missing:
                raise UpgradeProofError(
                    f"{name}.{table} lost prior shared-column rows: {missing}"
                )
        elif source_rows != destination_rows:
            raise UpgradeProofError(
                f"{name}.{table} changed prior shared-column row values"
            )
        preserved += sum(source_rows.values())
    return preserved


def _materialize_prior_fixtures(
    stores: dict[str, object], store_paths: dict[str, Path]
) -> tuple[dict[str, dict[str, TableState]], dict[str, str]]:
    before: dict[str, dict[str, TableState]] = {}
    source_states: dict[str, str] = {}
    for name in REQUIRED_STORES:
        entry = stores[name]
        assert isinstance(entry, dict)
        source_states[name] = str(entry["state"])
        spec = _fixture_spec(name, entry)
        if spec is None:
            if name != "projection":
                raise UpgradeProofError(f"unclassified absent prior store: {name}")
            if store_paths[name].exists():
                raise UpgradeProofError("absent projection fixture already exists")
            continue
        scripts, version = spec
        _create_fixture(
            store_paths[name],
            scripts,
            version,
            seed=name != "projection",
        )
        before[name] = _snapshot(store_paths[name])
    return before, source_states


def _open_with_current_writers(
    scratch: Path,
    state_backend: Path,
    store_paths: dict[str, Path],
) -> CurrentContracts:
    from spice.agent.maximmetrics import (
        MaximMetricEventWrite,
        MAXIM_METRICS_TABLE_SQL,
        maxim_metric_records,
        record_maxim_metric_events,
    )
    from spice.mail.ackschema import ACK_STATE_SCHEMA_VERSION
    from spice.mail.ackstate import prepare_directive_history_database
    from spice.paths import set_state_backend
    from spice.serve.team.projection import (
        PROJECTION_SCHEMA_VERSION,
        PROJECTION_TABLES,
        ServeProjectionStore,
    )
    from spice.serve.team.schema import TEAM_AUTHORITY_SCHEMA_VERSION
    from spice.serve.team.store import ServeTeamStore

    with ServeTeamStore(
        path=store_paths["team"],
        directive_state_path=store_paths["ack"],
        projection_path=store_paths["projection"],
    ).connect():
        pass
    prepare_directive_history_database(store_paths["ack"])
    with ServeProjectionStore(store_paths["projection"]).connect():
        pass
    set_state_backend(str(state_backend))
    try:
        record_maxim_metric_events(
            scratch,
            [
                MaximMetricEventWrite(
                    event_type="fire",
                    bag_name="release-proof-current-writer",
                    driver_name="codex",
                )
            ],
            now=99.0,
        )
        if len(maxim_metric_records(scratch)) < 2:
            raise UpgradeProofError("current maxim writer did not retain prior row")
    finally:
        set_state_backend(None)
    return CurrentContracts(
        TEAM_AUTHORITY_SCHEMA_VERSION,
        ACK_STATE_SCHEMA_VERSION,
        PROJECTION_SCHEMA_VERSION,
        PROJECTION_TABLES,
        MAXIM_METRICS_TABLE_SQL,
    )


def _expected_columns(schema: str, table: str) -> tuple[str, ...]:
    connection = sqlite3.connect(":memory:")
    try:
        connection.execute(schema)
        return tuple(
            str(row[1])
            for row in connection.execute(
                f"PRAGMA table_xinfo({_quote_identifier(table)})"
            )
        )
    finally:
        connection.close()


def _current_store_results(
    store_paths: dict[str, Path],
    before: dict[str, dict[str, TableState]],
    source_states: dict[str, str],
    contracts: CurrentContracts,
) -> dict[str, dict[str, object]]:
    after = {
        name: _snapshot(path) for name, path in store_paths.items() if path.is_file()
    }
    if set(after) != set(REQUIRED_STORES):
        raise UpgradeProofError(
            "current writers did not open the exact store inventory"
        )
    preserved = {
        "team": _assert_rows_preserved("team", before["team"], after["team"]),
        "ack": _assert_rows_preserved("ack", before["ack"], after["ack"]),
        "maxim-metrics": _assert_rows_preserved(
            "maxim-metrics",
            before["maxim-metrics"],
            after["maxim-metrics"],
            allow_additional=True,
        ),
    }
    expected_maxim = _expected_columns(contracts.maxim_table_sql, "maxim_metric_events")
    if after["maxim-metrics"]["maxim_metric_events"].columns != expected_maxim:
        raise UpgradeProofError("maxim-metrics did not reach the current table shape")
    if set(contracts.projection_tables) - set(after["projection"]):
        raise UpgradeProofError("projection writer did not create every family table")
    results = {
        "team": {
            "source": source_states["team"],
            "version": _version(store_paths["team"]),
            "expected_version": contracts.team_version,
            "preserved_rows": preserved["team"],
        },
        "ack": {
            "source": source_states["ack"],
            "version": _version(store_paths["ack"]),
            "expected_version": contracts.ack_version,
            "preserved_rows": preserved["ack"],
        },
        "maxim-metrics": {
            "source": source_states["maxim-metrics"],
            "version": None,
            "shape": "current",
            "preserved_rows": preserved["maxim-metrics"],
        },
        "projection": {
            "source": source_states["projection"],
            "version": _version(store_paths["projection"]),
            "expected_version": contracts.projection_version,
            "preserved_rows": 0,
        },
    }
    for name in ("team", "ack", "projection"):
        if results[name]["version"] != results[name]["expected_version"]:
            raise UpgradeProofError(f"{name} did not reach its current version")
    return results


def rehearse_prior_stores(root: Path) -> dict[str, object]:
    """Materialize tagged stores and prove the current writers open them."""
    manifest = _validate_manifest(prior_store_manifest(root))
    release = manifest["releases"][0]
    assert isinstance(release, dict)
    stores = release["stores"]
    assert isinstance(stores, dict)

    with tempfile.TemporaryDirectory(prefix="spice-prior-store-proof-") as raw:
        scratch = Path(raw)
        state_backend = scratch / "backend"
        store_paths = {
            "team": scratch / "spiceteams.sqlite3",
            "ack": scratch / "spiceacks.sqlite3",
            "maxim-metrics": (
                state_backend / "shared" / "data" / "spicemaxims.sqlite3"
            ),
            "projection": scratch / "spiceprojections.sqlite3",
        }
        before, source_states = _materialize_prior_fixtures(stores, store_paths)
        contracts = _open_with_current_writers(scratch, state_backend, store_paths)
        results = _current_store_results(store_paths, before, source_states, contracts)
    return {
        "schema_version": SCHEMA_VERSION,
        "release": {"tag": release["tag"], "commit": release["commit"]},
        "stores": results,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    actions = parser.add_subparsers(dest="action", required=True)
    export = actions.add_parser("export")
    export.add_argument("--repository", required=True, type=Path)
    export.add_argument("--output", required=True, type=Path)
    export.add_argument("--quiet", action="store_true")
    artifact = actions.add_parser("export-artifact")
    artifact.add_argument("--repository", required=True, type=Path)
    artifact.add_argument("--output", required=True, type=Path)
    artifact.add_argument("--quiet", action="store_true")
    rehearse = actions.add_parser("rehearse")
    rehearse.add_argument("--root", default=Path.cwd(), type=Path)
    rehearse.set_defaults(quiet=False)
    arguments = parser.parse_args()
    try:
        if arguments.action == "export":
            payload = export_prior_store_manifest(
                arguments.repository, arguments.output
            )
        elif arguments.action == "export-artifact":
            payload = export_prior_artifact(arguments.repository, arguments.output)
        else:
            payload = rehearse_prior_stores(arguments.root)
    except (OSError, sqlite3.DatabaseError, RuntimeError, ValueError) as exc:
        print(f"prior-store upgrade proof: {exc}", file=sys.stderr)
        return 2
    if not arguments.quiet:
        print(json.dumps(payload, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

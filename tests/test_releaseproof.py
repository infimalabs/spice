"""Hermetic source and toolchain boundary contracts."""

from __future__ import annotations

import hashlib
import json
import os
import stat
import subprocess
import sys
import zipfile
from pathlib import Path
from typing import cast

import pytest

from tests.test_releasepackaging import DECLARED_PACKAGING_PINS, TOOLCHAIN_RELATIVE_PATH
from tests.test_releaseproofhelpers import (
    CONTAINERFILE,
    EVIDENCE,
    HOSTNATIVE,
    INSTALLED_UPGRADE,
    PROJECT_ROOT,
    REHEARSAL,
    SOURCE_EXPORTER,
    SOURCE_INITIALIZER,
    UPGRADE,
    _file_inventory,
    _git,
    _source_repository,
    _test_sha256,
    _write_test_wheel,
)


def _write_ignored_residue(repository: Path, marker: str) -> None:
    for relative in (
        ".cache/download",
        ".spice/operations.sqlite3",
        ".venv/bin/python",
        "build/stale.py",
        "dist/stale.whl",
        "node_modules/playwright/cache",
    ):
        path = repository / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(f"{marker}:{relative}\n", encoding="utf-8")


def _export(repository: Path, destination: Path) -> dict[str, object]:
    completed = subprocess.run(
        [str(SOURCE_EXPORTER), str(destination)],
        cwd=repository,
        check=True,
        capture_output=True,
        text=True,
    )
    assert completed.stdout.strip() == str(destination)
    return json.loads(
        (destination / ".release-proof/source.json").read_text(encoding="utf-8")
    )


def _content_identity(root: Path) -> list[tuple[str, int, str]]:
    identity = []
    for path in sorted(
        candidate for candidate in root.rglob("*") if candidate.is_file()
    ):
        identity.append(
            (
                path.relative_to(root).as_posix(),
                stat.S_IMODE(path.stat().st_mode),
                hashlib.sha256(path.read_bytes()).hexdigest(),
            )
        )
    return identity


def _initialize(context: Path) -> dict[str, object]:
    completed = subprocess.run(
        [sys.executable, str(SOURCE_INITIALIZER), str(context)],
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(completed.stdout)


def test_source_export_contains_exact_head_and_is_stable_across_ignored_residue(
    tmp_path,
):
    repository, source = _source_repository(tmp_path)
    _write_ignored_residue(repository, "first run")
    first = tmp_path / "context-one"
    first_provenance = _export(repository, first)

    _write_ignored_residue(repository, "second run")
    second = tmp_path / "context-two"
    second_provenance = _export(repository, second)

    expected_provenance = {"schema_version": 1, "source": source}
    assert first_provenance == expected_provenance
    assert second_provenance == expected_provenance
    assert _file_inventory(first) == [
        ".gitignore",
        ".release-proof/prior-artifact/manifest.json",
        ".release-proof/prior-stores.json",
        ".release-proof/source.json",
        "payload.txt",
    ]
    prior_stores = json.loads(
        (first / ".release-proof/prior-stores.json").read_text(encoding="utf-8")
    )
    assert prior_stores == {"schema_version": 1, "releases": []}
    prior_artifact = json.loads(
        (first / ".release-proof/prior-artifact/manifest.json").read_text(
            encoding="utf-8"
        )
    )
    assert prior_artifact == {
        "schema_version": 1,
        "release": None,
        "state": "absent",
        "wheel": None,
    }
    assert _content_identity(first) == _content_identity(second)


def test_synthetic_repository_keeps_source_identity_and_clean_git_semantics(tmp_path):
    repository, source = _source_repository(tmp_path)
    _write_ignored_residue(repository, "host-only")
    first = tmp_path / "context-one"
    second = tmp_path / "context-two"
    _export(repository, first)
    _export(repository, second)

    first_identities = _initialize(first)
    second_identities = _initialize(second)
    expected = {
        "schema_version": 1,
        "source": source,
        "synthetic": {
            "commit": _git(first, "rev-parse", "HEAD^{commit}"),
            "tree": _git(first, "rev-parse", "HEAD^{tree}"),
        },
    }

    assert first_identities == expected
    assert second_identities == expected
    assert (
        json.loads(
            (first / ".git/release-proof-identities.json").read_text(encoding="utf-8")
        )
        == expected
    )
    assert _git(first, "status", "--porcelain=v1", "--untracked-files=all") == ""
    assert _git(first, "ls-files", ".release-proof") == "\n".join(
        [
            ".release-proof/prior-artifact/manifest.json",
            ".release-proof/prior-stores.json",
            ".release-proof/source.json",
        ]
    )
    assert _git(first, "log", "-1", "--format=%s") == (
        "Synthetic release-proof source snapshot"
    )


def test_synthetic_repository_preserves_a_force_tracked_ignored_path(tmp_path):
    repository, source = _source_repository(tmp_path, tracked_ignored=True)
    context = tmp_path / "context"
    _export(repository, context)

    identities = _initialize(context)

    assert identities["source"] == source
    assert _git(context, "ls-files", "tracked-ignored.txt") == "tracked-ignored.txt"
    assert (context / "tracked-ignored.txt").read_text(encoding="utf-8") == (
        "tracked source despite its ignore rule\n"
    )
    assert _git(context, "cat-file", "-t", str(source["tree"])) == "tree"
    assert _git(context, "status", "--porcelain=v1", "--untracked-files=all") == ""


def test_synthetic_repository_preserves_sha256_object_format_and_provenance(
    tmp_path,
):
    repository, source = _source_repository(tmp_path, object_format="sha256")
    context = tmp_path / "context"
    _export(repository, context)

    identities = _initialize(context)
    synthetic = cast(dict[str, object], identities["synthetic"])

    assert identities["source"] == source
    assert _git(context, "rev-parse", "--show-object-format") == "sha256"
    assert {
        len(str(source["commit"])),
        len(str(source["tree"])),
        len(str(synthetic["commit"])),
        len(str(synthetic["tree"])),
    } == {64}
    assert (
        source["commit"] == synthetic["commit"],
        source["tree"] == synthetic["tree"],
    ) == (False, False)
    assert _git(context, "status", "--porcelain=v1", "--untracked-files=all") == ""


def test_source_export_binds_the_latest_tag_reachable_from_head_parent(tmp_path):
    repository, _source = _source_repository(tmp_path)
    tagged_sources = {
        # The exporter builds whatever a tag points at, so the tagged commit has
        # to be a real distribution. uv's own backend keeps that hermetic: it
        # ships inside the uv binary, so no build requirement is fetched.
        "pyproject.toml": (
            "[build-system]\n"
            'requires = ["uv_build"]\n'
            'build-backend = "uv_build"\n\n'
            "[project]\n"
            'name = "spice-harness"\n'
            'version = "1.2.3"\n\n'
            "[tool.uv.build-backend]\n"
            'module-root = ""\n'
            'module-name = "spice"\n'
        ),
        "spice/__init__.py": "",
        "spice/serve/team/schema.py": 'TEAM_SCHEMA = "CREATE TABLE team_old(id)"\n',
        "spice/mail/ackstate.py": (
            'ACK_STATE_TABLE_SQL = "CREATE TABLE ack_old(id)"\n'
            'ACK_STATE_INDEX_SQL = "CREATE INDEX ack_old_idx ON ack_old(id)"\n'
        ),
        "spice/agent/maximmetrics.py": (
            'MAXIM_METRICS_TABLE_SQL = "CREATE TABLE maxim_metric_events(id)"\n'
            "MAXIM_METRICS_EVENT_INDEX_SQL = "
            '"CREATE INDEX maxim_event ON maxim_metric_events(id)"\n'
            "MAXIM_METRICS_RECURRENCE_INDEX_SQL = "
            '"CREATE INDEX maxim_recurrence ON maxim_metric_events(id)"\n'
            "MAXIM_METRICS_FIRE_RECENCY_INDEX_SQL = "
            '"CREATE INDEX maxim_recency ON maxim_metric_events(id)"\n'
        ),
    }
    for relative, source_text in tagged_sources.items():
        path = repository / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(source_text, encoding="utf-8")
    _git(repository, "add", "--all")
    _git(repository, "commit", "--quiet", "--message", "supported predecessor")
    predecessor = _git(repository, "rev-parse", "HEAD^{commit}")
    _git(repository, "tag", "v1.2.3")
    (repository / "payload.txt").write_text("current head\n", encoding="utf-8")
    _git(repository, "add", "payload.txt")
    _git(repository, "commit", "--quiet", "--message", "candidate")
    _git(repository, "tag", "v9.9.9")

    context = tmp_path / "context"
    _export(repository, context)
    manifest = json.loads(
        (context / ".release-proof/prior-stores.json").read_text(encoding="utf-8")
    )

    artifact = json.loads(
        (context / ".release-proof/prior-artifact/manifest.json").read_text(
            encoding="utf-8"
        )
    )
    carried = context / ".release-proof/prior-artifact" / artifact["wheel"]["name"]

    assert (artifact["state"], artifact["release"]) == (
        "built",
        {"tag": "v1.2.3", "commit": predecessor},
    )
    assert _test_sha256(carried) == artifact["wheel"]["sha256"]
    # uv build seeds its output directory with a catch-all ignore rule, which
    # would silently keep the carried wheel out of the synthetic commit.
    assert sorted(path.name for path in carried.parent.iterdir()) == [
        "manifest.json",
        carried.name,
    ]

    release = manifest["releases"][0]
    assert (release["tag"], release["commit"]) == ("v1.2.3", predecessor)
    assert set(release["stores"]) == {
        "team",
        "ack",
        "maxim-metrics",
        "projection",
    }
    assert release["stores"]["projection"] == {"state": "absent", "sources": {}}
    for name in ("team", "ack", "maxim-metrics"):
        assert release["stores"][name]["state"] == "source"
        assert all(
            path.endswith(".py") and isinstance(source_text, str) and source_text
            for path, source_text in release["stores"][name]["sources"].items()
        )


def test_prior_artifact_export_refuses_a_release_tag_it_cannot_build(tmp_path):
    """A tagged predecessor that will not build is broken, not absent.

    Recording it as absent would look identical to a repository that has cut no
    release at all, and the in-place upgrade gate would then refuse it with the
    wrong reason far downstream. The build command's own stderr is the answer.
    """
    repository, _source = _source_repository(tmp_path)
    _git(repository, "tag", "v1.2.3")
    (repository / "payload.txt").write_text("current head\n", encoding="utf-8")
    _git(repository, "add", "payload.txt")
    _git(repository, "commit", "--quiet", "--message", "candidate")

    with pytest.raises(UPGRADE.UpgradeProofError) as failure:
        UPGRADE.export_prior_artifact(repository, tmp_path / "artifact")

    assert str(failure.value).startswith("building the v1.2.3 wheel failed (")


def test_prior_release_rehearsal_opens_every_store_and_preserves_rows():
    evidence = UPGRADE.rehearse_prior_stores(PROJECT_ROOT)

    assert evidence["release"]["tag"] == "v0.28.0"
    assert set(evidence["stores"]) == {
        "team",
        "ack",
        "maxim-metrics",
        "projection",
    }
    assert (
        evidence["stores"]["team"]["version"]
        == evidence["stores"]["team"]["expected_version"]
    )
    assert (
        evidence["stores"]["ack"]["version"]
        == evidence["stores"]["ack"]["expected_version"]
    )
    assert evidence["stores"]["projection"]["source"] == "source"
    assert (
        evidence["stores"]["projection"]["version"]
        == evidence["stores"]["projection"]["expected_version"]
    )
    assert evidence["stores"]["maxim-metrics"]["shape"] == "current"
    for name in ("team", "ack", "maxim-metrics"):
        assert evidence["stores"][name]["preserved_rows"] > 0


def test_prior_release_manifest_rejects_missing_or_unclassified_stores():
    manifest = UPGRADE.prior_store_manifest(PROJECT_ROOT)
    missing = json.loads(json.dumps(manifest))
    del missing["releases"][0]["stores"]["ack"]
    with pytest.raises(UPGRADE.UpgradeProofError, match="inventory"):
        UPGRADE._validate_manifest(missing)

    absent = json.loads(json.dumps(manifest))
    absent["releases"][0]["stores"]["team"] = {"state": "absent", "sources": {}}
    with pytest.raises(UPGRADE.UpgradeProofError, match="required prior store"):
        UPGRADE._validate_manifest(absent)


def _artifact_manifest(root: Path, payload: dict[str, object]) -> None:
    directory = root / REHEARSAL.PRIOR_ARTIFACT_DIRECTORY
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "manifest.json").write_text(
        json.dumps(payload, sort_keys=True), encoding="utf-8"
    )


def test_in_place_upgrade_refuses_a_predecessor_that_was_never_built(tmp_path):
    _artifact_manifest(
        tmp_path,
        {"schema_version": 1, "release": None, "state": "absent", "wheel": None},
    )
    with pytest.raises(REHEARSAL.RehearsalError, match="built predecessor artifact"):
        REHEARSAL._carried_predecessor(tmp_path)


def test_in_place_upgrade_refuses_a_manifest_naming_an_uncarried_wheel(tmp_path):
    _artifact_manifest(
        tmp_path,
        {
            "schema_version": 1,
            "release": {"tag": "v0.27.0", "commit": "0" * 40},
            "state": "built",
            "wheel": {"name": "spice_harness-0.27.0-py3-none-any.whl", "sha256": "0"},
        },
    )
    with pytest.raises(REHEARSAL.RehearsalError, match="missing"):
        REHEARSAL._carried_predecessor(tmp_path)


@pytest.mark.parametrize(
    "wheel",
    [None, {}, {"sha256": "0"}, {"name": ""}, {"name": "../escape.whl"}],
    ids=["null", "empty", "unnamed", "blank-name", "traversing-name"],
)
def test_in_place_upgrade_cites_a_manifest_that_contradicts_itself(tmp_path, wheel):
    """A built state with no usable wheel name must read as a corrupt carry.

    Every one of these already exited non-zero, but as a raw TypeError, KeyError
    or AttributeError out of a container build log, where a corrupt carry and a
    bug in the gate look exactly alike.
    """
    _artifact_manifest(
        tmp_path,
        {
            "schema_version": 1,
            "release": {"tag": "v0.27.0", "commit": "0" * 40},
            "state": "built",
            "wheel": wheel,
        },
    )

    with pytest.raises(REHEARSAL.RehearsalError) as failure:
        REHEARSAL._carried_predecessor(tmp_path)

    assert str(failure.value).endswith(
        f"{tmp_path / REHEARSAL.PRIOR_ARTIFACT_MANIFEST} records wheel={wheel!r}"
    )


def test_in_place_upgrade_refuses_a_wheel_that_exists_outside_the_carry(tmp_path):
    """The traversal case above is only cited when the escape target is absent.

    Put a real file where '../escape.whl' resolves and the unguarded expression
    stops raising anything at all: it finds a file, returns it, and the gate
    installs a wheel from outside .release-proof/prior-artifact/ that no carried
    manifest ever hashed. Refusing the name is what prevents that, not the
    later is_file() check, so this asserts the escape target is untouched.
    """
    escape = tmp_path / REHEARSAL.PRIOR_ARTIFACT_DIRECTORY.parent / "escape.whl"
    escape.parent.mkdir(parents=True, exist_ok=True)
    escape.write_bytes(b"a wheel nobody carried\n")
    _artifact_manifest(
        tmp_path,
        {
            "schema_version": 1,
            "release": {"tag": "v0.27.0", "commit": "0" * 40},
            "state": "built",
            "wheel": {"name": "../escape.whl", "sha256": "0"},
        },
    )

    with pytest.raises(REHEARSAL.RehearsalError) as failure:
        REHEARSAL._carried_predecessor(tmp_path)

    assert str(failure.value).endswith(
        f"{tmp_path / REHEARSAL.PRIOR_ARTIFACT_MANIFEST} records "
        "wheel={'name': '../escape.whl', 'sha256': '0'}"
    )
    assert escape.read_bytes() == b"a wheel nobody carried\n"


def test_in_place_upgrade_cites_a_manifest_that_is_not_an_object(tmp_path):
    _artifact_manifest(tmp_path, cast(dict[str, object], ["not", "a", "manifest"]))

    with pytest.raises(REHEARSAL.RehearsalError) as failure:
        REHEARSAL._carried_predecessor(tmp_path)

    assert str(failure.value).endswith(
        f"{tmp_path / REHEARSAL.PRIOR_ARTIFACT_MANIFEST} holds list"
    )


def test_in_place_upgrade_rejects_an_installed_path_outside_scratch(tmp_path):
    root = tmp_path / "source"
    root.mkdir()
    venv = tmp_path / "venv"
    venv.mkdir()
    repository = tmp_path / "scratch" / "repository"
    repository.mkdir(parents=True)
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()
    report = {
        "import_origin": str(venv / "site-packages" / "spice" / "store.py"),
        "paths": {REHEARSAL.UPGRADE_TEAM_STORE: str(elsewhere / "store.sqlite3")},
    }
    with pytest.raises(REHEARSAL.RehearsalError, match="escaped.*repository"):
        REHEARSAL._validated_installed_paths(
            report,
            expected={REHEARSAL.UPGRADE_TEAM_STORE},
            root=root,
            repository=repository,
            venv=venv,
        )


def test_in_place_upgrade_fails_closed_on_an_unrecorded_state_file(tmp_path):
    state = tmp_path / "upgrade-state" / ".git" / ".spice" / "data"
    state.mkdir(parents=True)
    (state / REHEARSAL.UPGRADE_TEAM_STORE).write_bytes(b"")
    (state / "spicerogue.sqlite3").write_bytes(b"")
    paths = {REHEARSAL.UPGRADE_TEAM_STORE: state / REHEARSAL.UPGRADE_TEAM_STORE}
    with pytest.raises(REHEARSAL.RehearsalError, match="spicerogue.sqlite3"):
        REHEARSAL._assert_rehearsed_state_files(
            paths, required={REHEARSAL.UPGRADE_TEAM_STORE}
        )


def test_in_place_upgrade_refuses_a_governed_store_with_no_rehearsal(tmp_path):
    state = tmp_path / "upgrade-state" / ".git" / ".spice" / "data"
    state.mkdir(parents=True)
    (state / REHEARSAL.UPGRADE_TEAM_STORE).write_bytes(b"")
    paths = {name: state / name for name in REHEARSAL.UPGRADE_GOVERNED_STATE}

    with pytest.raises(REHEARSAL.RehearsalError, match="did not exercise"):
        REHEARSAL._assert_rehearsed_state_files(
            paths, required=set(REHEARSAL.UPGRADE_GOVERNED_STATE)
        )


def test_in_place_upgrade_accepts_the_rehearsed_foreign_task_store(tmp_path):
    state = tmp_path / "upgrade-state" / ".git" / ".spice" / "data"
    state.mkdir(parents=True)
    required = {REHEARSAL.UPGRADE_TEAM_STORE, "taskchampion.sqlite3"}
    paths = {name: state / name for name in required}
    for path in paths.values():
        path.write_bytes(b"")

    inventory = REHEARSAL._assert_rehearsed_state_files(paths, required=required)

    assert inventory == sorted(required)


def test_in_place_upgrade_rejects_source_shadowing_the_installed_probe(tmp_path):
    root = tmp_path / "source"
    root.mkdir()
    repository = tmp_path / "repository"
    repository.mkdir()
    venv = tmp_path / "venv"
    venv.mkdir()
    report = {
        "import_origin": str(root / "spice" / "serve" / "team" / "store.py"),
        "paths": {},
    }

    with pytest.raises(REHEARSAL.RehearsalError, match="imported Spice from source"):
        REHEARSAL._validated_installed_paths(
            report,
            expected=set(),
            root=root,
            repository=repository,
            venv=venv,
        )


def test_in_place_upgrade_rejects_destructive_authority_replacement():
    seeded = {
        name: {"preserved": [f"{name}-before"]}
        for name in REHEARSAL.UPGRADE_AUTHORITY_STATE
    }
    upgraded = {
        name: {
            "preserved": [f"{name}-before"],
            "written": [f"{name}-after"],
        }
        for name in REHEARSAL.UPGRADE_AUTHORITY_STATE
    }
    before = {
        name: (1, index)
        for index, name in enumerate(REHEARSAL.UPGRADE_AUTHORITY_STATE, start=1)
    }
    after = dict(before)
    after[REHEARSAL.UPGRADE_TEAM_STORE] = (1, 999)

    with pytest.raises(
        REHEARSAL.RehearsalError,
        match=f"reinitialized authority store {REHEARSAL.UPGRADE_TEAM_STORE}",
    ):
        REHEARSAL._assert_authority_preserved(seeded, upgraded, before, after)


def test_in_place_upgrade_inventory_answers_to_current_source():
    """The stores this repository declares are all accounted for right now."""
    declared = REHEARSAL._assert_state_inventory_is_declared(PROJECT_ROOT)

    assert set(declared) == set(REHEARSAL.UPGRADE_GOVERNED_STATE) | {
        "taskchampion.sqlite3"
    }


def test_in_place_upgrade_fails_closed_on_an_undeclared_new_store(tmp_path):
    """A store added lazily never appears in a run, so source is the check."""
    package = tmp_path / "spice" / "newthing"
    package.mkdir(parents=True)
    declared = [
        *REHEARSAL.UPGRADE_GOVERNED_STATE,
        "taskchampion.sqlite3",
        "spicenewthing.sqlite3",
    ]
    (package / "store.py").write_text(
        "".join(
            f'STORE_{index}_DATABASE_FILENAME = "{name}"\n'
            for index, name in enumerate(declared)
        ),
        encoding="utf-8",
    )

    with pytest.raises(REHEARSAL.RehearsalError) as failure:
        REHEARSAL._assert_state_inventory_is_declared(tmp_path)

    assert "spicenewthing.sqlite3" in str(failure.value)


def test_in_place_upgrade_fails_when_governed_store_has_no_rehearsal(monkeypatch):
    registry = dict(REHEARSAL.UPGRADE_REHEARSAL_BY_STORE)
    registry.pop(REHEARSAL.UPGRADE_TEAM_STORE)
    monkeypatch.setattr(REHEARSAL, "UPGRADE_REHEARSAL_BY_STORE", registry)

    with pytest.raises(REHEARSAL.RehearsalError, match="registry drifted"):
        REHEARSAL._assert_state_inventory_is_declared(PROJECT_ROOT)


def test_in_place_upgrade_fails_when_governed_inventory_alone_grows(monkeypatch):
    monkeypatch.setattr(
        REHEARSAL,
        "UPGRADE_GOVERNED_STATE",
        (*REHEARSAL.UPGRADE_GOVERNED_STATE, "spiceunrehearsed.sqlite3"),
    )

    with pytest.raises(REHEARSAL.RehearsalError, match="registry drifted"):
        REHEARSAL._assert_state_inventory_is_declared(PROJECT_ROOT)


def test_in_place_upgrade_refuses_a_tree_that_declares_no_store(tmp_path):
    """An empty read is a broken probe, not a clean inventory."""
    (tmp_path / "spice").mkdir()

    with pytest.raises(REHEARSAL.RehearsalError, match="nothing to answer to"):
        REHEARSAL._assert_state_inventory_is_declared(tmp_path)


def test_in_place_upgrade_is_a_registered_release_proof_check():
    assert "in-place-upgrade" in REHEARSAL.CHECKS


def test_prior_release_rehearsal_detects_reversed_team_adoption_order(monkeypatch):
    from spice.serve.team import store

    monkeypatch.setattr(
        store,
        "TEAM_AUTHORITY_SCHEMA_VERSION",
        store.TEAM_AUTHORITY_SCHEMA_VERSION - 1,
    )

    with pytest.raises(store.SpiceError, match="newer schema version"):
        UPGRADE.rehearse_prior_stores(PROJECT_ROOT)


def test_installed_upgrade_names_team_store_when_adoption_is_reversed(
    tmp_path, monkeypatch
):
    from spice.errors import SpiceError
    from spice.serve.team import store

    class RefusingTeamStore:
        def team_state(self, _team_id):
            raise SpiceError("newer schema version")

    monkeypatch.setattr(store, "ServeTeamStore", RefusingTeamStore)

    with pytest.raises(
        SystemExit,
        match=r"spiceteams\.sqlite3: newer schema version",
    ):
        INSTALLED_UPGRADE._authority_verify_and_write(tmp_path)


def test_artifact_upgrade_verifies_named_authorities_before_console_reads(
    tmp_path, monkeypatch
):
    repository = tmp_path / "repository"
    repository.mkdir()
    venv = tmp_path / "venv"
    venv.mkdir()
    seed = REHEARSAL._UpgradeSeed(
        venv=venv,
        python=venv / "bin" / "python",
        console=venv / "bin" / "spice",
        repository=repository,
        environment={},
        facts={},
        paths={},
        identities={},
        versions={},
        tasks=["UPGRADE-1kH8Proof"],
    )

    def fake_run(command, **_kwargs):
        if command[0] != "uv":
            raise AssertionError("console read ran before named authority verification")

    def fake_probe(_python, _root, _repository, action, _environment, _failures):
        if action == "paths":
            return {}
        raise REHEARSAL.RehearsalError(
            "spiceteams.sqlite3: team authority database was written by newer schema"
        )

    monkeypatch.setattr(REHEARSAL, "_run", fake_run)
    monkeypatch.setattr(REHEARSAL, "_run_installed_probe", fake_probe)
    monkeypatch.setattr(REHEARSAL, "_validated_installed_paths", lambda *_a, **_k: {})

    with pytest.raises(REHEARSAL.RehearsalError, match=r"spiceteams\.sqlite3"):
        REHEARSAL._upgrade_installed_package(
            tmp_path,
            tmp_path / "current.whl",
            tmp_path,
            seed,
            None,
        )


def test_rehearsal_declares_every_gate_and_runs_during_the_container_build():
    containerfile = CONTAINERFILE.read_text(encoding="utf-8")

    assert REHEARSAL.PYTHON_GATE_COMMAND == ("uv", "run", "--locked", "pytest")
    assert REHEARSAL.RUFF_GATE_COMMAND == (
        "uv",
        "run",
        "--locked",
        "ruff",
        "check",
        ".",
    )
    assert REHEARSAL.PRIOR_UPGRADE_GATE_COMMAND == (
        "uv",
        "run",
        "--locked",
        "python",
        "release-proof/upgrade.py",
        "rehearse",
        "--root",
        ".",
    )
    assert REHEARSAL.BROWSER_GATE_COMMAND == (
        "node",
        "tests/browser/run_release_smokes.js",
    )
    assert REHEARSAL.MUTATION_GATE_COMMAND == (
        "uv",
        "run",
        "--locked",
        "spice",
        "study",
        "mutations",
        "spice/config/layers.py",
        "--test",
        "tests/test_configlayer.py",
        "--max-mutants",
        "20",
        "--timeout",
        "30",
        "--ratchet",
        "tests/mutation-ratchet.json",
        "--json",
    )
    assert REHEARSAL.CHECKS == (
        "packaging-toolchain",
        "python",
        "ruff",
        "prior-store-upgrades",
        "browser-release-manifest",
        "deterministic-mutation-cohort",
        "build-sdist",
        "build-wheel",
        "metadata",
        "isolated-install",
        "installed-imports",
        "installed-console",
        "in-place-upgrade",
        "sdist-rebuild",
        "wheel-member-content",
        "clean-worktree",
    )
    assert (
        "RUN python3 release-proof/rehearse.py --artifacts /proof/artifacts"
        in containerfile
    )


def test_canonical_artifacts_are_built_once_outside_the_source_and_checked_exactly(
    tmp_path, monkeypatch
):
    root = tmp_path / "source"
    root.mkdir()
    artifacts = tmp_path / "artifacts"
    calls: list[tuple[tuple[str, ...], Path]] = []

    def build_tools(command, *, cwd, **_kwargs):
        argv = tuple(command)
        calls.append((argv, cwd))
        if "--sdist" in argv:
            (artifacts / "spice_harness-1.2.3.tar.gz").write_bytes(b"sdist\n")
        if "--wheel" in argv:
            _write_test_wheel(
                artifacts / "spice_harness-1.2.3-py3-none-any.whl",
                {"spice/__init__.py": b"namespace package\n"},
                year=2024,
            )
        return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

    monkeypatch.setattr(REHEARSAL, "_run", build_tools)

    sdist, wheel = REHEARSAL._build_canonical_artifacts(root, artifacts, "1.2.3")

    assert (sdist.name, wheel.name) == (
        "spice_harness-1.2.3.tar.gz",
        "spice_harness-1.2.3-py3-none-any.whl",
    )
    assert tuple(cwd for _command, cwd in calls) == (
        artifacts,
        artifacts,
        artifacts,
    )
    assert (
        sum("--sdist" in command for command, _cwd in calls),
        sum("--wheel" in command for command, _cwd in calls),
        calls[-1][0],
    ) == (
        1,
        1,
        (
            *REHEARSAL.packaging_python_command(root),
            "-m",
            "twine",
            "check",
            str(sdist),
            str(wheel),
        ),
    )


def test_git_private_records_resolve_from_a_linked_worktree(tmp_path):
    repository, _source = _source_repository(tmp_path)
    linked = tmp_path / "linked"
    _git(repository, "worktree", "add", "--detach", str(linked))
    record = Path(
        subprocess.run(
            ["git", "rev-parse", "--git-path", "release-proof-identities.json"],
            check=True,
            cwd=linked,
            capture_output=True,
            text=True,
        ).stdout.strip()
    )
    if not record.is_absolute():
        record = linked / record
    record.write_text(
        json.dumps({"schema_version": 1, "source": {"commit": "abc"}}),
        encoding="utf-8",
    )

    resolved = REHEARSAL.git_private_path(linked, "release-proof-identities.json")
    payload = REHEARSAL._load_git_private_json(linked, "release-proof-identities.json")

    assert ((linked / ".git").is_file(), resolved.resolve(), payload) == (
        True,
        record.resolve(),
        {"schema_version": 1, "source": {"commit": "abc"}},
    )


def test_missing_git_private_record_names_the_script_that_writes_it(tmp_path):
    repository, _source = _source_repository(tmp_path)

    with pytest.raises(REHEARSAL.RehearsalError) as failure:
        REHEARSAL._load_git_private_json(repository, "release-proof-toolchain.json")

    message = str(failure.value)
    assert (
        "release-proof-toolchain.json" in message,
        "release-proof/toolchain.py" in message,
        REHEARSAL.GIT_PRIVATE_RECORD_PRODUCERS,
    ) == (
        True,
        True,
        {
            "release-proof-identities.json": "release-proof/init-source.py",
            "release-proof-toolchain.json": "release-proof/toolchain.py",
        },
    )


def test_rehearsal_materializes_only_the_committed_source_boundary(tmp_path):
    repository, _source = _source_repository(tmp_path)
    ignored = repository / "build" / "lib" / "spice" / "stale.py"
    ignored.parent.mkdir(parents=True)
    ignored.write_text("host-only residue\n", encoding="utf-8")
    scratch = tmp_path / "scratch"
    scratch.mkdir()

    materialized = REHEARSAL._materialize_committed_source(repository, scratch)

    assert _content_identity(materialized) == [
        (
            ".gitignore",
            0o644,
            hashlib.sha256((repository / ".gitignore").read_bytes()).hexdigest(),
        ),
        (
            "payload.txt",
            0o644,
            hashlib.sha256((repository / "payload.txt").read_bytes()).hexdigest(),
        ),
    ]
    assert ((materialized / "build" / "lib" / "spice" / "stale.py").exists(),) == (
        False,
    )


def test_failure_artifacts_are_deterministic_bounded_and_secret_redacted(tmp_path):
    store = EVIDENCE.FailureArtifactStore(tmp_path)
    environment = {
        "PATH": os.environ["PATH"],  # env-policy: allow
        "SERVICE_TOKEN": "environment-secret-value",
    }
    token_url = "https://user:pass@example.test/api?access_token=url-secret&safe=yes"

    for index in range(EVIDENCE.MAX_FAILURE_ARTIFACTS + 3):
        store.record(
            "browser gate",
            ["probe", token_url],
            index + 1,
            "environment-secret-value\n" + ("x" * EVIDENCE.MAX_FAILURE_BYTES),
            f"request failed: {token_url}\n",
            environment=environment,
        )

    files = sorted((tmp_path / EVIDENCE.FAILURE_DIRNAME).glob("*.log"))
    last = files[-1].read_text(encoding="utf-8")
    assert (
        len(files),
        files[0].name,
        files[-1].name,
        max(path.stat().st_size for path in files) <= EVIDENCE.MAX_FAILURE_BYTES,
    ) == (
        EVIDENCE.MAX_FAILURE_ARTIFACTS,
        "01-browser-gate.log",
        "08-overflow.log",
        True,
    )
    assert (
        "environment-secret-value" in last,
        "url-secret" in last,
        "user:pass" in last,
        "<redacted-env:SERVICE_TOKEN>" in last,
        "access_token=%3Credacted%3E" in last,
    ) == (False, False, False, True, True)


def test_url_credentials_redact_userinfo_query_and_fragment_values():
    diagnostic = (
        "request="
        "https://user:pass@example.test/callback?api_key=query-secret&safe=yes"
        "#/route?access_token=fragment-secret&state=ok\n"
        "redirect=https://example.test/done#token=second-fragment&tab=summary"
    )

    assert EVIDENCE.redact_text(diagnostic, {}) == (
        "request="
        "https://<redacted>@example.test/callback?api_key=%3Credacted%3E&safe=yes"
        "#/route?access_token=%3Credacted%3E&state=ok\n"
        "redirect=https://example.test/done#token=%3Credacted%3E&tab=summary"
    )


def test_pytest_count_evidence_uses_the_final_summary():
    output = (
        "bringing up nodes...\n"
        "................................\n"
        "998 passed, 4 skipped, 2 xfailed, 7 deselected in 12.34s\n"
    )

    assert EVIDENCE.parse_pytest_counts(output) == {
        "passed": 998,
        "skipped": 4,
        "xfailed": 2,
        "deselected": 7,
        "total": 1004,
    }


def test_host_native_companion_records_macos_beside_unchanged_linux_proof(
    tmp_path, monkeypatch
):
    evidence_dir = tmp_path / "artifacts"
    evidence_dir.mkdir()
    container_path = evidence_dir / "release-proof.json"
    source_commit = _git(PROJECT_ROOT, "rev-parse", "HEAD^{commit}")
    container_path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "claim_boundary": {"operating_system": "linux"},
                "source_identity": {
                    "schema_version": 1,
                    "source": {
                        "commit": source_commit,
                        "tree": "0" * len(source_commit),
                        "commit_epoch": 1,
                    },
                    "synthetic": {
                        "commit": "1" * len(source_commit),
                        "tree": "2" * len(source_commit),
                    },
                },
            },
            sort_keys=True,
        )
        + "\n",
        encoding="utf-8",
    )
    container_before = container_path.read_bytes()

    def host_command(command, **_kwargs):
        if command[0] == "git":
            return subprocess.CompletedProcess(
                command,
                0,
                stdout=source_commit + "\n",
                stderr="",
            )
        audio = Path(command[2])
        audio.write_bytes(b"FORM\x00\x00native speech")
        return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

    monkeypatch.setattr(HOSTNATIVE.platform, "system", lambda: "Darwin")
    monkeypatch.setattr(HOSTNATIVE.platform, "release", lambda: "25.5.0")
    monkeypatch.setattr(HOSTNATIVE.platform, "machine", lambda: "arm64")
    monkeypatch.setattr(HOSTNATIVE.select, "kqueue", object(), raising=False)
    monkeypatch.setattr(HOSTNATIVE, "_run_command", host_command)
    monkeypatch.setattr(
        HOSTNATIVE,
        "_probe_kqueue_event",
        lambda: {
            "status": "passed",
            "backend": "kqueue",
            "production_path": "spice.serve.livebus._KqueueWatch",
            "event": "filesystem-write",
            "timeout_seconds": 5.0,
            "elapsed_ms": 12.5,
        },
    )
    monkeypatch.setattr(
        HOSTNATIVE,
        "_appearance",
        lambda _root, _failures: {"status": "passed", "style": "dark"},
    )

    report = HOSTNATIVE.collect_host_native_evidence(PROJECT_ROOT, evidence_dir)

    assert report["claim_boundary"] == {
        "operating_system": "macos",
        "container_operating_system": "linux",
        "container_evidence_unchanged": True,
    }
    assert report["source_identity"] == {
        "agreement": "exact",
        "checkout_head": source_commit,
        "container_source_commit": source_commit,
    }
    assert report["checks"] == {
        "kqueue-or-fsevents": {
            "status": "passed",
            "backend": "kqueue",
            "production_path": "spice.serve.livebus._KqueueWatch",
            "event": "filesystem-write",
            "timeout_seconds": 5.0,
            "elapsed_ms": 12.5,
        },
        "appearance": {"status": "passed", "style": "dark"},
        "speech": {
            "status": "passed",
            "backend": "/usr/bin/say",
            "bytes": len(b"FORM\x00\x00native speech"),
            "sha256": hashlib.sha256(b"FORM\x00\x00native speech").hexdigest(),
        },
    }
    assert (
        container_path.read_bytes(),
        json.loads(
            (evidence_dir / "release-proof-macos.json").read_text(encoding="utf-8")
        ),
    ) == (container_before, report)


@pytest.mark.skipif(sys.platform != "darwin", reason="requires native kqueue")
def test_host_native_probe_observes_a_real_bounded_kqueue_event():
    result = HOSTNATIVE._probe_kqueue_event()

    assert result["status"] == "passed"
    assert result["backend"] == "kqueue"
    assert result["production_path"] == "spice.serve.livebus._KqueueWatch"
    assert result["event"] == "filesystem-write"
    assert result["timeout_seconds"] == HOSTNATIVE.KQUEUE_EVENT_TIMEOUT_SECONDS
    assert (
        0
        <= result["elapsed_ms"]
        <= (
            (
                HOSTNATIVE.KQUEUE_EVENT_TIMEOUT_SECONDS
                + HOSTNATIVE.LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S
            )
            * 1000
        )
    )


def test_mutation_rehearsal_requires_the_exact_committed_cohort(tmp_path):
    ratchet = tmp_path / "tests" / "mutation-ratchet.json"
    ratchet.parent.mkdir()
    expected = {
        "spice/config/layers.py": {
            "killed": 13,
            "mutants": 20,
            "score": 0.65,
            "survived": 7,
            "timed_out": 0,
        }
    }
    ratchet.write_text(
        json.dumps({"version": 1, "modules": expected}),
        encoding="utf-8",
    )
    output = json.dumps(
        {
            "artifactKind": "spice.study.mutations",
            "reports": [
                {
                    "path": "spice/config/layers.py",
                    **expected["spice/config/layers.py"],
                    "results": [],
                }
            ],
            "ratchetRegressions": [],
        }
    )

    assert REHEARSAL.verify_mutation_cohort(tmp_path, output) == expected


def test_wheel_member_comparison_ignores_only_outer_zip_container_bytes(tmp_path):
    canonical = tmp_path / "canonical.whl"
    rebuilt = tmp_path / "rebuilt.whl"
    members = {
        "spice/__init__.py": b"namespace package\n",
        "spice_harness.dist-info/METADATA": b"Name: spice-harness\n",
    }
    _write_test_wheel(
        canonical,
        members,
        year=2024,
        compression=zipfile.ZIP_STORED,
    )
    _write_test_wheel(
        rebuilt,
        members,
        year=2025,
        compression=zipfile.ZIP_DEFLATED,
    )

    assert (
        canonical.read_bytes() == rebuilt.read_bytes(),
        REHEARSAL.wheel_member_mismatches(canonical, rebuilt),
    ) == (False, [])


def test_wheel_member_comparison_catalogs_every_exact_delta(tmp_path):
    canonical = tmp_path / "canonical.whl"
    rebuilt = tmp_path / "rebuilt.whl"
    _write_test_wheel(
        canonical,
        {
            "changed.txt": b"canonical\n",
            "missing.txt": b"missing\n",
            "same.txt": b"same\n",
        },
        year=2024,
    )
    _write_test_wheel(
        rebuilt,
        {
            "changed.txt": b"rebuilt\n",
            "extra.txt": b"extra\n",
            "same.txt": b"same\n",
        },
        year=2025,
    )

    assert REHEARSAL.wheel_member_mismatches(canonical, rebuilt) == [
        {
            "kind": "missing-from-rebuilt",
            "member": "missing.txt",
            "canonical_sha256": hashlib.sha256(b"missing\n").hexdigest(),
        },
        {
            "kind": "extra-in-rebuilt",
            "member": "extra.txt",
            "rebuilt_sha256": hashlib.sha256(b"extra\n").hexdigest(),
        },
        {
            "kind": "content-changed",
            "member": "changed.txt",
            "canonical_sha256": hashlib.sha256(b"canonical\n").hexdigest(),
            "rebuilt_sha256": hashlib.sha256(b"rebuilt\n").hexdigest(),
        },
    ]


def test_rehearsal_receipt_carries_the_artifacts_it_installs_and_rebuilds(
    tmp_path, monkeypatch
):
    root, artifacts = _release_receipt_fixture(tmp_path)
    carried: list[tuple[str, Path]] = []
    rebuilt_hashes: list[str] = []

    monkeypatch.setattr(
        REHEARSAL,
        "_run_source_gates",
        lambda _root, _scratch, _failures: {
            "python": {"passed": 999, "total": 999},
            "ruff": {"passed": True},
            "browser": {
                "schemaVersion": 1,
                "counts": {"failed": 0, "passed": 45, "skipped": 1, "total": 46},
                "scenarios": [{"path": "serve_smoke.js", "status": "passed"}],
                "externalState": [
                    {"path": "live_smoke.js", "reason": "requires live state"}
                ],
            },
            "mutation": {"spice/config/layers.py": {"killed": 13, "mutants": 20}},
            "upgrades": {
                "schema_version": 1,
                "release": {"tag": "v0.27.0", "commit": "a" * 40},
                "stores": {
                    name: {"preserved_rows": 1}
                    for name in ("team", "ack", "maxim-metrics", "projection")
                },
            },
        },
    )

    monkeypatch.setattr(
        REHEARSAL,
        "verify_packaging_toolchain",
        lambda _root, _failures: dict(DECLARED_PACKAGING_PINS),
    )

    def build_canonical(_root, artifact_dir, version, _failures, *, project_root):
        carried.append(("built", project_root))
        sdist = artifact_dir / f"spice_harness-{version}.tar.gz"
        wheel = artifact_dir / f"spice_harness-{version}-py3-none-any.whl"
        sdist.write_bytes(b"canonical sdist\n")
        _write_test_wheel(
            wheel,
            {"spice/__init__.py": b"namespace package\n"},
            year=2024,
        )
        return sdist, wheel

    def validate_installed(_root, wheel, _version, _scratch, _failures):
        carried.append(("installed", wheel))

    def rebuild_from_sdist(sdist, version, scratch, _failures, *, project_root):
        carried.append(("rebuilt", project_root))
        carried.append(("rebuilt-from", sdist))
        rebuilt = scratch / f"spice_harness-{version}-py3-none-any.whl"
        _write_test_wheel(
            rebuilt,
            {"spice/__init__.py": b"namespace package\n"},
            year=2025,
        )
        rebuilt_hashes.append(_test_sha256(rebuilt))
        return rebuilt

    def clean_status(command, **_kwargs):
        stdout = f".git/{command[-1]}\n" if "--git-path" in command else ""
        return subprocess.CompletedProcess(command, 0, stdout=stdout, stderr="")

    monkeypatch.setattr(REHEARSAL, "_build_canonical_artifacts", build_canonical)
    monkeypatch.setattr(
        REHEARSAL,
        "_materialize_committed_source",
        lambda _root, _scratch: root,
    )
    monkeypatch.setattr(REHEARSAL, "_validate_installed_wheel", validate_installed)
    monkeypatch.setattr(
        REHEARSAL,
        "_validate_in_place_upgrade",
        lambda _root, wheel, _scratch, _failures: (
            carried.append(("upgraded", wheel))
            or {"adopted": {"from": 783663365, "to": 2, "store": "spiceteams.sqlite3"}}
        ),
    )
    monkeypatch.setattr(REHEARSAL, "_rebuild_wheel_from_sdist", rebuild_from_sdist)
    monkeypatch.setattr(REHEARSAL, "_run", clean_status)

    receipt = REHEARSAL.rehearse(root, artifacts)
    sdist = artifacts / "spice_harness-9.8.7.tar.gz"
    wheel = artifacts / "spice_harness-9.8.7-py3-none-any.whl"

    assert carried == [
        ("built", root),
        ("installed", wheel),
        ("upgraded", wheel),
        ("rebuilt", root),
        ("rebuilt-from", sdist),
    ]
    _assert_release_receipt(receipt, artifacts, sdist, wheel, rebuilt_hashes[0])


def test_packaging_preflight_stops_the_rehearsal_before_the_source_gates(
    tmp_path, monkeypatch
):
    """The whole point of moving this check first: fail in seconds, not minutes.

    The observed failure ran the full test, browser and mutation gates and only
    then discovered the artifact phase had no toolchain, so the source gates are
    wired to fail here if they are ever reached.
    """
    root, artifacts = _release_receipt_fixture(tmp_path)

    def refuse_source_gates(*_arguments):
        raise AssertionError("source gates ran before the packaging toolchain existed")

    monkeypatch.setattr(REHEARSAL, "_run_source_gates", refuse_source_gates)
    monkeypatch.setattr(REHEARSAL.shutil, "which", lambda _name: None)

    with pytest.raises(REHEARSAL.RehearsalError) as failure:
        REHEARSAL.rehearse(root, artifacts)

    assert "uv" in str(failure.value)


def _release_receipt_fixture(tmp_path: Path) -> tuple[Path, Path]:
    root = tmp_path / "source"
    git_dir = root / ".git"
    git_dir.mkdir(parents=True)
    (root / "release-proof").mkdir(parents=True)
    (root / TOOLCHAIN_RELATIVE_PATH).write_text(
        json.dumps({"schema_version": 1, "pinned": dict(DECLARED_PACKAGING_PINS)}),
        encoding="utf-8",
    )
    (root / "pyproject.toml").write_text(
        '[project]\nname = "spice-harness"\nversion = "9.8.7"\n',
        encoding="utf-8",
    )
    (git_dir / "release-proof-identities.json").write_text(
        json.dumps({"schema_version": 1, "source": {}, "synthetic": {}}),
        encoding="utf-8",
    )
    (git_dir / "release-proof-toolchain.json").write_text(
        json.dumps({"schema_version": 1, "resolved": {}}),
        encoding="utf-8",
    )
    return root, tmp_path / "artifacts"


def _assert_release_receipt(
    receipt,
    artifacts: Path,
    sdist: Path,
    wheel: Path,
    rebuilt_hash: str,
) -> None:
    assert receipt["artifacts"] == {
        "sdist": {
            "filename": sdist.name,
            "bytes": sdist.stat().st_size,
            "sha256": _test_sha256(sdist),
        },
        "wheel": {
            "filename": wheel.name,
            "bytes": wheel.stat().st_size,
            "sha256": _test_sha256(wheel),
        },
        "installed_wheel_sha256": _test_sha256(wheel),
        "sdist_rebuilt_from_sha256": _test_sha256(sdist),
    }
    assert receipt["content_comparison"] == {
        "canonical_members": 1,
        "rebuilt_members": 1,
        "mismatches": [],
        "rebuilt_wheel_sha256": rebuilt_hash,
        "outer_archive_reproducibility": "deferred",
    }
    assert (
        _test_sha256(wheel) == rebuilt_hash,
        receipt["artifact_rehearsal"]["checks"],
    ) == (False, list(REHEARSAL.CHECKS))
    assert receipt["tests"] == {
        "python": {"passed": 999, "total": 999},
        "ruff": {"passed": True},
    }
    assert receipt["browser"]["counts"] == {
        "failed": 0,
        "passed": 45,
        "skipped": 1,
        "total": 46,
    }
    assert set(receipt["upgrades"]["stores"]) == {
        "team",
        "ack",
        "maxim-metrics",
        "projection",
    }
    # The prior-store gate replays a tagged store against the source tree; this
    # one is the artifact's own evidence, so the receipt carries both separately.
    assert receipt["in_place_upgrade"] == {
        "adopted": {"from": 783663365, "to": 2, "store": "spiceteams.sqlite3"}
    }
    # The receipt carries the packaging pins the artifact chain actually ran on,
    # so host evidence states its own toolchain instead of implying the image's.
    assert receipt["artifact_rehearsal"]["packaging_toolchain"] == (
        DECLARED_PACKAGING_PINS
    )
    assert receipt["claim_boundary"] == {
        "operating_system": "linux",
        "host_native_companion": "release-proof-macos.json",
        "host_native_checks": ["kqueue-or-fsevents", "appearance", "speech"],
    }
    assert tuple(sorted(path.name for path in artifacts.iterdir())) == (
        "release-proof.json",
        wheel.name,
        sdist.name,
    )
    assert json.loads(
        (artifacts / "release-proof.json").read_text(encoding="utf-8")
    ) == (receipt)

"""Direct-host predecessor reconstruction and carry-integrity contracts."""

from __future__ import annotations

import json

import pytest

from tests.test_releaseproofhelpers import (
    REHEARSAL,
    _git,
    _release_pyproject,
    _source_repository,
    _test_sha256,
    _write_release_tree,
)


def _artifact_manifest(root, payload: dict[str, object]) -> None:
    directory = root / REHEARSAL.PRIOR_ARTIFACT_DIRECTORY
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "manifest.json").write_text(
        json.dumps(payload, sort_keys=True), encoding="utf-8"
    )


def test_existing_corrupt_carry_is_refused_instead_of_regenerated(
    tmp_path, monkeypatch
):
    directory = tmp_path / REHEARSAL.PRIOR_ARTIFACT_DIRECTORY
    directory.mkdir(parents=True)
    wheel = directory / "spice_harness-0.27.0-py3-none-any.whl"
    wheel.write_bytes(b"tampered predecessor wheel\n")
    _artifact_manifest(
        tmp_path,
        {
            "schema_version": 1,
            "release": {"tag": "v0.27.0", "commit": "0" * 40},
            "state": "built",
            "wheel": {"name": wheel.name, "sha256": "0" * 64},
        },
    )
    monkeypatch.setattr(
        REHEARSAL._inplace_upgrade.upgrade_proof,
        "export_prior_artifact",
        lambda *_args: pytest.fail("an existing corrupt carry was regenerated"),
    )

    with pytest.raises(REHEARSAL.RehearsalError, match="does not match"):
        REHEARSAL._resolve_predecessor(tmp_path, tmp_path / "scratch")


def test_host_rehearsal_derives_post_release_predecessor_without_residue(tmp_path):
    repository, _source = _source_repository(tmp_path)
    _write_release_tree(repository, "1.2.3")
    _git(repository, "add", "--all")
    _git(repository, "commit", "--quiet", "--message", "predecessor")
    predecessor_commit = _git(repository, "rev-parse", "HEAD^{commit}")
    _git(repository, "tag", "v1.2.3")
    (repository / "pyproject.toml").write_text(
        _release_pyproject("1.3.0"), encoding="utf-8"
    )
    _git(repository, "add", "pyproject.toml")
    _git(repository, "commit", "--quiet", "--message", "release")
    _git(repository, "tag", "v1.3.0")
    (repository / "payload.txt").write_text("post-release work\n", encoding="utf-8")
    _git(repository, "add", "payload.txt")
    _git(repository, "commit", "--quiet", "--message", "post-release work")
    clean_before = _git(repository, "status", "--porcelain=v1", "--untracked-files=all")

    scratch = tmp_path / "scratch"
    wheel = REHEARSAL._resolve_predecessor(repository, scratch)
    manifest = json.loads(
        (scratch / "host-predecessor" / REHEARSAL.PRIOR_ARTIFACT_MANIFEST).read_text(
            encoding="utf-8"
        )
    )

    assert manifest["release"] == {
        "tag": "v1.2.3",
        "commit": predecessor_commit,
    }
    assert _test_sha256(wheel) == manifest["wheel"]["sha256"]
    assert wheel.parent == (
        scratch / "host-predecessor" / REHEARSAL.PRIOR_ARTIFACT_DIRECTORY
    )
    assert (
        _git(repository, "status", "--porcelain=v1", "--untracked-files=all")
        == clean_before
        == ""
    )

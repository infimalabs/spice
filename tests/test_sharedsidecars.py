"""Shared and lane-local hidden sidecars across linked worktrees."""

from __future__ import annotations

import hashlib
import json
import shutil
import subprocess
from pathlib import Path

import pytest

from spice.agent import maxims
from spice.agent.driver import DRIVER
from spice.flexstate import (
    FLEX_SLICE_CLAIM_TTL_SECONDS,
    FlexSliceClaim,
    flex_slice_claims_state_path,
    git_state_path,
    load_flex_slice_claims,
    load_sticky_items,
    save_flex_slice_claims,
    save_sticky_items,
)
from spice.mail.attachments import (
    InboxAttachmentInput,
    resolve_shared_attachment_ref,
    write_inbox_attachments,
)
from spice.paths import git_common_dir, git_dir, shared_attachment_root
from spice.studies import complexity, fileloc, repodocs
from spice.tasks import artifacts, config, create

ACTOR = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
ORIGIN = "ack:1kCxkWxr"


@pytest.fixture(autouse=True)
def _reset_task_backend():
    config.set_backend(None)
    yield
    config.set_backend(None)


def test_shared_sidecars_round_trip_through_linked_worktree(tmp_path, monkeypatch):
    if shutil.which("task") is None:
        pytest.skip("Taskwarrior binary is required")
    repo, linked = _repo_with_linked_worktree(tmp_path)
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR)
    monkeypatch.setenv("CODEX_TURN_ID", "turn-shared-sidecars")
    monkeypatch.chdir(repo)

    handle = create.add(
        "shared sidecar task",
        project="task.unit",
        origin=ORIGIN,
        acceptance=["sidecars remain addressable from every worktree"],
    )
    artifact_source = repo / "evidence.txt"
    artifact_source.write_text("artifact bytes\n", encoding="utf-8")
    artifacts.add_artifact(handle, artifact_source, content_type="text/plain")
    manifest_path = artifacts.artifact_root() / handle / artifacts.MANIFEST_NAME
    manifest_bytes = manifest_path.read_bytes()
    artifact_manifest = json.loads(manifest_bytes)

    inbox_item = repo / ".spice" / "inbox" / "shared.txt"
    inbox_item.parent.mkdir(parents=True)
    stored_attachments = write_inbox_attachments(
        inbox_item,
        [
            InboxAttachmentInput(
                name="shared.png",
                content_type="image/png",
                data=b"shared attachment bytes",
            )
        ],
        repo_root=repo,
    )
    attachment_path = stored_attachments[0].path
    claim = FlexSliceClaim(
        path=Path("README.md"),
        actor=ACTOR,
        created_at=100.0,
        expires_at=100.0 + FLEX_SLICE_CLAIM_TTL_SECONDS,
    )
    save_flex_slice_claims((claim,), root=repo)
    primary_paths = _shared_sidecar_paths(repo)

    monkeypatch.chdir(linked)
    linked_paths = _shared_sidecar_paths(linked)
    linked_attachment = resolve_shared_attachment_ref(
        str(attachment_path), repo_root=linked
    )

    common_state = git_common_dir(repo) / ".spice"
    assert primary_paths == (
        common_state / "attachments",
        common_state / "artifacts" / "tasks",
        common_state / "flex-slice-claims.json",
    )
    assert linked_paths == primary_paths
    assert linked_attachment == attachment_path
    assert (
        attachment_path.parent.name
        == hashlib.sha256(b"shared attachment bytes").hexdigest()
    )
    assert linked_attachment.read_bytes() == b"shared attachment bytes"
    assert artifacts.show_artifact(handle, "A1") == "artifact bytes\n"
    assert manifest_path.read_bytes() == manifest_bytes
    assert artifact_manifest["task"] == handle
    assert (
        artifact_manifest["artifacts"][0]["sha256"]
        == hashlib.sha256(b"artifact bytes\n").hexdigest()
    )
    assert load_flex_slice_claims(root=linked, now=101.0) == (claim,)


def test_lane_local_sidecars_preserve_distinct_worktree_state(tmp_path):
    repo, linked = _repo_with_linked_worktree(tmp_path)
    primary_paths = _lane_sidecar_paths(repo)
    linked_paths = _lane_sidecar_paths(linked)

    _save_path_latch(repo, "primary.py")
    primary_latch_bytes = primary_paths[1].read_bytes()
    _save_path_latch(linked, "linked.py")
    maxims.set_maxim_bag_disabled("alpha", disabled=True, repo_root=repo)
    maxims.set_maxim_bag_disabled("beta", disabled=True, repo_root=linked)

    assert primary_paths == _expected_lane_sidecar_paths(repo)
    assert linked_paths == _expected_lane_sidecar_paths(linked)
    assert _load_path_latch(repo) == {Path("primary.py")}
    assert _load_path_latch(linked) == {Path("linked.py")}
    assert primary_paths[1].read_bytes() == primary_latch_bytes
    assert json.loads(primary_latch_bytes) == {
        "paths": ["primary.py"],
        "version": 1,
    }
    assert maxims.disabled_maxim_bag_names(repo) == frozenset({"alpha"})
    assert maxims.disabled_maxim_bag_names(linked) == frozenset({"beta"})


def _shared_sidecar_paths(repo: Path) -> tuple[Path, ...]:
    return (
        shared_attachment_root(repo),
        artifacts.artifact_root(),
        flex_slice_claims_state_path(root=repo),
    )


def _lane_sidecar_paths(repo: Path) -> tuple[Path, ...]:
    return tuple(
        git_state_path(relative, root=repo) for relative in _lane_sidecar_names()
    )


def _expected_lane_sidecar_paths(repo: Path) -> tuple[Path, ...]:
    root = git_dir(repo) / ".spice"
    return tuple(root / relative for relative in _lane_sidecar_names())


def _lane_sidecar_names() -> tuple[str, ...]:
    return (
        repodocs.REPO_DOC_CHAR_STICKY_STATE_GIT_PATH,
        fileloc.FILE_LOC_STICKY_STATE_GIT_PATH,
        fileloc.FILE_BYTE_STICKY_STATE_GIT_PATH,
        complexity.COMPLEXITY_CCN_STICKY_GIT_PATH,
        complexity.COMPLEXITY_LENGTH_STICKY_GIT_PATH,
        maxims.DISABLED_MAXIM_BAGS_GIT_PATH,
    )


def _save_path_latch(repo: Path, path: str) -> None:
    save_sticky_items(
        {Path(path)},
        root=repo,
        state_path=None,
        git_path=fileloc.FILE_LOC_STICKY_STATE_GIT_PATH,
        entries_key="paths",
        encode=lambda item: item.as_posix(),
    )


def _load_path_latch(repo: Path) -> set[Path]:
    return load_sticky_items(
        root=repo,
        state_path=None,
        git_path=fileloc.FILE_LOC_STICKY_STATE_GIT_PATH,
        entries_key="paths",
        decode=lambda item: Path(item) if isinstance(item, str) else None,
    )


def _repo_with_linked_worktree(tmp_path: Path) -> tuple[Path, Path]:
    repo = tmp_path / "repo"
    linked = tmp_path / "linked"
    repo.mkdir()
    _run(repo, "git", "init", "-q", "-b", "main")
    _run(repo, "git", "config", "user.email", "spice@example.test")
    _run(repo, "git", "config", "user.name", "Spice Tests")
    (repo / "README.md").write_text("initial\n", encoding="utf-8")
    (repo / "pyproject.toml").write_text(
        "[tool.spice.maxims.alpha]\n"
        'words = ["alpha"]\n'
        'message = "ALPHA reminder."\n'
        "\n"
        "[tool.spice.maxims.beta]\n"
        'words = ["beta"]\n'
        'message = "BETA reminder."\n',
        encoding="utf-8",
    )
    _run(repo, "git", "add", "README.md", "pyproject.toml")
    _run(repo, "git", "commit", "-qm", "initial")
    _run(repo, "git", "worktree", "add", "-q", "-b", "linked", str(linked))
    return repo, linked


def _run(cwd: Path, *args: str) -> None:
    subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True)

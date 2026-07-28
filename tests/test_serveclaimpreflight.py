"""Serve available-work launch preflight and claim-ordering contracts."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from http import HTTPStatus

from spice.agent import lifecycle
from spice.config.trust import RepositoryConfigApprovalRequiredError
from spice.serve import agentapi
from spice.tasks import identity
from tests.test_configtrusthelpers import approve_repository_config
from tests.test_servehelpers import THREAD_A, _patch_agent_status, _target
from tests.test_taskgitsync import _repo_with_upstream, _run


def test_available_work_refreshes_and_refuses_unapproved_config_before_claim(
    tmp_path, monkeypatch
):
    """A synchronized config gate is durable before assignment or launch state."""
    repo = _repo_with_upstream(tmp_path)
    target = _target(repo)
    (repo / ".git" / "info" / "exclude").write_text(
        ".agents/\n.spice/\n", encoding="utf-8"
    )
    approve_repository_config(repo)
    before = _git_head(repo)

    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", str(tmp_path / "remote.git"), str(peer))
    _run(peer, "git", "config", "user.email", "test@example.com")
    _run(peer, "git", "config", "user.name", "Test User")
    (peer / "spice.toml").write_text(
        '[agent]\nwrappers = ["fleet"]\n\n'
        "[wrappers.fleet.pytest]\n"
        'argv = ["spice", "dev", "pytest"]\n',
        encoding="utf-8",
    )
    _run(peer, "git", "add", "spice.toml")
    _run(peer, "git", "commit", "-m", "change executable configuration")
    _run(peer, "git", "push", "origin", "main")
    advanced = _git_head(peer)

    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    candidates = [_ready_row("task-gated"), _ready_row("task-spare")]
    events: list[str] = []
    claims: list[tuple[str, str, object]] = []
    launches: list[dict[str, object]] = []
    real_preflight = agentapi.preflight_automatic_agent_launch

    def observed_preflight(repo_root):
        events.append(f"sync:{_git_head(repo_root)}")
        try:
            return real_preflight(repo_root)
        finally:
            events.append(f"preflight:{_git_head(repo_root)}")

    def claim(task_uuid, actor, **kwargs):
        events.append("claim")
        claims.append((task_uuid, actor, kwargs["site"]))
        return True

    def launch(*_args, **kwargs):
        events.append("start")
        launches.append(kwargs)
        return {"ok": True, "action": "start"}, HTTPStatus.OK

    monkeypatch.setattr(
        agentapi.alloc, "ordered_visible_ready_rows", lambda _actor: candidates
    )
    monkeypatch.setattr(
        agentapi, "preflight_automatic_agent_launch", observed_preflight
    )
    monkeypatch.setattr(agentapi.claimstate, "do_claim", claim)
    monkeypatch.setattr(
        agentapi,
        "agent_ensure_response_payload",
        launch,
    )

    refused = agentapi.ensure_agent_for_available_work(
        target, thread_id=THREAD_A, retry_seconds=0.0
    )

    assert before != advanced
    assert _git_head(repo) == advanced
    assert events == [f"sync:{before}", f"preflight:{advanced}"]
    assert refused["failure"] == lifecycle.AGENT_FAILURE_CONFIG_APPROVAL_REQUIRED
    assert refused["trigger"] == "available-work"
    assert refused["taskHandle"] == identity.render_handle(candidates[0])
    assert "repository executable configuration wrappers.fleet" in refused["error"]
    assert "refusing command spice dev pytest" in refused["error"]
    assert "spice init --apply" in refused["error"]
    assert "claimReleased" not in refused
    assert not claims
    assert not list(lifecycle.agent_state_dir(repo).glob("*.log"))

    approve_repository_config(repo)
    started = agentapi.ensure_agent_for_available_work(
        target, thread_id=THREAD_A, retry_seconds=0.0
    )

    assert events[-4:] == [
        f"sync:{advanced}",
        f"preflight:{advanced}",
        "claim",
        "start",
    ]
    assert claims == [
        (
            "task-gated",
            THREAD_A,
            agentapi.claimstate.ClaimSite(repo.resolve(), "main", advanced),
        )
    ]
    assert launches[0]["launch_preflighted"] is True
    assert started == {
        "ok": True,
        "action": "start",
        "trigger": "available-work",
        "taskHandle": identity.render_handle(candidates[0]),
    }


def test_agent_ensure_names_repository_config_approval_refusal(tmp_path, monkeypatch):
    target = _target(tmp_path)
    refusal = RepositoryConfigApprovalRequiredError("approve exact digest")

    def refuse(*_args, **_kwargs):
        raise refusal

    monkeypatch.setattr(agentapi, "ensure_agent", refuse)

    payload, status = agentapi.agent_ensure_response_payload(target)

    assert status is HTTPStatus.PRECONDITION_REQUIRED
    assert payload == {
        "ok": False,
        "error": "Could not ensure agent: approve exact digest",
        "failure": lifecycle.AGENT_FAILURE_CONFIG_APPROVAL_REQUIRED,
    }


def _ready_row(uuid: str) -> dict[str, str]:
    ready_at = datetime.now(UTC) - timedelta(seconds=30)
    return {
        "uuid": uuid,
        "ready_at": ready_at.isoformat().replace("+00:00", "Z"),
        "project": "serve.queue",
    }


def _git_head(repo) -> str:
    return _run(repo, "git", "rev-parse", "HEAD").stdout.strip()

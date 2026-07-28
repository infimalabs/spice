"""Repository Spice consumers share the canonical layered configuration."""

import subprocess
from collections.abc import Callable
from pathlib import Path

import pytest

from spice.config import layers, values
from spice.agent.maxims import configured_maxim
from spice.agent.shellhook import (
    configured_agent_wrapper_definitions,
    render_agent_wrapper_lines,
)
from spice.cli.mounts import mounted_commands
from spice.errors import SpiceError
from spice.hooks.precommit import pre_commit_steps
from spice.policyconfig import resolve_policy
from spice.resourcelocks import configured_lock_settings
from spice.serve.web import serve_branding
from spice.tasks import config as task_config

SYSTEM_FILE_BYTES = 222
REPOSITORY_FILE_LOC = 333
REPOSITORY_LOCK_EXIT_CODE = 72

INVALID_DOMAIN_CONFIGS = {
    "policy": ("[policy.limits]\nfile_loc = 'bad'\n", "policy.limits.file_loc"),
    "task": ("[tasks]\nproject_min_depth = 'bad'\n", "tasks.project_min_depth"),
    "maxim": ("maxims.sample = 'bad'\n", "maxims.sample"),
    "command": ("[commands]\nbroken = 7\n", "commands"),
    "wrapper": ("wrappers = 'bad'\n", "wrappers"),
    "hook": ("[policy]\npre_commit = 'bad'\n", "policy.pre_commit"),
    "serve": ("serve = 'bad'\n", "serve"),
    "lock": (
        "[locks]\nlock_contention_exit_code = 'bad'\n",
        "locks.lock_contention_exit_code",
    ),
}


def _stand_up_fixture_repo(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """Make the fixture a real repository the argless consumers discover.

    Consumers that take no repo root resolve one from the working directory, so
    the fixture answers them by being a git worktree rather than by patching
    ``spice.paths.repo_root_from_cwd``. A stand-in installed there outlives the
    test: modules on these code paths are imported lazily, and each binds
    whatever object holds the name at its own first import, so the stand-in
    becomes the permanent ``repo_root_from_cwd`` of whichever module loads next.
    """
    subprocess.run(["git", "init", "-q", str(tmp_path)], check=True)
    system_root = tmp_path / "installed-spice"
    system_root.mkdir()
    (system_root / "spice.toml").write_text("", encoding="utf-8")
    monkeypatch.setattr(layers.paths, "runtime_spice_source", lambda: system_root)
    monkeypatch.chdir(tmp_path)


def test_repository_spice_toml_overrides_every_consumer_domain(tmp_path, monkeypatch):
    _stand_up_fixture_repo(tmp_path, monkeypatch)
    (tmp_path / "installed-spice" / "spice.toml").write_text(
        """
        [policy.limits]
        file_loc = 111
        file_bytes = 222

        [policy.pre_commit_builtins]
        complexity = false

        [tasks]
        stems = ["systemstem"]

        [agent]
        model = "system-model"
        wrappers = ["custom"]

        [wrappers.custom.echo]
        argv = ["echo", "system"]

        [commands]
        layered-check = ["echo", "system"]

        [locks]
        lock_contention_exit_code = 71

        [locks.named.tool]
        path = ".spice/tool.lock"

        [serve]
        brand = "System Brand"
        default_lifetime = "Drive"

        [maxims.sample]
        words = ["sample"]
        message = "system message"
        """,
        encoding="utf-8",
    )
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "fixture"\n',
        encoding="utf-8",
    )
    (tmp_path / "spice.toml").write_text(
        """
        [policy.limits]
        file_loc = 333

        [policy.pre_commit_builtins]
        complexity = true

        [tasks]
        stems = ["repostem"]

        [agent]
        model = "repository-model"

        [wrappers.custom.echo]
        argv = ["echo", "repository"]

        [commands]
        layered-check = ["echo", "repository"]

        [locks]
        lock_contention_exit_code = 72

        [serve]
        brand = "Repository Brand"

        [maxims.sample]
        message = "repository message"
        """,
        encoding="utf-8",
    )

    policy = resolve_policy(tmp_path)
    assert policy.limits.file_loc == REPOSITORY_FILE_LOC
    assert policy.limits.file_bytes == SYSTEM_FILE_BYTES
    assert "repostem" in task_config.approved_stems()
    assert values.configured_agent_model(tmp_path) == "repository-model"
    wrappers, _sources = configured_agent_wrapper_definitions(tmp_path)
    assert wrappers["custom"]["echo"]["argv"] == ["echo", "repository"]
    assert mounted_commands(tmp_path)[("layered-check",)] == (
        "echo",
        "repository",
    )
    assert (
        configured_lock_settings(tmp_path).locks["tool"].contention_exit_code
        == REPOSITORY_LOCK_EXIT_CODE
    )
    step_keys = {step.key for step in pre_commit_steps(tmp_path, [])}
    assert "complexity" in step_keys
    assert serve_branding(tmp_path).name == "Repository Brand"
    assert configured_maxim("sample", repo_root=tmp_path) == "repository message"


@pytest.mark.parametrize(
    ("source_name", "relative_path"),
    [
        ("repository", Path("spice.toml")),
        ("worktree", Path(".spice/config/spice.toml")),
    ],
)
@pytest.mark.parametrize(("domain", "configured"), INVALID_DOMAIN_CONFIGS.items())
def test_invalid_layered_consumer_value_reports_winning_key_source_and_path(
    tmp_path, monkeypatch, source_name, relative_path, domain, configured
):
    _stand_up_fixture_repo(tmp_path, monkeypatch)
    config_text, effective_key = configured
    source_path = tmp_path / relative_path
    source_path.parent.mkdir(parents=True, exist_ok=True)
    source_path.write_text(config_text, encoding="utf-8")

    outcome = _validation_outcome(lambda: _load_invalid_domain(domain, tmp_path))

    assert outcome["state"] == "rejected"
    assert effective_key in outcome["message"]
    assert f"source={source_name}" in outcome["message"]
    assert f"path={source_path}" in outcome["message"]


def _load_invalid_domain(domain: str, repo_root: Path) -> object:
    loaders: dict[str, Callable[[], object]] = {
        "policy": lambda: resolve_policy(repo_root),
        "task": task_config.project_depth_bounds,
        "maxim": lambda: configured_maxim("sample", repo_root=repo_root),
        "command": lambda: mounted_commands(repo_root),
        "wrapper": lambda: render_agent_wrapper_lines(repo_root),
        "hook": lambda: pre_commit_steps(repo_root, []),
        "serve": lambda: serve_branding(repo_root),
        "lock": lambda: configured_lock_settings(repo_root),
    }
    return loaders[domain]()


def _validation_outcome(operation: Callable[[], object]) -> dict[str, str]:
    try:
        operation()
    except SpiceError as exc:
        return {"state": "rejected", "message": str(exc)}
    return {"state": "accepted", "message": "configuration accepted"}

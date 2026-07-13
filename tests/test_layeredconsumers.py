"""Repository Spice consumers share the canonical layered configuration."""

import subprocess

from spice import config
from spice.agent.maxims import configured_maxim
from spice.agent.shellhook import configured_agent_wrapper_definitions
from spice.cli.mounts import mounted_commands
from spice.hooks.precommit import pre_commit_steps
from spice.policyconfig import resolve_policy
from spice.resourcelocks import configured_lock_settings
from spice.serve.web import serve_branding
from spice.tasks import config as task_config

PROJECT_FILE_BYTES = 222
REPOSITORY_FILE_LOC = 333
REPOSITORY_LOCK_EXIT_CODE = 72


def test_repository_spice_toml_overrides_every_consumer_domain(tmp_path, monkeypatch):
    subprocess.run(["git", "init", "-q", str(tmp_path)], check=True)
    (tmp_path / "pyproject.toml").write_text(
        """
        [project]
        name = "fixture"

        [tool.spice.policy.limits]
        file_loc = 111
        file_bytes = 222

        [tool.spice.policy.pre_commit_builtins]
        complexity = false

        [tool.spice.tasks]
        stems = ["projectstem"]

        [tool.spice.agent]
        model = "project-model"
        wrappers = ["custom"]

        [tool.spice.wrappers.custom.echo]
        argv = ["echo", "project"]

        [tool.spice.commands]
        layered-check = ["echo", "project"]

        [tool.spice.locks]
        lock_contention_exit_code = 71

        [tool.spice.locks.named.tool]
        path = ".spice/tool.lock"

        [tool.spice.serve]
        brand = "Project Brand"

        [tool.spice.maxims.sample]
        words = ["sample"]
        message = "project message"
        """,
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
    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr("spice.paths.repo_root_from_cwd", lambda: tmp_path)

    policy = resolve_policy(tmp_path)
    assert policy.limits.file_loc == REPOSITORY_FILE_LOC
    assert policy.limits.file_bytes == PROJECT_FILE_BYTES
    assert "repostem" in task_config.approved_stems()
    assert config.configured_agent_model(tmp_path) == "repository-model"
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

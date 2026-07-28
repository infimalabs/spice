"""Repository Spice consumers share the canonical layered configuration."""

import subprocess
from collections.abc import Callable
from pathlib import Path

import pytest

from spice.config import edit, layers, values
from spice.agent.maxims import configured_maxim, maxim_names
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

PROJECT_FILE_BYTES = 222
REPOSITORY_FILE_LOC = 333
REPOSITORY_LOCK_EXIT_CODE = 72

FALSE_DISABLE_CASES = (
    (
        ("commands",),
        "probe",
        '[tool.spice.commands]\nprobe = ["echo", "probe"]\n',
        "[commands]\nprobe = false\n",
    ),
    (
        ("maxims",),
        "polling",
        "",
        "[maxims]\npolling = false\n",
    ),
    (
        ("policy", "pre_commit_builtins"),
        "formatters",
        "",
        "[policy.pre_commit_builtins]\nformatters = false\n",
    ),
    (
        ("policy", "taste", "words"),
        "master",
        "",
        "[policy.taste.words]\nmaster = false\n",
    ),
    (
        ("tasks", "reports"),
        "oready",
        "",
        "[tasks.reports]\noready = false\n",
    ),
    (
        ("wrappers",),
        "probe",
        """
        [tool.spice.agent]
        wrappers = ["probe-group"]

        [tool.spice.wrappers.probe-group.probe]
        argv = ["echo", "probe"]
        """,
        "[wrappers]\nprobe-group = false\n",
    ),
    (
        ("wrappers", "*"),
        "rtk",
        "",
        """
        [wrappers.common]
        rtk = false
        probe = { argv = ["echo", "probe"] }
        """,
    ),
)

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
    monkeypatch.chdir(tmp_path)


def test_repository_spice_toml_overrides_every_consumer_domain(tmp_path, monkeypatch):
    _stand_up_fixture_repo(tmp_path, monkeypatch)
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

    policy = resolve_policy(tmp_path)
    assert policy.limits.file_loc == REPOSITORY_FILE_LOC
    assert policy.limits.file_bytes == PROJECT_FILE_BYTES
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


def test_false_disable_matrix_covers_every_declared_registry():
    assert tuple(case[0] for case in FALSE_DISABLE_CASES) == (
        layers.FALSE_DISABLE_REGISTRY_PATHS
    )


@pytest.mark.parametrize(
    ("registry_path", "entry", "project_config", "repository_config"),
    FALSE_DISABLE_CASES,
    ids=("commands", "maxims", "pre-commit", "taste", "reports", "groups", "entries"),
)
def test_false_disables_one_inherited_entry_in_every_registry(
    tmp_path,
    monkeypatch,
    registry_path,
    entry,
    project_config,
    repository_config,
):
    _stand_up_fixture_repo(tmp_path, monkeypatch)
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "fixture"\n\n' + project_config,
        encoding="utf-8",
    )
    assert entry in _resolved_registry_names(registry_path, tmp_path)

    (tmp_path / "spice.toml").write_text(repository_config, encoding="utf-8")

    assert entry not in _resolved_registry_names(registry_path, tmp_path)


@pytest.mark.parametrize(
    "source_name",
    [
        "repository",
        "worktree",
    ],
)
@pytest.mark.parametrize(("domain", "configured"), INVALID_DOMAIN_CONFIGS.items())
def test_invalid_layered_consumer_value_reports_winning_key_source_and_path(
    tmp_path, monkeypatch, source_name, domain, configured
):
    _stand_up_fixture_repo(tmp_path, monkeypatch)
    config_text, effective_key = configured
    source_path = (
        tmp_path / "spice.toml"
        if source_name == "repository"
        else edit.worktree_config_path(tmp_path)
    )
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


def _resolved_registry_names(
    registry_path: tuple[str, ...], repo_root: Path
) -> set[str]:
    if registry_path == ("commands",):
        return {".".join(path) for path in mounted_commands(repo_root)}
    if registry_path == ("maxims",):
        return set(maxim_names(repo_root))
    if registry_path == ("policy", "pre_commit_builtins"):
        return {step.key for step in pre_commit_steps(repo_root, [])}
    if registry_path == ("policy", "taste", "words"):
        return set(resolve_policy(repo_root).taste.words)
    if registry_path == ("tasks", "reports"):
        prefix = "report."
        suffix = ".description"
        return {
            line.removeprefix(prefix).split(suffix, maxsplit=1)[0]
            for line in task_config._report_lines(repo_root)
            if line.startswith(prefix) and suffix in line
        }
    if registry_path in {("wrappers",), ("wrappers", "*")}:
        suffix = "() {"
        return {
            line.removesuffix(suffix)
            for line in render_agent_wrapper_lines(repo_root)
            if line.endswith(suffix)
        }
    raise AssertionError(f"registry test has no consumer for {registry_path}")


def _validation_outcome(operation: Callable[[], object]) -> dict[str, str]:
    try:
        operation()
    except SpiceError as exc:
        return {"state": "rejected", "message": str(exc)}
    return {"state": "accepted", "message": "configuration accepted"}

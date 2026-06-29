"""Magic-number gate configuration."""

import os
import subprocess
from pathlib import Path

import pytest

from spice.errors import SpiceError
from spice.hooks import precommit
from spice.policy import (
    C_GRAMMAR_SUFFIXES,
    MAGIC_BASELINE_REF,
    MAGIC_EXAMINE_VALUE_THRESHOLD,
    MAGIC_SUFFIXES,
)

PROJECT_ROOT = Path(__file__).resolve().parents[1]


def test_magic_numbers_guard_uses_default_policy_when_unconfigured(
    tmp_path, monkeypatch
):
    seen: dict[str, object] = {}

    def detect(
        paths: list[Path],
        *,
        root: Path,
        baseline_ref: str,
        examine_threshold: int,
        examine_threshold_for_path,
        suffixes: tuple[str, ...],
        c_grammar_suffixes: tuple[str, ...],
    ):
        seen["paths"] = paths
        seen["root"] = root
        seen["baseline_ref"] = baseline_ref
        seen["examine_threshold"] = examine_threshold
        seen["examine_threshold_for_path"] = examine_threshold_for_path
        seen["suffixes"] = suffixes
        seen["c_grammar_suffixes"] = c_grammar_suffixes
        return []

    monkeypatch.setattr(precommit.magicnums, "detect_magic_regressions", detect)

    precommit._run_magic_numbers_guard(tmp_path, [Path("src/app.py")])

    examine_threshold_for_path = seen.pop("examine_threshold_for_path")
    assert callable(examine_threshold_for_path)
    assert (
        examine_threshold_for_path(Path("src/app.py")) == MAGIC_EXAMINE_VALUE_THRESHOLD
    )
    assert seen == {
        "paths": [Path("src/app.py")],
        "root": tmp_path,
        "baseline_ref": MAGIC_BASELINE_REF,
        "examine_threshold": MAGIC_EXAMINE_VALUE_THRESHOLD,
        "suffixes": MAGIC_SUFFIXES,
        "c_grammar_suffixes": C_GRAMMAR_SUFFIXES,
    }


def test_magic_numbers_guard_reads_configured_threshold(tmp_path):
    repo = _git_init(tmp_path / "repo")
    _write_repo_file(repo, "src/app.py", "def run(value):\n    return value > 1\n")
    _git(repo, "add", "src/app.py")
    _git(repo, "commit", "-m", "base")
    _write_repo_file(
        repo,
        "pyproject.toml",
        "[tool.spice.policy.magic]\nexamine_threshold = 100\n",
    )
    _write_repo_file(repo, "src/app.py", "def run(value):\n    return value > 75\n")
    _git(repo, "add", ".")

    precommit._run_magic_numbers_guard(repo, [Path("src/app.py")])

    findings = precommit.magicnums.detect_magic_regressions(
        [Path("src/app.py")],
        root=repo,
        examine_threshold=100,
    )
    assert findings == []


def test_magic_numbers_guard_applies_scoped_threshold_and_global_fallback(tmp_path):
    repo = _git_init(tmp_path / "repo")
    scoped_path = Path("src/high/app.py")
    default_path = Path("src/default/app.py")
    _write_repo_file(
        repo, scoped_path.as_posix(), "def run(value):\n    return value > 1\n"
    )
    _write_repo_file(
        repo, default_path.as_posix(), "def run(value):\n    return value > 1\n"
    )
    _git(repo, "add", ".")
    _git(repo, "commit", "-m", "base")
    _write_repo_file(
        repo,
        "pyproject.toml",
        "[tool.spice.policy.magic]\n"
        "examine_threshold = 10\n"
        "\n"
        '[tool.spice.policy.scopes."src/high/**"]\n'
        "magic.examine_threshold = 100\n",
    )
    _write_repo_file(
        repo, scoped_path.as_posix(), "def run(value):\n    return value > 75\n"
    )
    _write_repo_file(
        repo, default_path.as_posix(), "def run(value):\n    return value > 75\n"
    )
    _git(repo, "add", ".")

    with pytest.raises(SpiceError) as exc_info:
        precommit._run_magic_numbers_guard(repo, [scoped_path, default_path])

    message = str(exc_info.value)
    assert "src/default/app.py:2: 75" in message
    assert "src/high/app.py" not in message


def test_magic_numbers_guard_reads_configured_baseline_ref(tmp_path):
    repo = _git_init(tmp_path / "repo")
    _write_repo_file(repo, "src/app.py", "def run(value):\n    return value > 1\n")
    _git(repo, "add", "src/app.py")
    _git(repo, "commit", "-m", "base")
    base = _git(repo, "rev-parse", "HEAD").stdout.strip()
    _write_repo_file(repo, "src/app.py", "def run(value):\n    return value > 75\n")
    _git(repo, "add", "src/app.py")
    _git(repo, "commit", "-m", "head")
    _write_repo_file(
        repo,
        "pyproject.toml",
        f'[tool.spice.policy.magic]\nbaseline_ref = "{base}"\n',
    )
    _git(repo, "add", ".")

    with pytest.raises(SpiceError) as exc_info:
        precommit._run_magic_numbers_guard(repo, [Path("src/app.py")])

    message = str(exc_info.value)
    assert f"magic-numbers: 1 regression(s) vs {base}" in message
    assert "src/app.py:2: 75" in message


def _git_init(repo: Path) -> Path:
    _run(["git", "init", "-b", "main", str(repo)])
    _git(repo, "config", "user.email", "spice@example.test")
    _git(repo, "config", "user.name", "Spice Tests")
    return repo


def _write_repo_file(repo: Path, name: str, text: str) -> None:
    path = repo / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def _git(repo: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return _run(["git", "-C", str(repo), *args])


def _run(args: list[str]) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()  # env-policy: allow
    env["PYTHONPATH"] = os.pathsep.join(
        entry for entry in (str(PROJECT_ROOT), env.get("PYTHONPATH", "")) if entry
    )
    result = subprocess.run(
        args,
        capture_output=True,
        check=False,
        env=env,
        text=True,
    )
    if result.returncode != 0:
        raise AssertionError(
            f"command failed: {' '.join(args)}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return result

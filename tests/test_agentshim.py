"""The tracked spice.sh agent shim degrades truthfully on a broken checkout."""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

from spice.hooks.install import write_agent_shim

EXIT_OK = 0
EXIT_UNRUNNABLE = 127


def _checkout(tmp_path: Path, wrap_source: str) -> Path:
    repo = tmp_path / "checkout"
    (repo / "spice" / "agent").mkdir(parents=True)
    (repo / "spice" / "cli").mkdir(parents=True)
    (repo / "spice" / "__main__.py").write_text("", encoding="utf-8")
    (repo / "spice" / "cli" / "entry.py").write_text(
        "def main() -> int:\n    return 0\n", encoding="utf-8"
    )
    (repo / "spice" / "agent" / "wrap.py").write_text(wrap_source, encoding="utf-8")
    bin_dir = repo / ".venv" / "bin"
    bin_dir.mkdir(parents=True)
    os.symlink(sys.executable, bin_dir / "python")
    write_agent_shim(repo)
    return repo


def _run_shim(repo: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["sh", str(repo / "spice.sh"), "true"],
        capture_output=True,
        check=False,
        cwd=repo,
        text=True,
    )


def test_agent_shim_runs_local_checkout_when_importable(tmp_path):
    repo = _checkout(tmp_path, "VALUE = 1\n")

    completed = _run_shim(repo)

    assert completed.returncode == EXIT_OK


def test_agent_shim_names_broken_file_and_recovery_path(tmp_path):
    conflicted = "<<<<<<< HEAD\nVALUE = 1\n=======\nVALUE = 2\n>>>>>>> other\n"
    repo = _checkout(tmp_path, conflicted)

    completed = _run_shim(repo)

    assert completed.returncode == EXIT_UNRUNNABLE
    assert "cannot import" in completed.stderr
    assert "spice/agent/wrap.py" in completed.stderr
    assert "conflict markers" in completed.stderr
    assert "installed spice entrypoint" in completed.stderr

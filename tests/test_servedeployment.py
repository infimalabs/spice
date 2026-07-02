"""Single-install deployment contract for serve.

Serve is the single main-tree deployment: it runs from the installed spice
runtime and only ever *operates* other worktrees. Discovered worktrees are
work targets, never runtime providers — no serve path may re-derive an
interpreter, venv, or import path from the tree it is operating. This guards
that invariant at the source level so the single-install battery's removals
(and any future drift) cannot quietly reintroduce a per-worktree runtime into
serve. See docs/design/accepted/single-install-runtime-model.md.
"""

from __future__ import annotations

import dataclasses
from pathlib import Path

from spice.serve.worktree.target import WorktreeTarget

PROJECT_ROOT = Path(__file__).resolve().parent.parent
SERVE_DIR = PROJECT_ROOT / "spice" / "serve"

# Tokens that only appear when code derives a runtime from a specific tree:
# the worktree-spice detection/exec helpers, and the venv/import-path env that
# the per-tree runtime injection used. Serve must reference none of them.
FORBIDDEN_RUNTIME_TOKENS = (
    "worktree_spice_source",
    "worktree_spice_environment",
    "worktree_spice_python_command",
    "runtime_uses_worktree_spice",
    "VIRTUAL_ENV",
    "PYTHONPATH",
    ".venv",
)

# A worktree target is pure location metadata. A field naming an interpreter,
# venv, or runtime would be a channel for a per-tree runtime to leak in.
FORBIDDEN_TARGET_FIELD_SUBSTRINGS = ("venv", "python", "interpreter", "runtime")


def _serve_sources() -> list[Path]:
    return sorted(SERVE_DIR.rglob("*.py"))


def test_serve_never_derives_runtime_from_a_worktree():
    offenders: list[str] = []
    for source in _serve_sources():
        text = source.read_text(encoding="utf-8")
        for token in FORBIDDEN_RUNTIME_TOKENS:
            if token in text:
                rel = source.relative_to(PROJECT_ROOT)
                offenders.append(f"{rel}: references per-tree runtime token {token!r}")
    assert not offenders, (
        "serve must run from the installed runtime, never one derived from an "
        "operated worktree:\n" + "\n".join(offenders)
    )


def test_worktree_target_is_pure_location_metadata():
    field_names = {field.name for field in dataclasses.fields(WorktreeTarget)}
    assert field_names == {"id", "repo_root", "name", "branch"}, field_names
    leaks = {
        name
        for name in field_names
        for needle in FORBIDDEN_TARGET_FIELD_SUBSTRINGS
        if needle in name.lower()
    }
    assert not leaks, (
        "a discovered worktree is an operated target, not a runtime provider; "
        f"these fields would leak a per-tree runtime: {sorted(leaks)}"
    )

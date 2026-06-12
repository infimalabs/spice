"""Sticky study state: once a file breaches a flex limit it stays watched.

Shape guards allow headroom (`flex_limit`) over a base limit, but a file that
ever breached keeps its base limit until it shrinks back under it. The breach
set persists in the git dir so it survives checkouts and rebases without
touching the working tree.

Library seam: target-repo tools may import the public sticky-state helpers in
this module; underscored names remain private.
"""

from __future__ import annotations

import json
import subprocess
from collections.abc import Callable
from pathlib import Path
from typing import Any, TypeVar

from spice.policy import flex_limit as flex_limit  # single source of the ratio

StickyKey = TypeVar("StickyKey")
Item = TypeVar("Item")


def git_state_path(git_path: str, *, root: Path) -> Path:
    completed = subprocess.run(
        ["git", "rev-parse", "--git-path", git_path],
        capture_output=True,
        cwd=root,
        check=True,
        text=True,
    )
    raw_path = Path(completed.stdout.strip())
    return raw_path if raw_path.is_absolute() else root / raw_path


def load_sticky_items(
    *,
    root: Path,
    state_path: Path | None,
    git_path: str,
    entries_key: str,
    decode: Callable[[Any], StickyKey | None],
    version: int = 1,
) -> set[StickyKey]:
    path = state_path or git_state_path(git_path, root=root)
    if not path.exists():
        return set()
    payload = json.loads(path.read_text(encoding="utf-8"))
    if payload.get("version") != version:
        return set()
    items: set[StickyKey] = set()
    for raw in payload.get(entries_key, []):
        if (item := decode(raw)) is not None:
            items.add(item)
    return items


def save_sticky_items(
    items: set[StickyKey],
    *,
    root: Path,
    state_path: Path | None,
    git_path: str,
    entries_key: str,
    encode: Callable[[StickyKey], Any],
    version: int = 1,
) -> None:
    path = state_path or git_state_path(git_path, root=root)
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = {
        "version": version,
        entries_key: [encode(item) for item in sorted(items, key=str)],
    }
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")


def sticky_items_after_flex_breaches(
    items: list[Item],
    sticky_items: set[StickyKey],
    *,
    key_for_item: Callable[[Item], StickyKey],
    is_breach: Callable[[Item], bool],
) -> set[StickyKey]:
    updated = set(sticky_items)
    for item in items:
        if is_breach(item):
            updated.add(key_for_item(item))
    return updated


def sticky_paths_after_renames(
    sticky_paths: set[Path],
    renames: dict[Path, Path],
) -> set[Path]:
    if not sticky_paths:
        return sticky_paths
    updated = set(sticky_paths)
    for old_path, new_path in renames.items():
        if old_path in sticky_paths:
            updated.add(new_path)
    return updated


def sticky_function_keys_after_renames(
    sticky_keys: set[tuple[str, str]],
    renames: dict[Path, Path],
) -> set[tuple[str, str]]:
    if not sticky_keys:
        return sticky_keys
    updated = set(sticky_keys)
    for old_path, new_path in renames.items():
        old_name = old_path.as_posix()
        new_name = new_path.as_posix()
        for path, symbol in sticky_keys:
            if path == old_name:
                updated.add((new_name, symbol))
    return updated

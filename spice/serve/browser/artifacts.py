"""Browser-smoke artifact path helpers for `spice serve`."""

from __future__ import annotations

from pathlib import Path

from spice.paths import STATE_DIRNAME

SERVE_BROWSER_ARTIFACT_DIR = Path(STATE_DIRNAME) / "serve" / "browser"


def serve_browser_artifact_path(
    filename: str, *, root: Path | None = None, create: bool = True
) -> Path:
    name = str(filename or "").strip()
    candidate = Path(name)
    if not name or candidate.is_absolute() or candidate.name != name:
        raise ValueError("serve browser artifact filename must be a plain filename")
    if name in (".", ".."):
        raise ValueError("serve browser artifact filename must be a plain filename")
    base = Path.cwd() if root is None else root
    path = base / SERVE_BROWSER_ARTIFACT_DIR / name
    if create:
        path.parent.mkdir(parents=True, exist_ok=True)
    return path

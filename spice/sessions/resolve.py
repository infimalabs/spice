"""Resolve transcript files from thread ids, paths, or the ambient agent."""

from __future__ import annotations

from pathlib import Path

from spice.agent.driver import ALL_DRIVERS, AgentDriver, driver_for
from spice.agent.identity import ambient_thread, canonical_thread_id
from spice.sessions.util import dedupe_paths

THREAD_ID_LENGTH = 32


def looks_like_thread_id(value: str) -> bool:
    canonical = canonical_thread_id(value)
    return len(canonical) == THREAD_ID_LENGTH and all(
        c in "0123456789abcdef" for c in canonical
    )


def resolve_files(raw_files: list[str]) -> list[Path]:
    """Resolve inputs (paths or thread ids) to transcript files.

    With no inputs, the ambient agent's own transcript is the subject — the
    no-arg `spice session` is an agent looking at itself.
    """
    if raw_files:
        files = [resolve_file_input(value) for value in raw_files]
    else:
        ambient = ambient_thread()
        if ambient is not None:
            current, driver = ambient
            files = [driver.thread_transcript_path(current)]
        else:
            files = sorted(Path.cwd().glob("*.jsonl"))
    missing = [str(path) for path in files if not path.exists()]
    if missing:
        raise SystemExit(f"Missing files: {', '.join(missing)}")
    if not files:
        raise SystemExit(
            "No JSONL files found and no ambient agent thread id was available."
        )
    return dedupe_paths(files)


def resolve_file_input(raw_value: str) -> Path:
    path_candidate = Path(raw_value).expanduser()
    if path_candidate.exists():
        return path_candidate.resolve()
    if looks_like_thread_id(raw_value):
        return resolve_thread_transcript(canonical_thread_id(raw_value))
    return path_candidate.resolve()


def resolve_thread_transcript(thread_id: str, repo_root: Path | None = None) -> Path:
    canonical = canonical_thread_id(thread_id)
    preferred = driver_for(repo_root or Path.cwd())
    errors: list[str] = []
    for driver in _ordered_drivers(preferred):
        try:
            return driver.thread_transcript_path(canonical)
        except (RuntimeError, SystemExit) as exc:
            errors.append(str(exc))
    detail = errors[0] if errors else "no configured drivers"
    raise SystemExit(detail)


def _ordered_drivers(preferred: AgentDriver) -> list[AgentDriver]:
    return [preferred, *(driver for driver in ALL_DRIVERS if driver is not preferred)]

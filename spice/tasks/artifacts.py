"""Task-addressed sidecar artifacts stored outside the worktree."""

from __future__ import annotations

import contextlib
import hashlib
import mimetypes
import os
import re
import shutil
import tempfile
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

from spice.errors import SpiceError
from spice.paths import atomic_write_json, fsync_directory
from spice.tasks import config, identity

TASK_ARTIFACT_DIR = Path(config.SHARED_DIR) / "artifacts" / "tasks"
MANIFEST_NAME = "manifest.json"
OBJECTS_DIR = "objects"
PAYLOAD_NAME = "payload"
METADATA_NAME = "metadata.json"
KIB_BYTES = 1024
MIB_BYTES = KIB_BYTES * KIB_BYTES
MAX_ARTIFACT_BYTES = 16 * MIB_BYTES
MAX_ARTIFACTS_PER_TASK = 32
MAX_NAME_CHARS = 96
HASH_CHUNK_BYTES = MIB_BYTES
TEXT_TYPES = frozenset(
    {
        "text/plain",
        "text/markdown",
        "application/json",
        "text/csv",
        "text/tab-separated-values",
    }
)
BINARY_TYPES = frozenset(
    {
        "image/png",
        "image/jpeg",
        "image/webp",
        "application/pdf",
    }
)
RETENTIONS = frozenset({"permanent", "prunable"})
DEFAULT_RETENTION = "permanent"

_SAFE_NAME_RE = re.compile(r"[^A-Za-z0-9._-]+")


def add_artifact(
    handle: str,
    path: str | Path,
    *,
    name: str | None = None,
    content_type: str | None = None,
    retention: str = DEFAULT_RETENTION,
) -> str:
    rendered, _row = _resolve_rendered_handle(handle)
    source = Path(path).expanduser()
    if not source.is_file():
        raise SpiceError(f"artifact source is not a file: {source}")
    size = source.stat().st_size
    if size > MAX_ARTIFACT_BYTES:
        raise SpiceError(
            f"artifact exceeds {MAX_ARTIFACT_BYTES} bytes: {source} ({size} bytes)"
        )
    safe_name = _artifact_name(name or source.name)
    ctype = _content_type(content_type, safe_name, source)
    retention = _retention(retention)
    task_dir = _task_dir_for_write(rendered)
    manifest = _read_manifest(task_dir, rendered)
    entries = _manifest_entries(manifest)
    if len(entries) >= MAX_ARTIFACTS_PER_TASK:
        raise SpiceError(
            f"task already has {MAX_ARTIFACTS_PER_TASK} artifacts: {rendered}"
        )
    digest = _sha256_file(source)
    object_dir = task_dir / OBJECTS_DIR / digest
    payload_path = object_dir / PAYLOAD_NAME
    object_dir.mkdir(parents=True, exist_ok=True)
    if not _payload_matches(payload_path, digest):
        _copy_file_atomic(source, payload_path)
    metadata = {
        "content_type": ctype,
        "filename": safe_name,
        "sha256": digest,
        "size": size,
    }
    atomic_write_json(object_dir / METADATA_NAME, metadata)
    fsync_directory(object_dir)
    entry = {
        "id": _next_artifact_id(entries),
        "name": safe_name,
        "sha256": digest,
        "content_type": ctype,
        "size": size,
        "created_at": _now_iso(),
        "source": "spice task artifact add",
        "retention": retention,
    }
    entries.append(entry)
    _write_manifest(task_dir, rendered, entries)
    config.mark_task_backend_changed("artifact")
    return "\n".join(
        [
            f"added {entry['id']} {safe_name} {ctype} {_format_size(size)}",
            f"retention {retention}",
            f"next: spice task artifact show {rendered} {entry['id']}",
        ]
    )


def list_artifacts(handle: str) -> str:
    rendered, _row = _resolve_rendered_handle(handle)
    entries = _entries_for_handle(rendered)
    if not entries:
        return f"no artifacts for {rendered}"
    return "\n".join(_summary_line(entry) for entry in entries)


def show_artifact(handle: str, artifact_id: str) -> str:
    rendered, _row = _resolve_rendered_handle(handle)
    task_dir = _task_dir_for_read(rendered)
    if task_dir is None:
        raise SpiceError(f"no artifacts for {rendered}")
    entry = _artifact_entry(_entries_for_dir(task_dir, rendered), artifact_id)
    payload = _payload_path(task_dir, entry)
    _require_payload_integrity(payload, str(entry["sha256"]), str(entry["id"]))
    content_type = str(entry.get("content_type") or "")
    if content_type in TEXT_TYPES:
        try:
            return payload.read_text(encoding="utf-8")
        except UnicodeDecodeError as exc:
            raise SpiceError(f"artifact {artifact_id} is not valid UTF-8") from exc
    return f"path {payload}"


def render_artifact_lines(rendered: str) -> list[str]:
    entries = _entries_for_handle(rendered)
    if not entries:
        return []
    lines = ["artifacts:"]
    for entry in entries:
        lines.append(f"  {_summary_line(entry)}")
        lines.append(f"     spice task artifact show {rendered} {entry['id']}")
    return lines


def prune_artifacts(*, older_than: str | None = None, apply: bool = False) -> str:
    root = artifact_root()
    if not root.is_dir():
        return "no task artifacts"
    cutoff = _cutoff(older_than) if older_than else None
    pruned: list[str] = []
    skipped: list[str] = []
    for task_dir in sorted(path for path in root.iterdir() if path.is_dir()):
        manifest = _read_manifest(task_dir, task_dir.name)
        entries = _manifest_entries(manifest)
        if not entries:
            continue
        task_label = str(manifest.get("task") or task_dir.name)
        status = _task_status_for_dir(task_dir, task_label)
        if status != "completed":
            skipped.append(f"skipped {task_label}: status {status}")
            continue
        remaining: list[dict[str, Any]] = []
        to_remove: list[dict[str, Any]] = []
        for entry in entries:
            if _prune_candidate(entry, cutoff):
                pruned.append(f"{task_label} {entry['id']} {entry['name']}")
                to_remove.append(entry)
                continue
            remaining.append(entry)
        if apply and len(remaining) != len(entries):
            for entry in to_remove:
                _remove_payload_if_unreferenced(task_dir, entry, remaining)
            _write_manifest(task_dir, task_label, remaining)
    if apply and pruned:
        config.mark_task_backend_changed("artifact")
    action = "pruned" if apply else "would prune"
    lines = [f"{action} {item}" for item in pruned]
    lines.extend(skipped)
    if not lines:
        return "no prunable artifacts"
    if not apply and pruned:
        lines.append("dry_run true; pass --apply to remove")
    return "\n".join(lines)


def artifact_root() -> Path:
    return config.git_common_dir(config.repo_root()) / TASK_ARTIFACT_DIR


def _resolve_rendered_handle(handle: str) -> tuple[str, dict[str, Any]]:
    row = identity.resolve(handle)
    return identity.render_handle(row), row


def _task_dir_for_write(rendered: str) -> Path:
    existing = _task_dir_for_read(rendered)
    return existing or artifact_root() / rendered


def _task_dir_for_read(rendered: str) -> Path | None:
    root = artifact_root()
    try:
        incepted = identity.incepted_of_handle(rendered)
    except SpiceError:
        return None
    candidates = [root / rendered, *sorted(root.glob(f"*-{incepted}")), root / incepted]
    matches: list[Path] = []
    seen: set[Path] = set()
    for candidate in candidates:
        resolved = candidate.resolve()
        if resolved in seen:
            continue
        seen.add(resolved)
        if (candidate / MANIFEST_NAME).is_file():
            matches.append(candidate)
    if len(matches) > 1:
        names = ", ".join(str(path) for path in matches)
        raise SpiceError(f"multiple artifact manifests found for {rendered}: {names}")
    return matches[0] if matches else None


def _read_manifest(task_dir: Path, rendered: str) -> dict[str, Any]:
    path = task_dir / MANIFEST_NAME
    try:
        loaded = path.read_text(encoding="utf-8")
    except OSError:
        return {"version": 1, "task": rendered, "artifacts": []}
    import json

    try:
        data = json.loads(loaded)
    except json.JSONDecodeError as exc:
        raise SpiceError(f"artifact manifest is invalid JSON: {path}") from exc
    if not isinstance(data, dict):
        raise SpiceError(f"artifact manifest must be an object: {path}")
    data.setdefault("version", 1)
    data.setdefault("task", rendered)
    data.setdefault("artifacts", [])
    return data


def _write_manifest(
    task_dir: Path, rendered: str, entries: list[dict[str, Any]]
) -> None:
    atomic_write_json(
        task_dir / MANIFEST_NAME,
        {"version": 1, "task": rendered, "artifacts": entries},
    )
    fsync_directory(task_dir)


def _manifest_entries(manifest: dict[str, Any]) -> list[dict[str, Any]]:
    raw = manifest.get("artifacts")
    if not isinstance(raw, list):
        raise SpiceError("artifact manifest field 'artifacts' must be a list")
    entries: list[dict[str, Any]] = []
    for item in raw:
        if isinstance(item, dict):
            entries.append(item)
    return entries


def _entries_for_handle(rendered: str) -> list[dict[str, Any]]:
    task_dir = _task_dir_for_read(rendered)
    if task_dir is None:
        return []
    return _entries_for_dir(task_dir, rendered)


def _entries_for_dir(task_dir: Path, rendered: str) -> list[dict[str, Any]]:
    return _manifest_entries(_read_manifest(task_dir, rendered))


def _artifact_entry(entries: list[dict[str, Any]], artifact_id: str) -> dict[str, Any]:
    needle = artifact_id.strip()
    for entry in entries:
        if str(entry.get("id") or "") == needle:
            return entry
    raise SpiceError(f"unknown artifact id: {artifact_id}")


def _payload_path(task_dir: Path, entry: dict[str, Any]) -> Path:
    digest = str(entry.get("sha256") or "")
    if not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise SpiceError(f"artifact {entry.get('id') or '?'} has invalid sha256")
    return task_dir / OBJECTS_DIR / digest / PAYLOAD_NAME


def _summary_line(entry: dict[str, Any]) -> str:
    return (
        f"{entry.get('id')} {entry.get('name')} {entry.get('content_type')} "
        f"{_format_size(int(entry.get('size') or 0))} "
        f"{entry.get('retention') or DEFAULT_RETENTION}"
    )


def _format_size(size: int) -> str:
    if size < KIB_BYTES:
        return f"{size} B"
    if size < MIB_BYTES:
        return f"{size / KIB_BYTES:.1f} KiB"
    return f"{size / MIB_BYTES:.1f} MiB"


def _artifact_name(raw: str) -> str:
    cleaned = _SAFE_NAME_RE.sub("-", Path(raw).name).strip(".-")
    if not cleaned:
        cleaned = "artifact"
    return cleaned[:MAX_NAME_CHARS]


def _content_type(raw: str | None, name: str, source: Path) -> str:
    detected = (raw or "").strip().lower()
    if not detected:
        detected = (mimetypes.guess_type(name)[0] or "").lower()
    if not detected:
        detected = "text/plain" if _looks_utf8(source) else "application/octet-stream"
    if detected not in TEXT_TYPES and detected not in BINARY_TYPES:
        allowed = ", ".join(sorted((*TEXT_TYPES, *BINARY_TYPES)))
        raise SpiceError(
            f"unsupported artifact content type {detected!r}; allowed: {allowed}"
        )
    return detected


def _looks_utf8(source: Path) -> bool:
    try:
        source.read_text(encoding="utf-8")
    except UnicodeDecodeError:
        return False
    return True


def _retention(raw: str) -> str:
    value = (raw or DEFAULT_RETENTION).strip().lower()
    if value not in RETENTIONS:
        raise SpiceError(
            f"invalid artifact retention {raw!r}; use permanent or prunable"
        )
    return value


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(HASH_CHUNK_BYTES), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _payload_matches(path: Path, digest: str) -> bool:
    try:
        return _sha256_file(path) == digest
    except OSError:
        return False


def _require_payload_integrity(path: Path, digest: str, artifact_id: str) -> None:
    if not path.is_file():
        raise SpiceError(f"artifact {artifact_id} payload is missing")
    if not _payload_matches(path, digest):
        raise SpiceError(f"artifact {artifact_id} payload digest mismatch")


def _copy_file_atomic(source: Path, target: Path) -> None:
    target.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(
        dir=target.parent, prefix=f".{target.name}.", suffix=".tmp"
    )
    try:
        with os.fdopen(fd, "wb") as out, source.open("rb") as inp:
            shutil.copyfileobj(inp, out)
            out.flush()
            os.fsync(out.fileno())
        os.replace(tmp_name, target)
    except BaseException:
        with contextlib.suppress(OSError):
            os.unlink(tmp_name)
        raise


def _next_artifact_id(entries: list[dict[str, Any]]) -> str:
    highest = 0
    for entry in entries:
        raw = str(entry.get("id") or "")
        if raw.startswith("A") and raw[1:].isdigit():
            highest = max(highest, int(raw[1:]))
    return f"A{highest + 1}"


def _now_iso() -> str:
    return datetime.now(UTC).isoformat(timespec="seconds").replace("+00:00", "Z")


def _cutoff(duration: str) -> datetime:
    return datetime.now(UTC) - timedelta(seconds=config.parse_duration(duration))


def _created_at(entry: dict[str, Any]) -> datetime | None:
    raw = str(entry.get("created_at") or "")
    if not raw:
        return None
    try:
        return datetime.fromisoformat(raw.replace("Z", "+00:00"))
    except ValueError:
        return None


def _prune_candidate(entry: dict[str, Any], cutoff: datetime | None) -> bool:
    if str(entry.get("retention") or DEFAULT_RETENTION) != "prunable":
        return False
    if cutoff is None:
        return True
    created = _created_at(entry)
    return created is not None and created < cutoff


def _task_status_for_dir(task_dir: Path, task_label: str) -> str:
    for handle in (task_label, task_dir.name):
        try:
            row = identity.resolve(handle)
        except SpiceError:
            continue
        return str(row.get("status") or "unknown")
    try:
        incepted = identity.incepted_of_handle(task_dir.name)
    except SpiceError:
        return "orphaned"
    try:
        row = identity.resolve(incepted)
    except SpiceError:
        return "orphaned"
    return str(row.get("status") or "unknown")


def _remove_payload_if_unreferenced(
    task_dir: Path, entry: dict[str, Any], remaining: list[dict[str, Any]]
) -> None:
    digest = str(entry.get("sha256") or "")
    if any(other.get("sha256") == digest for other in remaining):
        return
    with contextlib.suppress(FileNotFoundError):
        shutil.rmtree(task_dir / OBJECTS_DIR / digest)

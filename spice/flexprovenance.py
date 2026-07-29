"""Deterministic per-path authorship for flex-limit jitter.

The actor running a scan is not necessarily the actor whose content is being
scanned.  Published task merges preserve their author in a ``Task-Session``
trailer, so identical content can retain one flex boundary in every linked
worktree.  New content remains attributed to the active author until it lands.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path

from spice.process.git import git_read, git_run

_OBJECT_ID_RE = re.compile(r"^[0-9a-f]{40}(?:[0-9a-f]{24})?$")
_PUBLISHED_REFS = (
    "refs/remotes/origin/HEAD",
    "refs/remotes/origin/main",
    "@{upstream}",
    "HEAD",
)


@dataclass(frozen=True)
class FlexProvenance:
    """One visible provenance decision used as a jitter seed."""

    seed: str
    source: str
    commit: str = ""
    blob: str = ""


class FlexProvenanceResolver:
    """Resolve and cache the author identity for candidate path content."""

    def __init__(self, repo_root: Path, active_actor: str) -> None:
        self.repo_root = repo_root.expanduser().resolve()
        self.active_actor = active_actor.strip()
        self._published_head = _published_head(self.repo_root)
        self._cache: dict[tuple[str, str], FlexProvenance] = {}

    def seed_for_path(self, path: Path) -> str:
        return self.resolve(path).seed

    def resolve(self, path: Path) -> FlexProvenance:
        repo_path = _repo_path(path)
        blob = _candidate_blob(self.repo_root, repo_path)
        cache_key = (repo_path.as_posix(), blob)
        if cached := self._cache.get(cache_key):
            return cached

        published = _published_provenance(
            self.repo_root,
            self._published_head,
            repo_path,
            blob,
        )
        if published is not None:
            resolved = published
        elif self.active_actor:
            resolved = FlexProvenance(
                seed=self.active_actor,
                source="active-author",
                blob=blob,
            )
        elif blob:
            resolved = FlexProvenance(
                seed=blob,
                source="candidate-blob",
                blob=blob,
            )
        else:
            resolved = FlexProvenance(
                seed=repo_path.as_posix(),
                source="candidate-path",
            )
        self._cache[cache_key] = resolved
        return resolved


def _repo_path(path: Path) -> Path:
    normalized = path.as_posix().replace("\\", "/").strip()
    while normalized.startswith("./"):
        normalized = normalized[2:]
    candidate = Path(normalized or ".")
    if candidate.is_absolute() or ".." in candidate.parts:
        return Path(".")
    return candidate


def _candidate_blob(repo_root: Path, path: Path) -> str:
    if path.as_posix() == ".":
        return ""
    blob = git_read(
        repo_root,
        "hash-object",
        f"--path={path.as_posix()}",
        "--",
        path.as_posix(),
    )
    return blob if _OBJECT_ID_RE.fullmatch(blob) else ""


def _published_head(repo_root: Path) -> str:
    for ref in _PUBLISHED_REFS:
        commit = git_read(repo_root, "rev-parse", "--verify", f"{ref}^{{commit}}")
        if _OBJECT_ID_RE.fullmatch(commit):
            return commit
    return ""


def _published_provenance(
    repo_root: Path,
    published_head: str,
    path: Path,
    candidate_blob: str,
) -> FlexProvenance | None:
    if not published_head or not candidate_blob or path.as_posix() == ".":
        return None
    result = git_run(
        repo_root,
        "log",
        "--first-parent",
        "--no-renames",
        "--raw",
        "--full-index",
        "--no-abbrev",
        "--format=%H%x09%(trailers:key=Task-Session,valueonly,separator=%x2c)",
        published_head,
        "--",
        path.as_posix(),
    )
    if result.returncode != 0:
        return None

    commit = ""
    session = ""
    for line in result.stdout.splitlines():
        if line.startswith(":"):
            metadata, separator, _rendered_path = line.partition("\t")
            fields = metadata.split()
            if (
                separator
                and len(fields) >= 5
                and fields[3] == candidate_blob
                and _OBJECT_ID_RE.fullmatch(commit)
            ):
                author = session.split(",", 1)[0].strip()
                return FlexProvenance(
                    seed=author or commit,
                    source="published-task-session" if author else "published-commit",
                    commit=commit,
                    blob=candidate_blob,
                )
            continue
        head, separator, trailers = line.partition("\t")
        if _OBJECT_ID_RE.fullmatch(head):
            commit = head
            session = trailers if separator else ""
    return None

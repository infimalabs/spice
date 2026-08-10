"""Deterministic per-path authorship for flex-limit jitter.

The actor running a scan is not necessarily the actor whose content is being
scanned.  Published task merges preserve their author in a ``Task-Session``
trailer, so identical content can retain one flex boundary in every linked
worktree.  New content remains attributed to the active author until it lands.
"""

from __future__ import annotations

import re
from collections.abc import Iterable, Mapping
from dataclasses import dataclass
from pathlib import Path

from spice.errors import SpiceError
from spice.process.git import git_read, git_run, run_git_command

_OBJECT_ID_RE = re.compile(r"^[0-9a-f]{40}(?:[0-9a-f]{24})?$")
_PUBLISHED_REFS = (
    "refs/remotes/origin/HEAD",
    "refs/remotes/origin/main",
    "@{upstream}",
    "HEAD",
)
_HISTORY_MARKER = "flex-provenance-commit"
_HISTORY_FORMAT = (
    f"{_HISTORY_MARKER}%x00%H%x00%(trailers:key=Task-Session,valueonly,separator=%x2c)"
)
_MAX_HISTORY_PATHSPEC_BYTES = 128 * 1024


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
        self._candidate_blobs: dict[str, str] = {}
        self._cache: dict[tuple[str, str], FlexProvenance] = {}

    def seed_for_path(self, path: Path) -> str:
        return self.resolve(path).seed

    def preload(self, paths: Iterable[Path]) -> None:
        """Resolve one tracked candidate set with bounded Git process work."""
        repo_paths = tuple(
            dict.fromkeys(
                repo_path
                for path in paths
                if (repo_path := _repo_path(path)).as_posix() != "."
            )
        )
        missing = tuple(
            path for path in repo_paths if path.as_posix() not in self._candidate_blobs
        )
        if missing:
            self._candidate_blobs.update(_candidate_blobs(self.repo_root, missing))

        candidates = {
            path: self._candidate_blobs[path.as_posix()]
            for path in repo_paths
            if (
                path.as_posix(),
                self._candidate_blobs[path.as_posix()],
            )
            not in self._cache
        }
        published = _published_provenances(
            self.repo_root,
            self._published_head,
            candidates,
        )
        for path, blob in candidates.items():
            key = (path.as_posix(), blob)
            self._cache[key] = published.get(key) or self._fallback(path, blob)

    def resolve(self, path: Path) -> FlexProvenance:
        repo_path = _repo_path(path)
        rendered = repo_path.as_posix()
        if rendered not in self._candidate_blobs:
            self._candidate_blobs[rendered] = _candidate_blob(self.repo_root, repo_path)
        blob = self._candidate_blobs[rendered]
        cache_key = (repo_path.as_posix(), blob)
        if cached := self._cache.get(cache_key):
            return cached

        published = _published_provenance(
            self.repo_root,
            self._published_head,
            repo_path,
            blob,
        )
        resolved = published or self._fallback(repo_path, blob)
        self._cache[cache_key] = resolved
        return resolved

    def _fallback(self, repo_path: Path, blob: str) -> FlexProvenance:
        if self.active_actor:
            return FlexProvenance(
                seed=self.active_actor,
                source="active-author",
                blob=blob,
            )
        if blob:
            return FlexProvenance(
                seed=blob,
                source="candidate-blob",
                blob=blob,
            )
        return FlexProvenance(
            seed=repo_path.as_posix(),
            source="candidate-path",
        )


def preload_flex_provenance(
    resolver: FlexProvenanceResolver,
    paths: Iterable[Path],
) -> None:
    """Populate one resolver through the public bulk-provenance seam."""
    resolver.preload(paths)


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


def _candidate_blobs(repo_root: Path, paths: tuple[Path, ...]) -> dict[str, str]:
    rendered = tuple(path.as_posix() for path in paths)
    result = run_git_command(
        ["git", "-C", str(repo_root), "hash-object", "--stdin-paths"],
        capture_output=True,
        text=True,
        input="\n".join(rendered) + "\n",
    )
    blobs = result.stdout.splitlines()
    if result.returncode != 0:
        detail = result.stderr.strip() or f"exit {result.returncode}"
        raise SpiceError(f"bulk flex provenance hashing failed: {detail}")
    if len(blobs) != len(paths) or not all(
        _OBJECT_ID_RE.fullmatch(blob) for blob in blobs
    ):
        raise SpiceError(
            "bulk flex provenance hashing returned malformed output: "
            f"expected {len(paths)} object id(s), received {len(blobs)}"
        )
    return dict(zip(rendered, blobs, strict=True))


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


def _published_provenances(
    repo_root: Path,
    published_head: str,
    candidates: Mapping[Path, str],
) -> dict[tuple[str, str], FlexProvenance]:
    targets = {
        path.as_posix(): blob
        for path, blob in candidates.items()
        if blob and path.as_posix() != "."
    }
    if not published_head or not targets:
        return {}
    pathspecs = _bounded_history_pathspecs(tuple(targets))
    command = [
        "log",
        "--first-parent",
        "--no-renames",
        "--raw",
        "-z",
        "--full-index",
        "--no-abbrev",
        f"--format={_HISTORY_FORMAT}",
        published_head,
    ]
    if pathspecs:
        command.extend(["--", *pathspecs])
    result = git_run(repo_root, *command)
    if result.returncode != 0:
        detail = result.stderr.strip() or f"exit {result.returncode}"
        raise SpiceError(f"bulk flex provenance history failed: {detail}")
    return _parse_published_provenances(result.stdout, targets)


def _bounded_history_pathspecs(paths: tuple[str, ...]) -> tuple[str, ...]:
    encoded_bytes = sum(len(path.encode("utf-8")) + 1 for path in paths)
    return paths if encoded_bytes <= _MAX_HISTORY_PATHSPEC_BYTES else ()


def _parse_published_provenances(
    output: str,
    targets: Mapping[str, str],
) -> dict[tuple[str, str], FlexProvenance]:
    resolved: dict[tuple[str, str], FlexProvenance] = {}
    commit = ""
    session = ""
    records = output.split("\0")
    index = 0
    while index < len(records):
        record = records[index].lstrip("\n")
        if record == _HISTORY_MARKER and index + 2 < len(records):
            commit = records[index + 1]
            session = records[index + 2]
            index += 3
            continue
        if record.startswith(":") and index + 1 < len(records):
            fields = record.split()
            path = records[index + 1]
            candidate_blob = targets.get(path)
            if (
                len(fields) >= 5
                and candidate_blob
                and fields[3] == candidate_blob
                and _OBJECT_ID_RE.fullmatch(commit)
            ):
                author = session.split(",", 1)[0].strip()
                resolved.setdefault(
                    (path, candidate_blob),
                    FlexProvenance(
                        seed=author or commit,
                        source=(
                            "published-task-session" if author else "published-commit"
                        ),
                        commit=commit,
                        blob=candidate_blob,
                    ),
                )
            index += 2
            continue
        index += 1
    return resolved

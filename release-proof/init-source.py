#!/usr/bin/env python3
"""Turn an exported source snapshot into a clean synthetic Git repository."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
from pathlib import Path
from typing import TypedDict

SOURCE_PROVENANCE = Path(".release-proof/source.json")
IDENTITIES_GIT_PATH = "release-proof-identities.json"
OBJECT_FORMAT_BY_ID_LENGTH = {40: "sha1", 64: "sha256"}
OBJECT_ID = re.compile(
    "|".join(rf"[0-9a-f]{{{length}}}" for length in OBJECT_FORMAT_BY_ID_LENGTH)
)


class SourceIdentity(TypedDict):
    commit: str
    tree: str
    commit_epoch: int


def _git_environment() -> dict[str, str]:
    return {
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_CONFIG_NOSYSTEM": "1",
        "LC_ALL": "C",
        "PATH": os.environ["PATH"],  # env-policy: allow
        "TZ": "UTC",
    }


def _git(root: Path, *arguments: str, environment: dict[str, str]) -> str:
    completed = subprocess.run(
        ["git", "-C", str(root), *arguments],
        check=True,
        capture_output=True,
        env=environment,
        text=True,
    )
    return completed.stdout.strip()


def _load_source(root: Path) -> SourceIdentity:
    source_path = root / SOURCE_PROVENANCE
    payload = json.loads(source_path.read_text(encoding="utf-8"))
    if payload.get("schema_version") != 1 or set(payload) != {
        "schema_version",
        "source",
    }:
        raise SystemExit(f"invalid source provenance schema: {source_path}")

    source = payload.get("source")
    if not isinstance(source, dict) or set(source) != {
        "commit",
        "tree",
        "commit_epoch",
    }:
        raise SystemExit(f"invalid source identity: {source_path}")

    commit = source.get("commit")
    tree = source.get("tree")
    epoch = source.get("commit_epoch")
    if not isinstance(commit, str) or OBJECT_ID.fullmatch(commit) is None:
        raise SystemExit(f"invalid source commit: {source_path}")
    if not isinstance(tree, str) or OBJECT_ID.fullmatch(tree) is None:
        raise SystemExit(f"invalid source tree: {source_path}")
    if len(commit) != len(tree):
        raise SystemExit(f"inconsistent source object formats: {source_path}")
    if not isinstance(epoch, int) or epoch < 0:
        raise SystemExit(f"invalid source commit timestamp: {source_path}")
    return {"commit": commit, "tree": tree, "commit_epoch": epoch}


def _source_object_format(source: SourceIdentity) -> str:
    """Select the Git hash algorithm already encoded by the source IDs."""
    return OBJECT_FORMAT_BY_ID_LENGTH[len(source["tree"])]


def _write_json(path: Path, payload: dict[str, object]) -> None:
    temporary = path.with_name(f".{path.name}.tmp")
    temporary.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    temporary.replace(path)


def initialize(root: Path) -> dict[str, object]:
    root = root.resolve(strict=True)
    source = _load_source(root)
    environment = _git_environment()

    _git(
        root,
        "init",
        "--quiet",
        "--initial-branch=release-proof",
        f"--object-format={_source_object_format(source)}",
        environment=environment,
    )
    _git(root, "config", "core.autocrlf", "false", environment=environment)
    _git(root, "config", "core.filemode", "true", environment=environment)
    _git(root, "config", "commit.gpgsign", "false", environment=environment)
    # The context came exclusively from ``git archive``, so every path here
    # belongs to tracked HEAD. Force is necessary when HEAD itself tracks a
    # path that a later ignore rule also matches; it cannot admit host residue.
    _git(
        root,
        "add",
        "--force",
        "--all",
        "--",
        ".",
        f":(exclude){SOURCE_PROVENANCE.as_posix()}",
        environment=environment,
    )
    exported_tree = _git(root, "write-tree", environment=environment)
    if exported_tree != source["tree"]:
        raise SystemExit(
            "exported tracked tree does not match source provenance: "
            f"expected {source['tree']}, resolved {exported_tree}"
        )

    _git(
        root,
        "add",
        "--force",
        "--",
        SOURCE_PROVENANCE.as_posix(),
        environment=environment,
    )
    commit_environment = dict(environment)
    commit_environment.update(
        {
            "GIT_AUTHOR_DATE": f"{source['commit_epoch']} +0000",
            "GIT_AUTHOR_EMAIL": "release-proof@spice.invalid",
            "GIT_AUTHOR_NAME": "Spice Release Proof",
            "GIT_COMMITTER_DATE": f"{source['commit_epoch']} +0000",
            "GIT_COMMITTER_EMAIL": "release-proof@spice.invalid",
            "GIT_COMMITTER_NAME": "Spice Release Proof",
        }
    )
    _git(
        root,
        "commit",
        "--quiet",
        "--message",
        "Synthetic release-proof source snapshot",
        environment=commit_environment,
    )

    synthetic = {
        "commit": _git(root, "rev-parse", "HEAD^{commit}", environment=environment),
        "tree": _git(root, "rev-parse", "HEAD^{tree}", environment=environment),
    }
    identities: dict[str, object] = {
        "schema_version": 1,
        "source": source,
        "synthetic": synthetic,
    }
    identities_path = Path(
        _git(
            root,
            "rev-parse",
            "--git-path",
            IDENTITIES_GIT_PATH,
            environment=environment,
        )
    )
    if not identities_path.is_absolute():
        identities_path = root / identities_path
    _write_json(identities_path, identities)

    status = _git(
        root,
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
        environment=environment,
    )
    if status:
        raise SystemExit(f"synthetic release-proof worktree is dirty:\n{status}")
    return identities


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("source", type=Path)
    arguments = parser.parse_args()
    identities = initialize(arguments.source)
    print(json.dumps(identities, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

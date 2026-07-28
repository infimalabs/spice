"""Ordered, digest-bound release plan data and operation rendering."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from spice.commandplan import command_plan_payload


@dataclass(frozen=True)
class ReleasePlanOperation:
    """One ordered release action described without executing it."""

    action: str
    detail: str


@dataclass(frozen=True)
class ReleasePlan:
    """A complete operator-readable plan for one mutating release verb."""

    repository: Path
    action: str
    version: str
    source_commit: str
    notes_sha256: str | None
    release_commit: str | None
    notes_file: Path | None
    operations: tuple[ReleasePlanOperation, ...]
    schema_version: int = 1

    def payload(self) -> dict[str, object]:
        return command_plan_payload(
            command=f"release {self.action}",
            metadata={
                "repository": str(self.repository),
                "action": self.action,
                "version": self.version,
                "source_commit": self.source_commit,
                "notes_sha256": self.notes_sha256,
                "release_commit": self.release_commit,
                "notes_file": (
                    str(self.notes_file) if self.notes_file is not None else None
                ),
            },
            operations=[
                {
                    "kind": operation.action,
                    "target": operation.detail,
                    "scope": "repository",
                    "action": operation.action,
                    "detail": operation.detail,
                    "source_commit": self.source_commit,
                    "notes_sha256": self.notes_sha256,
                    "release_commit": self.release_commit,
                }
                for operation in self.operations
            ],
        )

    def rows(self) -> list[str]:
        digest = str(self.payload()["plan_digest"])
        rows = [
            f"release-plan schema={self.schema_version} action={self.action} "
            f"version={self.version} digest={digest}",
            f"repository={self.repository}",
        ]
        if self.release_commit is not None:
            rows.append(f"release-commit={self.release_commit}")
        if self.notes_file is not None:
            rows.append(f"notes-file={self.notes_file}")
        rows.extend(
            f"{order}. {operation.action} {operation.detail}"
            for order, operation in enumerate(self.operations, start=1)
        )
        rows.append("preview: no changes applied; pass --apply to execute")
        return rows


def publication_operations(version: str) -> tuple[ReleasePlanOperation, ...]:
    return (
        curated_notes_operation(),
        ReleasePlanOperation(
            "check-publish", f"dry-run upload artifacts for {version}"
        ),
        ReleasePlanOperation("push-main", "push HEAD to origin/main"),
        ReleasePlanOperation("publish-package", f"upload spice-harness {version}"),
        ReleasePlanOperation("wait-for-pypi", f"wait for PyPI to report {version}"),
        *github_publication_operations(version, check_notes=False),
    )


def curated_notes_operation() -> ReleasePlanOperation:
    return ReleasePlanOperation(
        "check-release-notes",
        "refuse notes that exactly match the generated Highlights draft",
    )


def github_publication_operations(
    version: str,
    *,
    check_notes: bool = True,
) -> tuple[ReleasePlanOperation, ...]:
    notes = (curated_notes_operation(),) if check_notes else ()
    return (
        *notes,
        ReleasePlanOperation("create-tag", f"create v{version} when absent"),
        ReleasePlanOperation("push-tag", f"push v{version} to origin"),
        ReleasePlanOperation(
            "create-github-release", f"publish release v{version} when absent"
        ),
    )

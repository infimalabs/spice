"""Release command planning, targeting, and note-curation checks."""

import json
import shlex
from pathlib import Path

import pytest

import spice.release as release
from spice.cli.mounts import mounted_commands
from spice.commandplan import PLAN_DIGEST_HEX_LENGTH
from spice.errors import SpiceError
from spice.release import build_release_parser


def test_release_parser_accepts_prepare_notes_publish_and_one_pass():
    parser = build_release_parser()

    prepare = parser.parse_args(["prepare", "minor"])
    notes = parser.parse_args(
        ["notes", "0.3.0", "--output", "notes.md", "--release-commit", "HEAD"]
    )
    publish = parser.parse_args(
        ["publish", "--notes-file", "curated.md", "--release-commit", "HEAD"]
    )
    github = parser.parse_args(["github", "0.3.0", "--release-commit", "HEAD"])
    preview = parser.parse_args(
        ["range", "0.3.0", "--release-commit", "refs/remotes/origin/main"]
    )
    one_pass = parser.parse_args(["minor", "--notes-file", "curated.md"])

    assert prepare.release_mode == "prepare"
    assert prepare.bump == "minor"
    assert release.BUMP_CHOICES == ("minor", "patch")
    assert notes.release_mode == "notes"
    assert notes.version == "0.3.0"
    assert notes.output == Path("notes.md")
    assert notes.release_commit == "HEAD"
    assert publish.release_mode == "publish"
    assert publish.notes_file == Path("curated.md")
    assert publish.release_commit == "HEAD"
    assert github.release_mode == "github"
    assert github.version == "0.3.0"
    assert github.release_commit == "HEAD"
    assert preview.release_mode == "range"
    assert preview.version == "0.3.0"
    assert preview.release_commit == "refs/remotes/origin/main"
    assert one_pass.release_mode == "release"
    assert one_pass.bump == "minor"
    assert one_pass.notes_file == Path("curated.md")


def test_prepare_instructions_handoff_applies_publish(capsys):
    release.print_prepare_instructions("0.30.0")

    output = capsys.readouterr().out
    publish_command = output.rsplit("then run ", maxsplit=1)[1].strip()
    publish_argv = shlex.split(publish_command)
    publish = build_release_parser().parse_args(publish_argv[2:])

    assert publish_argv[:2] == ["spice", "release"]
    assert publish.release_mode == "publish"
    assert publish.notes_file == Path("/tmp/spice-release-0.30.0-notes.md")
    assert publish.apply is True


def test_release_human_and_json_preview_share_order_without_running_gates(
    tmp_path, monkeypatch, capsys
):
    parser = build_release_parser()
    monkeypatch.setattr(release, "repo_root", lambda: tmp_path)
    monkeypatch.setattr(release, "ensure_clean_worktree", lambda root: None)
    monkeypatch.setattr(release, "ensure_release_preconditions", lambda root: None)
    monkeypatch.setattr(release, "preview_bumped_version", lambda bump: "0.10.0")
    monkeypatch.setattr(release, "git", lambda *args: "source-head")
    monkeypatch.setattr(
        release,
        "run_release_gates",
        lambda *args, **kwargs: pytest.fail("preview must not run release gates"),
    )

    assert release.handle_release(parser.parse_args(["prepare", "minor"])) == 0
    human = capsys.readouterr().out.splitlines()
    assert (
        release.handle_release(parser.parse_args(["prepare", "minor", "--json"])) == 0
    )
    machine = json.loads(capsys.readouterr().out)

    assert machine["version"] == "0.10.0"
    assert [row.split(" ", 2)[1] for row in human if row[:1].isdigit()] == [
        operation["action"] for operation in machine["operations"]
    ]
    assert human[-1] == "preview: no changes applied; pass --apply to execute"


@pytest.mark.parametrize(
    "argv",
    (
        ("minor",),
        ("patch",),
        ("prepare", "minor"),
        ("publish",),
        ("github",),
    ),
    ids=("minor", "patch", "prepare", "publish", "github"),
)
def test_every_mutating_release_verb_builds_an_ordered_plan(
    argv, tmp_path, monkeypatch
):
    parser = build_release_parser()
    monkeypatch.setattr(release, "ensure_clean_worktree", lambda root: None)
    monkeypatch.setattr(release, "ensure_release_preconditions", lambda root: None)
    monkeypatch.setattr(release, "ensure_notes_file", lambda path: None)
    monkeypatch.setattr(release, "preview_bumped_version", lambda bump: "0.10.0")
    monkeypatch.setattr(release, "current_version", lambda: "0.9.0")
    monkeypatch.setattr(release, "git", lambda *args: "source-head")
    monkeypatch.setattr(
        release, "release_commit_for_target", lambda version, target: "release-head"
    )
    monkeypatch.setattr(
        release, "ensure_publish_release_commit_is_head", lambda commit: None
    )

    plan = release.plan_release(parser.parse_args(list(argv)), tmp_path)
    payload = plan.payload()

    assert payload["protocol"] == "spice.command-plan"
    assert len(payload["plan_digest"]) == PLAN_DIGEST_HEX_LENGTH
    assert [item["order"] for item in payload["operations"]] == list(
        range(1, len(plan.operations) + 1)
    )
    assert [row.split(" ", 2)[1] for row in plan.rows() if row[:1].isdigit()] == [
        item["action"] for item in payload["operations"]
    ]


def test_release_plan_digest_binds_source_commit_and_release_notes(
    tmp_path, monkeypatch
):
    parser = build_release_parser()
    notes = tmp_path / "notes.md"
    notes.write_text("first notes\n", encoding="utf-8")
    source_commit = "first-source"
    release_target = "first-release"

    monkeypatch.setattr(release, "ensure_clean_worktree", lambda root: None)
    monkeypatch.setattr(release, "ensure_notes_file", lambda path: None)
    monkeypatch.setattr(release, "current_version", lambda: "0.9.0")
    monkeypatch.setattr(
        release, "release_commit_for_target", lambda version, target: release_target
    )
    monkeypatch.setattr(
        release, "ensure_publish_release_commit_is_head", lambda commit: None
    )
    monkeypatch.setattr(release, "git", lambda *args: source_commit)
    args = parser.parse_args(["publish", "--notes-file", str(notes)])

    first = release.plan_release(args, tmp_path).payload()
    notes.write_text("second notes\n", encoding="utf-8")
    second = release.plan_release(args, tmp_path).payload()
    source_commit = "second-source"
    third = release.plan_release(args, tmp_path).payload()
    release_target = "second-release"
    fourth = release.plan_release(args, tmp_path).payload()

    assert first["plan_digest"] != second["plan_digest"]
    assert second["plan_digest"] != third["plan_digest"]
    assert third["plan_digest"] != fourth["plan_digest"]


def test_publish_plan_checks_release_notes_before_expensive_gates(
    tmp_path, monkeypatch
):
    notes = tmp_path / "curated.md"
    notes.write_text("## Highlights\n\n- Curated.\n", encoding="utf-8")
    monkeypatch.setattr(release, "ensure_clean_worktree", lambda root: None)
    monkeypatch.setattr(release, "current_version", lambda: "0.30.1")
    monkeypatch.setattr(
        release,
        "release_commit_for_target",
        lambda version, target: "release-head",
    )
    monkeypatch.setattr(
        release, "ensure_publish_release_commit_is_head", lambda commit: None
    )
    monkeypatch.setattr(release, "git", lambda *args: "source-head")
    args = build_release_parser().parse_args(["publish", "--notes-file", str(notes)])

    plan = release.plan_release(args, tmp_path)

    assert [operation.action for operation in plan.operations[:2]] == [
        "check-release-notes",
        "verify-installed-runtime",
    ]


def test_release_docs_show_lane_release_workflow():
    release_doc = Path("docs/release.md").read_text(encoding="utf-8")
    release_section = release_doc.split("\n\n", 1)[1]
    help_text = build_release_parser().format_help()
    normalized_help = " ".join(help_text.split())
    normalized_section = " ".join(release_section.split())
    release_commands = (
        release_section.split("```sh", 1)[1].split("```", 1)[0].strip().splitlines()
    )

    assert "{check,minor,patch,prepare,notes,range,publish,github}" in help_text
    assert "clean synchronized worktree" in normalized_help
    assert normalized_section.startswith(
        "Releases are cut from a clean synchronized worktree with this "
        "repository's mounted `spice release` command. Lane branches are "
        "allowed; the release command pushes the prepared release commit to "
        "`origin/main`."
    )
    assert release_commands == [
        "spice release check           # run the release gates only; bumps nothing",
        "spice release range           # preview latest-release-tag..HEAD before prepare",
        "spice release prepare minor   # preview bump, validation, and commit",
        "spice release prepare minor --apply",
        "spice release notes > /tmp/spice-release-notes.md",
        "spice release publish --notes-file /tmp/spice-release-notes.md --apply",
        "spice release minor           # preview one-pass bump, validation, and publish",
        "spice release minor --apply",
    ]
    assert (
        "Bare `minor`, `patch`, `prepare`, `publish`, and `github` also remain "
        "mutation-free: each renders its ordered release plan, while `--json` "
        "renders the same plan for machines. Only `--apply` runs that plan."
        in normalized_section
    )
    assert (
        "Before `prepare`, the bare `spice release range` command resolves the "
        "highest version tag merged into the current `HEAD` and previews "
        "`latest-tag..HEAD` without requiring a future version literal."
        in normalized_section
    )
    assert (
        "When release history is unusual, pass `--release-commit <rev>` to "
        "choose the commit used for `spice release range`, `spice release "
        "notes`, or `spice release github`." in normalized_section
    )
    assert (
        "Bare `spice release notes` is state-aware: before `prepare` it labels "
        "the draft `unreleased`; after the bump commit it recognizes the "
        "untagged current version and writes versioned package and release-tag "
        "markers." in normalized_section
    )
    assert "first release gate is the installed-runtime boundary" in normalized_section
    assert "runs it with `-P` and no `PYTHONPATH`" in normalized_section
    assert "branch state alone" in normalized_section
    assert "registry-installed release with no `direct_url.json`" in normalized_section
    assert "exact checked-out release tag" in normalized_section
    assert "reports both candidate and installed identities" in normalized_section
    assert "comparing raw bytes, not a rendered diff" in normalized_section
    assert release_section.index("Use a minor release") < release_section.index(
        "Use a patch release"
    )


def test_repo_mounts_release_command(tmp_path):
    (tmp_path / ".git").mkdir()
    (tmp_path / "spice.toml").write_text(
        '[commands]\nrelease = ["uv", "run", "python", "-m", "spice.release"]\n',
        encoding="utf-8",
    )

    assert mounted_commands(tmp_path)[("release",)] == (
        "uv",
        "run",
        "python",
        "-m",
        "spice.release",
    )


def test_release_notes_mode_writes_output_without_release_sync(tmp_path, monkeypatch):
    parser = build_release_parser()
    notes_path = tmp_path / "notes.md"
    args = parser.parse_args(["notes", "0.3.0", "--output", str(notes_path)])

    def fail_release_sync(_root):
        raise AssertionError("notes generation is read-only")

    starting_cwd = Path.cwd()
    monkeypatch.setattr(release, "repo_root", lambda: tmp_path)
    monkeypatch.setattr(release, "ensure_clean_worktree", fail_release_sync)
    monkeypatch.setattr(
        release,
        "release_commit_for_version",
        lambda version: f"commit-for-{version}",
    )
    monkeypatch.setattr(
        release,
        "release_notes_for_version",
        lambda version, commit: f"notes for {version} at {commit}\n",
    )

    result = release.handle_release(args)

    assert result == 0
    assert Path.cwd() == starting_cwd
    assert notes_path.read_text(encoding="utf-8") == (
        "notes for 0.3.0 at commit-for-0.3.0\n"
    )


def test_release_notes_mode_uses_explicit_release_commit_target(tmp_path, monkeypatch):
    parser = build_release_parser()
    notes_path = tmp_path / "notes.md"
    args = parser.parse_args(
        [
            "notes",
            "0.3.0",
            "--release-commit",
            "main",
            "--output",
            str(notes_path),
        ]
    )

    seen = []
    monkeypatch.setattr(release, "repo_root", lambda: tmp_path)
    monkeypatch.setattr(
        release,
        "release_commit_for_target",
        lambda version, target: seen.append((version, target)) or "resolved-main",
    )
    monkeypatch.setattr(release, "release_version_at_commit", lambda commit: "0.3.0")
    monkeypatch.setattr(
        release,
        "release_notes_for_version",
        lambda version, commit: f"notes for {version} at {commit}\n",
    )

    result = release.handle_release(args)

    assert result == 0
    assert seen == [("0.3.0", "main")]
    assert notes_path.read_text(encoding="utf-8") == (
        "notes for 0.3.0 at resolved-main\n"
    )


def test_release_commit_for_tagged_version_uses_tagged_commit(monkeypatch):
    def fake_git(*args):
        if args == ("tag", "--list", "v0.9.0"):
            return "v0.9.0"
        if args == ("rev-list", "-n", "1", "v0.9.0"):
            return "tagged-commit"
        raise AssertionError(args)

    monkeypatch.setattr(release, "git", fake_git)

    assert release.release_commit_for_version("0.9.0") == "tagged-commit"


def test_release_commit_for_current_unreleased_version_uses_head(monkeypatch):
    def fake_git(*args):
        if args == ("tag", "--list", "v0.9.0"):
            return ""
        if args == ("rev-parse", "HEAD"):
            return "current-head"
        if args == (
            "log",
            "--format=%H",
            "--grep",
            "^release: bump to 0.9.0$",
            "-n",
            "1",
        ):
            return "old-bump-commit"
        raise AssertionError(args)

    monkeypatch.setattr(release, "git", fake_git)
    monkeypatch.setattr(release, "current_version", lambda: "0.9.0")

    assert release.release_commit_for_version("0.9.0") == "current-head"


def test_release_commit_for_target_resolves_explicit_commitish(monkeypatch):
    def fake_git(*args):
        if args == ("rev-parse", "--verify", "main^{commit}"):
            return "resolved-main"
        raise AssertionError(args)

    monkeypatch.setattr(release, "git", fake_git)

    assert release.release_commit_for_target("0.9.0", "main") == "resolved-main"


def test_publish_mode_with_head_target_runs_gates_before_publish(tmp_path, monkeypatch):
    parser = build_release_parser()
    args = parser.parse_args(["publish", "--release-commit", "HEAD", "--apply"])
    calls = []

    monkeypatch.setattr(release, "repo_root", lambda: tmp_path)
    monkeypatch.setattr(release, "ensure_clean_worktree", lambda root: None)
    monkeypatch.setattr(release, "current_version", lambda: "0.9.0")
    monkeypatch.setattr(release, "git", lambda *args: "source-head")
    monkeypatch.setattr(
        release,
        "release_commit_for_target",
        lambda version, target: calls.append(("target", version, target)) or "head",
    )
    monkeypatch.setattr(release, "release_version_at_commit", lambda commit: "0.9.0")
    monkeypatch.setattr(
        release,
        "ensure_publish_release_commit_is_head",
        lambda commit: calls.append(("head", commit)),
    )
    monkeypatch.setattr(
        release,
        "ensure_curated_release_notes",
        lambda notes_file, commit: calls.append(("notes", notes_file, commit)),
    )
    monkeypatch.setattr(
        release,
        "require_installed_cli_matches_release",
        lambda root: calls.append(("installed", root)),
    )
    monkeypatch.setattr(
        release, "clean_build_artifacts", lambda root: calls.append(("clean", root))
    )
    monkeypatch.setattr(
        release, "run_constitution_gate", lambda: calls.append("constitution")
    )
    monkeypatch.setattr(
        release, "run_artifact_gate", lambda version: calls.append(version)
    )
    monkeypatch.setattr(
        release,
        "publish_release",
        lambda version, notes_file, *, release_commit=None: calls.append(
            ("publish", version, notes_file, release_commit)
        ),
    )

    result = release._handle_release_from_root(args, tmp_path)

    assert result == 0
    assert calls == [
        ("target", "0.9.0", "HEAD"),
        ("head", "head"),
        ("notes", None, "head"),
        ("installed", tmp_path),
        ("clean", tmp_path),
        "constitution",
        "0.9.0",
        ("publish", "0.9.0", None, "head"),
    ]


def test_release_apply_refuses_untouched_generated_notes_before_publication(
    tmp_path, monkeypatch
):
    canonical = (
        "> [!IMPORTANT]\n"
        "## Highlights\n\n"
        "_Replace this line with a short, curated set of highlights folded from "
        "the changes below._\n\n"
        "<details>\n<summary>Task-level changes</summary>\n\n"
        "- Fixed the release path. (abcdef1)\n"
        "</details>\n"
    )
    notes = tmp_path / "notes.md"
    notes.write_bytes(canonical.encode("utf-8"))
    args = build_release_parser().parse_args(
        ["github", "0.30.0", "--notes-file", str(notes), "--apply"]
    )
    plan = release.ReleasePlan(
        repository=tmp_path,
        action="github",
        version="0.30.0",
        source_commit="source-head",
        notes_sha256="notes-digest",
        release_commit="release-head",
        notes_file=notes,
        operations=(),
    )

    monkeypatch.setattr(
        release,
        "canonical_release_notes_for_commit",
        lambda commit: canonical,
    )
    monkeypatch.setattr(
        release,
        "publish_github_release",
        lambda *args, **kwargs: pytest.fail("untouched notes reached publication"),
    )

    with pytest.raises(SpiceError, match="Highlights placeholder"):
        release.apply_release_plan(args, tmp_path, plan)


def test_release_apply_accepts_curated_highlights_with_generated_inventory_preserved(
    tmp_path, monkeypatch
):
    placeholder = (
        "_Replace this line with a short, curated set of highlights folded from "
        "the changes below._"
    )
    canonical = (
        "> [!IMPORTANT]\n"
        "## Highlights\n\n"
        f"{placeholder}\n\n"
        "<details>\n<summary>Task-level changes</summary>\n\n"
        "- Fixed the release path. (abcdef1)\n"
        "</details>\n"
    )
    curated = canonical.replace(
        f"> [!IMPORTANT]\n## Highlights\n\n{placeholder}",
        "## Highlights\n\n- Refused uncurated generated release notes.",
    )
    assert (
        curated.split("<details>", maxsplit=1)[1]
        == canonical.split("<details>", maxsplit=1)[1]
    )

    notes = tmp_path / "notes.md"
    notes.write_bytes(curated.encode("utf-8"))
    args = build_release_parser().parse_args(
        ["github", "0.30.0", "--notes-file", str(notes), "--apply"]
    )
    plan = release.ReleasePlan(
        repository=tmp_path,
        action="github",
        version="0.30.0",
        source_commit="source-head",
        notes_sha256="notes-digest",
        release_commit="release-head",
        notes_file=notes,
        operations=(),
    )
    published = []

    monkeypatch.setattr(
        release,
        "canonical_release_notes_for_commit",
        lambda commit: canonical,
    )
    monkeypatch.setattr(
        release,
        "publish_github_release",
        lambda version, notes_file, *, release_commit=None: published.append(
            (version, notes_file, release_commit)
        ),
    )

    assert release.apply_release_plan(args, tmp_path, plan) == 0
    assert published == [("0.30.0", notes, "release-head")]


def test_canonical_release_notes_derive_version_and_range_from_candidate_commit(
    monkeypatch,
):
    record_calls = []

    def fake_git(*args):
        if args == ("show", "release-head:pyproject.toml"):
            return '[project]\nversion = "0.30.0"\n'
        if args == (
            "tag",
            "--merged",
            "release-head",
            "--list",
            "v*",
            "--sort=-v:refname",
        ):
            return "v0.30.0\nv0.29.0"
        if args == ("rev-parse", "--short", "release-head"):
            return "release"
        raise AssertionError(args)

    monkeypatch.setattr(release, "git", fake_git)
    monkeypatch.setattr(
        release,
        "commit_records",
        lambda previous, commit: record_calls.append((previous, commit)) or [],
    )

    notes = release.canonical_release_notes_for_commit("release-head")

    assert record_calls == [("v0.29.0", "release-head")]
    assert "- PyPI release: `spice-harness==0.30.0`" in notes
    assert "- Commit range: `v0.29.0..release`" in notes
    assert "- Release tag: `v0.30.0`" in notes

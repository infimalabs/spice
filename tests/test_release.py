"""Release command parsing and release-note highlights."""

import subprocess
import sys
from pathlib import Path

import pytest

import spice.agent.driver as agent_driver
import spice.release as release
from spice.errors import SpiceError
from spice.cli.mounts import mounted_commands
from spice.release import (
    ReleaseRecord,
    build_release_parser,
    edited_release_highlight,
    render_release_notes,
)


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
    one_pass = parser.parse_args(["minor"])

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
        "spice release prepare minor   # bump, validate, commit, stop before publish",
        "spice release notes > /tmp/spice-release-notes.md",
        "spice release publish --notes-file /tmp/spice-release-notes.md",
        "spice release minor           # one-pass bump, validate, commit, publish",
    ]
    # The docs must keep naming the trap: `prepare` reads like a rehearsal and
    # is not one, and `check` is the only action that answers the question
    # without changing the tree.
    assert (
        "It is the only mutation-free way to get that answer. `prepare` is not "
        "the safe rehearsal its name suggests, because it bumps the version and "
        "commits the bump before it stops." in normalized_section
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
    assert "comparing raw bytes, not a rendered diff" in normalized_section
    assert release_section.index("Use a minor release") < release_section.index(
        "Use a patch release"
    )


def test_repo_mounts_release_command(tmp_path):
    (tmp_path / ".git").mkdir()
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.commands]\n"
        'release = ["uv", "run", "python", "-m", "spice.release"]\n',
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
    args = parser.parse_args(["publish", "--release-commit", "HEAD"])
    calls = []

    monkeypatch.setattr(release, "repo_root", lambda: tmp_path)
    monkeypatch.setattr(release, "ensure_clean_worktree", lambda root: None)
    monkeypatch.setattr(release, "current_version", lambda: "0.9.0")
    monkeypatch.setattr(
        release,
        "release_commit_for_target",
        lambda version, target: calls.append(("target", version, target)) or "head",
    )
    monkeypatch.setattr(
        release,
        "ensure_publish_release_commit_is_head",
        lambda commit: calls.append(("head", commit)),
    )
    monkeypatch.setattr(
        release,
        "require_installed_cli_carries_release_tree",
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
        ("installed", tmp_path),
        ("clean", tmp_path),
        "constitution",
        "0.9.0",
        ("publish", "0.9.0", None, "head"),
    ]


def test_every_gate_running_mode_reaches_the_gates_through_one_shared_body(
    tmp_path, monkeypatch, capsys
):
    # Every mode that runs the gates must call run_release_gates itself, not
    # merely end up at the same leaf gates. That is what makes the check honest:
    # an edit to what a release verifies changes what every mode verifies in the
    # same edit, because there is only one sequence to edit.
    gate_calls = []

    def reached_a_leaf_gate_directly(*args, **kwargs):
        # Without this, re-inlining the sequence would run the real constitution
        # gate -- `uv run pytest` over the whole suite -- from inside a worker,
        # and the failure would arrive minutes later as a process deadline.
        raise AssertionError(
            "release modes must reach the gates through run_release_gates"
        )

    def record_gate_run(root, choose_version):
        # Resolving the seam is the point: it reports which version each mode
        # asked the artifact gate to build.
        version = choose_version()
        gate_calls.append((root, version))
        return version

    monkeypatch.setattr(release, "repo_root", lambda: tmp_path)
    monkeypatch.setattr(release, "ensure_clean_worktree", lambda root: None)
    monkeypatch.setattr(release, "current_version", lambda: "0.9.0")
    monkeypatch.setattr(release, "bump_version", lambda bump: f"1.0.0-from-{bump}")
    monkeypatch.setattr(release, "clean_build_artifacts", reached_a_leaf_gate_directly)
    monkeypatch.setattr(release, "run_constitution_gate", reached_a_leaf_gate_directly)
    monkeypatch.setattr(release, "run_artifact_gate", reached_a_leaf_gate_directly)
    monkeypatch.setattr(release, "run_release_gates", record_gate_run)
    monkeypatch.setattr(release, "ensure_notes_file", lambda notes_file: None)
    monkeypatch.setattr(release, "ensure_release_preconditions", lambda root: None)
    monkeypatch.setattr(release, "run", lambda command: None)
    monkeypatch.setattr(release, "print_prepare_instructions", lambda version: None)
    monkeypatch.setattr(
        release, "release_commit_for_target", lambda version, target: "head"
    )
    monkeypatch.setattr(
        release, "ensure_publish_release_commit_is_head", lambda commit: None
    )
    monkeypatch.setattr(
        release,
        "publish_release",
        lambda version, notes_file, *, release_commit=None: None,
    )
    parser = build_release_parser()

    results = [
        release._handle_release_from_root(parser.parse_args(argv), tmp_path)
        for argv in (["check"], ["publish"], ["prepare", "minor"], ["patch"])
    ]

    assert results == [0, 0, 0, 0]
    # check and publish verify the version already in the tree; prepare and
    # release verify the version they are about to ship.
    assert gate_calls == [
        (tmp_path, "0.9.0"),
        (tmp_path, "0.9.0"),
        (tmp_path, "1.0.0-from-minor"),
        (tmp_path, "1.0.0-from-patch"),
    ]
    assert "release gates passed for 0.9.0" in capsys.readouterr().out


def test_release_cleanup_removes_stale_build_and_distribution_trees(tmp_path):
    (tmp_path / "build" / "lib" / "spice").mkdir(parents=True)
    (tmp_path / "build" / "lib" / "spice" / "config.py").write_text(
        "stale = True\n", encoding="utf-8"
    )
    (tmp_path / "dist").mkdir()
    (tmp_path / "dist" / "stale.whl").write_text("stale\n", encoding="utf-8")
    (tmp_path / "keep.txt").write_text("keep\n", encoding="utf-8")

    release.clean_build_artifacts(tmp_path)

    assert sorted(path.name for path in tmp_path.iterdir()) == ["keep.txt"]


def test_installed_cli_probe_uses_mounted_parent_outside_candidate_sys_path(
    tmp_path, monkeypatch
):
    python = tmp_path / "tool" / "bin" / "python"
    python.parent.mkdir(parents=True)
    python.write_text("# runtime identity\n", encoding="utf-8")
    root = tmp_path / "deployment"
    module = root / "spice" / "tasks" / "git" / "boundaries.py"
    module.parent.mkdir(parents=True)
    module.write_text("# installed module\n", encoding="utf-8")
    (root / "pyproject.toml").write_text(
        '[project]\nname = "spice-harness"\n', encoding="utf-8"
    )
    observed = {}

    def probe(command, *, capture=False, cwd=None, env=None):
        observed["command"] = command
        observed["capture"] = capture
        observed["cwd"] = cwd
        observed["pythonpath"] = env.get("PYTHONPATH")
        return subprocess.CompletedProcess(command, 0, stdout=f"{module}\n", stderr="")

    monkeypatch.setenv(release.RUNTIME_PYTHON_ENV, str(python))
    monkeypatch.setenv("PYTHONPATH", "/candidate-shadow")
    monkeypatch.setattr(release, "run", probe)
    monkeypatch.setattr(
        release,
        "_source_identity",
        lambda source_root: (
            ("installed-commit", "installed-tree")
            if source_root == root
            else (_ for _ in ()).throw(AssertionError(source_root))
        ),
    )

    installed = release._installed_cli_source()

    assert observed == {
        "command": [
            str(python.absolute()),
            "-P",
            "-c",
            release.INSTALLED_CLI_PROBE_SCRIPT,
        ],
        "capture": True,
        "cwd": Path("/"),
        "pythonpath": None,
    }
    assert installed == release.InstalledCliSource(
        python.absolute(),
        module.resolve(),
        root.resolve(),
        "installed-commit",
        "installed-tree",
    )


def test_release_refuses_branch_tree_the_installed_cli_does_not_carry(
    tmp_path, monkeypatch
):
    installed = release.InstalledCliSource(
        Path("/tool/python"),
        Path("/deployment/spice/tasks/git/boundaries.py"),
        Path("/deployment"),
        "installed-commit",
        "installed-tree",
    )
    monkeypatch.setattr(release, "_installed_cli_source", lambda: installed)
    monkeypatch.setattr(
        release,
        "_source_identity",
        lambda _root: ("candidate-commit", "candidate-tree"),
    )

    with pytest.raises(SpiceError, match="branch state has no fleet effect"):
        release.require_installed_cli_carries_release_tree(tmp_path)


def test_release_refuses_a_candidate_worktree_interpreter(tmp_path, monkeypatch):
    installed = release.InstalledCliSource(
        tmp_path / ".venv" / "bin" / "python",
        tmp_path / "spice" / "tasks" / "git" / "boundaries.py",
        tmp_path,
        "candidate-commit",
        "candidate-tree",
    )
    monkeypatch.setattr(release, "_installed_cli_source", lambda: installed)
    monkeypatch.setattr(
        release,
        "_source_identity",
        lambda _root: (_ for _ in ()).throw(AssertionError("must not self-certify")),
    )

    with pytest.raises(SpiceError, match="independently installed CLI"):
        release.require_installed_cli_carries_release_tree(tmp_path)


def test_release_accepts_the_tree_the_installed_cli_imports(
    tmp_path, monkeypatch, capsys
):
    installed = release.InstalledCliSource(
        Path("/tool/python"),
        Path("/deployment/spice/tasks/git/boundaries.py"),
        Path("/deployment"),
        "installed-commit",
        "shared-tree",
    )
    monkeypatch.setattr(release, "_installed_cli_source", lambda: installed)
    monkeypatch.setattr(
        release,
        "_source_identity",
        lambda _root: ("candidate-commit", "shared-tree"),
    )

    assert release.require_installed_cli_carries_release_tree(tmp_path) == installed
    assert "candidate candidate-commit and installed installed-commit" in (
        capsys.readouterr().out
    )


def _git(repo: Path, *args: str) -> None:
    subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True)


def test_prepare_artifacts_do_not_make_the_publish_handoff_dirty(tmp_path, monkeypatch):
    repo = tmp_path / "repo"
    subprocess.run(["git", "init", "-q", "-b", "main", str(repo)], check=True)
    _git(repo, "config", "user.email", "r@example.test")
    _git(repo, "config", "user.name", "Release Tester")
    (repo / ".gitignore").write_text(
        Path(".gitignore").read_text(encoding="utf-8"),
        encoding="utf-8",
    )
    (repo / "pyproject.toml").write_text(
        '[project]\nname = "release-fixture"\nversion = "0.9.0"\n',
        encoding="utf-8",
    )
    (repo / "uv.lock").write_text("version = 1\n", encoding="utf-8")
    _git(repo, "add", ".")
    _git(repo, "commit", "-qm", "init")

    original_run = release.run
    published = []

    def git_output(*args):
        return subprocess.run(
            ["git", "-C", str(repo), *args],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()

    def artifact_tools(command, **kwargs):
        if command[0] == "git":
            return original_run(command, **kwargs)
        if command[:2] == ["uv", "build"]:
            artifacts = repo / "dist"
            artifacts.mkdir()
            (artifacts / "spice_harness-0.9.1.tar.gz").write_bytes(b"sdist\n")
            (artifacts / "spice_harness-0.9.1-py3-none-any.whl").write_bytes(b"wheel\n")
        return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

    def bump_version(bump):
        assert bump == "patch"
        project = repo / "pyproject.toml"
        project.write_text(
            project.read_text(encoding="utf-8").replace("0.9.0", "0.9.1"),
            encoding="utf-8",
        )
        return "0.9.1"

    monkeypatch.setattr(release, "repo_root", lambda: repo)
    monkeypatch.setattr(release, "run", artifact_tools)
    monkeypatch.setattr(release, "ensure_release_preconditions", lambda root: None)
    monkeypatch.setattr(
        release, "require_installed_cli_carries_release_tree", lambda root: None
    )
    monkeypatch.setattr(release, "run_constitution_gate", lambda: None)
    monkeypatch.setattr(release, "bump_version", bump_version)
    monkeypatch.setattr(release, "current_version", lambda: "0.9.1")
    monkeypatch.setattr(
        release,
        "publish_release",
        lambda version, notes_file, *, release_commit=None: published.append(
            (version, notes_file, release_commit)
        ),
    )

    parser = build_release_parser()
    before = git_output("status", "--porcelain")
    assert release.handle_release(parser.parse_args(["prepare", "patch"])) == 0
    after_prepare = git_output("status", "--porcelain")
    assert (repo / "dist" / "spice_harness-0.9.1.tar.gz").is_file()
    assert (repo / "dist" / "spice_harness-0.9.1-py3-none-any.whl").is_file()

    assert release.handle_release(parser.parse_args(["publish"])) == 0

    head = git_output("rev-parse", "HEAD")
    assert published == [("0.9.1", None, head)]
    assert (after_prepare, git_output("status", "--porcelain")) == (before, before)


def test_release_constitution_runs_executable_browser_gate(monkeypatch):
    calls = []
    monkeypatch.setattr(release, "run", lambda command: calls.append(command))
    monkeypatch.setattr(
        release, "run_browser_gate", lambda: calls.append("browser-gate")
    )

    release.run_constitution_gate()

    assert calls == [
        ["uv", "run", "pytest"],
        ["uv", "run", "ruff", "check", "."],
        "browser-gate",
    ]


def test_release_browser_gate_passes_canonical_playwright_config(tmp_path, monkeypatch):
    config_path = tmp_path / ".git" / ".spice" / "agents" / "playwright-mcp.json"
    calls = []

    monkeypatch.setenv("GIT_EDITOR", "preserved")
    monkeypatch.setattr(
        agent_driver,
        "write_playwright_mcp_config",
        lambda root: calls.append(("config", root)) or config_path,
    )
    monkeypatch.setattr(
        release,
        "run",
        lambda command, **kwargs: calls.append(("run", command, kwargs["env"])),
    )

    release.run_browser_gate(tmp_path)

    assert calls[0] == ("config", tmp_path)
    assert calls[1][0:2] == (
        "run",
        ["node", "tests/browser/run_release_smokes.js"],
    )
    assert calls[1][2][release.PLAYWRIGHT_MCP_CONFIG_ENV] == str(config_path)
    assert calls[1][2]["GIT_EDITOR"] == "preserved"


def test_release_runner_streams_or_captures_output_as_declared(capfd):
    streamed = release.run(
        [sys.executable, "-c", "print('streamed release progress', flush=True)"],
        capture=False,
    )
    visible = capfd.readouterr()
    captured = release.run(
        [sys.executable, "-c", "print('captured release result', flush=True)"],
        capture=True,
    )
    after_capture = capfd.readouterr()

    assert {
        "streamed_stdout": streamed.stdout,
        "visible_stdout": visible.out,
        "captured_stdout": captured.stdout,
        "post_capture_stdout": after_capture.out,
    } == {
        "streamed_stdout": None,
        "visible_stdout": "streamed release progress\n",
        "captured_stdout": "captured release result\n",
        "post_capture_stdout": "",
    }


def test_release_browser_manifest_completeness_is_fast_and_executable():
    completed = subprocess.run(
        ["node", "tests/browser/run_release_smokes.js", "--check-manifest"],
        check=True,
        capture_output=True,
        text=True,
    )

    assert completed.stdout.strip() == "PASS release browser manifest completeness"


def test_publish_release_with_head_commit_uses_current_artifacts(monkeypatch):
    calls = []

    def fake_git(*args):
        calls.append(("git", args))
        if args == ("rev-parse", "HEAD"):
            return "head-commit"
        raise AssertionError(args)

    def fake_run(command, **kwargs):
        calls.append(("run", command, "UV_PUBLISH_TOKEN" in kwargs.get("env", {})))

    monkeypatch.setattr(release, "git", fake_git)
    monkeypatch.setattr(release, "run", fake_run)
    monkeypatch.setattr(release, "read_pypi_token", lambda: "pypi-token")
    monkeypatch.setattr(
        release, "wait_for_pypi", lambda version: calls.append(("pypi", version))
    )
    monkeypatch.setattr(
        release,
        "publish_github_release",
        lambda version, notes_file, *, release_commit=None: calls.append(
            ("github", version, notes_file, release_commit)
        ),
    )

    release.publish_release("0.9.0", release_commit="head-commit")

    assert calls == [
        ("git", ("rev-parse", "HEAD")),
        (
            "run",
            [
                "uv",
                "publish",
                "--dry-run",
                "dist/spice_harness-0.9.0.tar.gz",
                "dist/spice_harness-0.9.0-py3-none-any.whl",
            ],
            True,
        ),
        ("run", ["git", "push", "origin", "HEAD:main"], False),
        (
            "run",
            [
                "uv",
                "publish",
                "dist/spice_harness-0.9.0.tar.gz",
                "dist/spice_harness-0.9.0-py3-none-any.whl",
            ],
            True,
        ),
        ("pypi", "0.9.0"),
        ("github", "0.9.0", None, "head-commit"),
        ("run", ["git", "status", "--short", "--branch"], False),
    ]


def test_publish_github_release_uses_explicit_release_commit(monkeypatch):
    git_calls = []
    run_calls = []

    def fake_git(*args):
        git_calls.append(args)
        if args == ("tag", "--list", "v0.9.0"):
            return ""
        raise AssertionError(args)

    def fake_run(command, **_kwargs):
        run_calls.append(command)

    monkeypatch.setattr(release, "git", fake_git)
    monkeypatch.setattr(release, "run", fake_run)
    monkeypatch.setattr(
        release, "github_release_url", lambda tag: f"https://example.test/{tag}"
    )

    release.publish_github_release("0.9.0", release_commit="target-commit")

    assert git_calls == [("tag", "--list", "v0.9.0")]
    assert run_calls == [
        ["git", "tag", "-a", "v0.9.0", "target-commit", "-m", "release: v0.9.0"],
        ["git", "push", "origin", "v0.9.0"],
    ]


def test_hermetic_wheel_env_preserves_process_environment(monkeypatch):
    monkeypatch.setenv("PYTHONPATH", "/some/worktree")
    monkeypatch.setenv("VIRTUAL_ENV", "/some/venv")
    monkeypatch.setenv("PATH", "/usr/bin")

    env = release.hermetic_wheel_env()

    assert {name: env.get(name) for name in ("PATH", "PYTHONPATH", "VIRTUAL_ENV")} == {
        "PATH": "/usr/bin",
        "PYTHONPATH": "/some/worktree",
        "VIRTUAL_ENV": "/some/venv",
    }


def test_release_highlight_rewrites_commit_subjects_into_sentences():
    assert (
        edited_release_highlight("Fix speech excerpts for final ACK messages")
        == "Fixed speech excerpts for final ACK messages."
    )
    assert (
        edited_release_highlight("Expose release tooling as spice command")
        == "Added release tooling as spice command."
    )


def test_release_notes_group_edited_highlights_by_project():
    notes = render_release_notes(
        version="0.3.0",
        release_commit="abcdef1234567890",
        release_short="abcdef1",
        current_tag="v0.3.0",
        previous_tag="v0.2.1",
        records=[
            ReleaseRecord(
                commit="1111111aaaa",
                subject="Fix speech excerpts for final ACK messages",
                project="serve",
            ),
            ReleaseRecord(
                commit="2222222bbbb",
                subject="Expose release tooling as spice command",
                project="cli",
            ),
            ReleaseRecord(
                commit="3333333cccc",
                subject="Fix speech excerpts for final ACK messages",
                project="serve",
            ),
            ReleaseRecord(
                commit="4444444dddd",
                subject="Fix narration media session retention",
                project="serve.ui",
            ),
            ReleaseRecord(
                commit="7777777ffff",
                subject="serve: Fix menu MODEL-abc (review)",
                project="serve.ui",
            ),
            ReleaseRecord(
                commit="5555555eeee",
                subject="Implement dynamic agent shell-hook surfaces",
                project="task.cli",
            ),
            ReleaseRecord(
                commit="6666666ffff",
                subject="Show agent stem in active header pills",
                project="agent.019ec753620c7cf2b18c06707ac93cbb.task",
            ),
        ],
    )

    assert notes == (
        "> [!IMPORTANT]\n"
        "> **Draft release notes — curate Highlights before publishing.** Replace\n"
        "> the placeholder under _Highlights_ with a short summary, then delete this\n"
        "> banner. The generated task inventory is already wrapped in the collapsed\n"
        "> _Task-level changes_ section below; keep that section intact. Omit from\n"
        "> Highlights any feature that was added and then functionally reverted\n"
        "> within this same release window — a net-zero change is not a highlight.\n"
        "\n"
        "## Highlights\n"
        "\n"
        "_Replace this line with a short, curated set of highlights folded from "
        "the changes below._\n"
        "\n"
        "<details>\n"
        "<summary>Task-level changes</summary>\n"
        "\n"
        "## Changes by project\n"
        "\n"
        "### Serve\n"
        "\n"
        "- Fixed speech excerpts for final ACK messages. (1111111, 3333333)\n"
        "\n"
        "### CLI\n"
        "\n"
        "- Added release tooling as spice command. (2222222)\n"
        "\n"
        "### Serve UI\n"
        "\n"
        "- Fixed narration media session retention. (4444444)\n"
        "- Fixed menu MODEL-abc. (7777777)\n"
        "\n"
        "### Task CLI\n"
        "\n"
        "- Implement dynamic agent shell-hook surfaces. (5555555)\n"
        "\n"
        "### General\n"
        "\n"
        "- Show agent stem in active header pills. (6666666)\n"
        "\n"
        "</details>\n"
        "\n"
        "## Package Notes\n"
        "\n"
        "- PyPI release: `spice-harness==0.3.0`\n"
        "- Release commit: `abcdef1`\n"
        "- Commit range: `v0.2.1..abcdef1`\n"
        "- Commit source: first-parent history grouped by `Task-Project` metadata\n"
        "- Release tag: `v0.3.0`\n"
    )


def test_release_notes_drop_task_handle_named_by_its_own_task_key():
    # A real KEY-INCEPTED handle trailing the subject is redundant on GitHub,
    # which already links the bare short SHA. Notes strip the handle -- but only
    # the one this commit's own Task-Key names, never a handle-shaped guess.
    task_key = "1kCXrHTm"
    kwargs = dict(
        version="0.5.0",
        release_commit="abcdef1234567890",
        release_short="abcdef1",
        current_tag="v0.5.0",
        previous_tag="v0.4.0",
    )
    with_handle = render_release_notes(
        **kwargs,
        records=[
            ReleaseRecord(
                commit="9999999abcd",
                subject=(f"lifecycle: Add release-notes trimming NOTES-{task_key}"),
                project="lifecycle.notes",
                task_key=task_key,
            )
        ],
    )
    without_handle = render_release_notes(
        **kwargs,
        records=[
            ReleaseRecord(
                commit="9999999abcd",
                subject="lifecycle: Add release-notes trimming",
                project="lifecycle.notes",
                task_key=task_key,
            )
        ],
    )

    # The rendered entry carries the edited highlight and the auto-linkable short
    # SHA, and reads identically whether or not the subject still carried the
    # handle -- proof the trailing NOTES-<key> token contributed nothing.
    assert "- Added release-notes trimming. (9999999)" in with_handle
    assert with_handle == without_handle


def test_release_notes_open_with_a_draft_curation_scaffold():
    notes = render_release_notes(
        version="0.4.0",
        release_commit="abcdef1234567890",
        release_short="abcdef1",
        current_tag="v0.4.0",
        previous_tag="v0.3.0",
        records=[
            ReleaseRecord(
                commit="1111111aaaa",
                subject="Add a thing",
                project="cli",
            )
        ],
    )

    # The generated notes are a draft to curate, not a finished body: they lead
    # with a visible banner and an empty Highlights placeholder, while the raw
    # per-task export is already preserved in its final collapsed structure.
    assert notes.startswith("> [!IMPORTANT]\n")
    assert "Draft release notes — curate Highlights before publishing." in notes
    # The banner steers curators away from features added and then reverted in
    # the same window, so a net-zero change never lands in Highlights.
    assert "any feature that was added and then functionally reverted" in notes
    assert "a net-zero change is not a highlight." in notes
    banner = notes.index("> [!IMPORTANT]")
    highlights = notes.index("## Highlights")
    placeholder = notes.index("_Replace this line with a short, curated set")
    details = notes.index("<details>")
    summary = notes.index("<summary>Task-level changes</summary>")
    changes = notes.index("## Changes by project")
    details_end = notes.index("</details>")
    package_notes = notes.index("## Package Notes")
    assert (
        banner
        < highlights
        < placeholder
        < details
        < summary
        < changes
        < details_end
        < package_notes
    )
    # The raw grouped export sits inside the collapsed task-level appendix.
    assert "### CLI" in notes and notes.index("### CLI") > changes

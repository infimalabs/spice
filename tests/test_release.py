"""Release command parsing and release-note highlights."""

import json
import subprocess
import sys
from pathlib import Path

import pytest

import spice.agent.driver as agent_driver
import spice.release as release
from spice.errors import SpiceError
from spice.release import (
    ReleaseRecord,
    build_release_parser,
    edited_release_highlight,
    render_release_notes,
)


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
    monkeypatch.setattr(release, "git", lambda *args: "source-head")
    monkeypatch.setattr(release, "bump_version", lambda bump: f"1.0.0-from-{bump}")
    monkeypatch.setattr(
        release, "preview_bumped_version", lambda bump: f"1.0.0-from-{bump}"
    )
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
        release, "ensure_curated_release_notes", lambda notes_file, commit: None
    )
    monkeypatch.setattr(
        release,
        "publish_release",
        lambda version, notes_file, *, release_commit=None: None,
    )
    parser = build_release_parser()

    results = [
        release._handle_release_from_root(parser.parse_args(argv), tmp_path)
        for argv in (
            ["check"],
            ["publish", "--apply"],
            ["prepare", "minor", "--apply"],
            ["patch", "--apply"],
        )
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


def _commit_deployment(root: Path) -> None:
    """Make ``root`` a deployment checkout carrying nothing its HEAD lacks."""
    subprocess.run(["git", "init", "-q", str(root)], check=True)
    for key, value in (("user.email", "deploy@example.test"), ("user.name", "Deploy")):
        subprocess.run(["git", "-C", str(root), "config", key, value], check=True)
    subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)
    subprocess.run(["git", "-C", str(root), "commit", "-q", "-m", "deploy"], check=True)


def _editable_probe_payload(module: Path) -> str:
    return (
        json.dumps(
            {
                "artifact": "",
                "files": [],
                "module": str(module),
                "registry": False,
                "version": "0.30.0",
            }
        )
        + "\n"
    )


def _tagged_release_candidate(root: Path, version: str, payload: str) -> Path:
    module = root / "spice" / "tasks" / "git" / "boundaries.py"
    module.parent.mkdir(parents=True)
    module.write_text(payload, encoding="utf-8")
    (root / "pyproject.toml").write_text(
        f'[project]\nname = "spice-harness"\nversion = "{version}"\n',
        encoding="utf-8",
    )
    _commit_deployment(root)
    subprocess.run(
        ["git", "-C", str(root), "tag", f"v{version}"],
        check=True,
        capture_output=True,
    )
    return module


def _registry_install(
    root: Path,
    version: str,
    payload: str,
) -> tuple[Path, Path]:
    tool = root / "tool"
    subprocess.run(
        [sys.executable, "-m", "venv", "--without-pip", str(tool)],
        check=True,
        capture_output=True,
    )
    python = tool / "bin" / "python"
    purelib = Path(
        subprocess.run(
            [
                str(python),
                "-P",
                "-c",
                "import sysconfig; print(sysconfig.get_paths()['purelib'])",
            ],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
    )
    module = purelib / "spice" / "tasks" / "git" / "boundaries.py"
    module.parent.mkdir(parents=True)
    module.write_text(payload, encoding="utf-8")
    dist_info = purelib / f"spice_harness-{version}.dist-info"
    dist_info.mkdir()
    (dist_info / "METADATA").write_text(
        f"Metadata-Version: 2.4\nName: spice-harness\nVersion: {version}\n",
        encoding="utf-8",
    )
    module_record = module.relative_to(purelib).as_posix()
    (dist_info / "RECORD").write_text(
        f"{module_record},,\n{dist_info.name}/METADATA,,\n{dist_info.name}/RECORD,,\n",
        encoding="utf-8",
    )
    return python, module


def test_registry_install_without_direct_url_matches_the_tagged_release(
    tmp_path, monkeypatch, capsys
):
    payload = "# released payload\n"
    candidate = tmp_path / "candidate"
    _tagged_release_candidate(candidate, "0.30.0", payload)
    python, module = _registry_install(
        tmp_path / "registry",
        "0.30.0",
        payload,
    )
    monkeypatch.setenv(release.RUNTIME_PYTHON_ENV, str(python))

    installed = release._installed_cli_identity()

    assert isinstance(installed, release.InstalledCliRegistry)
    assert installed.python == python.absolute()
    assert installed.module == module.resolve()
    assert installed.version == "0.30.0"
    assert release.require_installed_cli_matches_release(candidate) == installed
    output = capsys.readouterr().out
    assert "candidate tag v0.30.0" in output
    assert "installed spice-harness==0.30.0" in output

    ran = []
    monkeypatch.setattr(release, "repo_root", lambda: candidate)
    monkeypatch.setattr(
        release,
        "clean_build_artifacts",
        lambda root: ran.append(("clean", root)),
    )
    monkeypatch.setattr(
        release,
        "run_constitution_gate",
        lambda: ran.append("constitution"),
    )
    monkeypatch.setattr(release, "current_version", lambda: "0.30.0")
    monkeypatch.setattr(
        release,
        "run_artifact_gate",
        lambda version: ran.append(("artifact", version)),
    )

    check = build_release_parser().parse_args(["check"])
    assert release.handle_release(check) == 0
    assert ran == [
        ("clean", candidate),
        "constitution",
        ("artifact", "0.30.0"),
    ]
    assert "release gates passed for 0.30.0" in capsys.readouterr().out


def test_registry_install_refuses_an_untagged_candidate(tmp_path, monkeypatch):
    payload = "# released payload\n"
    candidate = tmp_path / "candidate"
    _tagged_release_candidate(candidate, "0.30.0", payload)
    subprocess.run(
        ["git", "-C", str(candidate), "tag", "--delete", "v0.30.0"],
        check=True,
        capture_output=True,
    )
    python, _module = _registry_install(
        tmp_path / "registry",
        "0.30.0",
        payload,
    )
    monkeypatch.setenv(release.RUNTIME_PYTHON_ENV, str(python))

    with pytest.raises(SpiceError) as raised:
        release.require_installed_cli_matches_release(candidate)

    message = str(raised.value)
    assert "requires the checked-out release tag" in message
    assert "candidate tag v0.30.0" in message
    assert "installed spice-harness==0.30.0" in message


def test_registry_install_refuses_a_different_version_with_both_identities(
    tmp_path, monkeypatch
):
    candidate = tmp_path / "candidate"
    _tagged_release_candidate(candidate, "0.30.0", "# candidate payload\n")
    python, _module = _registry_install(
        tmp_path / "registry",
        "0.29.0",
        "# installed payload\n",
    )
    monkeypatch.setenv(release.RUNTIME_PYTHON_ENV, str(python))

    with pytest.raises(SpiceError) as raised:
        release.require_installed_cli_matches_release(candidate)

    message = str(raised.value)
    assert "candidate tag v0.30.0" in message
    assert "installed spice-harness==0.29.0" in message


def test_registry_install_refuses_different_payload_with_both_artifacts(
    tmp_path, monkeypatch
):
    candidate = tmp_path / "candidate"
    _tagged_release_candidate(candidate, "0.30.0", "# candidate payload\n")
    python, _module = _registry_install(
        tmp_path / "registry",
        "0.30.0",
        "# tampered installed payload\n",
    )
    monkeypatch.setenv(release.RUNTIME_PYTHON_ENV, str(python))

    with pytest.raises(SpiceError) as raised:
        release.require_installed_cli_matches_release(candidate)

    message = str(raised.value)
    assert "candidate tag v0.30.0" in message
    assert "installed spice-harness==0.30.0" in message
    assert message.count("artifact sha256:") == 2


def test_registry_install_refuses_an_extra_runtime_payload_path(tmp_path, monkeypatch):
    payload = "# released payload\n"
    candidate = tmp_path / "candidate"
    _tagged_release_candidate(candidate, "0.30.0", payload)
    python, module = _registry_install(
        tmp_path / "registry",
        "0.30.0",
        payload,
    )
    (module.parents[3] / "spice" / "injected.py").write_text(
        "# not carried by the release tag\n",
        encoding="utf-8",
    )
    monkeypatch.setenv(release.RUNTIME_PYTHON_ENV, str(python))

    with pytest.raises(SpiceError) as raised:
        release.require_installed_cli_matches_release(candidate)

    message = str(raised.value)
    assert "payload paths" in message
    assert "candidate tag v0.30.0" in message
    assert "installed spice-harness==0.30.0" in message


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
    _commit_deployment(root)
    observed = {}
    real_run = release.run

    def probe(command, *, capture=False, cwd=None, env=None):
        # Only the interpreter probe is faked. Git runs for real so the
        # deployment's own drift is measured rather than asserted.
        if command[0] == "git":
            return real_run(command, capture=capture, cwd=cwd, env=env)
        observed["command"] = command
        observed["capture"] = capture
        observed["cwd"] = cwd
        observed["pythonpath"] = env.get("PYTHONPATH")
        return subprocess.CompletedProcess(
            command,
            0,
            stdout=_editable_probe_payload(module),
            stderr="",
        )

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

    installed = release._installed_cli_identity()

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


def test_release_refuses_a_deployment_carrying_uncommitted_edits(tmp_path, monkeypatch):
    """A matching committed identity must not vouch for a dirty deployment.

    The edit lands in the very module the probe resolves, which is what an
    editable install imports, and it leaves HEAD's tree hash untouched. Only
    reading the working tree can tell this deployment from a clean one.
    """
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
    _commit_deployment(root)
    committed_tree = _deployment_tree(root)
    module.write_text("# edited after deploy\n", encoding="utf-8")
    real_run = release.run

    def probe(command, *, capture=False, cwd=None, env=None):
        if command[0] == "git":
            return real_run(command, capture=capture, cwd=cwd, env=env)
        return subprocess.CompletedProcess(
            command,
            0,
            stdout=_editable_probe_payload(module),
            stderr="",
        )

    monkeypatch.setenv(release.RUNTIME_PYTHON_ENV, str(python))
    monkeypatch.setattr(release, "run", probe)

    assert _deployment_tree(root) == committed_tree
    with pytest.raises(SpiceError, match="dirty deployment executes code"):
        release.require_installed_cli_matches_release(tmp_path)


def _deployment_tree(root: Path) -> str:
    return subprocess.run(
        ["git", "-C", str(root), "rev-parse", "HEAD^{tree}"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


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
    monkeypatch.setattr(release, "_installed_cli_identity", lambda: installed)
    monkeypatch.setattr(
        release,
        "_source_identity",
        lambda _root: ("candidate-commit", "candidate-tree"),
    )

    with pytest.raises(SpiceError, match="branch state has no fleet effect"):
        release.require_installed_cli_matches_release(tmp_path)


def test_release_refuses_a_candidate_worktree_interpreter(tmp_path, monkeypatch):
    installed = release.InstalledCliSource(
        tmp_path / ".venv" / "bin" / "python",
        tmp_path / "spice" / "tasks" / "git" / "boundaries.py",
        tmp_path,
        "candidate-commit",
        "candidate-tree",
    )
    monkeypatch.setattr(release, "_installed_cli_identity", lambda: installed)
    monkeypatch.setattr(
        release,
        "_source_identity",
        lambda _root: (_ for _ in ()).throw(AssertionError("must not self-certify")),
    )

    with pytest.raises(SpiceError, match="independently installed CLI"):
        release.require_installed_cli_matches_release(tmp_path)


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
    monkeypatch.setattr(release, "_installed_cli_identity", lambda: installed)
    monkeypatch.setattr(
        release,
        "_source_identity",
        lambda _root: ("candidate-commit", "shared-tree"),
    )

    assert release.require_installed_cli_matches_release(tmp_path) == installed
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
        release, "require_installed_cli_matches_release", lambda root: None
    )
    monkeypatch.setattr(release, "run_constitution_gate", lambda: None)
    monkeypatch.setattr(release, "bump_version", bump_version)
    monkeypatch.setattr(release, "preview_bumped_version", lambda bump: "0.9.1")
    monkeypatch.setattr(release, "current_version", lambda: "0.9.1")
    monkeypatch.setattr(
        release, "ensure_curated_release_notes", lambda notes_file, commit: None
    )
    monkeypatch.setattr(
        release,
        "publish_release",
        lambda version, notes_file, *, release_commit=None: published.append(
            (version, notes_file, release_commit)
        ),
    )

    parser = build_release_parser()
    before = git_output("status", "--porcelain")
    assert (
        release.handle_release(parser.parse_args(["prepare", "patch", "--apply"])) == 0
    )
    after_prepare = git_output("status", "--porcelain")
    assert (repo / "dist" / "spice_harness-0.9.1.tar.gz").is_file()
    assert (repo / "dist" / "spice_harness-0.9.1-py3-none-any.whl").is_file()

    assert release.handle_release(parser.parse_args(["publish", "--apply"])) == 0

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

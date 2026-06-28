import subprocess
import venv
from pathlib import Path

import pytest

from spice.cli.parser import build_parser
from spice.errors import SpiceError
from spice.studies import typecheck
from spice.studies import cli as studies_cli
from spice.studies.shape import (
    configured_package_roots,
    name_cluster_error,
    name_cluster_errors,
    name_cluster_threshold,
)
from spice.studies.typecheck import (
    PYRIGHT_ARGS,
    python_typecheck_argv,
    python_typecheck_interpreter,
    python_typecheck_targets,
    run_python_typecheck,
)


def _make_package(root: Path, name: str) -> None:
    pkg = root / name
    pkg.mkdir(parents=True, exist_ok=True)
    (pkg / "mod.py").write_text("x = 1\n", encoding="utf-8")


def test_package_roots_explicit_policy_overrides_derivation(tmp_path):
    _make_package(tmp_path, "chosen")
    _make_package(tmp_path, "ignored")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.policy]\npackage_roots = ["chosen"]\n'
        '[tool.setuptools.packages.find]\ninclude = ["ignored*"]\n',
        encoding="utf-8",
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "chosen"]


def test_package_roots_derived_from_setuptools_find_excludes_non_python(tmp_path):
    _make_package(tmp_path, "app")
    # A build artifact matching the include glob but carrying no Python.
    (tmp_path / "app.egg-info").mkdir()
    (tmp_path / "app.egg-info" / "PKG-INFO").write_text("meta\n", encoding="utf-8")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.setuptools.packages.find]\nwhere = ["."]\ninclude = ["app*"]\n',
        encoding="utf-8",
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "app"]


def test_package_roots_derived_from_explicit_packages_list_keeps_top_level(tmp_path):
    _make_package(tmp_path, "app")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.setuptools]\npackages = ["app", "app.sub", "app.sub.deep"]\n',
        encoding="utf-8",
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "app"]


def test_package_roots_derived_from_poetry_packages(tmp_path):
    _make_package(tmp_path / "src", "pkg")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.poetry]\nname = "pkg"\n'
        '[[tool.poetry.packages]]\ninclude = "pkg"\nfrom = "src"\n',
        encoding="utf-8",
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "src" / "pkg"]


def test_package_roots_derived_from_poetry_name(tmp_path):
    _make_package(tmp_path, "widget")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.poetry]\nname = "widget"\n', encoding="utf-8"
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "widget"]


def test_package_roots_derived_from_hatch_wheel_packages(tmp_path):
    _make_package(tmp_path / "src", "gadget")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.hatch.build.targets.wheel]\npackages = ["src/gadget"]\n',
        encoding="utf-8",
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "src" / "gadget"]


def test_package_roots_derived_from_hatch_build_include(tmp_path):
    _make_package(tmp_path / "packages", "gadget")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.hatch.build]\ninclude = ["packages/gadget"]\n',
        encoding="utf-8",
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "packages" / "gadget"]


def test_package_roots_derived_from_flit_module(tmp_path):
    _make_package(tmp_path, "thing")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.flit.module]\nname = "thing"\n', encoding="utf-8"
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "thing"]


def test_package_roots_derived_from_pdm_includes(tmp_path):
    _make_package(tmp_path, "core")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.pdm.build]\nincludes = ["core"]\n', encoding="utf-8"
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "core"]


def test_package_roots_derived_from_src_layout(tmp_path):
    _make_package(tmp_path / "src", "lib")
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "anything"\n', encoding="utf-8"
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "src" / "lib"]


def test_package_roots_derived_from_project_name(tmp_path):
    _make_package(tmp_path, "app")
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "app"\n', encoding="utf-8"
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "app"]


def test_package_roots_empty_when_truly_underivable(tmp_path):
    _make_package(tmp_path, "app")
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "different"\n', encoding="utf-8"
    )

    assert configured_package_roots(tmp_path) == []


def test_package_roots_malformed_poetry_packages_fails_loudly(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[tool.poetry]\nname = "x"\npackages = "notalist"\n', encoding="utf-8"
    )

    with pytest.raises(SpiceError):
        configured_package_roots(tmp_path)


def test_python_typecheck_targets_follow_package_roots(tmp_path):
    _make_package(tmp_path, "app")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.setuptools.packages.find]\nwhere = ["."]\ninclude = ["app*"]\n',
        encoding="utf-8",
    )

    assert python_typecheck_targets(tmp_path) == ("app",)


def test_python_typecheck_targets_empty_without_a_package(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "x"\n', encoding="utf-8"
    )

    assert python_typecheck_targets(tmp_path) == ()


def test_python_typecheck_argv_appends_fixed_flags_and_targets(tmp_path, monkeypatch):
    monkeypatch.delenv("VIRTUAL_ENV", raising=False)

    argv = python_typecheck_argv(tmp_path, ("app",))

    assert argv[-len(PYRIGHT_ARGS) - 1 :] == (*PYRIGHT_ARGS, "app")
    assert "pyright" in " ".join(argv)
    assert "--pythonpath" not in argv


def test_python_typecheck_interpreter_uses_configured_override(tmp_path, monkeypatch):
    monkeypatch.delenv("VIRTUAL_ENV", raising=False)
    python = _write_fake_python(tmp_path / "tools" / "python")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.policy]\npython_typecheck_interpreter = "tools/python"\n',
        encoding="utf-8",
    )

    assert python_typecheck_interpreter(tmp_path) == python


def test_python_typecheck_interpreter_rejects_missing_configured_override(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.policy]\npython_typecheck_interpreter = "missing/python"\n',
        encoding="utf-8",
    )

    with pytest.raises(SpiceError, match="does not exist"):
        python_typecheck_interpreter(tmp_path)


def test_python_typecheck_interpreter_prefers_repo_local_virtual_env(
    tmp_path, monkeypatch
):
    active = _write_fake_python(tmp_path / "active-env" / "bin" / "python")
    _write_fake_python(tmp_path / ".venv" / "bin" / "python")
    monkeypatch.setenv("VIRTUAL_ENV", str(active.parents[1]))

    assert python_typecheck_interpreter(tmp_path) == active


def test_python_typecheck_interpreter_uses_dot_venv(tmp_path, monkeypatch):
    monkeypatch.delenv("VIRTUAL_ENV", raising=False)
    python = _write_fake_python(tmp_path / ".venv" / "bin" / "python")

    assert python_typecheck_interpreter(tmp_path) == python


def test_python_typecheck_interpreter_ignores_foreign_virtual_env(
    tmp_path, monkeypatch
):
    foreign = tmp_path.parent / "foreign-env"
    _write_fake_python(foreign / "bin" / "python")
    python = _write_fake_python(tmp_path / ".venv" / "bin" / "python")
    monkeypatch.setenv("VIRTUAL_ENV", str(foreign))

    assert python_typecheck_interpreter(tmp_path) == python


def test_python_typecheck_interpreter_resolves_uv_project(tmp_path, monkeypatch):
    monkeypatch.delenv("VIRTUAL_ENV", raising=False)
    python = _write_fake_python(tmp_path / "uv-env" / "bin" / "python")
    (tmp_path / "uv.lock").write_text("", encoding="utf-8")
    monkeypatch.setattr(typecheck, "find_tool", lambda name: "/usr/bin/uv")

    def fake_run(argv, **kwargs):
        assert argv[:6] == [
            "/usr/bin/uv",
            "run",
            "--directory",
            str(tmp_path),
            "--project",
            str(tmp_path),
        ]
        assert kwargs["cwd"] == tmp_path
        return subprocess.CompletedProcess(argv, 0, stdout=f"{python}\n", stderr="")

    monkeypatch.setattr(typecheck.subprocess, "run", fake_run)

    assert python_typecheck_interpreter(tmp_path) == python


def test_python_typecheck_argv_uses_detected_interpreter(tmp_path, monkeypatch):
    monkeypatch.delenv("VIRTUAL_ENV", raising=False)
    python = _write_fake_python(tmp_path / ".venv" / "bin" / "python")

    argv = python_typecheck_argv(tmp_path, ("app",))

    assert "--pythonpath" in argv
    assert argv[argv.index("--pythonpath") + 1] == str(python)


def test_run_python_typecheck_resolves_imports_from_repo_venv(tmp_path, monkeypatch):
    monkeypatch.delenv("VIRTUAL_ENV", raising=False)
    _make_package(tmp_path, "app")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.policy]\npackage_roots = ["app"]\n',
        encoding="utf-8",
    )
    _make_real_venv_with_package(tmp_path, "thirdparty")
    (tmp_path / "app" / "mod.py").write_text(
        "from thirdparty import VALUE\n\nvalue: int = VALUE\n",
        encoding="utf-8",
    )

    assert run_python_typecheck(tmp_path) is None


def test_run_python_typecheck_noops_without_targets(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "x"\n', encoding="utf-8"
    )

    assert run_python_typecheck(tmp_path) is None


def _write_fake_python(path: Path) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("#!/bin/sh\n", encoding="utf-8")
    path.chmod(0o755)
    return path


def _make_real_venv_with_package(repo_root: Path, package_name: str) -> None:
    builder = venv.EnvBuilder(with_pip=False)
    builder.create(repo_root / ".venv")
    python = python_typecheck_interpreter(repo_root)
    assert python is not None
    result = subprocess.run(
        [
            str(python),
            "-c",
            "import site; print(site.getsitepackages()[0])",
        ],
        capture_output=True,
        text=True,
        check=True,
    )
    site_packages = Path(result.stdout.strip())
    package = site_packages / package_name
    package.mkdir()
    (package / "__init__.py").write_text("VALUE: int = 1\n", encoding="utf-8")


def _name_cluster_repo(tmp_path: Path, names: list[str]) -> Path:
    pkg = tmp_path / "app"
    pkg.mkdir()
    for name in names:
        (pkg / f"{name}.py").write_text("x = 1\n", encoding="utf-8")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.policy]\npackage_roots = ["app"]\n', encoding="utf-8"
    )
    return tmp_path


def test_name_cluster_threshold_defaults_to_four(tmp_path):
    _name_cluster_repo(
        tmp_path, ["teamcommands", "teamfilters", "teammetrics", "teammailboxes"]
    )

    assert name_cluster_threshold(tmp_path) == 4
    error = name_cluster_error(tmp_path)
    assert "4 sibling modules" in error
    assert "prefix 'team'" in error


def test_name_cluster_threshold_can_be_configured_to_three(tmp_path):
    _name_cluster_repo(tmp_path, ["teamcommands", "teamfilters", "teammetrics"])
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.policy]\npackage_roots = ["app"]\nname_cluster_threshold = 3\n',
        encoding="utf-8",
    )

    assert name_cluster_threshold(tmp_path) == 3
    error = name_cluster_error(tmp_path)
    assert "3 sibling modules" in error
    assert "prefix 'team'" in error


def test_name_cluster_threshold_can_be_configured_to_four(tmp_path):
    _name_cluster_repo(
        tmp_path, ["teamcommands", "teamfilters", "teammetrics", "teammailboxes"]
    )
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.policy]\npackage_roots = ["app"]\nname_cluster_threshold = 4\n',
        encoding="utf-8",
    )

    assert name_cluster_threshold(tmp_path) == 4
    error = name_cluster_error(tmp_path)
    assert "4 sibling modules" in error
    assert "prefix 'team'" in error


def test_name_cluster_flags_shared_prefix_run(tmp_path):
    _name_cluster_repo(
        tmp_path, ["teamcommands", "teamfilters", "teammetrics", "teammailboxes"]
    )

    error = name_cluster_error(tmp_path)
    assert "name-cluster policy violation" in error
    assert "prefix 'team'" in error
    assert "namespace subpackage" in error


def test_name_cluster_flags_shared_suffix_run(tmp_path):
    _name_cluster_repo(
        tmp_path, ["onepayload", "twopayload", "redpayload", "bluepayload"]
    )

    assert "suffix 'payload'" in name_cluster_error(tmp_path)


def test_name_cluster_passes_two_siblings(tmp_path):
    _name_cluster_repo(tmp_path, ["teamcommands", "teamfilters"])

    assert name_cluster_errors(tmp_path) == []


def test_name_cluster_ignores_short_affix(tmp_path):
    _name_cluster_repo(tmp_path, ["webapp", "webcli", "webrun"])

    assert name_cluster_errors(tmp_path) == []


def test_study_shape_cli_fails_on_name_cluster(tmp_path, monkeypatch, capsys):
    _name_cluster_repo(
        tmp_path, ["teamcommands", "teamfilters", "teammetrics", "teammailboxes"]
    )
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    args = build_parser().parse_args(["study", "shape"])

    assert args.func(args) == 1
    assert "name-cluster policy violation" in capsys.readouterr().out

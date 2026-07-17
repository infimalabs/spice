import pytest

from spice import paths
from spice.cli.parser import build_parser
from spice.serve.browser.artifacts import (
    SERVE_BROWSER_ARTIFACT_DIR,
    SERVE_BROWSER_ARTIFACT_RELATIVE_DIR,
    serve_browser_artifact_path,
)
from spice.serve.cli import run_serve_browser_artifact_path
from spice.tasks import config as task_config

ARTIFACT_FILENAME = "composer-smoke.png"
EXIT_OK = 0


@pytest.fixture(autouse=True)
def reset_backend_overrides():
    yield
    paths.set_state_backend(None)
    task_config.set_backend(None)


def test_serve_browser_artifact_path_creates_dedicated_parent(tmp_path):
    path = serve_browser_artifact_path(ARTIFACT_FILENAME, root=tmp_path)

    assert path == tmp_path / SERVE_BROWSER_ARTIFACT_DIR / ARTIFACT_FILENAME
    assert path.parent.is_dir()


def test_serve_browser_artifact_path_cli_prints_destination(
    tmp_path, monkeypatch, capsys
):
    monkeypatch.chdir(tmp_path)
    args = build_parser().parse_args(
        ["serve", "browser-artifact-path", ARTIFACT_FILENAME]
    )

    result = args.func(args)

    assert args.func is run_serve_browser_artifact_path
    assert result == EXIT_OK
    assert capsys.readouterr().out.strip() == str(
        tmp_path / SERVE_BROWSER_ARTIFACT_DIR / ARTIFACT_FILENAME
    )


def test_serve_browser_artifact_path_cli_uses_total_backend(
    tmp_path, monkeypatch, capsys
):
    live = tmp_path / "live"
    live.mkdir()
    backend = tmp_path / "scratch"
    monkeypatch.chdir(live)
    args = build_parser().parse_args(
        [
            "serve",
            "--backend",
            str(backend),
            "browser-artifact-path",
            ARTIFACT_FILENAME,
        ]
    )

    result = args.func(args)

    expected = (
        paths.worktree_runtime_state_root(live)
        / SERVE_BROWSER_ARTIFACT_RELATIVE_DIR
        / ARTIFACT_FILENAME
    )
    assert result == EXIT_OK
    assert capsys.readouterr().out.strip() == str(expected)
    assert expected.parent.is_dir()
    assert list(live.iterdir()) == []
